//! Configuration loading from YAML, JSON, and environment variables.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use crate::config::{
    DisplayConfig, GatewayConfig, LlmProviderConfig, ProxyConfig, TerminalBackendType,
    TerminalConfig, TerminalHomeMode, WebConfig,
};
use crate::merge::{deep_merge, merge_configs};
use crate::paths;

const MANAGED_SCOPE_ENV: &str = "HERMES_MANAGED_DIR";
#[cfg(not(test))]
const DEFAULT_MANAGED_SCOPE_DIR: &str = "/etc/hermes";

// ---------------------------------------------------------------------------
// ConfigError conversion helpers
// ---------------------------------------------------------------------------

/// Helper function to convert serde_yaml::Error to ConfigError (avoids orphan rule).
fn yaml_to_config_error(e: serde_yaml::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert serde_json::Error to ConfigError (avoids orphan rule).
fn json_to_config_error(e: serde_json::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

/// Helper function to convert std::io::Error to ConfigError (avoids orphan rule).
fn io_to_config_error(e: std::io::Error) -> ConfigError {
    ConfigError::ParseError(e.to_string())
}

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn target_parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn atomic_temp_path(path: &Path) -> Result<PathBuf, ConfigError> {
    let parent = target_parent(path);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            ConfigError::ParseError(format!("invalid config path: {}", path.display()))
        })?;
    let nonce = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), nonce)))
}

/// Atomically write bytes to a path by writing a sibling temp file then renaming.
pub fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let parent = target_parent(path);
    std::fs::create_dir_all(parent).map_err(io_to_config_error)?;
    let tmp_path = atomic_temp_path(path)?;

    let result = (|| -> Result<(), ConfigError> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&tmp_path).map_err(io_to_config_error)?;
        file.write_all(bytes).map_err(io_to_config_error)?;
        file.sync_all().map_err(io_to_config_error)?;
        drop(file);
        replace_temp_file(&tmp_path, path)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn replace_temp_file(tmp_path: &Path, path: &Path) -> Result<(), ConfigError> {
    match std::fs::rename(tmp_path, path) {
        Ok(()) => Ok(()),
        Err(err) if is_cross_device_rename_error(&err) => {
            std::fs::copy(tmp_path, path).map_err(io_to_config_error)?;
            let _ = std::fs::remove_file(tmp_path);
            Ok(())
        }
        Err(err) => Err(io_to_config_error(err)),
    }
}

fn is_cross_device_rename_error(err: &std::io::Error) -> bool {
    let code = err.raw_os_error();
    #[cfg(unix)]
    if code == Some(18) {
        return true;
    }
    #[cfg(windows)]
    if code == Some(17) {
        return true;
    }
    false
}

#[cfg(unix)]
fn config_chmod_enabled() -> bool {
    let env_opt_out = ["HERMES_SKIP_CHMOD", "HERMES_CONTAINER"]
        .iter()
        .any(|key| std::env::var(key).is_ok_and(|value| !value.trim().is_empty()));
    !env_opt_out && !Path::new("/.dockerenv").exists()
}

#[cfg(unix)]
fn secure_config_file(path: &Path) -> Result<(), ConfigError> {
    if !config_chmod_enabled() {
        return Ok(());
    }
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(io_to_config_error)?
        .permissions();
    if permissions.mode() & 0o777 != 0o600 {
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions).map_err(io_to_config_error)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn secure_config_file(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

/// Crash-safe JSON write compatible with upstream `utils.atomic_json_write`.
pub fn atomic_json_write(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let bytes = serde_json::to_vec(value).map_err(json_to_config_error)?;
    atomic_write_bytes(path, &bytes)
}

/// Crash-safe pretty JSON write for user-facing state files.
pub fn atomic_json_write_pretty(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(json_to_config_error)?;
    atomic_write_bytes(path, &bytes)
}

/// Crash-safe YAML write with optional trailing content, matching Python setup helpers.
pub fn atomic_yaml_write(
    path: &Path,
    value: &serde_yaml::Value,
    extra_content: Option<&str>,
) -> Result<(), ConfigError> {
    let mut yaml = serde_yaml::to_string(value)
        .map(|yaml| normalize_yaml_sequence_indent(&yaml))
        .map_err(yaml_to_config_error)?;
    if let Some(extra) = extra_content {
        yaml.push_str(extra);
    }
    atomic_write_bytes(path, yaml.as_bytes())
}

fn normalize_yaml_sequence_indent(yaml: &str) -> String {
    let mut out = String::with_capacity(yaml.len());
    let mut previous_mapping_indent: Option<usize> = None;
    let mut active_sequence_shift: Option<(usize, usize, usize)> = None; // parent, original, shift
    for line in yaml.lines() {
        let trimmed = line.trim_start();
        let indent = line.len().saturating_sub(trimmed.len());
        let mut emitted = false;
        if let Some((parent_indent, original_indent, shift)) = active_sequence_shift {
            if trimmed.starts_with("- ") && indent == original_indent {
                out.push_str(&" ".repeat(shift));
                out.push_str(line);
                emitted = true;
            } else if !trimmed.is_empty() && indent <= parent_indent {
                active_sequence_shift = None;
            } else if indent > original_indent {
                out.push_str(&" ".repeat(shift));
                out.push_str(line);
                emitted = true;
            }
        }
        if trimmed.starts_with("- ") {
            if let Some(parent_indent) = previous_mapping_indent {
                if indent <= parent_indent {
                    let target_indent = parent_indent + 2;
                    let shift = target_indent.saturating_sub(indent);
                    out.push_str(&" ".repeat(target_indent));
                    out.push_str(trimmed);
                    active_sequence_shift = Some((parent_indent, indent, shift));
                    emitted = true;
                }
            }
        }
        if !emitted {
            out.push_str(line);
        }
        out.push('\n');
        if !trimmed.is_empty() && trimmed.ends_with(':') && !trimmed.starts_with("- ") {
            previous_mapping_indent = Some(indent);
        } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
            previous_mapping_indent = None;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// load_dotenv
// ---------------------------------------------------------------------------

/// Load user and project dotenv files into the process environment.
///
/// Behavior mirrors Python parity:
/// - `$HERMES_HOME/.env` overrides stale shell-exported values.
/// - `./.env` is sanitized and loaded as a fallback source.
/// - When user env exists, project env only fills missing keys.
pub fn load_dotenv() {
    let env_file = paths::env_path();
    let project_env = std::env::current_dir().ok().map(|p| p.join(".env"));

    if env_file.exists() {
        sanitize_env_file_if_needed(&env_file);
        // User env should override stale shell exports.
        load_dotenv_file(&env_file, true);
    }

    if let Some(project_env) = project_env.filter(|p| p.exists()) {
        sanitize_env_file_if_needed(&project_env);
        // Project env only fills gaps when user env exists; otherwise it can override.
        load_dotenv_file(&project_env, !env_file.exists());
    }

    if let Some(managed_env) = managed_scope_dir()
        .map(|dir| dir.join(".env"))
        .filter(|path| path.exists())
    {
        // Administrator-pinned env always wins over shell/user/project dotenv.
        load_dotenv_file(&managed_env, true);
    }
}

fn load_dotenv_file(path: &Path, override_existing: bool) {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            let value = strip_quotes(value.trim());
            if override_existing || std::env::var(key).is_err() {
                // SAFETY: called during startup/config loading.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}

fn sanitize_env_file_if_needed(path: &Path) {
    let Ok(original) = std::fs::read_to_string(path) else {
        return;
    };
    let sanitized = sanitize_env_lines(&original);
    if sanitized != original {
        let _ = atomic_write_bytes(path, sanitized.as_bytes());
    }
}

fn sanitize_env_lines(contents: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in contents.lines() {
        out.extend(split_concatenated_assignments(line));
    }
    let mut sanitized = out.join("\n");
    if contents.ends_with('\n') {
        sanitized.push('\n');
    }
    sanitized
}

fn split_concatenated_assignments(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return vec![line.to_string()];
    }

    let mut segments = Vec::new();
    let mut current = trimmed.to_string();
    loop {
        let Some((key, value)) = current.split_once('=') else {
            segments.push(current);
            break;
        };
        let key = key.trim();
        if key.is_empty() {
            segments.push(current);
            break;
        }
        if let Some(split_idx) = find_embedded_assignment_start(value) {
            let left = value[..split_idx].trim_end();
            segments.push(format!("{key}={left}"));
            current = value[split_idx..].trim_start().to_string();
            continue;
        }
        segments.push(format!("{key}={}", value.trim()));
        break;
    }
    segments
}

fn find_embedded_assignment_start(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_uppercase() {
            let mut j = i + 1;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'=' {
                let key = &value[i..j];
                if key.len() >= 4 && key.contains('_') {
                    return Some(i);
                }
            }
        }
        i += 1;
    }
    None
}

fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Return the active managed-scope directory, if configured.
///
/// Python Hermes treats `$HERMES_MANAGED_DIR` as an optional administrator
/// overlay and falls back to `/etc/hermes` in production. Unit tests skip the
/// default path so a developer machine's real managed install cannot leak into
/// deterministic tests.
pub fn managed_scope_dir() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(MANAGED_SCOPE_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty() && path.is_dir())
    {
        return Some(path);
    }

    #[cfg(not(test))]
    {
        let path = PathBuf::from(DEFAULT_MANAGED_SCOPE_DIR);
        if path.is_dir() {
            return Some(path);
        }
    }

    None
}

fn load_managed_config_yaml_value() -> Option<serde_yaml::Value> {
    let path = managed_scope_dir()?.join("config.yaml");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!("managed scope: failed to read {}: {err}", path.display());
            return None;
        }
    };
    if contents.trim().is_empty() {
        return None;
    }

    let mut root: serde_yaml::Value = match serde_yaml::from_str(&contents) {
        Ok(root) => root,
        Err(err) => {
            tracing::warn!("managed scope: failed to parse {}: {err}", path.display());
            return None;
        }
    };
    expand_env_vars_in_yaml(&mut root);
    let serde_yaml::Value::Mapping(ref mut map) = root else {
        tracing::warn!(
            "managed scope: ignoring non-mapping config at {}",
            path.display()
        );
        return None;
    };
    crate::python_yaml_compat::normalize_config_yaml_root(map);
    mark_platform_enabled_explicit(&mut root, "slack");
    mark_platform_enabled_explicit(&mut root, "ntfy");
    Some(root)
}

fn load_managed_config_overlay_json() -> Option<serde_json::Value> {
    let yaml = load_managed_config_yaml_value()?;
    match serde_json::to_value(yaml) {
        Ok(value) if value.is_object() => Some(value),
        Ok(_) => None,
        Err(err) => {
            tracing::warn!("managed scope: failed to convert config overlay: {err}");
            None
        }
    }
}

fn read_config_yaml_as_json(path: &Path) -> Option<serde_json::Value> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => return None,
    };
    if contents.trim().is_empty() {
        return Some(serde_json::json!({}));
    }
    match serde_yaml::from_str::<serde_json::Value>(&contents) {
        Ok(value) if value.is_object() => Some(value),
        Ok(_) => {
            tracing::warn!("Ignoring non-mapping config at {}", path.display());
            None
        }
        Err(err) => {
            tracing::warn!("Failed to parse {}: {err}", path.display());
            None
        }
    }
}

