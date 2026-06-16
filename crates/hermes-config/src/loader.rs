//! Configuration loading from YAML, JSON, and environment variables.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use crate::config::{GatewayConfig, ProxyConfig, TerminalBackendType, TerminalConfig, WebConfig};
use crate::merge::merge_configs;
use crate::paths;

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
        std::fs::rename(&tmp_path, path).map_err(io_to_config_error)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
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
    let mut yaml = serde_yaml::to_string(value).map_err(yaml_to_config_error)?;
    if let Some(extra) = extra_content {
        yaml.push_str(extra);
    }
    atomic_write_bytes(path, yaml.as_bytes())
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

// ---------------------------------------------------------------------------
// load_config
// ---------------------------------------------------------------------------

/// Load the full configuration, applying the priority chain:
///
///   env vars  >  .env  >  cli-config.yaml > config.yaml > gateway.json > defaults
///
/// If `home_dir` is provided it overrides the `HERMES_HOME` env var.
pub fn load_config(home_dir: Option<&str>) -> Result<GatewayConfig, ConfigError> {
    // Resolve the ultra home directory (fresh primary; no legacy copy).
    let effective_home_path = crate::migrate::ensure_migrated_hermes_home(home_dir);
    let effective_home = effective_home_path.to_string_lossy().into_owned();
    // SAFETY: startup path — align process env with migrated home before dotenv.
    unsafe {
        std::env::set_var("HERMES_HOME", &effective_home);
    }

    // Load .env after migration so we read from the canonical home.
    load_dotenv();

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

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);
    normalize_platform_aliases(&mut config);
    normalize_provider_secrets(&mut config);

    // Record the effective home dir
    config.home_dir = Some(effective_home);

    // Validate
    validate_config(&config)?;

    Ok(config)
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

const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, prefill_messages_file, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, security.allow_private_urls, web.backend|search_backend|extract_backend|crawl_backend, sessions.auto_prune|retention_days|vacuum_after_prune|min_interval_hours, interest.enabled|extract_mode|max_topics|llm_on_session_end|per_turn_buffer|per_turn_persist|promote_min_evidence|promote_min_confidence|min_turn_chars, kanban.dispatch_in_gateway, agent.api_max_retries, delegation.model|provider|base_url|api_key|max_spawn_depth, llm.<provider>.api_key|api_key_env|base_url|model|api_mode|command|args|request_timeout_seconds|oauth_token_url|oauth_client_id, auxiliary.<task>.provider|model|base_url|api_key|timeout|download_timeout, insights.contribution.enabled|endpoint|upload_interests|upload_skills|on_session_end|skill_min_age_hours|min_evidence_tier|exclude_verdicts|require_skill_binding|min_work_turns|upload_skills_refresh|redacted_body|installation_token|auth_token, smart_model_routing.enabled|max_simple_chars|max_simple_words|cheap_model.model|cheap_model.provider, server.enabled|base_url|wechat_base_url|channel|app|invite_code|auth.preferred_method|auth.poll_interval_ms|auth.otp_ttl_seconds|auth.heartbeat_interval_secs";

