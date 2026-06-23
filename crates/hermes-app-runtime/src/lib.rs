//! Non-UI application runtime policy and agent configuration.
//!
//! This crate stays below `hermes-cli`: tests for agent configuration and
//! query-mode policy should not compile terminal UI, platform adapters, cron
//! wiring, or slash-command rendering.

use std::collections::HashSet;
use std::path::PathBuf;

use hermes_agent::agent_loop::{
    CheapModelRouteConfig, RetryConfig, RuntimeProviderConfig, SmartModelRoutingConfig,
};
use hermes_agent::smart_model_routing::ApiMode;
use hermes_agent::AgentConfig;
use hermes_config::{normalize_service_tier, GatewayConfig, LlmProviderConfig};
use hermes_core::AgentError;
use hermes_provider_runtime::{
    active_llm_provider_config, normalize_runtime_provider_name, resolve_provider_and_model,
};
use serde_json::Value;

pub const QUERY_ALLOW_TOOLS_ENV_KEY: &str = "HERMES_QUERY_ALLOW_TOOLS";
pub const QUERY_DISABLE_TOOLS_ENV_KEY: &str = "HERMES_QUERY_DISABLE_TOOLS";

fn build_retry_config(config: &GatewayConfig) -> RetryConfig {
    let mut retry_cfg = RetryConfig::default();
    if let Some(max_retries) = config.agent.api_max_retries {
        retry_cfg.max_retries = max_retries;
    }
    let mut seen = HashSet::new();

    let mut push_candidate = |candidate: &str, retry_cfg: &mut RetryConfig| {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            return;
        }
        let identity = trimmed.to_ascii_lowercase();
        if seen.insert(identity) {
            retry_cfg.fallback_models.push(trimmed.to_string());
        }
    };

    for model in &config.fallback_models {
        push_candidate(model, &mut retry_cfg);
    }
    if let Some(model) = config.fallback_model.as_deref() {
        push_candidate(model, &mut retry_cfg);
    }

    if !retry_cfg.fallback_models.is_empty() {
        retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODELS") {
        let parsed: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        if !parsed.is_empty() {
            retry_cfg.fallback_models = parsed;
            retry_cfg.fallback_model = retry_cfg.fallback_models.first().cloned();
            return retry_cfg;
        }
    }

    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODEL") {
        let value = raw.trim();
        if !value.is_empty() {
            retry_cfg.fallback_model = Some(value.to_string());
            retry_cfg.fallback_models = vec![value.to_string()];
        }
    }

    retry_cfg
}

fn parse_provider_api_mode(value: &str) -> Option<ApiMode> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "chat_completions" => Some(ApiMode::ChatCompletions),
        "anthropic_messages" => Some(ApiMode::AnthropicMessages),
        "codex_responses" => Some(ApiMode::CodexResponses),
        "bedrock_converse" => Some(ApiMode::BedrockConverse),
        _ => None,
    }
}