/// Load raw user config plus managed-scope overlay for standalone runtime readers.
///
/// This intentionally returns a raw value instead of [`GatewayConfig`] so
/// narrow subsystems can keep reading upstream-compatible keys that the typed
/// config model does not own, while still honoring administrator-pinned leaves.
pub fn load_effective_config_yaml_value(path: &Path) -> Option<serde_json::Value> {
    let ignore_user_config = env_truthy("HERMES_IGNORE_USER_CONFIG");
    let mut base = if ignore_user_config {
        None
    } else {
        read_config_yaml_as_json(path)
    }
    .unwrap_or_else(|| serde_json::json!({}));
    let had_user = base.as_object().is_some_and(|obj| !obj.is_empty());

    let managed = load_managed_config_overlay_json();
    if let Some(overlay) = &managed {
        deep_merge(&mut base, overlay);
    }

    if had_user || managed.is_some() {
        Some(base)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// load_config
// ---------------------------------------------------------------------------

/// Load the full configuration, applying the priority chain:
///
///   managed config/env > env vars > .env > cli-config.yaml > config.yaml > gateway.json > defaults
///
/// If `home_dir` is provided it overrides the `HERMES_HOME` env var.
pub fn load_config(home_dir: Option<&str>) -> Result<GatewayConfig, ConfigError> {
    // Load .env before anything else so env overrides see those values.
    load_dotenv();

    // Determine effective hermes home
    let effective_home = home_dir
        .map(|s| s.to_string())
        .or_else(|| std::env::var("HERMES_HOME").ok())
        .unwrap_or_else(|| paths::hermes_home().to_string_lossy().to_string());

    let config_yaml_path = Path::new(&effective_home).join("config.yaml");
    let cli_config_yaml_path = Path::new(&effective_home).join("cli-config.yaml");
    let gateway_json_path = Path::new(&effective_home).join("gateway.json");
    let ignore_user_config = env_truthy("HERMES_IGNORE_USER_CONFIG");

    // Start from defaults
    let mut config = GatewayConfig::default();

    // Layer 1: gateway.json (lowest priority file source)
    if !ignore_user_config && gateway_json_path.exists() {
        match load_from_json(&gateway_json_path) {
            Ok(json_cfg) => config = json_cfg,
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", gateway_json_path.display());
            }
        }
    }

    // Layer 2: config.yaml (higher priority file source)
    if !ignore_user_config && config_yaml_path.exists() {
        match load_from_yaml(&config_yaml_path) {
            Ok(yaml_cfg) => {
                config = merge_configs(&yaml_cfg, &config);
            }
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", config_yaml_path.display());
            }
        }
    }

    // Layer 2.5: cli-config.yaml (CLI-specific + high-priority overlay)
    if !ignore_user_config && cli_config_yaml_path.exists() {
        match load_from_yaml(&cli_config_yaml_path) {
            Ok(cli_cfg) => {
                config = merge_configs(&cli_cfg, &config);
            }
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", cli_config_yaml_path.display());
            }
        }
    }

    bridge_terminal_config_to_env(&config.terminal);
    bridge_web_config_to_env(&config.web);
    bridge_display_config_to_env(&config.display);

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);
    normalize_platform_aliases(&mut config);
    normalize_provider_secrets(&mut config);

    let managed_overlay = load_managed_config_overlay_json();
    if let Some(overlay) = &managed_overlay {
        let mut managed_config = config.clone();
        if apply_managed_config_overlay(&mut managed_config, overlay) {
            normalize_platform_aliases(&mut managed_config);
            normalize_provider_secrets(&mut managed_config);
            match validate_config(&managed_config) {
                Ok(()) => {
                    config = managed_config;
                    bridge_managed_overlay_to_env(&config, overlay);
                }
                Err(err) => {
                    tracing::warn!("managed scope: ignoring invalid config overlay: {err}");
                }
            }
        }
    }

    // Record the effective home dir
    config.home_dir = Some(effective_home);

    // Validate
    validate_config(&config)?;

    Ok(config)
}

fn apply_managed_config_overlay(config: &mut GatewayConfig, overlay: &serde_json::Value) -> bool {
    let original = config.clone();
    let mut base = match serde_json::to_value(&*config) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("managed scope: failed to serialize base config: {err}");
            return false;
        }
    };
    deep_merge(&mut base, overlay);
    match serde_json::from_value::<GatewayConfig>(base) {
        Ok(mut merged) => {
            merged.home_dir = original.home_dir;
            *config = merged;
            true
        }
        Err(err) => {
            tracing::warn!("managed scope: failed to apply config overlay: {err}");
            *config = original;
            false
        }
    }
}

fn bridge_managed_overlay_to_env(config: &GatewayConfig, overlay: &serde_json::Value) {
    if overlay.get("model").is_some() {
        if let Some(model) = config.model.as_deref() {
            set_env_override("HERMES_MODEL", model.to_string());
        }
    }
    if overlay.get("personality").is_some() {
        if let Some(personality) = config.personality.as_deref() {
            set_env_override("HERMES_PERSONALITY", personality.to_string());
        }
    }
    if overlay.get("max_turns").is_some() {
        set_env_override("HERMES_MAX_TURNS", config.max_turns.to_string());
    }
    if overlay.get("system_prompt").is_some() {
        if let Some(prompt) = config.system_prompt.as_deref() {
            set_env_override("HERMES_SYSTEM_PROMPT", prompt.to_string());
        }
    }
    if overlay.get("prefill_messages_file").is_some() {
        if let Some(path) = config.prefill_messages_file.as_deref() {
            set_env_override("HERMES_PREFILL_MESSAGES_FILE", path.to_string());
        }
    }
    if let Some(kanban) = overlay.get("kanban").and_then(|value| value.as_object()) {
        if kanban.contains_key("dispatch_in_gateway") {
            set_env_override(
                "HERMES_KANBAN_DISPATCH_IN_GATEWAY",
                config.kanban.dispatch_in_gateway.to_string(),
            );
        }
    }
    if let Some(security) = overlay.get("security").and_then(|value| value.as_object()) {
        if security.contains_key("allow_private_urls") {
            set_env_override(
                "HERMES_ALLOW_PRIVATE_URLS",
                config.security.allow_private_urls.to_string(),
            );
        }
    }
    bridge_managed_section_to_env(
        &config.terminal,
        overlay.get("terminal"),
        terminal_config_env_bridge_pairs(),
    );
    bridge_managed_section_to_env(
        &config.web,
        overlay.get("web"),
        web_config_env_bridge_pairs(),
    );
    bridge_managed_section_to_env(
        &config.display,
        overlay.get("display"),
        display_config_env_bridge_pairs(),
    );
}

fn bridge_managed_section_to_env<T: serde::Serialize>(
    config: &T,
    overlay: Option<&serde_json::Value>,
    pairs: &[(&str, &str)],
) {
    let Some(overlay_map) = overlay.and_then(|value| value.as_object()) else {
        return;
    };
    let config_value = match serde_json::to_value(config) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!("managed scope: failed to bridge config section to env: {err}");
            return;
        }
    };
    for (overlay_key, env_key) in pairs {
        if !overlay_map.contains_key(*overlay_key) {
            continue;
        }
        let config_key = match *overlay_key {
            "env_type" => "backend",
            "cwd" => "workdir",
            key => key,
        };
        if let Some(value) = config_value
            .get(config_key)
            .and_then(json_value_to_env_string)
        {
            set_env_override(env_key, value);
        }
    }
}

fn json_value_to_env_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        serde_json::Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(json_value_to_env_string)
                .collect::<Vec<_>>()
                .join(",");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

fn set_env_override(name: &str, value: String) {
    if value.trim().is_empty() {
        return;
    }
    // SAFETY: configuration loading runs during CLI/gateway startup.
    unsafe { std::env::set_var(name, value) };
}