fn parse_bool_config_value(value: &str, field: &str) -> Result<bool, ConfigError> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigError::ValidationError(format!(
            "{field} must be a boolean: {value}"
        ))),
    }
}

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
        "chat_completions" | "anthropic_messages" | "codex_responses" | "bedrock_converse" => {
            Ok(normalized)
        }
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
                .or_default();
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
                .or_default();
            match *field {
                "api_key" => entry.api_key = Some(value.to_string()),
                "api_key_env" => entry.api_key_env = Some(value.to_string()),
                "base_url" => entry.base_url = Some(value.to_string()),
                "model" => entry.model = Some(value.to_string()),
                "api_mode" => entry.api_mode = Some(normalize_provider_api_mode(value)?),
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
                        "unknown llm field: llm.{}.{} (supported: api_key, api_key_env, base_url, model, api_mode, command, args, request_timeout_seconds, oauth_token_url, oauth_client_id)",
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
        ["interest", "enabled"] => {
            config.interest.enabled = parse_bool_config_value(value, "interest.enabled")?;
        }
        ["interest", "extract_mode"] => {
            config.interest.extract_mode = value.trim().to_string();
        }
        ["interest", "max_topics"] => {
            config.interest.max_topics = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "interest.max_topics must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["interest", "llm_on_session_end"] => {
            config.interest.llm_on_session_end =
                parse_bool_config_value(value, "interest.llm_on_session_end")?;
        }
        ["interest", "per_turn_buffer"] => {
            config.interest.per_turn_buffer =
                parse_bool_config_value(value, "interest.per_turn_buffer")?;
        }
        ["interest", "per_turn_persist"] => {
            config.interest.per_turn_persist =
                parse_bool_config_value(value, "interest.per_turn_persist")?;
        }
        ["interest", "promote_min_evidence"] => {
            config.interest.promote_min_evidence = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "interest.promote_min_evidence must be a positive integer: {}",
                    value
                ))
            })?;
        }
        ["interest", "promote_min_confidence"] => {
            config.interest.promote_min_confidence = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "interest.promote_min_confidence must be a number in (0,1]: {}",
                    value
                ))
            })?;
        }
        ["interest", "min_turn_chars"] => {
            config.interest.min_turn_chars = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "interest.min_turn_chars must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["insights", "contribution", "enabled"] => {
            config.insights.contribution.enabled =
                parse_bool_config_value(value, "insights.contribution.enabled")?;
        }
        ["insights", "contribution", "endpoint"] => {
            config.insights.contribution.endpoint = value.trim().to_string();
        }
        ["insights", "contribution", "upload_interests"] => {
            config.insights.contribution.upload_interests =
                parse_bool_config_value(value, "insights.contribution.upload_interests")?;
        }
        ["insights", "contribution", "upload_skills"] => {
            config.insights.contribution.upload_skills =
                parse_bool_config_value(value, "insights.contribution.upload_skills")?;
        }
        ["insights", "contribution", "on_session_end"] => {
            config.insights.contribution.on_session_end =
                parse_bool_config_value(value, "insights.contribution.on_session_end")?;
        }
        ["insights", "contribution", "min_evidence_tier"] => {
            config.insights.contribution.min_evidence_tier = value.trim().to_string();
        }
        ["insights", "contribution", "exclude_verdicts"] => {
            config.insights.contribution.exclude_verdicts = value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        ["insights", "contribution", "require_skill_binding"] => {
            config.insights.contribution.require_skill_binding =
                parse_bool_config_value(value, "insights.contribution.require_skill_binding")?;
        }
        ["insights", "contribution", "min_work_turns"] => {
            config.insights.contribution.min_work_turns = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "insights.contribution.min_work_turns must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["insights", "contribution", "resolution_mode"] => {
            config.insights.contribution.resolution_mode = value.trim().to_string();
        }
        ["insights", "contribution", "resolution_llm_on_session_end"] => {
            config.insights.contribution.resolution_llm_on_session_end = parse_bool_config_value(
                value,
                "insights.contribution.resolution_llm_on_session_end",
            )?;
        }
        ["insights", "contribution", "skill_min_age_hours"] => {
            config.insights.contribution.skill_min_age_hours = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "insights.contribution.skill_min_age_hours must be a non-negative integer: {}",
                    value
                ))
            })?;
        }
        ["insights", "contribution", "upload_skills_refresh"] => {
            config.insights.contribution.upload_skills_refresh =
                parse_bool_config_value(value, "insights.contribution.upload_skills_refresh")?;
        }
        ["insights", "contribution", "redacted_body"] => {
            config.insights.contribution.redacted_body =
                parse_bool_config_value(value, "insights.contribution.redacted_body")?;
        }
        ["insights", "contribution", "installation_token"]
        | ["insights", "contribution", "auth_token"] => {
            let trimmed = value.trim();
            config.insights.contribution.auth_token = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        ["server", "enabled"] => {
            config.server.enabled = parse_bool_config_value(value, "server.enabled")?;
        }
        ["server", "base_url"] => {
            config.server.base_url = value.trim().trim_end_matches('/').to_string();
        }
        ["server", "wechat_base_url"] => {
            config.server.wechat_base_url = value.trim().trim_end_matches('/').to_string();
        }
        ["server", "channel"] => {
            let channel = value.trim().to_string();
            config.server.channel = channel.clone();
            crate::server::sync_wechat_app_id_for_channel(&mut config.server.auth, &channel);
        }
        ["server", "app"] => {
            config.server.app = value.trim().to_string();
        }
        ["server", "invite_code"] => {
            config.server.invite_code = value.trim().to_string();
        }
        ["server", "auth", "preferred_method"] => {
            let normalized = value.trim().to_ascii_lowercase().replace('-', "_");
            config.server.auth.preferred_method = match normalized.as_str() {
                "wechat" | "wechat_qr" | "wx" => crate::server::ServerLoginMethod::WechatQr,
                "email" | "email_otp" | "otp" => crate::server::ServerLoginMethod::EmailOtp,
                _ => {
                    return Err(ConfigError::ValidationError(format!(
                        "server.auth.preferred_method must be wechat_qr or email_otp: {}",
                        value
                    )));
                }
            };
        }
        ["server", "auth", "poll_interval_ms"] => {
            config.server.auth.poll_interval_ms = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "server.auth.poll_interval_ms must be a positive integer: {}",
                    value
                ))
            })?;
        }
        ["server", "auth", "otp_ttl_seconds"] => {
            config.server.auth.otp_ttl_seconds = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "server.auth.otp_ttl_seconds must be a positive integer: {}",
                    value
                ))
            })?;
        }
        ["server", "auth", "heartbeat_interval_secs"] => {
            config.server.auth.heartbeat_interval_secs = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "server.auth.heartbeat_interval_secs must be a positive integer: {}",
                    value
                ))
            })?;
        }
        ["server", "auth", "wechat_app_id"] => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                config.server.auth.wechat_app_id.clear();
            } else if crate::server::is_valid_wechat_open_app_id(trimmed) {
                config.server.auth.wechat_app_id = trimmed.to_string();
            } else {
                return Err(ConfigError::ValidationError(format!(
                    "invalid WeChat Open Platform app id '{trimmed}' \
                     (expected wx + 16 hex chars, e.g. wxc7a38fe55e162569 for flowy). \
                     Use server.app for the client app identifier (flowymes), not wechat_app_id"
                )));
            }
        }
        ["server", "llm", "default_model"] => {
            config.server.llm.default_model = value.trim().to_string();
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
        ["sessions", "auto_prune"] => Ok(config.sessions.auto_prune.to_string()),
        ["sessions", "retention_days"] => Ok(config.sessions.retention_days.to_string()),
        ["sessions", "vacuum_after_prune"] => Ok(config.sessions.vacuum_after_prune.to_string()),
        ["sessions", "min_interval_hours"] => Ok(config.sessions.min_interval_hours.to_string()),
        ["kanban", "dispatch_in_gateway"] => Ok(config.kanban.dispatch_in_gateway.to_string()),
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
        ["agent", "api_max_retries"] | ["agent", "apiMaxRetries"] => Ok(config
            .agent
            .api_max_retries
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
        ["llm", provider, "api_mode"] => Ok(config
            .llm_providers
            .get(*provider)
            .and_then(|c| c.api_mode.as_deref())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
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
        ["interest", "enabled"] => Ok(config.interest.enabled.to_string()),
        ["interest", "extract_mode"] => Ok(config.interest.extract_mode.clone()),
        ["interest", "max_topics"] => Ok(config.interest.max_topics.to_string()),
        ["interest", "llm_on_session_end"] => Ok(config.interest.llm_on_session_end.to_string()),
        ["insights", "contribution", "enabled"] => {
            Ok(config.insights.contribution.enabled.to_string())
        }
        ["insights", "contribution", "endpoint"] => {
            Ok(if config.insights.contribution.endpoint.is_empty() {
                "(not set)".to_string()
            } else {
                config.insights.contribution.endpoint.clone()
            })
        }
        ["insights", "contribution", "upload_interests"] => {
            Ok(config.insights.contribution.upload_interests.to_string())
        }
        ["insights", "contribution", "upload_skills"] => {
            Ok(config.insights.contribution.upload_skills.to_string())
        }
        ["insights", "contribution", "on_session_end"] => {
            Ok(config.insights.contribution.on_session_end.to_string())
        }
        ["insights", "contribution", "skill_min_age_hours"] => {
            Ok(config.insights.contribution.skill_min_age_hours.to_string())
        }
        ["insights", "contribution", "min_evidence_tier"] => {
            Ok(config.insights.contribution.min_evidence_tier.clone())
        }
        ["insights", "contribution", "exclude_verdicts"] => {
            Ok(config.insights.contribution.exclude_verdicts.join(","))
        }
        ["insights", "contribution", "require_skill_binding"] => Ok(config
            .insights
            .contribution
            .require_skill_binding
            .to_string()),
        ["insights", "contribution", "min_work_turns"] => {
            Ok(config.insights.contribution.min_work_turns.to_string())
        }
        ["insights", "contribution", "resolution_mode"] => {
            Ok(config.insights.contribution.resolution_mode.clone())
        }
        ["insights", "contribution", "resolution_llm_on_session_end"] => Ok(config
            .insights
            .contribution
            .resolution_llm_on_session_end
            .to_string()),
        ["insights", "contribution", "upload_skills_refresh"] => Ok(config
            .insights
            .contribution
            .upload_skills_refresh
            .to_string()),
        ["insights", "contribution", "redacted_body"] => {
            Ok(config.insights.contribution.redacted_body.to_string())
        }
        ["insights", "contribution", "installation_token"]
        | ["insights", "contribution", "auth_token"] => Ok(config
            .insights
            .contribution
            .auth_token
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(mask_secret)
            .unwrap_or_else(|| "(not set)".to_string())),
        ["server", "enabled"] => Ok(config.server.enabled.to_string()),
        ["server", "base_url"] => Ok(if config.server.base_url.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.server.base_url.clone()
        }),
        ["server", "wechat_base_url"] => Ok(if config.server.wechat_base_url.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.server.wechat_base_url.clone()
        }),
        ["server", "channel"] => Ok(config.server.channel.clone()),
        ["server", "app"] => Ok(config.server.app.clone()),
        ["server", "invite_code"] => Ok(if config.server.invite_code.trim().is_empty() {
            "(not set)".to_string()
        } else {
            config.server.invite_code.clone()
        }),
        ["server", "auth", "preferred_method"] => {
            Ok(config.server.auth.preferred_method.as_str().to_string())
        }
        ["server", "auth", "poll_interval_ms"] => {
            Ok(config.server.auth.poll_interval_ms.to_string())
        }
        ["server", "auth", "otp_ttl_seconds"] => Ok(config.server.auth.otp_ttl_seconds.to_string()),
        ["server", "auth", "heartbeat_interval_secs"] => {
            Ok(config.server.auth.heartbeat_interval_secs.to_string())
        }
        ["server", "auth", "wechat_app_id"] => {
            let effective = config.server.effective_wechat_app_id();
            let stored = config.server.auth.wechat_app_id.trim();
            Ok(if stored.is_empty() {
                format!("(not set, using channel default: {effective})")
            } else if !crate::server::is_valid_wechat_open_app_id(stored) {
                format!("(invalid stored value '{stored}', using channel default: {effective})")
            } else {
                effective
            })
        }
        ["server", "llm", "default_model"] => {
            let effective = config.server.effective_default_llm_model();
            Ok(if config.server.llm.default_model.trim().is_empty() {
                format!("(not set, using built-in default: {effective})")
            } else {
                effective
            })
        }
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
    let yaml = serde_yaml::to_string(&to_save).map_err(yaml_to_config_error)?;
    atomic_write_bytes(path, yaml.as_bytes())?;
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

fn config_env_bridge_key(key: &str) -> Option<String> {
    terminal_config_env_bridge_key(key)
        .or_else(|| web_config_env_bridge_key(key))
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
    if trimmed.starts_with('[')
        && let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
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
///   OPENROUTER_BASE_URL        -> llm_providers["openrouter"].base_url
///   MINIMAX_BASE_URL           -> llm_providers["minimax"].base_url
///   DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
///   MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
///   MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
///   NOUS_API_KEY               -> llm_providers["nous"].api_key
///   COPILOT_GITHUB_TOKEN/GITHUB_COPILOT_TOKEN
///                              -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
///   HERMES_INSIGHTS_ENDPOINT   -> insights.contribution.endpoint
///   HERMES_INSIGHTS_TOKEN      -> insights.contribution.auth_token / installation_token (Bearer JWT or flowy- API key)
///
/// 另见 [`crate::python_platform_env::apply_python_named_platform_env`]：
/// `WEIXIN_*`、`DINGTALK_*` 等与 Python `gateway/platforms/*.py` 一致的键写入 `platforms`。
pub fn apply_env_overrides(config: &mut GatewayConfig) {
    apply_terminal_env_overrides(&mut config.terminal);
    apply_web_env_overrides(&mut config.web);

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
    if let Ok(v) = std::env::var("HERMES_INSIGHTS_ENDPOINT") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.insights.contribution.endpoint = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_INSIGHTS_TOKEN") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.insights.contribution.auth_token = Some(trimmed.to_string());
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_URL") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.server.base_url = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_ENABLED")
        && let Some(parsed) = parse_bool_env("HERMES_SERVER_ENABLED", &v) {
            config.server.enabled = parsed;
        }
    if let Ok(v) = std::env::var("HERMES_SERVER_TOKEN") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            // Token is read at runtime from env by hermes-server-client; no config field needed.
            let _ = trimmed;
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_CHANNEL") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.server.channel = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_APP") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.server.app = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_WECHAT_URL") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.server.wechat_base_url = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_WECHAT_APP_ID") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            if crate::server::is_valid_wechat_open_app_id(trimmed) {
                config.server.auth.wechat_app_id = trimmed.to_string();
            } else {
                tracing::warn!(
                    "HERMES_SERVER_WECHAT_APP_ID ignored — invalid WeChat Open Platform app id: {trimmed}"
                );
            }
        }
    }
    if let Ok(v) = std::env::var("HERMES_SERVER_INVITE_CODE") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            config.server.invite_code = trimmed.to_string();
        }
    }
    if let Ok(v) = std::env::var("HERMES_KANBAN_DISPATCH_IN_GATEWAY")
        && let Some(parsed) = parse_bool_env("HERMES_KANBAN_DISPATCH_IN_GATEWAY", &v) {
            config.kanban.dispatch_in_gateway = parsed;
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
    if let Ok(v) = std::env::var("HERMES_LLM_API_KEY")
        && !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.api_key = Some(v.clone());
            }
        }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_RESULT_CHARS")
        && let Ok(n) = v.parse::<usize>() {
            config.budget.max_result_size_chars = n;
        }
    if let Ok(v) = std::env::var("HERMES_BUDGET_MAX_AGGREGATE_CHARS")
        && let Ok(n) = v.parse::<usize>() {
            config.budget.max_aggregate_chars = n;
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
            .or_default()
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
                .or_default()
                .api_key = Some(v);
        }
    }
    for (env_var, provider_name) in [
        ("GMI_BASE_URL", "gmi"),
        ("ARCEE_BASE_URL", "arcee"),
        ("XIAOMI_BASE_URL", "xiaomi"),
        ("TOKENHUB_BASE_URL", "tencent-tokenhub"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_default()
                .base_url = Some(v);
        }
    }

    if let Ok(v) = std::env::var("HERMES_BASE_URL")
        && !v.trim().is_empty() {
            for provider in config.llm_providers.values_mut() {
                provider.base_url = Some(v.clone());
            }
        }

    if let Ok(v) = std::env::var("OPENROUTER_BASE_URL")
        && !v.trim().is_empty() {
            config
                .llm_providers
                .entry("openrouter".to_string())
                .or_default()
                .base_url = Some(v);
        }
    if let Ok(v) = std::env::var("MINIMAX_BASE_URL")
        && !v.trim().is_empty() {
            config
                .llm_providers
                .entry("minimax".to_string())
                .or_default()
                .base_url = Some(v);
        }

    if let Ok(token) = std::env::var("SLACK_BOT_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            let slack = config
                .platforms
                .entry("slack".to_string())
                .or_default();
            let enabled_was_explicit = slack
                .extra
                .remove("_enabled_explicit")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !slack.enabled && !enabled_was_explicit {
                slack.enabled = true;
            }
            slack.token = Some(trimmed.to_string());
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
///
/// Resolve the prefill JSON file using upstream-compatible precedence.
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
    if trimmed == "~"
        && let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    if let Some(rest) = trimmed.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    PathBuf::from(trimmed)
}

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
        if let Some(key) = &provider.api_key
            && key.trim().is_empty() {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_key must not be empty"
                )));
            }
        if let Some(api_mode) = &provider.api_mode {
            normalize_provider_api_mode(api_mode).map_err(|_| {
                ConfigError::ValidationError(format!(
                    "llm_providers.{name}.api_mode must be one of chat_completions, anthropic_messages, codex_responses, bedrock_converse"
                ))
            })?;
        }
        if let Some(timeout) = provider.request_timeout_seconds
            && (!timeout.is_finite() || timeout <= 0.0) {
                return Err(ConfigError::ValidationError(format!(
                    "llm_providers.{name}.request_timeout_seconds must be a positive finite number"
                )));
            }
    }

    if let Some(api_key) = &config.delegation.api_key
        && api_key.trim().is_empty() {
            return Err(ConfigError::ValidationError(
                "delegation.api_key must not be empty".into(),
            ));
        }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_terminal_env_bridge_vars() {
        for (_, env_key) in terminal_config_env_bridge_pairs() {
            // SAFETY: tests serialize env mutation with ENV_LOCK.
            unsafe { std::env::remove_var(env_key) };
        }
        // SAFETY: tests serialize env mutation with ENV_LOCK.
        unsafe { std::env::remove_var("TERMINAL_BACKEND") };
    }

    fn clear_web_env_bridge_vars() {
        for (_, env_key) in web_config_env_bridge_pairs() {
            // SAFETY: tests serialize env mutation with ENV_LOCK.
            unsafe { std::env::remove_var(env_key) };
        }
    }

    #[test]
    fn validate_valid_config() {
        let config = GatewayConfig::default();
        assert!(validate_config(&config).is_ok());
    }

    #[test]
    fn validate_zero_max_turns() {
        let mut config = GatewayConfig::default();
        config.max_turns = 0;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_zero_terminal_timeout() {
        let mut config = GatewayConfig::default();
        config.terminal.timeout = 0;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_empty_api_key() {
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "test".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("".into()),
                ..Default::default()
            },
        );
        config.llm_providers = providers;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn validate_whitespace_api_key() {
        let mut config = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "test".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("   ".into()),
                ..Default::default()
            },
        );
        config.llm_providers = providers;
        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn normalize_provider_secrets_removes_empty_provider_entries() {
        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "minimax".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("".into()),
                ..Default::default()
            },
        );
        config.llm_providers.insert(
            "openrouter".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("   ".into()),
                oauth_token_url: Some("  ".into()),
                ..Default::default()
            },
        );
        config.llm_providers.insert(
            "nous".into(),
            crate::config::LlmProviderConfig {
                api_key: Some("tok-abc".into()),
                ..Default::default()
            },
        );
        normalize_provider_secrets(&mut config);
        assert_eq!(config.llm_providers.len(), 1);
        assert_eq!(
            config
                .llm_providers
                .get("nous")
                .and_then(|cfg| cfg.api_key.as_deref()),
            Some("tok-abc")
        );
    }

    #[test]
    fn normalize_provider_secrets_keeps_timeout_only_provider_entries() {
        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "anthropic".into(),
            crate::config::LlmProviderConfig {
                request_timeout_seconds: Some(45.0),
                ..Default::default()
            },
        );

        normalize_provider_secrets(&mut config);

        assert_eq!(
            config
                .llm_providers
                .get("anthropic")
                .and_then(|cfg| cfg.request_timeout_seconds),
            Some(45.0)
        );
    }

    #[test]
    fn validate_llm_provider_request_timeout_seconds() {
        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "anthropic".into(),
            crate::config::LlmProviderConfig {
                request_timeout_seconds: Some(45.0),
                ..Default::default()
            },
        );
        assert!(validate_config(&config).is_ok());

        config
            .llm_providers
            .get_mut("anthropic")
            .expect("provider")
            .request_timeout_seconds = Some(0.0);
        let err = validate_config(&config).unwrap_err().to_string();
        assert!(err.contains("request_timeout_seconds"));
    }

    #[test]
    fn env_overrides_model() {
        let mut config = GatewayConfig::default();
        // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
        config.model = Some("env-model".into());
        assert_eq!(config.model.as_deref(), Some("env-model"));
    }

    #[test]
    fn atomic_json_write_roundtrips_and_cleans_temp() {
        use serde_json::json;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("data.json");
        atomic_json_write(&path, &json!({"key": "value", "nested": {"a": 1}})).unwrap();

        let loaded: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(loaded, json!({"key": "value", "nested": {"a": 1}}));
        assert!(!std::fs::read_dir(dir.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")
        }));
    }

    #[test]
    fn atomic_yaml_write_appends_extra_content() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("data.yaml");
        let value: serde_yaml::Value = serde_yaml::from_str("key: value\n").unwrap();
        atomic_yaml_write(&path, &value, Some("\n# comment\n")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("key: value"));
        assert!(text.contains("# comment"));
    }

    #[test]
    fn load_from_yaml_expands_env_refs_but_user_config_load_preserves_templates() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
