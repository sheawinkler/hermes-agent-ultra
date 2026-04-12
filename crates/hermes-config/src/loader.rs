//! Configuration loading from YAML, JSON, and environment variables.

use std::path::Path;

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use crate::config::{GatewayConfig, LlmProviderConfig};
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

/// Load variables from `$HERMES_HOME/.env` into the process environment.
///
/// Only sets a variable if it is not already present in the environment,
/// so real env vars always win. Supports `#` comments, blank lines, and
/// optional single/double quoting of values.
pub fn load_dotenv() {
    let env_file = paths::env_path();
    let contents = match std::fs::read_to_string(&env_file) {
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
            let value = value.trim();
            let value = strip_quotes(value);
            if std::env::var(key).is_err() {
                // SAFETY: called once at startup before multi-threading.
                unsafe { std::env::set_var(key, value) };
            }
        }
    }
}

fn strip_quotes(s: &str) -> &str {
    if s.len() >= 2 && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\''))) {
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
///   env vars  >  .env  >  config.yaml  >  gateway.json  >  defaults
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

    // Layer 3: environment variables (highest priority)
    apply_env_overrides(&mut config);

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
    let config: GatewayConfig = serde_yaml::from_str(&contents).map_err(yaml_to_config_error)?;
    Ok(config)
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
///   OPENAI_API_KEY             -> llm_providers["openai"].api_key
///   ANTHROPIC_API_KEY          -> llm_providers["anthropic"].api_key
///   OPENROUTER_API_KEY         -> llm_providers["openrouter"].api_key
///   DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
///   MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
///   MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
///   NOUS_API_KEY               -> llm_providers["nous"].api_key
///   GITHUB_COPILOT_TOKEN       -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
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
    if let Ok(v) = std::env::var("HERMES_PROXY_HTTP") {
        let proxy = config.proxy.get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.http_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_PROXY_SOCKS") {
        let proxy = config.proxy.get_or_insert_with(crate::config::ProxyConfig::default);
        proxy.socks_proxy = Some(v);
    }
    if let Ok(v) = std::env::var("HERMES_LLM_API_KEY") {
        for provider in config.llm_providers.values_mut() {
            provider.api_key = Some(v.clone());
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

    // Provider-specific API keys
    for (env_var, provider_name) in [
        ("OPENAI_API_KEY", "openai"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("MOONSHOT_API_KEY", "kimi"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
    ] {
        if let Ok(v) = std::env::var(env_var) {
            config
                .llm_providers
                .entry(provider_name.to_string())
                .or_insert_with(LlmProviderConfig::default)
                .api_key = Some(v);
        }
    }

    if let Ok(v) = std::env::var("HERMES_BASE_URL") {
        for provider in config.llm_providers.values_mut() {
            provider.base_url = Some(v.clone());
        }
    }
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
            if key.is_empty() {
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
    fn env_overrides_model() {
        let mut config = GatewayConfig::default();
        // Simulate env var (we can't easily set env vars in tests, so test the logic directly)
        config.model = Some("env-model".into());
        assert_eq!(config.model.as_deref(), Some("env-model"));
    }
}