// ---------------------------------------------------------------------------
// load_from_yaml
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a YAML file.
pub fn load_from_yaml(path: &Path) -> Result<GatewayConfig, ConfigError> {
    load_from_yaml_inner(path, true)
}

fn load_from_yaml_preserving_env_refs(path: &Path) -> Result<GatewayConfig, ConfigError> {
    load_from_yaml_inner(path, false)
}

fn load_from_yaml_inner(path: &Path, expand_env_refs: bool) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let mut root: serde_yaml::Value =
        serde_yaml::from_str(&contents).map_err(yaml_to_config_error)?;
    if expand_env_refs {
        expand_env_vars_in_yaml(&mut root);
    }
    if let serde_yaml::Value::Mapping(ref mut m) = root {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    mark_platform_enabled_explicit(&mut root, "slack");
    mark_platform_enabled_explicit(&mut root, "ntfy");
    let mut config: GatewayConfig = serde_yaml::from_value(root).map_err(yaml_to_config_error)?;
    normalize_platform_aliases(&mut config);
    Ok(config)
}

fn expand_env_vars_in_yaml(value: &mut serde_yaml::Value) {
    match value {
        serde_yaml::Value::String(s) => {
            *s = expand_env_refs_in_str(s);
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                expand_env_vars_in_yaml(item);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for value in map.values_mut() {
                expand_env_vars_in_yaml(value);
            }
        }
        _ => {}
    }
}

fn expand_env_refs_in_str(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let name = &after_open[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            match std::env::var(name) {
                Ok(value) => output.push_str(&value),
                Err(_) => {
                    output.push_str("${");
                    output.push_str(name);
                    output.push('}');
                }
            }
        } else {
            output.push_str("${");
            output.push_str(name);
            output.push('}');
        }
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    output
}

fn mark_platform_enabled_explicit(root: &mut serde_yaml::Value, platform: &str) {
    let serde_yaml::Value::Mapping(root_map) = root else {
        return;
    };
    let platforms_key = serde_yaml::Value::String("platforms".to_string());
    let platform_key = serde_yaml::Value::String(platform.to_string());
    let enabled_key = serde_yaml::Value::String("enabled".to_string());
    let extra_key = serde_yaml::Value::String("extra".to_string());
    let marker_key = serde_yaml::Value::String("_enabled_explicit".to_string());

    let Some(serde_yaml::Value::Mapping(platforms)) = root_map.get_mut(&platforms_key) else {
        return;
    };
    let Some(serde_yaml::Value::Mapping(platform_block)) = platforms.get_mut(&platform_key) else {
        return;
    };
    if !platform_block.contains_key(&enabled_key) {
        return;
    }

    let extra_entry = platform_block
        .entry(extra_key)
        .or_insert_with(|| serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    if let serde_yaml::Value::Mapping(extra_map) = extra_entry {
        extra_map.insert(marker_key, serde_yaml::Value::Bool(true));
    }
}

fn normalize_platform_aliases(config: &mut GatewayConfig) {
    normalize_discord_allow_from_alias(config);
}

fn normalize_discord_allow_from_alias(config: &mut GatewayConfig) {
    let Some(discord) = config.platforms.get_mut("discord") else {
        return;
    };
    if !discord.allowed_users.is_empty() {
        return;
    }

    let alias_users = {
        let direct = discord.extra.get("allow_from");
        let nested = discord
            .extra
            .get("extra")
            .and_then(|value| value.get("allow_from"));
        direct
            .and_then(json_value_to_string_list)
            .or_else(|| nested.and_then(json_value_to_string_list))
            .filter(|users| !users.is_empty())
    };

    if let Some(users) = alias_users {
        discord.allowed_users = users;
    }
}

fn json_value_to_string_list(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::String(raw) => Some(comma_separated_values(raw)),
        serde_json::Value::Array(items) => {
            let values = items
                .iter()
                .filter_map(json_scalar_to_string)
                .flat_map(|raw| comma_separated_values(&raw))
                .collect::<Vec<_>>();
            Some(values)
        }
        _ => json_scalar_to_string(value).map(|raw| comma_separated_values(&raw)),
    }
}

fn json_scalar_to_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(raw) => Some(raw.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn comma_separated_values(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Load `config.yaml` from disk if it exists; otherwise return defaults (no env merge).
pub fn load_user_config_file(path: &Path) -> Result<GatewayConfig, ConfigError> {
    if path.exists() {
        let mut cfg = load_from_yaml_preserving_env_refs(path)?;
        normalize_platform_aliases(&mut cfg);
        normalize_provider_secrets(&mut cfg);
        validate_config(&cfg)?;
        Ok(cfg)
    } else {
        Ok(GatewayConfig::default())
    }
}

fn normalize_provider_secrets(config: &mut GatewayConfig) {
    for provider in config.llm_providers.values_mut() {
        if provider
            .api_key
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.api_key = None;
        }
        if provider
            .api_key_env
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.api_key_env = None;
        }
        if provider
            .base_url
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.base_url = None;
        }
        if provider
            .oauth_token_url
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.oauth_token_url = None;
        }
        if provider
            .oauth_client_id
            .as_ref()
            .is_some_and(|v| v.trim().is_empty())
        {
            provider.oauth_client_id = None;
        }
    }

    config.llm_providers.retain(|_, provider| {
        provider.api_key.is_some()
            || provider.api_key_env.is_some()
            || provider.base_url.is_some()
            || provider.command.is_some()
            || !provider.args.is_empty()
            || provider.model.is_some()
            || !provider.models.is_empty()
            || !provider.discover_models
            || provider.api_mode.is_some()
            || provider.max_tokens.is_some()
            || provider.temperature.is_some()
            || provider.extra_body.is_some()
            || provider.rate_limit.is_some()
            || !provider.credential_pool.is_empty()
            || provider.request_timeout_seconds.is_some()
            || provider.oauth_token_url.is_some()
            || provider.oauth_client_id.is_some()
    });
}

const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, prefill_messages_file, model_switch.persist_switch_by_default, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, security.allow_private_urls, web.backend|search_backend|extract_backend|crawl_backend, display.busy_input_mode|busy_ack_enabled|memory_notifications, sessions.auto_prune|retention_days|vacuum_after_prune|min_interval_hours, kanban.dispatch_in_gateway, agent.api_max_retries, delegation.model|provider|base_url|api_key|max_spawn_depth, llm.<provider>.api_key|api_key_env|base_url|model|models|discover_models|api_mode|command|args|request_timeout_seconds|oauth_token_url|oauth_client_id, auxiliary.<task>.provider|model|base_url|api_key|timeout|download_timeout, smart_model_routing.enabled|max_simple_chars|max_simple_words|cheap_model.model|cheap_model.provider";

fn mask_secret(s: &str) -> String {
    if s.is_empty() {
        return "(empty)".to_string();
    }
    if s.len() <= 4 {
        "***".to_string()
    } else {
        format!("***{}", &s[s.len() - 4..])
    }
}

fn parse_config_bool(key: &str, value: &str) -> Result<bool, ConfigError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::ValidationError(format!(
            "{key} must be a boolean: {value}"
        ))),
    }
}

fn parse_positive_timeout_seconds(key: &str, value: &str) -> Result<f64, ConfigError> {
    let parsed: f64 = value.parse().map_err(|_| {
        ConfigError::ValidationError(format!("{key} must be a positive finite number: {value}"))
    })?;
    if parsed.is_finite() && parsed > 0.0 {
        Ok(parsed)
    } else {
        Err(ConfigError::ValidationError(format!(
            "{key} must be a positive finite number: {value}"
        )))
    }
}

fn normalize_provider_api_mode(value: &str) -> Result<String, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
    match normalized.as_str() {
        "chat_completions"
        | "anthropic_messages"
        | "codex_responses"
        | "bedrock_converse" => Ok(normalized),
        _ => Err(ConfigError::ValidationError(format!(
            "llm provider api_mode must be one of chat_completions, anthropic_messages, codex_responses, bedrock_converse: {}",
            value
        ))),
    }
}

/// Apply a single scalar field used by `hermes config set` (does not touch other keys).
///
/// Supports dotted keys aligned with `GatewayConfig`:
/// - `budget.max_result_size_chars`, `budget.max_aggregate_chars`
/// - `proxy.http` / `proxy.http_proxy`, `proxy.socks` / `proxy.socks_proxy`
/// - `llm.<provider>.api_key`, `llm.<provider>.base_url`, `llm.<provider>.model`
/// - `llm.<provider>.command`, `llm.<provider>.args`
pub fn apply_user_config_patch(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    if !key.contains('.') {
        return apply_user_config_patch_flat(config, key, value);
    }
    apply_user_config_patch_dotted(config, key, value)
}