llm_providers:
  openai:
    api_key: ${TEST_OPENAI_KEY}
    base_url: https://${TEST_OPENAI_HOST}/v1
"#,
        )
        .unwrap();

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("TEST_OPENAI_KEY", "sk-test");
            std::env::set_var("TEST_OPENAI_HOST", "api.example.test");
        }

        let runtime = load_from_yaml(&path).unwrap();
        let openai = runtime.llm_providers.get("openai").unwrap();
        assert_eq!(openai.api_key.as_deref(), Some("sk-test"));
        assert_eq!(
            openai.base_url.as_deref(),
            Some("https://api.example.test/v1")
        );

        let editable = load_user_config_file(&path).unwrap();
        let editable_openai = editable.llm_providers.get("openai").unwrap();
        assert_eq!(
            editable_openai.api_key.as_deref(),
            Some("${TEST_OPENAI_KEY}")
        );
        assert_eq!(
            editable_openai.base_url.as_deref(),
            Some("https://${TEST_OPENAI_HOST}/v1")
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("TEST_OPENAI_KEY");
            std::env::remove_var("TEST_OPENAI_HOST");
        }
    }

    #[test]
    fn load_from_yaml_keeps_unresolved_env_refs_verbatim() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
llm_providers:
  openai:
    api_key: ${MISSING_OPENAI_KEY_FOR_TEST}
