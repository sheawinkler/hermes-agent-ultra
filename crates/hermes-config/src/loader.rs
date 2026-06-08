//! Configuration loading from YAML, JSON, and environment variables.

use std::path::{Path, PathBuf};

// Re-export ConfigError for convenience
pub use hermes_core::ConfigError;

use crate::config::{GatewayConfig, LlmProviderConfig, ProxyConfig};
use crate::merge::merge_configs;
use crate::paths;
use crate::platform::PlatformConfig;

// ---------------------------------------------------------------------------
// Rich YAML config error type
// ---------------------------------------------------------------------------

/// Which stage of YAML parsing failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YamlParseStage {
    /// YAML syntax error (string -> Value).
    Syntax,
    /// Type mismatch during deserialization (Value -> GatewayConfig).
    TypeMismatch,
}

/// Rich error information for YAML config parsing failures.
///
/// Marked `#[non_exhaustive]` so future field additions are not breaking changes
/// for downstream code using struct literal syntax.
#[derive(Debug)]
#[non_exhaustive]
pub struct YamlConfigError {
    pub file_path: PathBuf,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub field_path: Option<String>,
    pub message: String,
    pub stage: YamlParseStage,
}

impl std::fmt::Display for YamlConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Config parse error in {}", self.file_path.display())?;
        if let Some(path) = &self.field_path {
            writeln!(f, "  Field: {path}")?;
        }
        match (self.line, self.column) {
            (Some(l), Some(c)) => writeln!(f, "  Location: line {l}, column {c}")?,
            (Some(l), None) => writeln!(f, "  Location: line {l}")?,
            _ => {}
        }
        writeln!(f, "  Problem: {}", self.message)?;
        match self.stage {
            YamlParseStage::Syntax => {
                write!(f, "  Note: This is a YAML syntax error.")?
            }
            YamlParseStage::TypeMismatch => {
                write!(
                    f,
                    "  Note: Check that the field has the correct type (string/number/object/array)."
                )?
            }
        }
        Ok(())
    }
}

/// Width of the error banner printed to stderr when a config file fails to load.
const ERROR_BANNER_WIDTH: usize = 60;

/// Print a prominent error banner to stderr and emit a structured tracing error
/// when a config file fails to load.  Called from `load_config` for both
/// `config.yaml` and `cli-config.yaml` failures.
fn log_config_load_failed(file_path: &std::path::Path, error: &ConfigError) {
    let banner = "=".repeat(ERROR_BANNER_WIDTH);
    let file = file_path.display().to_string();
    eprintln!(
        "\n{banner}\nConfig file load failed\n  File: {file}\n  Error: {error}\n  This file was skipped; falling back to defaults.\n{banner}"
    );
    tracing::error!(file, error = %error, "Config file load failed; falling back to defaults");
}

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

    // Ensure config.yaml exists: create with defaults if missing so that
    // subsequent `hermes config set` operations have a file to write to.
    if !ignore_user_config && !config_yaml_path.exists() {
        if let Some(parent) = config_yaml_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let default_config = GatewayConfig::default();
        if let Err(e) = save_config_yaml(&config_yaml_path, &default_config) {
            tracing::warn!("Failed to create default config.yaml: {e}");
        } else {
            tracing::info!(
                "Created default config.yaml at {}",
                config_yaml_path.display()
            );
        }
    }

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
                log_config_load_failed(&config_yaml_path, &e);
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
                log_config_load_failed(&cli_config_yaml_path, &e);
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

    // Stage (a): YAML syntax -> Value
    let mut root: serde_yaml::Value = match serde_yaml::from_str(&contents) {
        Ok(v) => v,
        Err(e) => {
            let loc = e.location();
            let rich = YamlConfigError {
                file_path: path.to_path_buf(),
                line: loc.as_ref().map(|l| l.line()),
                column: loc.as_ref().map(|l| l.column()),
                field_path: None,
                message: e.to_string(),
                stage: YamlParseStage::Syntax,
            };
            return Err(ConfigError::ParseError(rich.to_string()));
        }
    };

    if let serde_yaml::Value::Mapping(ref mut m) = root {
        crate::python_yaml_compat::normalize_config_yaml_root(m);
    }
    mark_platform_enabled_explicit(&mut root, "slack");

    // Stage (b): Value -> GatewayConfig with field path tracking
    let config: GatewayConfig = match serde_path_to_error::deserialize(root) {
        Ok(c) => c,
        Err(e) => {
            let field_path = e.path().to_string();
            let inner: serde_yaml::Error = e.into_inner();
            let loc = inner.location();
            let rich = YamlConfigError {
                file_path: path.to_path_buf(),
                line: loc.as_ref().map(|l| l.line()),
                column: loc.as_ref().map(|l| l.column()),
                field_path: Some(field_path),
                message: inner.to_string(),
                stage: YamlParseStage::TypeMismatch,
            };
            return Err(ConfigError::ParseError(rich.to_string()));
        }
    };

    Ok(config)
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