fn apply_user_config_patch_flat(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    match key {
        "model" => {
            config.model = Some(value.to_string());
        }
        "personality" => {
            config.personality = Some(value.to_string());
        }
        "max_turns" => {
            config.max_turns = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "max_turns must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        "system_prompt" => {
            config.system_prompt = Some(value.to_string());
        }
        "prefill_messages_file" => {
            config.prefill_messages_file = Some(value.to_string());
        }
        other => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                other, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

fn apply_user_config_patch_dotted(
    config: &mut GatewayConfig,
    key: &str,
    value: &str,
) -> Result<(), ConfigError> {
    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => {
            config.budget.max_result_size_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_result_size_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["budget", "max_aggregate_chars"] => {
            config.budget.max_aggregate_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "budget.max_aggregate_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["proxy", "http"] | ["proxy", "http_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.http_proxy = Some(value.to_string());
        }
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => {
            let proxy = config.proxy.get_or_insert_with(ProxyConfig::default);
            proxy.socks_proxy = Some(value.to_string());
        }
        ["security", "allow_private_urls"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "security.allow_private_urls must be a boolean: {}",
                        value
                    )));
                }
            };
            config.security.allow_private_urls = parsed;
        }
        ["web", "backend"] => {
            config.web.backend = value.trim().to_string();
        }
        ["web", "search_backend"] => {
            config.web.search_backend = value.trim().to_string();
        }
        ["web", "extract_backend"] => {
            config.web.extract_backend = value.trim().to_string();
        }
        ["web", "crawl_backend"] => {
            config.web.crawl_backend = value.trim().to_string();
        }
        ["display", "busy_input_mode"] => {
            let normalized = match value.trim().to_ascii_lowercase().as_str() {
                "queue" | "queued" => "queue",
                "steer" | "steering" => "steer",
                "interrupt" | "interrupted" | "replace" | "" => "interrupt",
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "display.busy_input_mode must be one of interrupt, queue, steer: {value}"
                    )));
                }
            };
            config.display.busy_input_mode = Some(normalized.to_string());
        }
        ["display", "busy_ack_enabled"] => {
            config.display.busy_ack_enabled =
                Some(parse_config_bool("display.busy_ack_enabled", value)?);
        }
        ["display", "memory_notifications"] => {
            config.display.memory_notifications =
                Some(parse_config_bool("display.memory_notifications", value)?);
        }
        ["sessions", "auto_prune"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "sessions.auto_prune must be a boolean: {}",
                        value
                    )));
                }
            };
            config.sessions.auto_prune = parsed;
        }
        ["sessions", "retention_days"] => {
            config.sessions.retention_days = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "sessions.retention_days must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["sessions", "vacuum_after_prune"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "sessions.vacuum_after_prune must be a boolean: {}",
                        value
                    )));
                }
            };
            config.sessions.vacuum_after_prune = parsed;
        }
        ["sessions", "min_interval_hours"] => {
            config.sessions.min_interval_hours = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "sessions.min_interval_hours must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["kanban", "dispatch_in_gateway"] => {
            config.kanban.dispatch_in_gateway =
                parse_config_bool("kanban.dispatch_in_gateway", value)?;
        }
        ["model_switch", "persist_switch_by_default"] | ["model", "persist_switch_by_default"] => {
            config.model_switch.persist_switch_by_default =
                parse_config_bool("model_switch.persist_switch_by_default", value)?;
        }
        ["agent", "api_max_retries"] | ["agent", "apiMaxRetries"] => {
            config.agent.api_max_retries = Some(value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "agent.api_max_retries must be a non-negative integer: {value}"
                ))
            })?);
        }
        ["delegation", "model"] => {
            config.delegation.model = Some(value.to_string());
        }
        ["delegation", "provider"] => {
            config.delegation.provider = Some(value.to_string());
        }
        ["delegation", "base_url"] => {
            config.delegation.base_url = Some(value.to_string());
        }
        ["delegation", "api_key"] => {
            config.delegation.api_key = Some(value.to_string());
        }
        ["delegation", "max_spawn_depth"] => {
            config.delegation.max_spawn_depth = Some(value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "delegation.max_spawn_depth must be a non-negative integer: {value}"
                ))
            })?);
        }
        ["auxiliary", task, field] => {
            let entry = config
                .auxiliary
                .entry((*task).to_string())
                .or_insert_with(crate::config::AuxiliaryTaskConfig::default);
            match *field {
                "provider" => entry.provider = value.to_string(),
                "model" => entry.model = value.to_string(),
                "base_url" => entry.base_url = value.to_string(),
                "api_key" => entry.api_key = value.to_string(),
                "timeout" | "timeout_secs" => {
                    entry.timeout = Some(value.parse().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "auxiliary.{}.{} must be a non-negative integer: {}",
                            task, field, value
                        ))
                    })?);
                }
                "download_timeout" => {
                    entry.download_timeout = Some(value.parse().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "auxiliary.{}.download_timeout must be a non-negative integer: {}",
                            task, value
                        ))
                    })?);
                }
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown auxiliary field: auxiliary.{}.{} (supported: provider, model, base_url, api_key, timeout, download_timeout)",
                        task, other
                    )));
                }
            }
        }
        ["llm", provider, field] => {
            let entry = config
                .llm_providers
                .entry((*provider).to_string())
                .or_insert_with(LlmProviderConfig::default);
            match *field {
                "api_key" => entry.api_key = Some(value.to_string()),
                "api_key_env" => entry.api_key_env = Some(value.to_string()),
                "base_url" => entry.base_url = Some(value.to_string()),
                "model" => entry.model = Some(value.to_string()),
                "models" => {
                    entry.models = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "discover_models" => {
                    entry.discover_models =
                        parse_config_bool(&format!("llm.{}.discover_models", provider), value)?;
                }
                "api_mode" => entry.api_mode = Some(normalize_provider_api_mode(value)?),
                "max_tokens" | "max_output_tokens" => {
                    let parsed = value.parse::<u32>().map_err(|_| {
                        ConfigError::ValidationError(format!(
                            "llm.{}.{} must be a positive integer: {}",
                            provider, field, value
                        ))
                    })?;
                    if parsed == 0 {
                        return Err(ConfigError::ValidationError(format!(
                            "llm.{}.{} must be a positive integer: {}",
                            provider, field, value
                        )));
                    }
                    entry.max_tokens = Some(parsed);
                }
                "command" => entry.command = Some(value.to_string()),
                "args" => {
                    entry.args = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "request_timeout_seconds" => {
                    entry.request_timeout_seconds =
                        Some(parse_positive_timeout_seconds(key, value)?);
                }
                "oauth_token_url" => entry.oauth_token_url = Some(value.to_string()),
                "oauth_client_id" => entry.oauth_client_id = Some(value.to_string()),
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown llm field: llm.{}.{} (supported: api_key, api_key_env, base_url, model, models, discover_models, api_mode, max_tokens, max_output_tokens, command, args, request_timeout_seconds, oauth_token_url, oauth_client_id)",
                        provider, other
                    )));
                }
            }
        }
        ["smart_model_routing", "enabled"] => {
            let normalized = value.trim().to_ascii_lowercase();
            let parsed = match normalized.as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "smart_model_routing.enabled must be a boolean: {}",
                        value
                    )));
                }
            };
            config.smart_model_routing.enabled = parsed;
        }
        ["smart_model_routing", "max_simple_chars"] => {
            config.smart_model_routing.max_simple_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_chars must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "max_simple_words"] => {
            config.smart_model_routing.max_simple_words = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "smart_model_routing.max_simple_words must be a usize: {}",
                    value
                ))
            })?;
        }
        ["smart_model_routing", "cheap_model", "model"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.model = Some(value.to_string());
        }
        ["smart_model_routing", "cheap_model", "provider"] => {
            let cheap = config
                .smart_model_routing
                .cheap_model
                .get_or_insert_with(crate::CheapModelRouteConfig::default);
            cheap.provider = Some(value.to_string());
        }
        _ => {
            return Err(ConfigError::NotFound(format!(
                "unknown config key: {} (supported: {})",
                key, CONFIG_PATCH_HELP
            )));
        }
    }
    Ok(())
}