fn configured_agent_max_tokens(provider_config: Option<&LlmProviderConfig>) -> Option<u32> {
    if let Ok(raw) = std::env::var("HERMES_MAX_TOKENS") {
        if let Ok(value) = raw.trim().parse::<u32>() {
            if value > 0 {
                return Some(value);
            }
        }
    }
    provider_config.and_then(|cfg| cfg.max_tokens.filter(|value| *value > 0))
}

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
    let (resolved_provider, _) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(resolved_provider.as_str());
    let provider_config = active_llm_provider_config(
        config,
        resolved_provider.as_str(),
        runtime_provider.as_str(),
    );
    let provider_extra_body = provider_config.and_then(|cfg| cfg.extra_body.clone());
    let max_tokens = configured_agent_max_tokens(provider_config);
    let extra_body =
        merge_service_tier_extra_body(provider_extra_body, config.agent.normalized_service_tier());
    let skip_memory_env = std::env::var("HERMES_SKIP_MEMORY")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let skip_context_files_env = std::env::var("HERMES_SKIP_CONTEXT_FILES")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let hermes_home = config
        .home_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let skip_memory = skip_memory_env || hermes_home.join(".memory_disabled").exists();
    let skip_context_files = config.agent.skip_context_files || skip_context_files_env;

    let retry_cfg = build_retry_config(config);
    let max_delegate_depth = config
        .delegation
        .max_spawn_depth
        .map(|depth| depth.max(1))
        .unwrap_or_else(|| AgentConfig::default().max_delegate_depth);

    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        extra_body,
        hermes_home: config.home_dir.clone(),
        provider: Some(resolved_provider),
        stream: config.streaming.enabled,
        max_tokens,
        max_delegate_depth,
        delegation_model: config
            .delegation
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_provider: config
            .delegation
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_base_url: config
            .delegation
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_api_key: config
            .delegation
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        skip_memory,
        skip_context_files,
        coding_context: config.agent.coding_context.clone(),
        platform: Some("cli".to_string()),
        enabled_skills: config.skills.enabled.clone(),
        disabled_skills: config.skills.disabled.clone(),
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        base_url: cfg.base_url.clone(),
                        request_timeout_seconds: cfg.request_timeout_seconds,
                        api_mode: cfg.api_mode.as_deref().and_then(parse_provider_api_mode),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        prefill_messages: hermes_config::load_prefill_messages(config),
        retry: retry_cfg,
        smart_model_routing: SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        code_index_enabled: config.agent.code_index_enabled,
        code_index_max_files: config.agent.code_index_max_files,
        code_index_max_symbols: config.agent.code_index_max_symbols,
        lsp_context_enabled: config.agent.lsp_context_enabled,
        lsp_context_max_chars: config.agent.lsp_context_max_chars,
        ..AgentConfig::default()
    }
}

fn merge_service_tier_extra_body(
    extra_body: Option<Value>,
    service_tier: Option<String>,
) -> Option<Value> {
    let Some(service_tier) = service_tier.and_then(|tier| normalize_service_tier(Some(&tier)))
    else {
        return extra_body;
    };
    let mut map = match extra_body {
        Some(Value::Object(map)) => map,
        Some(other) => {
            let mut map = serde_json::Map::new();
            map.insert("extra_body".to_string(), other);
            map
        }
        None => serde_json::Map::new(),
    };
    map.insert("service_tier".to_string(), Value::String(service_tier));
    Some(Value::Object(map))
}

pub fn resolve_cli_chat_provider_model_with(
    config_model: Option<&str>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
    normalize_provider_model: impl Fn(&str) -> Result<String, AgentError>,
) -> Result<String, AgentError> {
    let provider_override = provider_override
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    let model_override = model_override.map(str::trim).filter(|v| !v.is_empty());

    let mut current_model = config_model
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("gpt-4o")
        .to_string();

    if let Some(model) = model_override {
        current_model = model.to_string();
    } else if provider_override.is_none() {
        if let Ok(model_env) = std::env::var("HERMES_INFERENCE_MODEL") {
            let model_env = model_env.trim();
            if !model_env.is_empty() {
                current_model = model_env.to_string();
            }
        }
    }
    if let Some(provider) = provider_override.as_deref() {
        if let Some((_, model_name)) = current_model.split_once(':') {
            current_model = format!("{provider}:{}", model_name.trim());
        } else {
            current_model = format!("{provider}:{}", current_model.trim());
        }
    }
    if !current_model.contains(':') {
        current_model = normalize_provider_model(&current_model)?;
    }
    Ok(current_model)
}

pub fn apply_cli_chat_runtime_env(provider_model: &str) {
    let provider_model = provider_model.trim();
    if provider_model.is_empty() {
        return;
    }
    std::env::set_var("HERMES_MODEL", provider_model);
    std::env::set_var("HERMES_INFERENCE_MODEL", provider_model);
    if let Some((provider, _)) = provider_model.split_once(':') {
        let provider = provider.trim();
        if !provider.is_empty() {
            std::env::set_var("HERMES_INFERENCE_PROVIDER", provider);
            std::env::set_var("HERMES_TUI_PROVIDER", provider);
        }
    }
}