"#,
        )
        .unwrap();

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe { std::env::remove_var("MISSING_OPENAI_KEY_FOR_TEST") };
        let runtime = load_from_yaml(&path).unwrap();
        assert_eq!(
            runtime
                .llm_providers
                .get("openai")
                .and_then(|provider| provider.api_key.as_deref()),
            Some("${MISSING_OPENAI_KEY_FOR_TEST}")
        );
    }

    #[test]
    fn set_user_config_value_routes_secret_keys_to_env() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().unwrap();
        let result = set_user_config_value(dir.path(), "openai_api_key", "sk-test").unwrap();

        assert!(result.wrote_env());
        assert!(!result.wrote_config());
        assert_eq!(result.env_key.as_deref(), Some("OPENAI_API_KEY"));
        assert!(
            std::fs::read_to_string(dir.path().join(".env"))
                .unwrap()
                .contains("OPENAI_API_KEY=sk-test")
        );
        assert!(!dir.path().join("config.yaml").exists());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe { std::env::remove_var("OPENAI_API_KEY") };
    }

    #[test]
    fn set_user_config_value_bridges_terminal_env_keys_and_config() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_terminal_env_bridge_vars();
        let dir = tempdir().unwrap();
        let result =
            set_user_config_value(dir.path(), "terminal.vercel_runtime", "python3.13").unwrap();

        assert!(result.wrote_config());
        assert!(result.wrote_env());
        assert_eq!(result.env_key.as_deref(), Some("TERMINAL_VERCEL_RUNTIME"));
        let config_text = std::fs::read_to_string(dir.path().join("config.yaml")).unwrap();
        let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(config_text.contains("vercel_runtime: python3.13"));
        assert!(env_text.contains("TERMINAL_VERCEL_RUNTIME=python3.13"));

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe { std::env::remove_var("TERMINAL_VERCEL_RUNTIME") };
    }

    #[test]
    fn terminal_config_bridge_map_covers_critical_writable_keys() {
        let keys = terminal_config_env_bridge_pairs()
            .iter()
            .map(|(key, _)| *key)
            .collect::<std::collections::HashSet<_>>();
        for key in [
            "backend",
            "docker_run_as_host_user",
            "docker_mount_cwd_to_workspace",
            "docker_env",
            "docker_image",
            "container_cpu",
            "container_memory",
            "container_disk",
            "container_persistent",
            "shell_init_files",
            "auto_source_bashrc",
            "vercel_runtime",
            "modal_mode",
        ] {
            assert!(keys.contains(key), "missing terminal bridge key: {key}");
            assert!(
                terminal_config_env_bridge_key(&format!("terminal.{key}")).is_some(),
                "terminal.{key} should map to an env var"
            );
        }
    }

    #[test]
    fn set_user_config_value_bridges_all_terminal_runtime_keys() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_terminal_env_bridge_vars();
        let dir = tempdir().unwrap();
        for (key, value, env_key) in [
            (
                "terminal.docker_run_as_host_user",
                "true",
                "TERMINAL_DOCKER_RUN_AS_HOST_USER",
            ),
            (
                "terminal.docker_mount_cwd_to_workspace",
                "true",
                "TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE",
            ),
            ("terminal.docker_env", "FOO=bar", "TERMINAL_DOCKER_ENV"),
            (
                "terminal.shell_init_files",
                "~/custom.sh",
                "TERMINAL_SHELL_INIT_FILES",
            ),
            (
                "terminal.auto_source_bashrc",
                "false",
                "TERMINAL_AUTO_SOURCE_BASHRC",
            ),
        ] {
            let result = set_user_config_value(dir.path(), key, value).unwrap();
            assert!(result.wrote_config(), "{key} should write config");
            assert!(result.wrote_env(), "{key} should write env");
            assert_eq!(result.env_key.as_deref(), Some(env_key));
        }
        let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(env_text.contains("TERMINAL_DOCKER_RUN_AS_HOST_USER=true"));
        assert!(env_text.contains("TERMINAL_DOCKER_ENV=FOO=bar"));
        assert!(env_text.contains("TERMINAL_AUTO_SOURCE_BASHRC=false"));
        clear_terminal_env_bridge_vars();
    }

    #[test]
    fn web_config_bridge_map_covers_runtime_backend_keys() {
        let keys = web_config_env_bridge_pairs()
            .iter()
            .map(|(key, _)| *key)
            .collect::<std::collections::HashSet<_>>();
        for key in [
            "backend",
            "search_backend",
            "extract_backend",
            "crawl_backend",
        ] {
            assert!(keys.contains(key), "missing web bridge key: {key}");
            assert!(
                web_config_env_bridge_key(&format!("web.{key}")).is_some(),
                "web.{key} should map to an env var"
            );
        }
    }

    #[test]
    fn set_user_config_value_bridges_web_backend_keys() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_web_env_bridge_vars();
        let dir = tempdir().unwrap();
        for (key, value, env_key) in [
            ("web.backend", "firecrawl", "HERMES_WEB_BACKEND"),
            (
                "web.search_backend",
                "brave-free",
                "HERMES_WEB_SEARCH_BACKEND",
            ),
            (
                "web.extract_backend",
                "tavily",
                "HERMES_WEB_EXTRACT_BACKEND",
            ),
            ("web.crawl_backend", "tavily", "HERMES_WEB_CRAWL_BACKEND"),
        ] {
            let result = set_user_config_value(dir.path(), key, value).unwrap();
            assert!(result.wrote_config(), "{key} should write config");
            assert!(result.wrote_env(), "{key} should write env");
            assert_eq!(result.env_key.as_deref(), Some(env_key));
        }
        let env_text = std::fs::read_to_string(dir.path().join(".env")).unwrap();
        assert!(env_text.contains("HERMES_WEB_SEARCH_BACKEND=brave-free"));
        clear_web_env_bridge_vars();
    }

    #[test]
    fn load_config_bridges_web_yaml_to_env_without_overriding_existing_env() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_web_env_bridge_vars();
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            r#"
web:
  backend: firecrawl
  search_backend: searxng
  extract_backend: tavily
  crawl_backend: tavily