/// Display a single config field for `hermes config get` (same keys as [`apply_user_config_patch`]).
pub fn user_config_field_display(config: &GatewayConfig, key: &str) -> Result<String, ConfigError> {
    if !key.contains('.') {
        return Ok(match key {
            "model" => config
                .model
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "personality" => config
                .personality
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "max_turns" => config.max_turns.to_string(),
            "system_prompt" => config
                .system_prompt
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            "prefill_messages_file" => config
                .prefill_messages_file
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "(not set)".to_string()),
            other => {
                return Err(ConfigError::NotFound(format!(
                    "unknown config key: {} (supported: {})",
                    other, CONFIG_PATCH_HELP
                )));
            }
        });
    }

    let parts: Vec<&str> = key.split('.').collect();
    match parts.as_slice() {
        ["budget", "max_result_size_chars"] => Ok(config.budget.max_result_size_chars.to_string()),
        ["budget", "max_aggregate_chars"] => Ok(config.budget.max_aggregate_chars.to_string()),
        ["proxy", "http"] | ["proxy", "http_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.http_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["proxy", "socks"] | ["proxy", "socks_proxy"] => Ok(config
            .proxy
            .as_ref()
            .and_then(|p| p.socks_proxy.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["web", "backend"] => Ok(if config.web.backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.backend.clone()
        }),
        ["web", "search_backend"] => Ok(if config.web.search_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.search_backend.clone()
        }),
        ["web", "extract_backend"] => Ok(if config.web.extract_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.extract_backend.clone()
        }),
        ["web", "crawl_backend"] => Ok(if config.web.crawl_backend.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.web.crawl_backend.clone()
        }),
        ["display", "busy_input_mode"] => Ok(config
            .display
            .busy_input_mode
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "interrupt".to_string())),
        ["display", "busy_ack_enabled"] => Ok(config.display.busy_ack_enabled().to_string()),
        ["display", "memory_notifications"] => {
            Ok(config.display.memory_notifications_enabled().to_string())
        }
        ["sessions", "auto_prune"] => Ok(config.sessions.auto_prune.to_string()),
        ["sessions", "retention_days"] => Ok(config.sessions.retention_days.to_string()),
        ["sessions", "vacuum_after_prune"] => Ok(config.sessions.vacuum_after_prune.to_string()),
        ["sessions", "min_interval_hours"] => Ok(config.sessions.min_interval_hours.to_string()),
        ["kanban", "dispatch_in_gateway"] => Ok(config.kanban.dispatch_in_gateway.to_string()),
        ["model_switch", "persist_switch_by_default"] | ["model", "persist_switch_by_default"] => {
            Ok(config.model_switch.persist_switch_by_default.to_string())
        }
        ["agent", "api_max_retries"] | ["agent", "apiMaxRetries"] => Ok(config
            .agent
            .api_max_retries
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "model"] => Ok(config
            .delegation
            .model
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "provider"] => Ok(config
            .delegation
            .provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "base_url"] => Ok(config
            .delegation
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "api_key"] => Ok(config
            .delegation
            .api_key
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(mask_secret)
            .unwrap_or_else(|| "(not set)".to_string())),
        ["delegation", "max_spawn_depth"] => Ok(config
            .delegation
            .max_spawn_depth
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "api_key"] => Ok(
            match config
                .llm_providers
                .get(*provider)
                .and_then(|c| c.api_key.as_deref())
                .filter(|s| !s.is_empty())
            {
                Some(s) => mask_secret(s),
                None => "(not set)".to_string(),
            },
        ),
        ["llm", provider, "base_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.base_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "model"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "models"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.models.join(","))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "discover_models"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.discover_models.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "api_mode"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.api_mode.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "max_tokens"] | ["llm", provider, "max_output_tokens"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.max_tokens)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "command"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.command.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "args"] => Ok(config
            .llm_providers
            .get(*provider)
            .map(|c| c.args.join(","))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "request_timeout_seconds"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.request_timeout_seconds)
            .map(|value| value.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_token_url"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_token_url.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["llm", provider, "oauth_client_id"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.oauth_client_id.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "provider"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.provider.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "auto".to_string())),
        ["auxiliary", task, "model"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.model.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "base_url"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.base_url.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "api_key"] => Ok(config
            .auxiliary
            .get(*task)
            .map(|c| c.api_key.trim())
            .filter(|s| !s.is_empty())
            .map(mask_secret)
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "timeout"] | ["auxiliary", task, "timeout_secs"] => Ok(config
            .auxiliary
            .get(*task)
            .and_then(|c| c.timeout)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["auxiliary", task, "download_timeout"] => Ok(config
            .auxiliary
            .get(*task)
            .and_then(|c| c.download_timeout)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "enabled"] => Ok(config.smart_model_routing.enabled.to_string()),
        ["smart_model_routing", "max_simple_chars"] => {
            Ok(config.smart_model_routing.max_simple_chars.to_string())
        }
        ["smart_model_routing", "max_simple_words"] => {
            Ok(config.smart_model_routing.max_simple_words.to_string())
        }
        ["smart_model_routing", "cheap_model", "model"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        ["smart_model_routing", "cheap_model", "provider"] => Ok(config
            .smart_model_routing
            .cheap_model
            .as_ref()
            .and_then(|c| c.provider.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "(not set)".to_string())),
        _ => Err(ConfigError::NotFound(format!(
            "unknown config key: {} (supported: {})",
            key, CONFIG_PATCH_HELP
        ))),
    }
}

/// Serialize `GatewayConfig` to YAML. Creates parent directories. Omits `home_dir` from output.
pub fn save_config_yaml(path: &Path, config: &GatewayConfig) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_to_config_error)?;
    }
    let mut to_save = config.clone();
    to_save.home_dir = None;
    let yaml = serde_yaml::to_string(&to_save)
        .map(|yaml| normalize_yaml_sequence_indent(&yaml))
        .map_err(yaml_to_config_error)?;
    atomic_write_bytes(path, yaml.as_bytes())?;
    secure_config_file(path)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSetResult {
    pub config_path: Option<PathBuf>,
    pub env_path: Option<PathBuf>,
    pub env_key: Option<String>,
    pub config_key: Option<String>,
}

impl ConfigSetResult {
    pub fn wrote_config(&self) -> bool {
        self.config_path.is_some()
    }

    pub fn wrote_env(&self) -> bool {
        self.env_path.is_some()
    }
}

const EXPLICIT_ENV_CONFIG_KEYS: &[&str] = &[
    "OPENROUTER_API_KEY",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "WANDB_API_KEY",
    "TINKER_API_KEY",
    "HONCHO_API_KEY",
    "FIRECRAWL_API_KEY",
    "BROWSERBASE_API_KEY",
    "FAL_KEY",
    "SUDO_PASSWORD",
    "GITHUB_TOKEN",
    "TELEGRAM_BOT_TOKEN",
    "DISCORD_BOT_TOKEN",
    "SLACK_BOT_TOKEN",
    "SLACK_APP_TOKEN",
];

/// Python-compatible `hermes config set` persistence.
///
/// Secret-like all-caps keys are written to `$HERMES_HOME/.env`; normal dotted
/// keys are patched into `config.yaml` as raw YAML so list indices and future
/// upstream keys survive round-trips instead of being dropped by the typed
/// `GatewayConfig` serializer.
pub fn set_user_config_value(
    home_dir: &Path,
    key: &str,
    value: &str,
) -> Result<ConfigSetResult, ConfigError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(ConfigError::ValidationError(
            "config key must not be empty".to_string(),
        ));
    }

    let config_path = home_dir.join("config.yaml");
    let env_path = home_dir.join(".env");
    let bridge_env_key = config_env_bridge_key(key);
    let env_key = config_key_routes_to_env(key).or_else(|| bridge_env_key.clone());
    let writes_config = bridge_env_key.is_some() || env_key.is_none();

    let mut result = ConfigSetResult {
        config_path: None,
        env_path: None,
        env_key: None,
        config_key: None,
    };

    if writes_config {
        let mut root = load_user_config_yaml_value(&config_path)?;
        set_yaml_path(&mut root, &split_config_key(key), scalar_yaml_value(value))?;
        validate_user_config_value(&root)?;
        atomic_yaml_write(&config_path, &root, None)?;
        secure_config_file(&config_path)?;
        result.config_path = Some(config_path);
        result.config_key = Some(key.to_string());
    }

    if let Some(env_key) = env_key {
        save_env_key_value(&env_path, &env_key, value)?;
        // SAFETY: config writes run on the foreground CLI path.
        unsafe { std::env::set_var(&env_key, value) };
        result.env_path = Some(env_path);
        result.env_key = Some(env_key);
    }

    Ok(result)
}

fn canonical_env_key(key: &str) -> String {
    key.trim().replace(['.', '-'], "_").to_ascii_uppercase()
}

fn config_key_routes_to_env(key: &str) -> Option<String> {
    if key.contains('.') {
        return None;
    }
    let canonical = canonical_env_key(key);
    if EXPLICIT_ENV_CONFIG_KEYS.contains(&canonical.as_str())
        || canonical.ends_with("_API_KEY")
        || canonical.ends_with("_TOKEN")
        || canonical.starts_with("TERMINAL_SSH_")
    {
        Some(canonical)
    } else {
        None
    }
}

pub fn terminal_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("backend", "TERMINAL_ENV"),
        ("env_type", "TERMINAL_ENV"),
        ("workdir", "TERMINAL_CWD"),
        ("cwd", "TERMINAL_CWD"),
        ("timeout", "TERMINAL_TIMEOUT"),
        ("max_output_size", "TERMINAL_MAX_OUTPUT_SIZE"),
        ("docker_container_id", "TERMINAL_DOCKER_CONTAINER_ID"),
        ("docker_image", "TERMINAL_DOCKER_IMAGE"),
        (
            "docker_mount_cwd_to_workspace",
            "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
        ),
        (
            "docker_run_as_host_user",
            "TERMINAL_DOCKER_RUN_AS_HOST_USER",
        ),
        ("container_cpu", "TERMINAL_CONTAINER_CPU"),
        ("container_memory", "TERMINAL_CONTAINER_MEMORY"),
        ("container_disk", "TERMINAL_CONTAINER_DISK"),
        ("container_persistent", "TERMINAL_CONTAINER_PERSISTENT"),
        ("docker_env", "TERMINAL_DOCKER_ENV"),
        ("docker_forward_env", "TERMINAL_DOCKER_FORWARD_ENV"),
        ("docker_volumes", "TERMINAL_DOCKER_VOLUMES"),
        ("vercel_runtime", "TERMINAL_VERCEL_RUNTIME"),
        ("modal_mode", "TERMINAL_MODAL_MODE"),
        ("shell_init_files", "TERMINAL_SHELL_INIT_FILES"),
        ("auto_source_bashrc", "TERMINAL_AUTO_SOURCE_BASHRC"),
        ("home_mode", "TERMINAL_HOME_MODE"),
        ("ssh_host", "TERMINAL_SSH_HOST"),
        ("ssh_port", "TERMINAL_SSH_PORT"),
        ("ssh_user", "TERMINAL_SSH_USER"),
        ("ssh_key_path", "TERMINAL_SSH_KEY_PATH"),
    ]
}

pub fn terminal_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("terminal.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    terminal_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

pub fn web_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("backend", "HERMES_WEB_BACKEND"),
        ("search_backend", "HERMES_WEB_SEARCH_BACKEND"),
        ("extract_backend", "HERMES_WEB_EXTRACT_BACKEND"),
        ("crawl_backend", "HERMES_WEB_CRAWL_BACKEND"),
    ]
}