pub fn query_mode_tools_enabled(query_mode: bool, allow_tools_flag: bool) -> bool {
    if !query_mode {
        return true;
    }
    if allow_tools_flag {
        return true;
    }
    if hermes_config::env_var_enabled(QUERY_DISABLE_TOOLS_ENV_KEY) {
        return false;
    }
    // Backward compatible explicit-enable override (now redundant with default-on).
    if hermes_config::env_var_enabled(QUERY_ALLOW_TOOLS_ENV_KEY) {
        return true;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::LlmProviderConfig;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    fn test_normalize_provider_model(input: &str) -> Result<String, AgentError> {
        if input.contains(':') {
            Ok(input.to_string())
        } else {
            Ok(format!("openai:{input}"))
        }
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_api_key_env() {
        let mut cfg = GatewayConfig::default();
        let mut providers = std::collections::HashMap::new();
        providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers = providers;

        let agent_cfg = build_agent_config(&cfg, "custom:some-model");
        let runtime = agent_cfg
            .runtime_providers
            .get("custom")
            .expect("runtime provider should exist");
        assert_eq!(runtime.api_key_env.as_deref(), Some("MY_FALLBACK_KEY"));
    }

    #[test]
    fn test_build_agent_config_loads_prefill_messages_from_config() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_PREFILL_MESSAGES_FILE"]);
        std::env::remove_var("HERMES_PREFILL_MESSAGES_FILE");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("prefill.json"),
            r#"[{"role":"system","content":"cli prefill"},{"role":"user","content":"cli example"}]"#,
        )
        .unwrap();
        let cfg = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().to_string()),
            prefill_messages_file: Some("prefill.json".to_string()),
            ..GatewayConfig::default()
        };

        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.prefill_messages.len(), 2);
        assert_eq!(
            agent_cfg.prefill_messages[0].content.as_deref(),
            Some("cli prefill")
        );
        assert_eq!(
            agent_cfg.prefill_messages[1].content.as_deref(),
            Some("cli example")
        );
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_request_timeout_seconds() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
                request_timeout_seconds: Some(45.5),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "anthropic:claude-sonnet-4.5");
        let runtime = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("runtime provider should exist");

        assert_eq!(runtime.request_timeout_seconds, Some(45.5));
    }

    #[test]
    fn test_build_agent_config_maps_delegation_max_spawn_depth_without_legacy_ceiling() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.max_spawn_depth = Some(99);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 99);

        cfg.delegation.max_spawn_depth = Some(0);
        let agent_cfg = build_agent_config(&cfg, "openai:gpt-4o");
        assert_eq!(agent_cfg.max_delegate_depth, 1);
    }

    #[test]
    fn test_build_agent_config_maps_delegation_provider_model_runtime_overrides() {
        let mut cfg = GatewayConfig::default();
        cfg.delegation.model = Some(" google/gemini-3-flash-preview ".to_string());
        cfg.delegation.provider = Some(" openrouter ".to_string());
        cfg.delegation.base_url = Some(" http://localhost:1234/v1 ".to_string());
        cfg.delegation.api_key = Some(" local-key ".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:hermes-3");

        assert_eq!(
            agent_cfg.delegation_model.as_deref(),
            Some("google/gemini-3-flash-preview")
        );
        assert_eq!(agent_cfg.delegation_provider.as_deref(), Some("openrouter"));
        assert_eq!(
            agent_cfg.delegation_base_url.as_deref(),
            Some("http://localhost:1234/v1")
        );
        assert_eq!(agent_cfg.delegation_api_key.as_deref(), Some("local-key"));
    }

    #[test]
    fn test_build_agent_config_preserves_same_host_provider_api_modes() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "codex".to_string(),
            LlmProviderConfig {
                api_key_env: Some("CODEX_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("codex_responses".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key_env: Some("ANTHROPIC_KEY".to_string()),
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_mode: Some("anthropic_messages".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");
        let codex = agent_cfg
            .runtime_providers
            .get("codex")
            .expect("codex runtime provider should exist");
        let anthropic = agent_cfg
            .runtime_providers
            .get("anthropic")
            .expect("anthropic runtime provider should exist");

        assert_eq!(codex.api_key_env.as_deref(), Some("CODEX_KEY"));
        assert_eq!(
            codex.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(codex.api_mode, Some(ApiMode::CodexResponses));
        assert_eq!(anthropic.api_key_env.as_deref(), Some("ANTHROPIC_KEY"));
        assert_eq!(
            anthropic.base_url.as_deref(),
            Some("https://gateway.example.com/v1")
        );
        assert_eq!(anthropic.api_mode, Some(ApiMode::AnthropicMessages));
    }

    #[test]
    fn test_build_agent_config_maps_named_custom_runtime_provider() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "beans".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-beans".to_string()),
                base_url: Some("http://beans.local/v1".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "beans:my-model");
        assert_eq!(agent_cfg.provider.as_deref(), Some("beans"));
        let runtime = agent_cfg
            .runtime_providers
            .get("beans")
            .expect("named custom runtime provider should exist");
        assert_eq!(runtime.api_key.as_deref(), Some("sk-beans"));
        assert_eq!(runtime.base_url.as_deref(), Some("http://beans.local/v1"));
    }

    #[test]
    fn test_build_agent_config_maps_active_provider_max_tokens() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                max_tokens: Some(2048),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");

        assert_eq!(agent_cfg.max_tokens, Some(4096));
    }

    #[test]
    fn test_build_agent_config_maps_normalized_provider_max_tokens_alias() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);
        std::env::remove_var("HERMES_MAX_TOKENS");

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openai-codex".to_string(),
            LlmProviderConfig {
                max_tokens: Some(1234),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "codex:gpt-5");

        assert_eq!(agent_cfg.max_tokens, Some(1234));
    }

    #[test]
    fn test_build_agent_config_env_max_tokens_overrides_provider_cap() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_MAX_TOKENS"]);

        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "openrouter".to_string(),
            LlmProviderConfig {
                max_tokens: Some(4096),
                ..LlmProviderConfig::default()
            },
        );

        std::env::set_var("HERMES_MAX_TOKENS", "8192");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(8192));

        std::env::set_var("HERMES_MAX_TOKENS", "not-a-number");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));

        std::env::set_var("HERMES_MAX_TOKENS", "0");
        let agent_cfg = build_agent_config(&cfg, "openrouter:anthropic/claude-sonnet-4.6");
        assert_eq!(agent_cfg.max_tokens, Some(4096));
    }

    #[test]
    fn test_build_agent_config_forwards_provider_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "high",
                    "reasoning": { "effort": "high" }
                })),
                ..LlmProviderConfig::default()
            },
        );
        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        assert_eq!(
            agent_cfg
                .extra_body
                .as_ref()
                .and_then(|body| body.get("reasoning_effort"))
                .and_then(|value| value.as_str()),
            Some("high")
        );
    }

    #[test]
    fn test_build_agent_config_merges_fast_service_tier_into_extra_body() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.service_tier = Some("fast".to_string());
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                extra_body: Some(serde_json::json!({
                    "reasoning_effort": "medium"
                })),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
        let body = agent_cfg.extra_body.expect("extra body");
        assert_eq!(body["reasoning_effort"], "medium");
        assert_eq!(body["service_tier"], "priority");
    }

    #[test]
    fn test_build_agent_config_infers_provider_for_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("claude-opus-4-6".to_string());
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                model: Some("claude-opus-4-6".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "claude-opus-4-6");
        assert_eq!(agent_cfg.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_build_agent_config_maps_failover_chain_from_env() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::set_var(
            "HERMES_FALLBACK_MODELS",
            "nous:moonshotai/kimi-k2.6,openai:gpt-4o-mini",
        );
        std::env::remove_var("HERMES_FALLBACK_MODEL");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("nous:moonshotai/kimi-k2.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "nous:moonshotai/kimi-k2.6".to_string(),
                "openai:gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_build_agent_config_maps_single_failover_model_from_env() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");
        let cfg = GatewayConfig::default();
        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("anthropic:claude-3-5-sonnet")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
    }

    #[test]
    fn test_build_agent_config_maps_failover_chain_from_config() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::remove_var("HERMES_FALLBACK_MODEL");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec![
            "openrouter:anthropic/claude-sonnet-4.6".to_string(),
            "nous:Hermes-4".to_string(),
        ];
        cfg.fallback_model = Some("OpenRouter:anthropic/claude-sonnet-4.6".to_string());

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_model.as_deref(),
            Some("openrouter:anthropic/claude-sonnet-4.6")
        );
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "nous:Hermes-4".to_string()
            ]
        );
    }

    #[test]
    fn test_build_agent_config_env_failover_overrides_config() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_FALLBACK_MODELS", "HERMES_FALLBACK_MODEL"]);
        std::env::remove_var("HERMES_FALLBACK_MODELS");
        std::env::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");

        let mut cfg = GatewayConfig::default();
        cfg.fallback_models = vec!["openrouter:backup".to_string()];

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
        assert_eq!(
            agent_cfg.retry.fallback_models,
            vec!["anthropic:claude-3-5-sonnet".to_string()]
        );
    }

    #[test]
    fn test_build_agent_config_maps_agent_api_max_retries() {
        let mut cfg = GatewayConfig::default();
        cfg.agent.api_max_retries = Some(11);

        let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");

        assert_eq!(agent_cfg.retry.max_retries, 11);
    }

    #[test]
    fn resolve_cli_chat_provider_model_defaults_to_config_when_no_overrides() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::remove_var("HERMES_INFERENCE_MODEL");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("nous:moonshotai/kimi-k2.6"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_applies_provider_override() {
        let resolved = resolve_cli_chat_provider_model_with(
            Some("gpt-4o"),
            None,
            Some("anthropic"),
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "anthropic:gpt-4o");
    }

    #[test]
    fn resolve_cli_chat_provider_model_prefers_model_override_with_provider_prefix() {
        let resolved = resolve_cli_chat_provider_model_with(
            Some("openai:gpt-4o"),
            Some("moonshotai/kimi-k2.6"),
            Some("nous"),
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_uses_inference_model_env_when_no_flag_override() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::set_var("HERMES_INFERENCE_MODEL", "nous:moonshotai/kimi-k2.6");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("openai:gpt-4o"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_cli_chat_provider_model_normalizes_bare_model() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_INFERENCE_MODEL"]);
        std::env::remove_var("HERMES_INFERENCE_MODEL");
        let resolved = resolve_cli_chat_provider_model_with(
            Some("gpt-4o"),
            None,
            None,
            test_normalize_provider_model,
        )
        .expect("resolve");
        assert_eq!(resolved, "openai:gpt-4o");
    }

    #[test]
    fn apply_cli_chat_runtime_env_sets_provider_model() {
        let _lock = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

        apply_cli_chat_runtime_env("nous:openai/gpt-5.5");

        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("nous:openai/gpt-5.5")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("nous")
        );
        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("nous")
        );
    }

    #[test]
    fn apply_cli_chat_runtime_env_sets_tui_provider_when_absent() {
        let _lock = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        apply_cli_chat_runtime_env("custom-xuanji:deepseek-v4-pro");

        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );
    }

    #[test]
    fn query_mode_tools_enabled_defaults_on_for_query_mode() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_DISABLE_TOOLS_ENV_KEY);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        assert!(query_mode_tools_enabled(true, false));
        assert!(query_mode_tools_enabled(false, false));
    }

    #[test]
    fn query_mode_tools_enabled_respects_disable_env_and_flag_override() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        std::env::set_var(QUERY_DISABLE_TOOLS_ENV_KEY, "1");
        assert!(!query_mode_tools_enabled(true, false));
        assert!(query_mode_tools_enabled(true, true));
    }

    #[test]
    fn query_mode_tools_enabled_respects_legacy_allow_env() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[QUERY_DISABLE_TOOLS_ENV_KEY, QUERY_ALLOW_TOOLS_ENV_KEY]);
        std::env::remove_var(QUERY_DISABLE_TOOLS_ENV_KEY);
        std::env::remove_var(QUERY_ALLOW_TOOLS_ENV_KEY);
        assert!(query_mode_tools_enabled(true, false));
        std::env::set_var(QUERY_ALLOW_TOOLS_ENV_KEY, "1");
        assert!(query_mode_tools_enabled(true, false));
    }
}