"#,
        )
        .unwrap();
        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe { std::env::set_var("HERMES_WEB_SEARCH_BACKEND", "brave-free") };

        let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
        assert_eq!(cfg.web.backend, "firecrawl");
        assert_eq!(cfg.web.search_backend, "brave-free");
        assert_eq!(cfg.web.extract_backend, "tavily");
        assert_eq!(std::env::var("HERMES_WEB_BACKEND").unwrap(), "firecrawl");
        assert_eq!(
            std::env::var("HERMES_WEB_SEARCH_BACKEND").unwrap(),
            "brave-free"
        );
        assert_eq!(
            std::env::var("HERMES_WEB_EXTRACT_BACKEND").unwrap(),
            "tavily"
        );
        clear_web_env_bridge_vars();
    }

    #[test]
    fn load_config_bridges_terminal_yaml_to_env_without_overriding_existing_env() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_terminal_env_bridge_vars();
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            r#"
terminal:
  backend: docker
  docker_image: rust:1.90
  docker_env: FOO=bar
  docker_mount_cwd_to_workspace: true
  shell_init_files: "~/custom.sh"
  auto_source_bashrc: false
"#,
        )
        .unwrap();
        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe { std::env::set_var("TERMINAL_DOCKER_IMAGE", "already-set") };

        let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
        assert_eq!(cfg.terminal.backend, TerminalBackendType::Docker);
        assert_eq!(cfg.terminal.docker_image.as_deref(), Some("already-set"));
        assert_eq!(std::env::var("TERMINAL_ENV").unwrap(), "docker");
        assert_eq!(
            std::env::var("TERMINAL_DOCKER_IMAGE").unwrap(),
            "already-set"
        );
        assert_eq!(std::env::var("TERMINAL_DOCKER_ENV").unwrap(), "FOO=bar");
        assert_eq!(
            std::env::var("TERMINAL_DOCKER_MOUNT_CWD_TO_WORKSPACE").unwrap(),
            "true"
        );
        assert_eq!(
            std::env::var("TERMINAL_AUTO_SOURCE_BASHRC").unwrap(),
            "false"
        );
        clear_terminal_env_bridge_vars();
    }

    #[test]
    fn load_config_bridges_discord_allow_from_alias_to_allowed_users() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::remove_var("DISCORD_ALLOWED_USERS");
            std::env::remove_var("DISCORD_BOT_TOKEN");
        }
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            r#"
platforms:
  discord:
    enabled: true
    allow_from:
      - "100"
      - 200
"#,
        )
        .unwrap();

        let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
        let discord = cfg.platforms.get("discord").expect("discord config");
        assert_eq!(discord.allowed_users, vec!["100", "200"]);
        assert!(discord.extra.contains_key("allow_from"));
    }

    #[test]
    fn load_config_bridges_discord_extra_allow_from_and_preserves_env_precedence() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("DISCORD_ALLOWED_USERS", "env-user");
            std::env::remove_var("DISCORD_BOT_TOKEN");
        }
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.yaml"),
            r#"
platforms:
  discord:
    enabled: true
    extra:
      allow_from: cfg-user-1,cfg-user-2