pub fn web_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("web.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    web_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

pub fn display_config_env_bridge_pairs() -> &'static [(&'static str, &'static str)] {
    &[
        ("busy_input_mode", "HERMES_GATEWAY_BUSY_INPUT_MODE"),
        ("busy_ack_enabled", "HERMES_GATEWAY_BUSY_ACK_ENABLED"),
        (
            "memory_notifications",
            "HERMES_MEMORY_NOTIFICATIONS_ENABLED",
        ),
    ]
}

pub fn display_config_env_bridge_key(key: &str) -> Option<&'static str> {
    let normalized = key
        .trim()
        .strip_prefix("display.")
        .unwrap_or_else(|| key.trim())
        .replace('-', "_")
        .to_ascii_lowercase();
    display_config_env_bridge_pairs()
        .iter()
        .find_map(|(config_key, env_key)| (*config_key == normalized).then_some(*env_key))
}

fn config_env_bridge_key(key: &str) -> Option<String> {
    terminal_config_env_bridge_key(key)
        .or_else(|| web_config_env_bridge_key(key))
        .or_else(|| display_config_env_bridge_key(key))
        .map(ToString::to_string)
}

fn split_config_key(key: &str) -> Vec<String> {
    key.split('.')
        .filter(|part| !part.is_empty())
        .enumerate()
        .map(|(index, part)| {
            if index == 0 && part == "llm" {
                "llm_providers".to_string()
            } else {
                part.to_string()
            }
        })
        .collect()
}

fn scalar_yaml_value(value: &str) -> serde_yaml::Value {
    if value.is_empty() {
        return serde_yaml::Value::String(String::new());
    }
    let trimmed = value.trim();
    match trimmed.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" => return serde_yaml::Value::Bool(true),
        "false" | "no" | "off" => return serde_yaml::Value::Bool(false),
        _ => {}
    }
    if let Ok(n) = trimmed.parse::<i64>() {
        return serde_yaml::Value::Number(n.into());
    }
    serde_yaml::Value::String(value.to_string())
}

fn load_user_config_yaml_value(path: &Path) -> Result<serde_yaml::Value, ConfigError> {
    if !path.exists() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    if contents.trim().is_empty() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    serde_yaml::from_str(&contents).map_err(yaml_to_config_error)
}

fn validate_user_config_value(root: &serde_yaml::Value) -> Result<(), ConfigError> {
    let mut normalized = root.clone();
    if let serde_yaml::Value::Mapping(ref mut m) = normalized {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    mark_platform_enabled_explicit(&mut normalized, "slack");
    mark_platform_enabled_explicit(&mut normalized, "ntfy");
    let mut cfg: GatewayConfig =
        serde_yaml::from_value(normalized).map_err(yaml_to_config_error)?;
    normalize_platform_aliases(&mut cfg);
    normalize_provider_secrets(&mut cfg);
    validate_config(&cfg)
}

fn set_yaml_path(
    current: &mut serde_yaml::Value,
    parts: &[String],
    new_value: serde_yaml::Value,
) -> Result<(), ConfigError> {
    if parts.is_empty() {
        *current = new_value;
        return Ok(());
    }

    let head = parts[0].as_str();
    let tail = &parts[1..];
    if let Ok(index) = head.parse::<usize>() {
        ensure_sequence(current);
        let serde_yaml::Value::Sequence(seq) = current else {
            unreachable!("ensure_sequence always leaves a sequence")
        };
        while seq.len() <= index {
            seq.push(default_container_for(tail));
        }
        if tail.is_empty() {
            seq[index] = new_value;
        } else {
            set_yaml_path(&mut seq[index], tail, new_value)?;
        }
        return Ok(());
    }

    ensure_mapping(current);
    let serde_yaml::Value::Mapping(map) = current else {
        unreachable!("ensure_mapping always leaves a mapping")
    };
    let key = serde_yaml::Value::String(head.to_string());
    if tail.is_empty() {
        map.insert(key, new_value);
    } else {
        let entry = map
            .entry(key)
            .or_insert_with(|| default_container_for(tail));
        set_yaml_path(entry, tail, new_value)?;
    }
    Ok(())
}

fn ensure_mapping(value: &mut serde_yaml::Value) {
    if !matches!(value, serde_yaml::Value::Mapping(_)) {
        *value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
}

fn ensure_sequence(value: &mut serde_yaml::Value) {
    if !matches!(value, serde_yaml::Value::Sequence(_)) {
        *value = serde_yaml::Value::Sequence(Vec::new());
    }
}

fn default_container_for(tail: &[String]) -> serde_yaml::Value {
    if tail
        .first()
        .is_some_and(|part| part.parse::<usize>().is_ok())
    {
        serde_yaml::Value::Sequence(Vec::new())
    } else {
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new())
    }
}

fn save_env_key_value(path: &Path, key: &str, value: &str) -> Result<(), ConfigError> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let sanitized_value = value.replace('\n', "\\n");
    let mut lines = Vec::new();
    let mut replaced = false;

    for line in original.lines() {
        let line_key = line
            .split_once('=')
            .map(|(k, _)| k.trim())
            .filter(|k| !k.is_empty());
        if line_key == Some(key) {
            if !replaced {
                lines.push(format!("{key}={sanitized_value}"));
                replaced = true;
            }
        } else {
            lines.push(line.to_string());
        }
    }

    if !replaced {
        lines.push(format!("{key}={sanitized_value}"));
    }

    let mut out = lines.join("\n");
    out.push('\n');
    atomic_write_bytes(path, out.as_bytes())
}

// ---------------------------------------------------------------------------
// load_from_json
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a JSON file.
pub fn load_from_json(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let mut config: GatewayConfig =
        serde_json::from_str(&contents).map_err(json_to_config_error)?;
    normalize_platform_aliases(&mut config);
    Ok(config)
}

/// Resolve the prefill JSON file using upstream-compatible precedence.
///
/// `prefill_messages_file` at the top level is canonical. The nested
/// `agent.prefill_messages_file` key remains a legacy fallback for older
/// generated configs.
pub fn resolve_prefill_messages_file(config: &GatewayConfig) -> Option<String> {
    env_var_nonempty("HERMES_PREFILL_MESSAGES_FILE")
        .or_else(|| trimmed_optional(config.prefill_messages_file.as_deref()))
        .or_else(|| trimmed_optional(config.agent.prefill_messages_file.as_deref()))
}

/// Resolve a configured prefill JSON path, mapping relative paths under HERMES_HOME.
pub fn resolve_prefill_messages_path(config: &GatewayConfig) -> Option<PathBuf> {
    let path = expand_home_path(&resolve_prefill_messages_file(config)?);
    if path.is_absolute() {
        return Some(path);
    }
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(paths::hermes_home);
    Some(home.join(path))
}

/// Load configured ephemeral prefill messages.
///
/// Invalid or missing files match Python behavior: warn and continue without
/// prefill rather than blocking startup.
pub fn load_prefill_messages(config: &GatewayConfig) -> Vec<hermes_core::Message> {
    let Some(path) = resolve_prefill_messages_path(config) else {
        return Vec::new();
    };
    load_prefill_messages_file(&path)
}

pub fn load_prefill_messages_file(path: &Path) -> Vec<hermes_core::Message> {
    if !path.exists() {
        tracing::warn!("Prefill messages file not found: {}", path.display());
        return Vec::new();
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) => {
            tracing::warn!(
                "Failed to load prefill messages from {}: {}",
                path.display(),
                err
            );
            return Vec::new();
        }
    };
    let value = match serde_json::from_str::<serde_json::Value>(&contents) {
        Ok(value) => value,
        Err(err) => {
            tracing::warn!(
                "Failed to parse prefill messages from {}: {}",
                path.display(),
                err
            );
            return Vec::new();
        }
    };
    if !value.is_array() {
        tracing::warn!(
            "Prefill messages file must contain a JSON array: {}",
            path.display()
        );
        return Vec::new();
    }
    match serde_json::from_value::<Vec<hermes_core::Message>>(value) {
        Ok(messages) => messages,
        Err(err) => {
            tracing::warn!(
                "Failed to parse prefill messages from {}: {}",
                path.display(),
                err
            );
            Vec::new()
        }
    }
}

fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn expand_home_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(trimmed)
}

fn env_var_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_bool_env(name: &str, value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => {
            tracing::warn!("{name} is not a valid bool-like value: {value}");
            None
        }
    }
}

fn parse_list_env(value: &str, split_colon: bool) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
        }
    }
    let delimiter = if trimmed.contains(',') || !split_colon {
        ','
    } else {
        ':'
    };
    trimmed
        .split(delimiter)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn set_env_if_missing(name: &str, value: String) {
    if value.trim().is_empty() || env_var_nonempty(name).is_some() {
        return;
    }
    // SAFETY: configuration loading runs during CLI/gateway startup.
    unsafe { std::env::set_var(name, value) };
}

