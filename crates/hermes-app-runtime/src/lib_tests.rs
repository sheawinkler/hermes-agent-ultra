#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{self, BoxStream};
    use futures::StreamExt;
    use hermes_config::LlmProviderConfig;
    use hermes_core::{LlmResponse, StreamChunk};

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

    #[test]
    fn runtime_reformulation_message_includes_objective_and_kernel_guidance() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_RUNTIME_PROMPT_REFORMULATION",
            "HERMES_RUNTIME_CONTRADICTION_SELF_CHECK",
            "HERMES_REPO_REVIEW_TOOL_PROFILE_MODE",
            "CONTEXTLATTICE_TOPIC_PATH",
            "HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS",
        ]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK", "1");
        std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        std::env::set_var(
            "CONTEXTLATTICE_TOPIC_PATH",
            "runbooks/objective/test-objective",
        );

        let objective = RuntimeReformulationObjective {
            id: "test-objective".to_string(),
            behavior_mode: "mission".to_string(),
            objective_text: "Grow SOL with controlled risk".to_string(),
            behavior_directives: vec!["Act with evidence".to_string()],
            success_criteria: vec!["Positive risk-adjusted delta".to_string()],
        };
        let injected = build_runtime_reformulation_message(
            "provide 3 more ideas with contextlattice being one",
            Some(&objective),
        )
        .expect("reformulation");

        assert!(injected.contains(RUNTIME_REFORMULATION_PREFIX));
        assert!(injected.contains("tool-profile(mode): focus"));
        assert!(injected.contains("contextlattice(topic): runbooks/objective/test-objective"));
        assert!(injected.contains("objective(active): test-objective | behavior=mission"));
        assert!(injected.contains("- Act with evidence"));
        assert!(injected.contains("- Positive risk-adjusted delta"));
        assert!(injected.contains("UNPROVEN/CONTRADICTORY"));
        assert!(injected.contains("execute at least one concrete action"));
        assert!(injected.contains("Hermes intelligence kernel:"));
        assert!(injected.contains("research synthesis engine:"));
        assert!(injected.contains("ContextLattice memory cycle:"));
        assert!(injected.contains("read back memory"));
        assert!(injected.contains("user-request(routing-preview):"));
        assert!(injected.contains("full user request remains available as the next user message"));
    }

    #[test]
    fn runtime_reformulation_message_respects_toggle_off() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&["HERMES_RUNTIME_PROMPT_REFORMULATION"]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "off");
        assert!(build_runtime_reformulation_message("plain request", None).is_none());
    }

    #[test]
    fn runtime_reformulation_message_truncates_preview_without_losing_user_message() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_RUNTIME_PROMPT_REFORMULATION",
            "HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS",
        ]);
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS", "48");

        let long_prompt =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".repeat(12);
        let injected =
            build_runtime_reformulation_message(&long_prompt, None).expect("reformulation");
        assert!(injected.contains("user-request(routing-preview):"));
        assert!(injected.contains("preview truncated"));
        assert!(!injected.contains(&long_prompt));
        assert!(
            injected.contains("the full user request remains available as the next user message")
        );
    }

    #[test]
    fn resolve_catalog_model_candidate_prefers_suffix_match() {
        let catalog = vec![
            "nousresearch/hermes-4-405b".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("kimi-k2.6", &catalog).expect("candidate");
        assert_eq!(chosen, "moonshotai/kimi-k2.6");
    }

    #[test]
    fn resolve_catalog_model_candidate_uses_relative_match_for_near_miss() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let chosen = resolve_catalog_model_candidate("qwen3.6-max", &catalog).expect("candidate");
        assert_eq!(chosen, "qwen/qwen3.6-max-preview");
    }

    #[test]
    fn rank_catalog_model_candidates_returns_best_first() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let ranked = rank_catalog_model_candidates("qwen3.6-max", &catalog, 2);
        assert_eq!(
            ranked,
            vec![
                "qwen/qwen3.6-max-preview".to_string(),
                "qwen/qwen3.6-plus".to_string()
            ]
        );
    }

    #[test]
    fn query_mode_remediation_target_from_catalog_selects_close_model() {
        let catalog = vec![
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.6-max-preview".to_string(),
        ];
        let remediation =
            query_mode_remediation_target_from_catalog("openrouter:qwen3.6-max", &catalog)
                .expect("remediation");
        assert_eq!(
            remediation.next_model,
            "openrouter:qwen/qwen3.6-max-preview"
        );
        assert_eq!(
            remediation.close_matches.first().map(String::as_str),
            Some("qwen/qwen3.6-max-preview")
        );
    }

    #[test]
    fn query_mode_remediation_preserves_openai_dynamic_alias() {
        let catalog = vec!["gpt-4o-mini".to_string(), "gpt-5.4-mini".to_string()];

        assert!(query_mode_remediation_target_from_catalog("openai:dynamic", &catalog).is_none());
        assert!(
            query_mode_remediation_target_from_catalog("openai-codex:dynamic", &catalog).is_none()
        );
    }

    #[test]
    fn query_mode_model_not_found_detects_provider_shapes() {
        assert!(query_mode_model_not_found(&AgentError::LlmApi(
            "requested model does not exist".to_string()
        )));
        assert!(query_mode_model_not_found(&AgentError::Config(
            "OpenRouter catalog did not include model".to_string()
        )));
        assert!(!query_mode_model_not_found(&AgentError::Config(
            "bad config".to_string()
        )));
    }

    #[test]
    fn assistant_reply_from_result_prefers_last_assistant_message() {
        let result = AgentResult {
            messages: vec![
                Message::assistant("first"),
                Message::user("next"),
                Message::assistant("last"),
            ],
            ..AgentResult::default()
        };
        assert_eq!(assistant_reply_from_result(&result), "last");
    }

    struct FixedProvider {
        reply: &'static str,
    }

    #[async_trait]
    impl LlmProvider for FixedProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant(self.reply),
                usage: None,
                model: model.unwrap_or("test-model").to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            stream::empty().boxed()
        }
    }

    #[tokio::test]
    async fn run_noninteractive_query_uses_injected_provider_factory_and_returns_reply() {
        let _lock = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ]);
        let calls = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let calls_for_factory = Arc::clone(&calls);
        let outcome = run_noninteractive_query(
            &GatewayConfig::default(),
            "openai:gpt-5.5",
            "hello",
            Arc::new(AgentToolRegistry::new()),
            Vec::new(),
            AgentCallbacks::default(),
            move |_config, model| {
                calls_for_factory.lock().unwrap().push(model.to_string());
                Arc::new(FixedProvider {
                    reply: "runtime-ok",
                })
            },
        )
        .await
        .expect("query run");

        assert_eq!(outcome.active_model, "openai:gpt-5.5");
        assert_eq!(outcome.reply, "runtime-ok");
        assert_eq!(
            calls.lock().unwrap().as_slice(),
            &["openai:gpt-5.5".to_string()]
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("openai")
        );
    }
}