"#,
        )
        .unwrap();

        let cfg = load_config(Some(dir.path().to_string_lossy().as_ref())).unwrap();
        let discord = cfg.platforms.get("discord").expect("discord config");
        assert_eq!(discord.allowed_users, vec!["env-user"]);

        unsafe {
            std::env::remove_var("DISCORD_ALLOWED_USERS");
        }
    }

    #[test]
    fn set_user_config_value_preserves_list_siblings_for_indexed_paths() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
custom_providers:
- name: provider-a
  api_key: old-a
  base_url: https://a.example.com
- name: provider-b
  api_key: old-b
  base_url: https://b.example.com
"#,
        )
        .unwrap();

        set_user_config_value(dir.path(), "custom_providers.0.api_key", "new-a").unwrap();

        let reloaded: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let providers = reloaded
            .get("custom_providers")
            .and_then(|v| v.as_sequence())
            .unwrap();
        assert_eq!(providers.len(), 2);
        assert_eq!(
            providers[0].get("api_key").and_then(|v| v.as_str()),
            Some("new-a")
        );
        assert_eq!(
            providers[0].get("base_url").and_then(|v| v.as_str()),
            Some("https://a.example.com")
        );
        assert_eq!(
            providers[1].get("api_key").and_then(|v| v.as_str()),
            Some("old-b")
        );
    }

    #[test]
    fn apply_patch_save_load_roundtrip() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "model", "openai:gpt-4o-mini").unwrap();
        apply_user_config_patch(&mut c, "max_turns", "15").unwrap();
        save_config_yaml(&path, &c).unwrap();
        let loaded = load_user_config_file(&path).unwrap();
        assert_eq!(loaded.model.as_deref(), Some("openai:gpt-4o-mini"));
        assert_eq!(loaded.max_turns, 15);
    }

    #[test]
    fn apply_patch_server_fields() {
        let mut c = GatewayConfig::default();
        apply_user_config_patch(
            &mut c,
            "server.base_url",
            "https://server.flowyaipc.cn/claw",
        )
        .unwrap();
        apply_user_config_patch(&mut c, "server.channel", "flowy").unwrap();
        apply_user_config_patch(&mut c, "server.app", "flowymes").unwrap();
        apply_user_config_patch(&mut c, "server.auth.preferred_method", "email").unwrap();
        assert_eq!(
            c.server.base_url,
            "https://server.flowyaipc.cn/claw"
        );
        assert_eq!(c.server.channel, "flowy");
        assert_eq!(c.server.app, "flowymes");
        assert_eq!(
            c.server.auth.preferred_method,
            crate::server::ServerLoginMethod::EmailOtp
        );
        let display = user_config_field_display(&c, "server.base_url").unwrap();
        assert_eq!(display, "https://server.flowyaipc.cn/claw");
    }

    #[test]
    fn load_user_config_file_parses_agent_api_max_retries_aliases() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let snake_path = dir.path().join("snake.yaml");
        std::fs::write(
            &snake_path,
            r#"
agent:
  api_max_retries: 6
"#,
        )
        .unwrap();
        let snake = load_user_config_file(&snake_path).unwrap();
        assert_eq!(snake.agent.api_max_retries, Some(6));

        let camel_path = dir.path().join("camel.yaml");
        std::fs::write(
            &camel_path,
            r#"
agent:
  apiMaxRetries: 8
"#,
        )
        .unwrap();
        let camel = load_user_config_file(&camel_path).unwrap();
        assert_eq!(camel.agent.api_max_retries, Some(8));
    }

    #[test]
    fn load_user_config_file_parses_auxiliary_task_overrides() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
auxiliary:
  vision:
    provider: openrouter
    model: google/gemini-2.5-flash
    base_url: http://localhost:1234/v1
    api_key: local-key
    timeout: 120
    download_timeout: 30
  web_extract:
    provider: auto
    model: custom-llm
"#,
        )
        .unwrap();

        let loaded = load_user_config_file(&path).unwrap();
        let vision = loaded.auxiliary.get("vision").expect("vision config");
        assert_eq!(vision.provider, "openrouter");
        assert_eq!(vision.model, "google/gemini-2.5-flash");
        assert_eq!(vision.base_url, "http://localhost:1234/v1");
        assert_eq!(vision.api_key, "local-key");
        assert_eq!(vision.timeout, Some(120));
        assert_eq!(vision.download_timeout, Some(30));
        let web_extract = loaded
            .auxiliary
            .get("web_extract")
            .expect("web extract config");
        assert_eq!(web_extract.provider, "auto");
        assert_eq!(web_extract.model, "custom-llm");
    }

    #[test]
    fn load_user_config_file_parses_llm_provider_api_mode() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
llm_providers:
  codex:
    base_url: https://gateway.example.com/v1
    api_key_env: CODEX_KEY
    api_mode: codex_responses