fn bridge_terminal_config_to_env(terminal: &TerminalConfig) {
    let default = TerminalConfig::default();
    if terminal.backend != default.backend {
        set_env_if_missing(
            "TERMINAL_ENV",
            match terminal.backend {
                TerminalBackendType::Local => "local",
                TerminalBackendType::Docker => "docker",
                TerminalBackendType::Ssh => "ssh",
                TerminalBackendType::Daytona => "daytona",
                TerminalBackendType::Modal => "modal",
                TerminalBackendType::Singularity => "singularity",
            }
            .to_string(),
        );
    }
    if terminal.timeout != default.timeout {
        set_env_if_missing("TERMINAL_TIMEOUT", terminal.timeout.to_string());
    }
    if terminal.max_output_size != default.max_output_size {
        set_env_if_missing(
            "TERMINAL_MAX_OUTPUT_SIZE",
            terminal.max_output_size.to_string(),
        );
    }
    if let Some(value) = &terminal.workdir {
        set_env_if_missing("TERMINAL_CWD", value.clone());
    }
    if let Some(value) = &terminal.docker_container_id {
        set_env_if_missing("TERMINAL_DOCKER_CONTAINER_ID", value.clone());
    }
    if let Some(value) = &terminal.docker_image {
        set_env_if_missing("TERMINAL_DOCKER_IMAGE", value.clone());
    }
    if terminal.docker_mount_cwd_to_workspace != default.docker_mount_cwd_to_workspace {
        set_env_if_missing(
            "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
            terminal.docker_mount_cwd_to_workspace.to_string(),
        );
    }
    if terminal.docker_run_as_host_user != default.docker_run_as_host_user {
        set_env_if_missing(
            "TERMINAL_DOCKER_RUN_AS_HOST_USER",
            terminal.docker_run_as_host_user.to_string(),
        );
    }
    if let Some(value) = terminal.container_cpu {
        set_env_if_missing("TERMINAL_CONTAINER_CPU", value.to_string());
    }
    if let Some(value) = terminal.container_memory {
        set_env_if_missing("TERMINAL_CONTAINER_MEMORY", value.to_string());
    }
    if let Some(value) = terminal.container_disk {
        set_env_if_missing("TERMINAL_CONTAINER_DISK", value.to_string());
    }
    if terminal.container_persistent != default.container_persistent {
        set_env_if_missing(
            "TERMINAL_CONTAINER_PERSISTENT",
            terminal.container_persistent.to_string(),
        );
    }
    if let Some(value) = &terminal.docker_env {
        set_env_if_missing("TERMINAL_DOCKER_ENV", value.clone());
    }
    if !terminal.docker_forward_env.is_empty() {
        set_env_if_missing(
            "TERMINAL_DOCKER_FORWARD_ENV",
            terminal.docker_forward_env.join(","),
        );
    }
    if !terminal.docker_volumes.is_empty() {
        set_env_if_missing("TERMINAL_DOCKER_VOLUMES", terminal.docker_volumes.join(","));
    }
    if let Some(value) = &terminal.vercel_runtime {
        set_env_if_missing("TERMINAL_VERCEL_RUNTIME", value.clone());
    }
    if let Some(value) = &terminal.modal_mode {
        set_env_if_missing("TERMINAL_MODAL_MODE", value.clone());
    }
    if !terminal.shell_init_files.is_empty() {
        set_env_if_missing(
            "TERMINAL_SHELL_INIT_FILES",
            terminal.shell_init_files.join(","),
        );
    }
    if terminal.auto_source_bashrc != default.auto_source_bashrc {
        set_env_if_missing(
            "TERMINAL_AUTO_SOURCE_BASHRC",
            terminal.auto_source_bashrc.to_string(),
        );
    }
    if terminal.home_mode != default.home_mode {
        set_env_if_missing(
            "TERMINAL_HOME_MODE",
            terminal.home_mode.as_env_name().to_string(),
        );
    }
    if let Some(value) = &terminal.ssh_host {
        set_env_if_missing("TERMINAL_SSH_HOST", value.clone());
    }
    if let Some(value) = terminal.ssh_port {
        set_env_if_missing("TERMINAL_SSH_PORT", value.to_string());
    }
    if let Some(value) = &terminal.ssh_user {
        set_env_if_missing("TERMINAL_SSH_USER", value.clone());
    }
    if let Some(value) = &terminal.ssh_key_path {
        set_env_if_missing("TERMINAL_SSH_KEY_PATH", value.clone());
    }
}

fn bridge_web_config_to_env(web: &WebConfig) {
    if !web.backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_BACKEND", web.backend.clone());
    }
    if !web.search_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_SEARCH_BACKEND", web.search_backend.clone());
    }
    if !web.extract_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_EXTRACT_BACKEND", web.extract_backend.clone());
    }
    if !web.crawl_backend.trim().is_empty() {
        set_env_if_missing("HERMES_WEB_CRAWL_BACKEND", web.crawl_backend.clone());
    }
}

fn apply_web_env_overrides(config: &mut WebConfig) {
    if let Some(v) = env_var_nonempty("HERMES_WEB_BACKEND") {
        config.backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_SEARCH_BACKEND") {
        config.search_backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_EXTRACT_BACKEND") {
        config.extract_backend = v;
    }
    if let Some(v) = env_var_nonempty("HERMES_WEB_CRAWL_BACKEND") {
        config.crawl_backend = v;
    }
}

fn bridge_display_config_to_env(display: &DisplayConfig) {
    if let Some(mode) = display
        .busy_input_mode
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        set_env_if_missing("HERMES_GATEWAY_BUSY_INPUT_MODE", mode.to_string());
    }
    if let Some(enabled) = display.busy_ack_enabled {
        set_env_if_missing("HERMES_GATEWAY_BUSY_ACK_ENABLED", enabled.to_string());
    }
    if let Some(enabled) = display.memory_notifications {
        set_env_if_missing("HERMES_MEMORY_NOTIFICATIONS_ENABLED", enabled.to_string());
    }
}

fn apply_display_env_overrides(config: &mut DisplayConfig) {
    if let Some(v) = env_var_nonempty("HERMES_GATEWAY_BUSY_INPUT_MODE") {
        config.busy_input_mode = Some(v);
    }
    if let Some(v) = env_var_nonempty("HERMES_GATEWAY_BUSY_ACK_ENABLED") {
        if let Some(parsed) = parse_bool_env("HERMES_GATEWAY_BUSY_ACK_ENABLED", &v) {
            config.busy_ack_enabled = Some(parsed);
        }
    }
    if let Some(v) = env_var_nonempty("HERMES_MEMORY_NOTIFICATIONS_ENABLED") {
        if let Some(parsed) = parse_bool_env("HERMES_MEMORY_NOTIFICATIONS_ENABLED", &v) {
            config.memory_notifications = Some(parsed);
        }
    }
}

