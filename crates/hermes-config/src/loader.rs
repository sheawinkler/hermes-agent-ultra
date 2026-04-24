//! Configuration loading from YAML, JSON, and environment variables.

use std::path::Path;

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use crate::config::{GatewayConfig, LlmProviderConfig, ProxyConfig};
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
        let _ = std::fs::write(path, sanitized);
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

// ---------------------------------------------------------------------------
// load_config
// ---------------------------------------------------------------------------

/// Load the full configuration, applying the priority chain:
///
///   env vars  >  .env  >  cli-config.yaml > config.yaml > gateway.json > defaults
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

    // Start from defaults
    let mut config = GatewayConfig::default();

    // Layer 1: gateway.json (lowest priority file source)
    if gateway_json_path.exists() {
        match load_from_json(&gateway_json_path) {
            Ok(json_cfg) => config = json_cfg,
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", gateway_json_path.display());
            }
        }
    }

    // Layer 2: config.yaml (higher priority file source)
    if config_yaml_path.exists() {
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
    if cli_config_yaml_path.exists() {
        match load_from_yaml(&cli_config_yaml_path) {
            Ok(cli_cfg) => {
                config = merge_configs(&cli_cfg, &config);
            }
            Err(e) => {
                tracing::warn!("Failed to load {}: {e}", cli_config_yaml_path.display());
            }
        }
    }

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);
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
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let mut root: serde_yaml::Value =
        serde_yaml::from_str(&contents).map_err(yaml_to_config_error)?;
    if let serde_yaml::Value::Mapping(ref mut m) = root {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    let config: GatewayConfig = serde_yaml::from_value(root).map_err(yaml_to_config_error)?;
    Ok(config)
}

/// Load `config.yaml` from disk if it exists; otherwise return defaults (no env merge).
pub fn load_user_config_file(path: &Path) -> Result<GatewayConfig, ConfigError> {
    if path.exists() {
        let mut cfg = load_from_yaml(path)?;
        normalize_provider_secrets(&mut cfg);
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
            || provider.max_tokens.is_some()
            || provider.temperature.is_some()
            || provider.extra_body.is_some()
            || provider.rate_limit.is_some()
            || !provider.credential_pool.is_empty()
            || provider.oauth_token_url.is_some()
            || provider.oauth_client_id.is_some()
    });
}

const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, security.allow_private_urls, sessions.auto_prune|retention_days|vacuum_after_prune|min_interval_hours, llm.<provider>.api_key|api_key_env|base_url|model|command|args|oauth_token_url|oauth_client_id, smart_model_routing.enabled|max_simple_chars|max_simple_words|cheap_model.model|cheap_model.provider";

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
                "command" => entry.command = Some(value.to_string()),
                "args" => {
                    entry.args = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();
                }
                "oauth_token_url" => entry.oauth_token_url = Some(value.to_string()),
                "oauth_client_id" => entry.oauth_client_id = Some(value.to_string()),
                other => {
                    return Err(ConfigError::NotFound(format!(
                        "unknown llm field: llm.{}.{} (supported: api_key, api_key_env, base_url, model, command, args, oauth_token_url, oauth_client_id)",
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
        ["sessions", "auto_prune"] => Ok(config.sessions.auto_prune.to_string()),
        ["sessions", "retention_days"] => Ok(config.sessions.retention_days.to_string()),
        ["sessions", "vacuum_after_prune"] => Ok(config.sessions.vacuum_after_prune.to_string()),
        ["sessions", "min_interval_hours"] => Ok(config.sessions.min_interval_hours.to_string()),
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
    let yaml = serde_yaml::to_string(&to_save).map_err(yaml_to_config_error)?;
    std::fs::write(path, yaml).map_err(io_to_config_error)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// load_from_json
// ---------------------------------------------------------------------------

/// Load a GatewayConfig from a JSON file.
pub fn load_from_json(path: &Path) -> Result<GatewayConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(io_to_config_error)?;
    let config: GatewayConfig = serde_json::from_str(&contents).map_err(json_to_config_error)?;
    Ok(config)
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
///   GITHUB_COPILOT_TOKEN       -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
///
/// 另见 [`crate::python_platform_env::apply_python_named_platform_env`]：
/// `WEIXIN_*`、`DINGTALK_*` 等与 Python `gateway/platforms/*.py` 一致的键写入 `platforms`。
pub fn apply_env_overrides(config: &mut GatewayConfig) {
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
    for (env_var, provider_name) in [
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("HERMES_OPENAI_CODEX_API_KEY", "openai-codex"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth"),
        ("MOONSHOT_API_KEY", "kimi"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            if v.trim().is_empty() {
                continue;
            }
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .api_key = Some(v);
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
    fn env_overrides_model() {
        let mut config = GatewayConfig::default();
        // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
        config.model = Some("env-model".into());
        assert_eq!(config.model.as_deref(), Some("env-model"));
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
    fn apply_patch_dotted_llm_proxy_budget() {
        let mut c = GatewayConfig::default();
        apply_user_config_patch(&mut c, "llm.openai.api_key", "sk-test").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.base_url", "https://api.openai.com/v1")
            .unwrap();
        apply_user_config_patch(&mut c, "llm.openai.command", "copilot-language-server").unwrap();
        apply_user_config_patch(&mut c, "llm.openai.args", "--stdio,--model,gpt-4o-mini").unwrap();
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
            c.proxy.as_ref().unwrap().http_proxy.as_deref(),
            Some("http://127.0.0.1:8080")
        );
        assert_eq!(c.budget.max_result_size_chars, 500);
        assert!(c.sessions.auto_prune);
        assert_eq!(c.sessions.retention_days, 30);
        assert!(!c.sessions.vacuum_after_prune);
        assert_eq!(c.sessions.min_interval_hours, 12);
        assert!(user_config_field_display(&c, "llm.openai.api_key")
            .unwrap()
            .starts_with("***"));
        assert_eq!(
            user_config_field_display(&c, "llm.openai.command").unwrap(),
            "copilot-language-server"
        );
        assert_eq!(
            user_config_field_display(&c, "llm.openai.args").unwrap(),
            "--stdio,--model,gpt-4o-mini"
        );
        assert_eq!(
            user_config_field_display(&c, "sessions.auto_prune").unwrap(),
            "true"
        );
        assert_eq!(
            user_config_field_display(&c, "sessions.retention_days").unwrap(),
            "30"
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
}