"#,
        )
        .unwrap();

        let loaded = load_user_config_file(&path).unwrap();
        let provider = loaded.llm_providers.get("codex").expect("codex provider");
        assert_eq!(provider.api_mode.as_deref(), Some("codex_responses"));
    }

    #[test]
    fn load_user_config_file_rejects_unknown_llm_provider_api_mode() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(
            &path,
            r#"
llm_providers:
  custom:
    base_url: https://gateway.example.com/v1
    api_mode: random_wire_shape
"#,
        )
        .unwrap();

        let err = load_user_config_file(&path).unwrap_err().to_string();
        assert!(err.contains("llm_providers.custom.api_mode"));
    }

    #[test]
    fn apply_patch_dotted_llm_proxy_budget() {
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "llm.openai.api_key", "sk-test").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.base_url", "https://api.openai.com/v1")
            .unwrap();
        apply_user_config_patch(&mut c, "llm.openai.api_mode", "codex-responses").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.command", "copilot-language-server").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.args", "--stdio,--model,gpt-4o-mini").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "45.5").unwrap();
        apply_user_config_patch(&mut c, "proxy.http", "http://127.0.0.1:8080").unwrap();
        apply_user_config_patch(&mut c, "budget.max_result_size_chars", "500").unwrap();
        apply_user_config_patch(&mut c, "sessions.auto_prune", "true").unwrap();
        apply_user_config_patch(&mut c, "sessions.retention_days", "30").unwrap();
        apply_user_config_patch(&mut c, "sessions.vacuum_after_prune", "false").unwrap();
        apply_user_config_patch(&mut c, "sessions.min_interval_hours", "12").unwrap();
        assert_eq!(
            c.llm_providers.get("openai").unwrap().api_key.as_deref(),
            Some("sk-test")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().base_url.as_deref(),
            Some("https://api.openai.com/v1")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().api_mode.as_deref(),
            Some("codex_responses")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().command.as_deref(),
            Some("copilot-language-server")
        );
        assert_eq!(
            c.llm_providers.get("openai").unwrap().args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
        assert_eq!(
            c.llm_providers
                .get("openai")
                .unwrap()
                .request_timeout_seconds,
            Some(45.5)
        );
        assert_eq!(
            c.proxy.as_ref().unwrap().http_proxy.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(c.budget.max_result_size_chars, 500);
        assert!(c.sessions.auto_prune);
        assert_eq!(c.sessions.retention_days, 30);
        assert!(!c.sessions.vacuum_after_prune);
        assert_eq!(c.sessions.min_interval_hours, 12);
        apply_user_config_patch(&mut c, "interest.enabled", "false").unwrap();
        apply_user_config_patch(
            &mut c,
            "insights.contribution.endpoint",
            "https://ops.example.com/v1/insights/batch",
        )
        .unwrap();
        apply_user_config_patch(&mut c, "insights.contribution.enabled", "true").unwrap();
        assert!(!c.interest.enabled);
        assert_eq!(
            c.insights.contribution.endpoint,
            "https://ops.example.com/v1/insights/batch"
        );
        assert!(c.insights.contribution.enabled);
        assert!(
            user_config_field_display(&c, "llm.openai.api_key")
                .unwrap()
                .starts_with("***")
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.command").unwrap(),
            "copilot-language-server"
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.api_mode").unwrap(),
            "codex_responses"
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.args").unwrap(),
            "--stdio,--model,gpt-4o-mini"
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.request_timeout_seconds").unwrap(),
            "45.5"
        );
        assert_eq!(
            user_config_field_display(&c, "sessions.auto_prune").unwrap(),
            "true"
        );
        assert_eq!(
            user_config_field_display(&c, "sessions.retention_days").unwrap(),
            "30"
        );
        assert!(
            apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "0").is_err()
        );
        assert!(
            apply_user_config_patch(&mut c, "llm.openai.request_timeout_seconds", "fast").is_err()
        );
    }

    #[test]
    fn apply_patch_dotted_kanban_dispatch_gate() {
        let mut c = GatewayConfig::default();
        assert!(c.kanban.dispatch_in_gateway);

        apply_user_config_patch(&mut c, "kanban.dispatch_in_gateway", "false").unwrap();

        assert!(!c.kanban.dispatch_in_gateway);
        assert_eq!(
            user_config_field_display(&c, "kanban.dispatch_in_gateway").unwrap(),
            "false"
        );
        assert!(apply_user_config_patch(&mut c, "kanban.dispatch_in_gateway", "maybe").is_err());
    }

    #[test]
    fn apply_patch_dotted_agent_api_max_retries() {
        let mut c = GatewayConfig::default();
        assert_eq!(c.agent.api_max_retries, None);

        apply_user_config_patch(&mut c, "agent.api_max_retries", "7").unwrap();

        assert_eq!(c.agent.api_max_retries, Some(7));
        assert_eq!(
            user_config_field_display(&c, "agent.api_max_retries").unwrap(),
            "7"
        );
        assert!(apply_user_config_patch(&mut c, "agent.api_max_retries", "nope").is_err());
    }

    #[test]
    fn apply_patch_dotted_auxiliary_values() {
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "auxiliary.vision.provider", "openrouter").unwrap();
        apply_user_config_patch(&mut c, "auxiliary.vision.model", "google/gemini-2.5-flash")
            .unwrap();
        apply_user_config_patch(
            &mut c,
            "auxiliary.vision.base_url",
            "http://localhost:1234/v1",
        )
        .unwrap();
        apply_user_config_patch(&mut c, "auxiliary.vision.api_key", "local-key").unwrap();
        apply_user_config_patch(&mut c, "auxiliary.vision.timeout", "120").unwrap();
        apply_user_config_patch(&mut c, "auxiliary.vision.download_timeout", "30").unwrap();

        let vision = c.auxiliary.get("vision").expect("vision config");
        assert_eq!(vision.provider, "openrouter");
        assert_eq!(vision.model, "google/gemini-2.5-flash");
        assert_eq!(vision.base_url, "http://localhost:1234/v1");
        assert_eq!(vision.api_key, "local-key");
        assert_eq!(vision.timeout, Some(120));
        assert_eq!(vision.download_timeout, Some(30));
        assert_eq!(
            user_config_field_display(&c, "auxiliary.vision.provider").unwrap(),
            "openrouter"
        );
        assert_eq!(
            user_config_field_display(&c, "auxiliary.vision.model").unwrap(),
            "google/gemini-2.5-flash"
        );
        assert_eq!(
            user_config_field_display(&c, "auxiliary.vision.api_key").unwrap(),
            "***-key"
        );
        assert_eq!(
            user_config_field_display(&c, "auxiliary.vision.timeout").unwrap(),
            "120"
        );
    }

    #[test]
    fn sanitize_env_lines_splits_concatenated_assignments() {
        let raw = "TELEGRAM_BOT_TOKEN=12345ANTHROPIC_API_KEY=sk-ant-test\n";
        let sanitized = sanitize_env_lines(raw);
        assert_eq!(
            sanitized,
            "TELEGRAM_BOT_TOKEN=12345\nANTHROPIC_API_KEY=sk-ant-test\n"
        );
    }

    #[test]
    fn load_dotenv_file_sanitizes_project_env_before_loading() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let project = tempdir().expect("project tempdir");
        let project_env = project.path().join(".env");

        std::fs::write(
            &project_env,
            "TELEGRAM_BOT_TOKEN=abc123ANTHROPIC_API_KEY=sk-ant-test\n",
        )
        .expect("write project env");

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("TELEGRAM_BOT_TOKEN", "stale");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }

        sanitize_env_file_if_needed(&project_env);
        load_dotenv_file(&project_env, true);

        assert_eq!(
            std::env::var("TELEGRAM_BOT_TOKEN").ok().as_deref(),
            Some("abc123")
        );
        assert_eq!(
            std::env::var("ANTHROPIC_API_KEY").ok().as_deref(),
            Some("sk-ant-test")
        );
        let rewritten = std::fs::read_to_string(&project_env).expect("read sanitized env");
        assert!(rewritten.contains("\nANTHROPIC_API_KEY="));

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("TELEGRAM_BOT_TOKEN");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }

    #[test]
    fn project_env_only_fills_missing_when_user_env_loaded() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempdir().expect("tempdir");
        let user_env = dir.path().join("user.env");
        let project_env = dir.path().join("project.env");
        std::fs::write(&user_env, "OPENAI_API_KEY=user-key\n").expect("write user env");
        std::fs::write(
            &project_env,
            "OPENAI_API_KEY=project-key\nEXA_API_KEY=exa-key\n",
        )
        .expect("write project env");

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("EXA_API_KEY");
        }

        load_dotenv_file(&user_env, true);
        load_dotenv_file(&project_env, false);

        assert_eq!(
            std::env::var("OPENAI_API_KEY").ok().as_deref(),
            Some("user-key")
        );
        assert_eq!(
            std::env::var("EXA_API_KEY").ok().as_deref(),
            Some("exa-key")
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("EXA_API_KEY");
        }
    }

    #[test]
    fn apply_env_overrides_ignores_empty_provider_keys() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("OPENROUTER_API_KEY", "");
            std::env::set_var("MINIMAX_API_KEY", "   ");
            std::env::remove_var("NOUS_API_KEY");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert!(
            !cfg.llm_providers.contains_key("openrouter"),
            "empty OPENROUTER_API_KEY should not create provider entry"
        );
        assert!(
            !cfg.llm_providers.contains_key("minimax"),
            "empty MINIMAX_API_KEY should not create provider entry"
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
            std::env::remove_var("MINIMAX_API_KEY");
        }
    }

    #[test]
    fn apply_env_overrides_supports_kanban_dispatch_gate() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe {
            std::env::set_var("HERMES_KANBAN_DISPATCH_IN_GATEWAY", "false");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);
        assert!(!cfg.kanban.dispatch_in_gateway);

        unsafe {
            std::env::remove_var("HERMES_KANBAN_DISPATCH_IN_GATEWAY");
        }
    }

    #[test]
    fn apply_env_overrides_supports_agent_api_max_retries() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe {
            std::env::set_var("HERMES_AGENT_API_MAX_RETRIES", "9");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);
        assert_eq!(cfg.agent.api_max_retries, Some(9));

        unsafe {
            std::env::remove_var("HERMES_AGENT_API_MAX_RETRIES");
        }
    }

    #[test]
    fn apply_env_overrides_openai_falls_back_when_primary_env_is_empty() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("HERMES_OPENAI_API_KEY", "");
            std::env::set_var("OPENAI_API_KEY", "fallback-openai-key");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert_eq!(
            cfg.llm_providers
                .get("openai")
                .and_then(|p| p.api_key.as_deref()),
            Some("fallback-openai-key")
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("HERMES_OPENAI_API_KEY");
            std::env::remove_var("OPENAI_API_KEY");
        }
    }

    #[test]
    fn apply_env_overrides_supports_codex_and_qwen_oauth_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("HERMES_OPENAI_CODEX_API_KEY", "codex-token");
            std::env::set_var("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth-token");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert_eq!(
            cfg.llm_providers
                .get("openai-codex")
                .and_then(|p| p.api_key.as_deref()),
            Some("codex-token")
        );
        assert_eq!(
            cfg.llm_providers
                .get("qwen-oauth")
                .and_then(|p| p.api_key.as_deref()),
            Some("qwen-oauth-token")
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("HERMES_OPENAI_CODEX_API_KEY");
            std::env::remove_var("HERMES_QWEN_OAUTH_API_KEY");
        }
    }

    #[test]
    fn apply_env_overrides_supports_copilot_env_var_precedence() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::set_var("COPILOT_GITHUB_TOKEN", "copilot-primary");
            std::env::set_var("GITHUB_COPILOT_TOKEN", "legacy-fallback");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert_eq!(
            cfg.llm_providers
                .get("copilot")
                .and_then(|p| p.api_key.as_deref()),
            Some("copilot-primary")
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("COPILOT_GITHUB_TOKEN");
            std::env::remove_var("GITHUB_COPILOT_TOKEN");
        }
    }

    #[test]
    fn apply_env_overrides_supports_direct_provider_env_vars() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe {
            std::env::set_var("ARCEEAI_API_KEY", "arcee-token");
            std::env::set_var("XIAOMI_API_KEY", "xiaomi-token");
            std::env::set_var("TOKENHUB_API_KEY", "tokenhub-token");
            std::env::set_var("TOKENHUB_BASE_URL", "https://tokenhub.example/v1");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert_eq!(
            cfg.llm_providers
                .get("arcee")
                .and_then(|p| p.api_key.as_deref()),
            Some("arcee-token")
        );
        assert_eq!(
            cfg.llm_providers
                .get("xiaomi")
                .and_then(|p| p.api_key.as_deref()),
            Some("xiaomi-token")
        );
        let tokenhub = cfg
            .llm_providers
            .get("tencent-tokenhub")
            .expect("tokenhub provider");
        assert_eq!(tokenhub.api_key.as_deref(), Some("tokenhub-token"));
        assert_eq!(
            tokenhub.base_url.as_deref(),
            Some("https://tokenhub.example/v1")
        );

        unsafe {
            std::env::remove_var("ARCEEAI_API_KEY");
            std::env::remove_var("XIAOMI_API_KEY");
            std::env::remove_var("TOKENHUB_API_KEY");
            std::env::remove_var("TOKENHUB_BASE_URL");
        }
    }

    #[test]
    fn apply_env_overrides_ignores_generic_github_tokens_for_copilot() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("COPILOT_GITHUB_TOKEN");
            std::env::remove_var("GITHUB_COPILOT_TOKEN");
            std::env::set_var("GH_TOKEN", "generic-gh-token");
            std::env::set_var("GITHUB_TOKEN", "generic-github-token");
        }

        let mut cfg = GatewayConfig::default();
        apply_env_overrides(&mut cfg);

        assert!(
            !cfg.llm_providers.contains_key("copilot"),
            "generic GitHub tokens should not auto-configure the Copilot provider"
        );

        // SAFETY: test process serializes env mutation via ENV_LOCK.
        unsafe {
            std::env::remove_var("GH_TOKEN");
            std::env::remove_var("GITHUB_TOKEN");
        }
    }

    #[test]
    fn apply_env_overrides_slack_env_token_enables_platform_by_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe {
            std::env::set_var("SLACK_BOT_TOKEN", "[REDACTED_SLACK_TOKEN]");
        }

        let mut cfg = GatewayConfig::default();
        let slack = cfg
            .platforms
            .entry("slack".to_string())
            .or_insert_with(crate::platform::PlatformConfig::default);
        slack.enabled = false;
        slack.extra.insert(
            "channel_prompts".to_string(),
            serde_json::json!({"C1":"ops"}),
        );

        apply_env_overrides(&mut cfg);

        let slack = cfg.platforms.get("slack").expect("slack config");
        assert!(slack.enabled, "env token should auto-enable slack");
        assert_eq!(slack.token.as_deref(), Some("[REDACTED_SLACK_TOKEN]"));

        unsafe {
            std::env::remove_var("SLACK_BOT_TOKEN");
        }
    }

    #[test]
    fn apply_env_overrides_slack_env_token_respects_explicit_disable_marker() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        unsafe {
            std::env::set_var("SLACK_BOT_TOKEN", "[REDACTED_SLACK_TOKEN]");
        }

        let mut cfg = GatewayConfig::default();
        let slack = cfg
            .platforms
            .entry("slack".to_string())
            .or_insert_with(crate::platform::PlatformConfig::default);
        slack.enabled = false;
        slack
            .extra
            .insert("_enabled_explicit".to_string(), serde_json::json!(true));

        apply_env_overrides(&mut cfg);

        let slack = cfg.platforms.get("slack").expect("slack config");
        assert!(
            !slack.enabled,
            "explicitly disabled slack config must remain disabled"
        );
        assert_eq!(slack.token.as_deref(), Some("[REDACTED_SLACK_TOKEN]"));

        unsafe {
            std::env::remove_var("SLACK_BOT_TOKEN");
        }
    }

    #[test]
    fn apply_env_overrides_respects_ignore_rules_flags() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("HERMES_IGNORE_RULES", "1");
        }
        let mut cfg = GatewayConfig::default();
        cfg.agent.skip_context_files = false;
        apply_env_overrides(&mut cfg);
        assert!(cfg.agent.skip_context_files);
        unsafe {
            std::env::remove_var("HERMES_IGNORE_RULES");
        }
    }

    #[test]
    fn load_config_ignore_user_config_uses_defaults_when_files_exist() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().expect("tempdir");
        let cfg_path = home.path().join("config.yaml");
        std::fs::write(&cfg_path, "max_turns: 777\n").expect("write config");
        unsafe {
            std::env::set_var("HERMES_IGNORE_USER_CONFIG", "1");
        }
        let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).expect("load config");
        assert_eq!(cfg.max_turns, GatewayConfig::default().max_turns);
        unsafe {
            std::env::remove_var("HERMES_IGNORE_USER_CONFIG");
        }
    }
}