fn apply_terminal_env_overrides(config: &mut TerminalConfig) {
    if let Some(v) =
        env_var_nonempty("TERMINAL_ENV").or_else(|| env_var_nonempty("TERMINAL_BACKEND"))
    {
        match TerminalBackendType::from_env_name(&v) {
            Some(backend) => config.backend = backend,
            None => tracing::warn!("Unknown TERMINAL_ENV '{v}'"),
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_TIMEOUT") {
        if let Ok(n) = v.parse::<u64>() {
            config.timeout = n;
        } else {
            tracing::warn!("TERMINAL_TIMEOUT is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_MAX_OUTPUT_SIZE") {
        if let Ok(n) = v.parse::<usize>() {
            config.max_output_size = n;
        } else {
            tracing::warn!("TERMINAL_MAX_OUTPUT_SIZE is not a valid usize: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CWD") {
        config.workdir = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_CONTAINER_ID") {
        config.docker_container_id = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_IMAGE") {
        config.docker_image = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE")
        .and_then(|v| parse_bool_env("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE", &v))
    {
        config.docker_mount_cwd_to_workspace = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_RUN_AS_HOST_USER")
        .and_then(|v| parse_bool_env("TERMINAL_DOCKER_RUN_AS_HOST_USER", &v))
    {
        config.docker_run_as_host_user = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_CPU") {
        if let Ok(n) = v.parse::<u32>() {
            config.container_cpu = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_CPU is not a valid u32: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_MEMORY") {
        if let Ok(n) = v.parse::<u64>() {
            config.container_memory = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_MEMORY is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_DISK") {
        if let Ok(n) = v.parse::<u64>() {
            config.container_disk = Some(n);
        } else {
            tracing::warn!("TERMINAL_CONTAINER_DISK is not a valid u64: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_CONTAINER_PERSISTENT")
        .and_then(|v| parse_bool_env("TERMINAL_CONTAINER_PERSISTENT", &v))
    {
        config.container_persistent = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_ENV") {
        config.docker_env = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_FORWARD_ENV") {
        config.docker_forward_env = parse_list_env(&v, false);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_DOCKER_VOLUMES") {
        config.docker_volumes = parse_list_env(&v, false);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_VERCEL_RUNTIME") {
        config.vercel_runtime = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_MODAL_MODE") {
        config.modal_mode = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SHELL_INIT_FILES") {
        config.shell_init_files = parse_list_env(&v, true);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_AUTO_SOURCE_BASHRC")
        .and_then(|v| parse_bool_env("TERMINAL_AUTO_SOURCE_BASHRC", &v))
    {
        config.auto_source_bashrc = v;
    }
    if let Some(v) = env_var_nonempty("TERMINAL_HOME_MODE") {
        match TerminalHomeMode::from_env_name(&v) {
            Some(mode) => config.home_mode = mode,
            None => tracing::warn!("Unknown TERMINAL_HOME_MODE '{v}'"),
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_HOST") {
        config.ssh_host = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_PORT") {
        if let Ok(n) = v.parse::<u16>() {
            config.ssh_port = Some(n);
        } else {
            tracing::warn!("TERMINAL_SSH_PORT is not a valid u16: {v}");
        }
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_USER") {
        config.ssh_user = Some(v);
    }
    if let Some(v) = env_var_nonempty("TERMINAL_SSH_KEY_PATH") {
        config.ssh_key_path = Some(v);
    }
}

// ---------------------------------------------------------------------------
// apply_env_overrides
// ---------------------------------------------------------------------------

/// Override configuration fields from environment variables.
///
/// Environment variable mapping:
///   HERMES_MODEL           -> config.model
///   HERMES_PERSONALITY     -> config.personality
///   HERMES_HOME            -> config.home_dir
///   HERMES_MAX_TURNS       -> config.max_turns
///   HERMES_SYSTEM_PROMPT   -> config.system_prompt
///   HERMES_PROXY_HTTP      -> config.proxy.http_proxy
///   HERMES_PROXY_SOCKS     -> config.proxy.socks_proxy
///   HERMES_LLM_API_KEY     -> all llm_providers[*].api_key
///   HERMES_BUDGET_MAX_RESULT_CHARS -> config.budget.max_result_size_chars
///   HERMES_BUDGET_MAX_AGGREGATE_CHARS -> config.budget.max_aggregate_chars
///   HERMES_OPENAI_API_KEY      -> llm_providers["openai"].api_key
///   OPENAI_API_KEY             -> llm_providers["openai"].api_key (legacy fallback)
///   ANTHROPIC_API_KEY          -> llm_providers["anthropic"].api_key
///   OPENROUTER_API_KEY         -> llm_providers["openrouter"].api_key
///   DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
///   MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
///   MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
///   NOUS_API_KEY               -> llm_providers["nous"].api_key
///   COPILOT_GITHUB_TOKEN/GITHUB_COPILOT_TOKEN
///                              -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
///
/// 另见 [`crate::python_platform_env::apply_python_named_platform_env`]：
/// `WEIXIN_*`、`DINGTALK_*` 等与 Python `gateway/platforms/*.py` 一致的键写入 `platforms`。
pub fn apply_env_overrides(config: &mut GatewayConfig) {
    apply_terminal_env_overrides(&mut config.terminal);
    apply_web_env_overrides(&mut config.web);
    apply_display_env_overrides(&mut config.display);

    if let Ok(v) = std::env::var("HERMES_MODEL") {
        config.model = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_PERSONALITY") {
        config.personality = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_HOME") {
        config.home_dir = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_MAX_TURNS") {
        if let Ok(n) = v.parse::<u32>() {
            config.max_turns = n;
        } else {
            tracing::warn!("HERMES_MAX_TURNS is not a valid u32: {v}");
        }
    }
    if let Ok(v) = std::env::var("HERMES_SYSTEM_PROMPT") {
        config.system_prompt = Some(v);
    }
    if let Some(v) = env_var_nonempty("HERMES_PREFILL_MESSAGES_FILE") {
        config.prefill_messages_file = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_KANBAN_DISPATCH_IN_GATEWAY") {
        if let Some(parsed) = parse_bool_env("HERMES_KANBAN_DISPATCH_IN_GATEWAY", &v) {
            config.kanban.dispatch_in_gateway = parsed;
        }
    }
    if let Ok(v) = std::env::var("HERMES_AGENT_API_MAX_RETRIES") {
        if let Ok(parsed) = v.parse::<u32>() {
            config.agent.api_max_retries = Some(parsed);
        } else {
            tracing::warn!("HERMES_AGENT_API_MAX_RETRIES is not a valid u32: {v}");
        }
    }
    if env_truthy("HERMES_AGENT_SKIP_CONTEXT_FILES") || env_truthy("HERMES_IGNORE_RULES") {
        config.agent.skip_context_files = true;
    }
    if let Ok(v) = std::env::var("HERMES_ALLOW_PRIVATE_URLS") {
        let normalized = v.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "1" | "true" | "yes" | "on" => config.security.allow_private_urls = true,
            "0" | "false" | "no" | "off" => config.security.allow_private_urls = false,
            _ => tracing::warn!("HERMES_ALLOW_PRIVATE_URLS is not a valid bool-like value: {v}"),
        }
    }
    if let Ok(v) = std::env::var("HERMES_PROXY_HTTP") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.http_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_PROXY_SOCKS") {
        let proxy = config
            .proxy
            .get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.socks_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_LLM_API_KEY") {
        if !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.api_key = Some(v.clone());
            }
        }
    }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_RESULT_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_result_size_chars = n;
        }
    }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_AGGREGATE_CHARS") {
        if let Ok(n) = v.parse::<usize>() {
            config.budget.max_aggregate_chars = n;
        }
    }

    // Provider-specific API keys (prefer HERMES_OPENAI_API_KEY over legacy OPENAI_API_KEY).
    let openai_env = std::env::var("HERMES_OPENAI_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            std::env::var("OPENAI_API_KEY")
                .ok()
                .filter(|v| !v.trim().is_empty())
        });
    if let Some(v) = openai_env {
        config
            .llm_providers
            .entry("openai".to_string())
            .or_insert_with(LlmProviderConfig::default)
            .api_key = Some(v);
    }
    let mut env_overridden_providers = std::collections::HashSet::new();
    for (env_var, provider_name) in [
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("HERMES_OPENAI_CODEX_API_KEY", "openai-codex"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth"),
        ("MOONSHOT_API_KEY", "kimi"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GMI_API_KEY", "gmi"),
        ("ARCEEAI_API_KEY", "arcee"),
        ("ARCEE_API_KEY", "arcee"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("TOKENHUB_API_KEY", "tencent-tokenhub"),
        ("COPILOT_GITHUB_TOKEN", "copilot"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
        ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
        ("LLAMA_CPP_API_KEY", "llama-cpp"),
        ("VLLM_API_KEY", "vllm"),
        ("MLX_API_KEY", "mlx"),
        ("APPLE_ANE_API_KEY", "apple-ane"),
        ("SGLANG_API_KEY", "sglang"),
        ("TGI_API_KEY", "tgi"),
        ("LMSTUDIO_API_KEY", "lmstudio"),
        ("LMDEPLOY_API_KEY", "lmdeploy"),
        ("LOCALAI_API_KEY", "localai"),
        ("KOBOLDCPP_API_KEY", "koboldcpp"),
        ("TEXT_GENERATION_WEBUI_API_KEY", "text-generation-webui"),
        ("TABBYAPI_API_KEY", "tabbyapi"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            if !env_overridden_providers.insert(provider_name) {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .api_key = Some(v);
        }
    }
    for (env_var, provider_name) in [
        ("GMI_BASE_URL", "gmi"),
        ("ARCEE_BASE_URL", "arcee"),
        ("XIAOMI_BASE_URL", "xiaomi"),
        ("TOKENHUB_BASE_URL", "tencent-tokenhub"),
        ("OLLAMA_BASE_URL", "ollama-local"),
        ("LLAMA_CPP_BASE_URL", "llama-cpp"),
        ("VLLM_BASE_URL", "vllm"),
        ("MLX_BASE_URL", "mlx"),
        ("APPLE_ANE_BASE_URL", "apple-ane"),
        ("SGLANG_BASE_URL", "sglang"),
        ("TGI_BASE_URL", "tgi"),
        ("LMSTUDIO_BASE_URL", "lmstudio"),
        ("LMDEPLOY_BASE_URL", "lmdeploy"),
        ("LOCALAI_BASE_URL", "localai"),
        ("KOBOLDCPP_BASE_URL", "koboldcpp"),
        ("TEXT_GENERATION_WEBUI_BASE_URL", "text-generation-webui"),
        ("TABBYAPI_BASE_URL", "tabbyapi"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .base_url = Some(v);
        }
    }

    if let Ok(v) = std::env::var("HERMES_BASE_URL") {
        if !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.base_url = Some(v.clone());
            }
        }
    }

    crate::python_platform_env::apply_python_named_platform_env(config);
}

// ---------------------------------------------------------------------------
// validate_config
// ---------------------------------------------------------------------------

/// Validate a fully-loaded configuration.
///
/// Checks:
/// - max_turns > 0
/// - SessionResetPolicy::Daily at_hour in 0..=23
/// - All LLM providers with an api_key set have a non-empty value
/// - Terminal timeout > 0
pub fn validate_config(config: &GatewayConfig) -> Result<(), ConfigError> {
    if config.max_turns == 0 {
        return Err(ConfigError::ValidationError(
            "max_turns must be greater than 0".into(),
        ));
    }

    if config.terminal.timeout == 0 {
        return Err(ConfigError::ValidationError(
            "terminal.timeout must be greater than 0".into(),
        ));
    }

    // Validate session reset policy (clamping already done during merge)
    let _ = config.session.reset_policy.validate();

    for (name, provider) in &config.llm_providers {
        if let Some(key) = &provider.api_key {
            if key.trim().is_empty() {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_key must not be empty"
                )));
            }
        }
        if let Some(api_mode) = &provider.api_mode {
            normalize_provider_api_mode(api_mode).map_err(|_| {
                ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_mode must be one of chat_completions, anthropic_messages, codex_responses, bedrock_converse"
                ))
            })?;
        }
        if let Some(timeout) = provider.request_timeout_seconds {
            if !timeout.is_finite() || timeout <= 0.0 {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.request_timeout_seconds must be a positive finite number"
                )));
            }
        }
        if matches!(provider.max_tokens, Some(0)) {
            return Err(ConfigError::ValidationError(format!(
                "llm_providers.{name}.max_tokens must be a positive integer"
            )));
        }
        if provider.models.iter().any(|model| model.trim().is_empty()) {
            return Err(ConfigError::ValidationError(format!(
                "llm_providers.{name}.models must not contain empty model ids"
            )));
        }
    }

    if let Some(api_key) = &config.delegation.api_key {
        if api_key.trim().is_empty() {
            return Err(ConfigError::ValidationError(
                "delegation.api_key must not be empty".into(),
            ));
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