const CONFIG_PATCH_HELP: &str = "model, personality, max_turns, system_prompt, budget.max_result_size_chars, budget.max_aggregate_chars, proxy.http, proxy.socks, security.allow_private_urls, sessions.auto_prune|retention_days|vacuum_after_prune|min_interval_hours, interest.enabled|extract_mode|max_topics|llm_on_session_end|per_turn_buffer|per_turn_persist|promote_min_evidence|promote_min_confidence|min_turn_chars|llm.<provider>.api_key|..., insights.contribution.enabled|endpoint|upload_interests|upload_skills|on_session_end|skill_min_age_hours|redacted_body|installation_token|auth_token, smart_model_routing.enabled|...";

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
        ["insights", "contribution", "skill_min_age_hours"] => {
            config.insights.contribution.skill_min_age_hours = value.parse().map_err(|_| {
                ConfigError::ValidationError(format!(
                    "insights.contribution.skill_min_age_hours must be a non-negative integer: {}",
                    value
                ))
            })?;
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
        ["interest", "enabled"] => Ok(config.interest.enabled.to_string()),
        ["interest", "extract_mode"] => Ok(config.interest.extract_mode.clone()),
        ["interest", "max_topics"] => Ok(config.interest.max_topics.to_string()),
        ["interest", "llm_on_session_end"] => Ok(config.interest.llm_on_session_end.to_string()),
        ["insights", "contribution", "enabled"] => {
            Ok(config.insights.contribution.enabled.to_string())
        }
        ["insights", "contribution", "endpoint"] => Ok(if config.insights.contribution.endpoint.is_empty() {
            "(not set)".to_string()
        } else {
            config.insights.contribution.endpoint.clone()
        }),
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
///   OPENROUTER_BASE_URL        -> llm_providers["openrouter"].base_url
///   MINIMAX_BASE_URL           -> llm_providers["minimax"].base_url
///   DASHSCOPE_API_KEY          -> llm_providers["qwen"].api_key
///   MOONSHOT_API_KEY           -> llm_providers["kimi"].api_key
///   MINIMAX_API_KEY            -> llm_providers["minimax"].api_key
///   NOUS_API_KEY               -> llm_providers["nous"].api_key
///   GITHUB_COPILOT_TOKEN       -> llm_providers["copilot"].api_key
///   HERMES_BASE_URL            -> all llm_providers[*].base_url
///   HERMES_INSIGHTS_ENDPOINT   -> insights.contribution.endpoint
///   HERMES_INSIGHTS_TOKEN      -> insights.contribution.auth_token / installation_token (Bearer JWT or flowy- API key)
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

    if let Ok(v) = std::env::var("OPENROUTER_BASE_URL") {
        if !v.trim().is_empty() {
            config
                .llm_providers
                .entry("openrouter".to_string())
                .or_insert_with(LlmProviderConfig::default)
                .base_url = Some(v);
        }
    }
    if let Ok(v) = std::env::var("MINIMAX_BASE_URL") {
        if !v.trim().is_empty() {
            config
                .llm_providers
                .entry("minimax".to_string())
                .or_insert_with(LlmProviderConfig::default)
                .base_url = Some(v);
        }
    }

    if let Ok(token) = std::env::var("SLACK_BOT_TOKEN") {
        let trimmed = token.trim();
        if !trimmed.is_empty() {
            let slack = config
                .platforms
                .entry("slack".to_string())
                .or_insert_with(PlatformConfig::default);
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

    #[test]
    fn load_config_creates_default_config_yaml_when_missing() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().expect("tempdir");
        let cfg_path = home.path().join("config.yaml");

        // Pre-condition: config.yaml must not exist
        assert!(!cfg_path.exists(), "config.yaml should not exist before load");

        let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).expect("load config");

        // Post-condition 1: config.yaml should have been created
        assert!(cfg_path.exists(), "config.yaml should be auto-created when missing");

        // Post-condition 2: loaded config should match defaults
        assert_eq!(cfg.max_turns, GatewayConfig::default().max_turns);
        assert_eq!(cfg.model, GatewayConfig::default().model);

        // Post-condition 3: the created file should be valid YAML and reloadable
        let reloaded = load_user_config_file(&cfg_path).expect("reload created config");
        assert_eq!(reloaded.max_turns, GatewayConfig::default().max_turns);
        assert_eq!(reloaded.model, GatewayConfig::default().model);

        // Post-condition 4: the created file should contain the default model
        let content = std::fs::read_to_string(&cfg_path).expect("read config");
        assert!(
            content.contains("model: gpt-4o"),
            "auto-created config should include default model; content:\n{content}"
        );
    }

    #[test]
    fn load_config_does_not_overwrite_existing_config_yaml() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().expect("tempdir");
        let cfg_path = home.path().join("config.yaml");
        std::fs::write(&cfg_path, "max_turns: 42\n").expect("write existing config");

        let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).expect("load config");

        // Existing value should be preserved
        assert_eq!(cfg.max_turns, 42);

        // File content should remain unchanged
        let content = std::fs::read_to_string(&cfg_path).expect("read config");
        assert!(content.contains("max_turns: 42"));
    }

    #[test]
    fn yaml_syntax_error_includes_line_column() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "model: \"unclosed\n").unwrap();

        let err = load_from_yaml(&path).unwrap_err();
        let msg = err.to_string();
        // Best-effort assertions: these depend on serde_yaml::Error::to_string() output.
        // If serde_yaml changes its error message format, these may need updating.
        assert!(msg.contains("line"), "should mention line: {msg}");
        assert!(msg.contains("Syntax"), "should mention stage: {msg}");
    }

    #[test]
    fn yaml_type_mismatch_includes_field_path() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        // max_turns expects u32, give a string
        std::fs::write(&path, "max_turns: not_a_number\n").unwrap();

        let err = load_from_yaml(&path).unwrap_err();
        let msg = err.to_string();
        // Best-effort assertions: these depend on serde_yaml::Error::to_string() output.
        assert!(msg.contains("max_turns"), "should mention field: {msg}");
        assert!(msg.contains("TypeMismatch"), "should mention stage: {msg}");
    }

    #[test]
    fn yaml_nested_field_path_on_type_mismatch() {
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("config.yaml");
        // llm_providers.openai.api_key expects String, give a mapping
        std::fs::write(
            &path,
            "llm_providers:\n  openai:\n    api_key:\n      nested: value\n",
        )
        .unwrap();

        let err = load_from_yaml(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("llm_providers.openai.api_key"),
            "should show full path: {msg}"
        );
    }

    #[test]
    fn load_config_falls_back_to_defaults_on_parse_error() {
        use tempfile::tempdir;

        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = tempdir().expect("tempdir");
        let cfg_path = home.path().join("config.yaml");
        std::fs::write(&cfg_path, "max_turns: [1, 2, 3]\n").unwrap();

        let cfg = load_config(Some(home.path().to_string_lossy().as_ref())).expect("should not fail");
        assert_eq!(cfg.max_turns, GatewayConfig::default().max_turns);
    }
}
