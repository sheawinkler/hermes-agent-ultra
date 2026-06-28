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

include!("loader/user_config_patch.rs");

include!("loader/config_write.rs");

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

include!("loader/env_bridges.rs");

#[cfg(test)]
mod tests;
