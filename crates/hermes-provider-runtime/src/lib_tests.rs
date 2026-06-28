#[cfg(test)]
mod tests {
    use super::*;
    use hermes_agent::provider_profiles;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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

    fn codex_sse_response_body(text: &str, model: &str) -> String {
        let delta = serde_json::json!({ "delta": text });
        let completed = serde_json::json!({
            "response": {
                "output": [
                    {
                        "type": "message",
                        "content": [
                            { "type": "output_text", "text": text }
                        ]
                    }
                ],
                "model": model,
                "status": "completed",
                "usage": {
                    "input_tokens": 1,
                    "output_tokens": 1
                }
            }
        });
        format!("event: response.output_text.delta\ndata: {delta}\n\nevent: response.completed\ndata: {completed}\n\n")
    }

    #[tokio::test]
    async fn build_provider_routes_chatgpt_openai_oauth_to_responses_backend() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(codex_sse_response_body("openai-pro-ok", "gpt-5.5")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3Qtb3BlbmFpLXByby1wYXJpdHkiLCJjaGF0Z3B0X3BsYW5fdHlwZSI6InBsdXMifX0.sig".to_string()),
                base_url: Some(server.uri()),
                ..LlmProviderConfig::default()
            },
        );

        let provider = build_provider(&config, "openai:gpt-5.5");
        let response = provider
            .chat_completion(
                &[hermes_core::Message::user("hello")],
                &[],
                None,
                None,
                Some("gpt-5.5"),
                None,
            )
            .await
            .expect("OpenAI ChatGPT OAuth provider should use Responses API");

        assert_eq!(response.message.content.as_deref(), Some("openai-pro-ok"));
        server.verify().await;
    }

    #[tokio::test]
    async fn build_provider_remaps_openai_dynamic_for_chatgpt_oauth_backend() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(body_partial_json(serde_json::json!({
                "model": "gpt-5.4"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(codex_sse_response_body("dynamic-ok", "gpt-5.4")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3QtZHluYW1pYy1wYXJpdHkiLCJjaGF0Z3B0X3BsYW5fdHlwZSI6InBsdXMifX0.sig".to_string()),
                base_url: Some(server.uri()),
                ..LlmProviderConfig::default()
            },
        );

        let provider = build_provider(&config, "openai:dynamic");
        let response = provider
            .chat_completion(
                &[hermes_core::Message::user("hello")],
                &[],
                None,
                None,
                Some("dynamic"),
                None,
            )
            .await
            .expect("OpenAI dynamic alias should resolve before the ChatGPT Codex request");

        assert_eq!(response.message.content.as_deref(), Some("dynamic-ok"));
        assert_eq!(response.model, "gpt-5.4");
        server.verify().await;
    }

    #[tokio::test]
    async fn provider_auth_resolver_supplies_openai_oauth_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(codex_sse_response_body("resolver-oauth-ok", "gpt-5.5")),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                base_url: Some(server.uri()),
                ..LlmProviderConfig::default()
            },
        );
        let token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3Qtb3BlbmFpLXByby1yZXNvbHZlciIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.sig";
        let provider = {
            let _guard = env_test_lock();
            let _env = EnvSnapshot::capture(&[
                "HERMES_OPENAI_API_KEY",
                "OPENAI_API_KEY",
                "HERMES_OPENAI_CODEX_API_KEY",
            ]);
            for key in [
                "HERMES_OPENAI_API_KEY",
                "OPENAI_API_KEY",
                "HERMES_OPENAI_CODEX_API_KEY",
            ] {
                std::env::remove_var(key);
            }
            build_provider_with_auth_resolver(
                &config,
                "openai:gpt-5.5",
                Some(&|provider| {
                    if provider == "openai" {
                        Some(token.to_string())
                    } else {
                        None
                    }
                }),
            )
        };

        let response = provider
            .chat_completion(
                &[hermes_core::Message::user("hello")],
                &[],
                None,
                None,
                Some("gpt-5.5"),
                None,
            )
            .await
            .expect("OpenAI OAuth resolver token should use Responses API");

        assert_eq!(
            response.message.content.as_deref(),
            Some("resolver-oauth-ok")
        );
        server.verify().await;
    }

    #[test]
    fn local_backend_specs_cover_macos_open_source_server_family() {
        let providers: Vec<&str> = local_backend_specs()
            .iter()
            .map(|spec| spec.provider)
            .collect();
        for expected in [
            "ollama-local",
            "llama-cpp",
            "vllm",
            "mlx",
            "apple-ane",
            "sglang",
            "tgi",
            "lmstudio",
            "lmdeploy",
            "localai",
            "koboldcpp",
            "text-generation-webui",
            "tabbyapi",
        ] {
            assert!(providers.contains(&expected), "missing {expected}");
        }
        assert_eq!(
            local_backend_spec("llamafile").map(|spec| spec.provider),
            Some("llama-cpp")
        );
        assert_eq!(
            local_backend_spec("omlx").map(|spec| spec.provider),
            Some("mlx")
        );
        assert_eq!(
            local_backend_spec("exllamav2").map(|spec| spec.provider),
            Some("tabbyapi")
        );
    }

    #[test]
    fn provider_runtime_diagnostic_reports_openai_pro_and_local_no_key() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
            "LLAMA_CPP_BASE_URL",
            "LLAMA_CPP_API_KEY",
        ]);
        for key in [
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
            "LLAMA_CPP_BASE_URL",
            "LLAMA_CPP_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let mut openai_cfg = GatewayConfig::default();
        openai_cfg.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3Qtb3BlbmFpLXByby1kaWFnIiwib3JnYW5pemF0aW9uX2lkIjoib3JnIn19.sig".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        let openai = provider_runtime_diagnostic(&openai_cfg, "openai:gpt-5.5");
        assert_eq!(openai.runtime_provider, "openai");
        assert!(openai.api_key_present);
        assert_eq!(openai.api_key_source.as_deref(), Some("config.api_key"));
        assert!(openai.uses_openai_pro_backend);

        let local_cfg = GatewayConfig::default();
        let local = provider_runtime_diagnostic(&local_cfg, "llamafile:local-gguf");
        assert_eq!(local.runtime_provider, "llama-cpp");
        assert_eq!(local.base_url.as_deref(), Some("http://127.0.0.1:8080/v1"));
        assert!(!local.api_key_present);
        assert!(local.local_no_key_allowed);

        let moa = provider_runtime_diagnostic(&local_cfg, "moa:default");
        assert_eq!(moa.runtime_provider, "moa");
        assert_eq!(moa.model, "default");
        assert!(moa.base_url.is_none());
        assert!(!moa.api_key_present);
        assert!(moa.local_no_key_allowed);
    }

    #[test]
    fn resolve_provider_and_model_uses_single_provider_fallback() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("stepfun".to_string(), LlmProviderConfig::default());

        let (provider, model) = resolve_provider_and_model(&cfg, "step-3.5-flash");
        assert_eq!(provider, "stepfun");
        assert_eq!(model, "step-3.5-flash");
    }

    #[test]
    fn test_resolve_provider_and_model_uses_named_custom_provider_model() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                model: Some("my-model".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let (provider, model) = resolve_provider_and_model(&cfg, "my-model");
        assert_eq!(provider, "custom");
        assert_eq!(model, "my-model");
    }

    #[test]
    fn startup_model_selector_keeps_primary_when_credentials_exist() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ]);
        for key in [
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let mut cfg = GatewayConfig {
            fallback_models: vec!["anthropic:claude-sonnet-4-6".to_string()],
            ..GatewayConfig::default()
        };
        cfg.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("primary-key".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let selection = select_startup_model_with_fallback(&cfg, "openai:dynamic");

        assert_eq!(selection.selected_model, "openai:dynamic");
        assert!(!selection.fallback_used);
        assert!(selection.skipped_unavailable_models.is_empty());
    }

    #[test]
    fn startup_model_selector_uses_first_credentialed_fallback() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENROUTER_API_KEY",
            "OPENROUTER_API_KEY",
            "HERMES_ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
        ]);
        for key in [
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENROUTER_API_KEY",
            "OPENROUTER_API_KEY",
            "HERMES_ANTHROPIC_API_KEY",
            "ANTHROPIC_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let mut cfg = GatewayConfig {
            fallback_models: vec![
                "openrouter:anthropic/claude-sonnet-4.6".to_string(),
                "anthropic:claude-sonnet-4-6".to_string(),
            ],
            ..GatewayConfig::default()
        };
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key: Some("fallback-key".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let selection = select_startup_model_with_fallback(&cfg, "openai:dynamic");

        assert_eq!(selection.requested_model, "openai:dynamic");
        assert_eq!(selection.selected_model, "anthropic:claude-sonnet-4-6");
        assert!(selection.fallback_used);
        assert_eq!(
            selection.skipped_unavailable_models,
            vec![
                "openai:dynamic".to_string(),
                "openrouter:anthropic/claude-sonnet-4.6".to_string()
            ]
        );
    }

    #[test]
    fn startup_model_selector_honors_env_fallback_override() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_NOUS_API_KEY",
            "NOUS_API_KEY",
        ]);
        for key in [
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_NOUS_API_KEY",
            "NOUS_API_KEY",
        ] {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_FALLBACK_MODELS", "nous:Hermes-4");

        let mut cfg = GatewayConfig {
            fallback_models: vec!["anthropic:claude-sonnet-4-6".to_string()],
            ..GatewayConfig::default()
        };
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                api_key: Some("nous-fallback-key".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let selection = select_startup_model_with_fallback(&cfg, "openai:dynamic");

        assert_eq!(selection.selected_model, "nous:Hermes-4");
        assert!(selection.fallback_used);
    }

    #[test]
    fn startup_model_selector_keeps_local_no_key_backend() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "OLLAMA_API_KEY",
            "OLLAMA_LOCAL_API_KEY",
        ]);
        for key in [
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "OLLAMA_API_KEY",
            "OLLAMA_LOCAL_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let cfg = GatewayConfig::default();
        let selection = select_startup_model_with_fallback(&cfg, "ollama:llama3.3");

        assert_eq!(selection.selected_model, "ollama:llama3.3");
        assert!(!selection.fallback_used);
    }

    #[test]
    fn startup_model_selector_uses_oauth_resolver_as_primary_credentials() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ]);
        for key in [
            "HERMES_FALLBACK_MODELS",
            "HERMES_FALLBACK_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let mut cfg = GatewayConfig {
            fallback_models: vec!["anthropic:claude-sonnet-4-6".to_string()],
            ..GatewayConfig::default()
        };
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                api_key: Some("fallback-key".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let resolver = |provider: &str| {
            if provider == "openai" {
                Some("oauth-token".to_string())
            } else {
                None
            }
        };
        let diagnostic =
            provider_runtime_diagnostic_with_auth_resolver(&cfg, "openai:dynamic", Some(&resolver));
        let selection = select_startup_model_with_fallback_and_auth_resolver(
            &cfg,
            "openai:dynamic",
            Some(&resolver),
        );

        assert_eq!(
            diagnostic.api_key_source.as_deref(),
            Some("oauth_resolver:openai")
        );
        assert_eq!(selection.selected_model, "openai:dynamic");
        assert!(!selection.fallback_used);
    }

    #[test]
    fn provider_api_key_from_env_supports_stepfun() {
        let _guard = env_test_lock();
        let hermes_var = "HERMES_STEPFUN_API_KEY";
        let stepfun_var = "STEPFUN_API_KEY";
        let _env = EnvSnapshot::capture(&[hermes_var, stepfun_var]);
        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);

        std::env::set_var(stepfun_var, "stepfun-direct");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-direct")
        );

        std::env::set_var(hermes_var, "stepfun-hermes");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-hermes")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_openai_codex() {
        let _guard = env_test_lock();
        let var = "HERMES_OPENAI_CODEX_API_KEY";
        let _env = EnvSnapshot::capture(&[var]);
        std::env::remove_var(var);
        std::env::set_var(var, "codex-oauth-token");
        assert_eq!(
            provider_api_key_from_env("openai-codex").as_deref(),
            Some("codex-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_anthropic_aliases() {
        let _guard = env_test_lock();
        let primary = "ANTHROPIC_API_KEY";
        let secondary = "ANTHROPIC_TOKEN";
        let tertiary = "CLAUDE_CODE_OAUTH_TOKEN";
        let _env = EnvSnapshot::capture(&[primary, secondary, tertiary]);
        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);

        std::env::set_var(tertiary, "claude-oauth-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("claude-oauth-token")
        );

        std::env::set_var(secondary, "anthropic-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-token")
        );

        std::env::set_var(primary, "anthropic-api-key");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-api-key")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_qwen_oauth() {
        let _guard = env_test_lock();
        let oauth_var = "HERMES_QWEN_OAUTH_API_KEY";
        let fallback_var = "DASHSCOPE_API_KEY";
        let _env = EnvSnapshot::capture(&[oauth_var, fallback_var]);
        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);

        std::env::set_var(fallback_var, "dashscope-fallback");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("dashscope-fallback")
        );

        std::env::set_var(oauth_var, "qwen-oauth-token");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("qwen-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_google_gemini_cli() {
        let _guard = env_test_lock();
        let var = "HERMES_GEMINI_OAUTH_API_KEY";
        let _env = EnvSnapshot::capture(&[var]);
        std::env::remove_var(var);
        std::env::set_var(var, "google-gemini-oauth-token");
        assert_eq!(
            provider_api_key_from_env("google-gemini-cli").as_deref(),
            Some("google-gemini-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_prefers_kimi_coding_key_for_code_provider() {
        let _guard = env_test_lock();
        let keys = [
            "KIMI_CODING_API_KEY",
            "KIMI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        std::env::set_var("KIMI_API_KEY", "sk-legacy");
        std::env::set_var("KIMI_CODING_API_KEY", "sk-kimi-code");
        assert_eq!(
            provider_api_key_from_env("kimi-coding").as_deref(),
            Some("sk-kimi-code")
        );
        assert_eq!(
            provider_api_key_from_env("kimi").as_deref(),
            Some("sk-legacy")
        );
        std::env::set_var("KIMI_CN_API_KEY", "sk-cn");
        assert_eq!(
            provider_api_key_from_env("kimi-coding-cn").as_deref(),
            Some("sk-cn")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_extended_registry() {
        let _guard = env_test_lock();
        let env_vars = [
            "AI_GATEWAY_API_KEY",
            "DEEPSEEK_API_KEY",
            "HF_TOKEN",
            "KILOCODE_API_KEY",
            "NVIDIA_API_KEY",
            "OLLAMA_LOCAL_API_KEY",
            "LLAMA_CPP_API_KEY",
            "VLLM_API_KEY",
            "MLX_API_KEY",
            "APPLE_ANE_API_KEY",
            "SGLANG_API_KEY",
            "TGI_API_KEY",
            "LMSTUDIO_API_KEY",
            "LMDEPLOY_API_KEY",
            "LOCALAI_API_KEY",
            "KOBOLDCPP_API_KEY",
            "TEXT_GENERATION_WEBUI_API_KEY",
            "TABBYAPI_API_KEY",
            "NOVITA_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "XAI_API_KEY",
            "XIAOMI_API_KEY",
            "ARCEEAI_API_KEY",
            "ARCEE_API_KEY",
            "GLM_API_KEY",
            "ZAI_API_KEY",
            "Z_AI_API_KEY",
            "GMI_API_KEY",
            "MINIMAX_CN_API_KEY",
            "NOUS_API_KEY",
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_COPILOT_TOKEN",
            "TOKENHUB_API_KEY",
        ];
        let _env = EnvSnapshot::capture(&env_vars);
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }
        let checks = [
            ("AI_GATEWAY_API_KEY", "ai-gateway"),
            ("AI_GATEWAY_API_KEY", "vercel"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("HF_TOKEN", "huggingface"),
            ("HF_TOKEN", "hf"),
            ("HF_TOKEN", "hugging-face"),
            ("HF_TOKEN", "huggingface-hub"),
            ("KILOCODE_API_KEY", "kilocode"),
            ("NVIDIA_API_KEY", "nvidia"),
            ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
            ("LLAMA_CPP_API_KEY", "llama-cpp"),
            ("VLLM_API_KEY", "vllm"),
            ("MLX_API_KEY", "mlx"),
            ("APPLE_ANE_API_KEY", "apple-ane"),
            ("SGLANG_API_KEY", "sglang"),
            ("TGI_API_KEY", "tgi"),
            ("LMSTUDIO_API_KEY", "lm-studio"),
            ("LMDEPLOY_API_KEY", "lm-deploy"),
            ("LOCALAI_API_KEY", "local-ai"),
            ("KOBOLDCPP_API_KEY", "kobold-cpp"),
            ("TEXT_GENERATION_WEBUI_API_KEY", "oobabooga"),
            ("TABBYAPI_API_KEY", "exllamav2"),
            ("NOVITA_API_KEY", "novita"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
            ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
            ("XAI_API_KEY", "xai"),
            ("XIAOMI_API_KEY", "xiaomi"),
            ("GLM_API_KEY", "zai"),
            ("GLM_API_KEY", "glm"),
            ("ZAI_API_KEY", "z-ai"),
            ("Z_AI_API_KEY", "zhipu"),
            ("GMI_API_KEY", "gmi-cloud"),
            ("GMI_API_KEY", "gmicloud"),
            ("ARCEEAI_API_KEY", "arcee-ai"),
            ("ARCEEAI_API_KEY", "arceeai"),
            ("XIAOMI_API_KEY", "mimo"),
            ("XIAOMI_API_KEY", "xiaomi-mimo"),
            ("TOKENHUB_API_KEY", "tencent-tokenhub"),
            ("TOKENHUB_API_KEY", "tencent"),
            ("TOKENHUB_API_KEY", "tokenhub"),
            ("MINIMAX_CN_API_KEY", "minimax_cn"),
            ("NOUS_API_KEY", "nous-api"),
            ("NOUS_API_KEY", "nous-portal-api"),
            ("COPILOT_GITHUB_TOKEN", "github-copilot"),
            ("GH_TOKEN", "github-models"),
            ("GITHUB_TOKEN", "copilot"),
            ("GITHUB_COPILOT_TOKEN", "copilot"),
        ];
        for (env_var, provider) in checks {
            for env_var in env_vars {
                std::env::remove_var(env_var);
            }
            let expected = format!("token-for-{provider}");
            std::env::set_var(env_var, expected.clone());
            assert_eq!(
                provider_api_key_from_env(provider).as_deref(),
                Some(expected.as_str())
            );
        }
    }

    #[test]
    fn normalize_runtime_provider_name_covers_local_and_cloud_aliases() {
        assert_eq!(
            normalize_runtime_provider_name("gemini-cli"),
            "google-gemini-cli"
        );
        assert_eq!(normalize_runtime_provider_name("nous_api"), "nous-api");
        assert_eq!(normalize_runtime_provider_name("nousapi"), "nous-api");
        assert_eq!(
            normalize_runtime_provider_name("nous-portal-api"),
            "nous-api"
        );
        assert_eq!(normalize_runtime_provider_name("mixture"), "moa");
        assert_eq!(normalize_runtime_provider_name("mixture-of-agents"), "moa");
        assert_eq!(normalize_runtime_provider_name("mixture_of_agents"), "moa");
        assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
        assert_eq!(normalize_runtime_provider_name("novita-ai"), "novita");
        assert_eq!(
            normalize_runtime_provider_name("alibaba-coding-plan"),
            "qwen"
        );
        assert_eq!(normalize_runtime_provider_name("opencode"), "opencode-zen");
        assert_eq!(normalize_runtime_provider_name("ollama"), "ollama-local");
        assert_eq!(normalize_runtime_provider_name("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("llamafile"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("ollvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("llvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("mlx-lm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("vmlx"), "mlx");
        assert_eq!(normalize_runtime_provider_name("omlx"), "mlx");
        assert_eq!(normalize_runtime_provider_name("mlx-vlm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("ane"), "apple-ane");
        assert_eq!(normalize_runtime_provider_name("lm-studio"), "lmstudio");
        assert_eq!(normalize_runtime_provider_name("lm_deploy"), "lmdeploy");
        assert_eq!(normalize_runtime_provider_name("local-ai"), "localai");
        assert_eq!(normalize_runtime_provider_name("kobold-cpp"), "koboldcpp");
        assert_eq!(
            normalize_runtime_provider_name("oobabooga"),
            "text-generation-webui"
        );
        assert_eq!(normalize_runtime_provider_name("tabby-api"), "tabbyapi");
        assert_eq!(normalize_runtime_provider_name("exllamav2"), "tabbyapi");
        assert_eq!(normalize_runtime_provider_name("glm"), "zai");
        assert_eq!(normalize_runtime_provider_name("z-ai"), "zai");
        assert_eq!(normalize_runtime_provider_name("zhipu"), "zai");
        assert_eq!(normalize_runtime_provider_name("github-copilot"), "copilot");
        assert_eq!(normalize_runtime_provider_name("github-models"), "copilot");
        assert_eq!(
            normalize_runtime_provider_name("github-copilot-acp"),
            "copilot-acp"
        );
        assert_eq!(
            normalize_runtime_provider_name("copilot-acp-agent"),
            "copilot-acp"
        );
        assert_eq!(normalize_runtime_provider_name("hf"), "huggingface");
        assert_eq!(
            normalize_runtime_provider_name("hugging-face"),
            "huggingface"
        );
        assert_eq!(
            normalize_runtime_provider_name("huggingface-hub"),
            "huggingface"
        );
        assert_eq!(normalize_runtime_provider_name("aigateway"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("vercel"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("gmi-cloud"), "gmi");
        assert_eq!(normalize_runtime_provider_name("gmicloud"), "gmi");
        assert_eq!(
            normalize_runtime_provider_name("google-ai-studio"),
            "gemini"
        );
        assert_eq!(normalize_runtime_provider_name("arcee-ai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("arceeai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("azure"), "azure-foundry");
        assert_eq!(
            normalize_runtime_provider_name("azure-ai-foundry"),
            "azure-foundry"
        );
        assert_eq!(normalize_runtime_provider_name("mimo"), "xiaomi");
        assert_eq!(normalize_runtime_provider_name("xiaomi-mimo"), "xiaomi");
        assert_eq!(
            normalize_runtime_provider_name("tencent-cloud"),
            "tencent-tokenhub"
        );
        assert_eq!(
            normalize_runtime_provider_name("tokenhub"),
            "tencent-tokenhub"
        );
        assert_eq!(normalize_runtime_provider_name("aws"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("aws-bedrock"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("amazon"), "bedrock");
    }

    #[test]
    fn provider_base_url_from_env_supports_api_provider_aliases() {
        let _guard = env_test_lock();
        let env_vars = [
            "COPILOT_API_BASE_URL",
            "GLM_BASE_URL",
            "KIMI_BASE_URL",
            "MINIMAX_CN_BASE_URL",
            "GMI_BASE_URL",
            "HF_BASE_URL",
            "AI_GATEWAY_BASE_URL",
            "TOKENHUB_BASE_URL",
            "ARCEE_BASE_URL",
            "XIAOMI_BASE_URL",
            "BEDROCK_BASE_URL",
            "LMSTUDIO_BASE_URL",
            "LMDEPLOY_BASE_URL",
            "LOCALAI_BASE_URL",
            "KOBOLDCPP_BASE_URL",
            "TEXT_GENERATION_WEBUI_BASE_URL",
            "TABBYAPI_BASE_URL",
        ];
        let _env = EnvSnapshot::capture(&env_vars);
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }

        std::env::set_var("COPILOT_API_BASE_URL", "https://copilot.example/v1");
        assert_eq!(
            provider_base_url_from_env("github-copilot").as_deref(),
            Some("https://copilot.example/v1")
        );
        std::env::set_var("GLM_BASE_URL", "https://glm.example/v4");
        assert_eq!(
            provider_base_url_from_env("z-ai").as_deref(),
            Some("https://glm.example/v4")
        );
        std::env::set_var("KIMI_BASE_URL", "https://kimi.example/v1");
        assert_eq!(
            provider_base_url_from_env("moonshot").as_deref(),
            Some("https://kimi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("kimi-coding").as_deref(),
            Some("https://kimi.example/v1")
        );
        std::env::set_var("MINIMAX_CN_BASE_URL", "https://minimax-cn.example/v1");
        assert_eq!(
            provider_base_url_from_env("minimax_cn").as_deref(),
            Some("https://minimax-cn.example/v1")
        );
        std::env::set_var("GMI_BASE_URL", "https://gmi.example/v1");
        assert_eq!(
            provider_base_url_from_env("gmi-cloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("gmicloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        std::env::set_var("HF_BASE_URL", "https://hf.example/v1");
        assert_eq!(
            provider_base_url_from_env("huggingface-hub").as_deref(),
            Some("https://hf.example/v1")
        );
        std::env::set_var("AI_GATEWAY_BASE_URL", "https://gateway.example/v1");
        assert_eq!(
            provider_base_url_from_env("vercel").as_deref(),
            Some("https://gateway.example/v1")
        );
        std::env::set_var("TOKENHUB_BASE_URL", "https://tokenhub.example/v1");
        assert_eq!(
            provider_base_url_from_env("tencent").as_deref(),
            Some("https://tokenhub.example/v1")
        );
        std::env::set_var("ARCEE_BASE_URL", "https://arcee.example/v1");
        assert_eq!(
            provider_base_url_from_env("arcee-ai").as_deref(),
            Some("https://arcee.example/v1")
        );
        std::env::set_var("XIAOMI_BASE_URL", "https://mimo.example/v1");
        assert_eq!(
            provider_base_url_from_env("mimo").as_deref(),
            Some("https://mimo.example/v1")
        );
        std::env::set_var("BEDROCK_BASE_URL", "https://bedrock-runtime.example");
        assert_eq!(
            provider_base_url_from_env("aws").as_deref(),
            Some("https://bedrock-runtime.example")
        );
        std::env::set_var("LMSTUDIO_BASE_URL", "http://localhost:1234/v1");
        assert_eq!(
            provider_base_url_from_env("lm-studio").as_deref(),
            Some("http://localhost:1234/v1")
        );
        std::env::set_var("LMDEPLOY_BASE_URL", "http://localhost:23333/v1");
        assert_eq!(
            provider_base_url_from_env("lm-deploy").as_deref(),
            Some("http://localhost:23333/v1")
        );
        std::env::set_var("LOCALAI_BASE_URL", "http://localhost:8080/v1");
        assert_eq!(
            provider_base_url_from_env("local-ai").as_deref(),
            Some("http://localhost:8080/v1")
        );
        std::env::set_var("KOBOLDCPP_BASE_URL", "http://localhost:5001/v1");
        assert_eq!(
            provider_base_url_from_env("kobold-cpp").as_deref(),
            Some("http://localhost:5001/v1")
        );
        std::env::set_var("TEXT_GENERATION_WEBUI_BASE_URL", "http://localhost:5000/v1");
        assert_eq!(
            provider_base_url_from_env("oobabooga").as_deref(),
            Some("http://localhost:5000/v1")
        );
        std::env::set_var("TABBYAPI_BASE_URL", "http://localhost:5000/v1");
        assert_eq!(
            provider_base_url_from_env("exllamav2").as_deref(),
            Some("http://localhost:5000/v1")
        );
    }

    #[test]
    fn provider_default_base_url_supports_upstream_aliases() {
        assert_eq!(
            provider_default_base_url("github-copilot"),
            Some(COPILOT_BASE_URL)
        );
        assert_eq!(provider_default_base_url("glm"), Some(ZAI_BASE_URL));
        assert_eq!(
            provider_default_base_url("minimax_cn"),
            Some(MINIMAX_CN_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("huggingface-hub"),
            Some(HUGGINGFACE_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("vercel"),
            Some(AI_GATEWAY_BASE_URL)
        );
        assert_eq!(provider_default_base_url("gmi-cloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("gmicloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("arcee-ai"), Some(ARCEE_BASE_URL));
        assert_eq!(provider_default_base_url("mimo"), Some(XIAOMI_BASE_URL));
        assert_eq!(
            provider_default_base_url("tencent"),
            Some(TENCENT_TOKENHUB_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("gemini"),
            Some(provider_profiles::GEMINI_OPENAI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("google-ai-studio"),
            Some(provider_profiles::GEMINI_OPENAI_BASE_URL)
        );
        assert_eq!(
            provider_profiles::gemini_openai_compatible_base_url(
                provider_profiles::GEMINI_NATIVE_BASE_URL
            ),
            provider_profiles::GEMINI_OPENAI_BASE_URL
        );
        assert_eq!(
            provider_default_base_url("llamafile"),
            Some(LLAMA_CPP_BASE_URL)
        );
        assert_eq!(provider_default_base_url("vmlx"), Some(MLX_BASE_URL));
        assert_eq!(provider_default_base_url("omlx"), Some(MLX_BASE_URL));
        assert_eq!(
            provider_default_base_url("lm-studio"),
            Some(LMSTUDIO_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("lmdeploy"),
            Some(LMDEPLOY_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("local-ai"),
            Some(LOCALAI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("kobold-cpp"),
            Some(KOBOLDCPP_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("oobabooga"),
            Some(TEXT_GENERATION_WEBUI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("tabby-api"),
            Some(TABBYAPI_BASE_URL)
        );
    }

    #[test]
    fn allow_no_api_key_for_local_backends_and_private_base_urls() {
        assert!(allow_no_api_key("ollama-local", "ollama-local", None));
        assert!(allow_no_api_key("lmstudio", "lmstudio", None));
        assert!(allow_no_api_key("koboldcpp", "koboldcpp", None));
        assert!(allow_no_api_key("moa", "moa", None));
        assert!(allow_no_api_key(
            "text-generation-webui",
            "text-generation-webui",
            None
        ));
        assert!(allow_no_api_key(
            "openai",
            "openai",
            Some("http://127.0.0.1:11434/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://192.168.1.20:8000/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://[::1]:11434/v1")
        ));
        assert!(!allow_no_api_key(
            "openai",
            "openai",
            Some("https://api.openai.com/v1")
        ));
    }
}
