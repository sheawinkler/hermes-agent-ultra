    /// Smoke test: config-centre OAuth metadata wins over env fallback, and
    /// env is used when config is empty. Mirrors the Python behaviour of
    /// `resolve_runtime_provider_credentials` where provider config takes
    /// precedence over environment lookup.
    #[test]
    fn test_oauth_refresh_config_prefers_provider_config_over_env() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/token".to_string()),
                oauth_client_id: Some("cfg-client".to_string()),
            },
        );
        // An unknown provider reachable only via config (env fallback is gated
        // on known providers, so this exercises the cfg_token_url.zip path).
        runtime_providers.insert(
            "custom-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: Some("https://cfg.example.com/custom-token".to_string()),
                oauth_client_id: Some("custom-client".to_string()),
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // Set conflicting env values — config must win.
        std::env::set_var("HERMES_QWEN_OAUTH_TOKEN_URL", "https://env.example.com/tok");
        std::env::set_var("HERMES_QWEN_OAUTH_CLIENT_ID", "env-client");

        let (token_url, client_id) = agent.oauth_refresh_config("qwen-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/token");
        assert_eq!(client_id, "cfg-client");

        // Unknown-provider path still resolves when config centre supplies both.
        let (token_url, client_id) = agent.oauth_refresh_config("custom-oauth").unwrap();
        assert_eq!(token_url, "https://cfg.example.com/custom-token");
        assert_eq!(client_id, "custom-client");

        std::env::remove_var("HERMES_QWEN_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_QWEN_OAUTH_CLIENT_ID");
    }

    #[test]
    fn test_runtime_provider_api_key_env_is_resolved() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "custom".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                base_url: None,
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        std::env::set_var("MY_FALLBACK_KEY", "env-secret");
        let resolved = agent.resolve_runtime_api_key("custom", None, None);
        assert_eq!(resolved.as_deref(), Some("env-secret"));
        std::env::remove_var("MY_FALLBACK_KEY");
    }

    #[test]
    fn test_local_runtime_providers_allow_no_key_and_default_base_url() {
        use futures::stream::BoxStream;

        let _guard = env_test_lock();
        let _llama_key = EnvVarGuard::remove("LLAMA_CPP_API_KEY");
        let _llama_url = EnvVarGuard::remove("LLAMA_CPP_BASE_URL");
        let _lmstudio_key = EnvVarGuard::remove("LMSTUDIO_API_KEY");
        let _lmstudio_url = EnvVarGuard::remove("LMSTUDIO_BASE_URL");

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        assert_eq!(
            agent.resolve_runtime_base_url("llamafile", None).as_deref(),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(
            agent.resolve_runtime_base_url("lmstudio", None).as_deref(),
            Some("http://127.0.0.1:1234/v1")
        );
        assert!(agent
            .build_runtime_provider("llamafile", "local-gguf", None, None, None, None, None)
            .is_ok());
        assert!(agent
            .build_runtime_provider("lmstudio", "local-model", None, None, None, None, None)
            .is_ok());
    }

    #[test]
    fn test_runtime_provider_api_key_env_supports_anthropic_aliases_and_gemini_oauth() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("ANTHROPIC_TOKEN");
        std::env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "claude-code-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("anthropic", None, None)
                .as_deref(),
            Some("claude-code-token")
        );
        std::env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

        std::env::set_var("HERMES_GEMINI_OAUTH_API_KEY", "gemini-oauth-token");
        assert_eq!(
            agent
                .resolve_runtime_api_key("google-gemini-cli", None, None)
                .as_deref(),
            Some("gemini-oauth-token")
        );
        std::env::remove_var("HERMES_GEMINI_OAUTH_API_KEY");
    }

    #[test]
    fn test_oauth_refresh_config_anthropic_defaults_available() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("anthropic").unwrap();
        assert_eq!(token_url, "https://console.anthropic.com/v1/oauth/token");
        assert_eq!(client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
    }

    #[test]
    fn test_oauth_refresh_config_openai_defaults_available() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        std::env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("openai").unwrap();
        assert_eq!(token_url, "https://auth.openai.com/oauth/token");
        assert_eq!(client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
    }

    #[test]
    fn test_oauth_refresh_config_nous_defaults_available() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        std::env::remove_var("HERMES_NOUS_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_NOUS_OAUTH_CLIENT_ID");
        std::env::remove_var("NOUS_PORTAL_BASE_URL");
        std::env::remove_var("NOUS_CLIENT_ID");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let (token_url, client_id) = agent.oauth_refresh_config("nous").unwrap();
        assert_eq!(token_url, "https://portal.nousresearch.com/api/oauth/token");
        assert_eq!(client_id, "hermes-cli");
    }

    #[test]
    fn test_runtime_provider_stepfun_env_key_and_base_url_defaults() {
        use futures::stream::BoxStream;
        let _guard = env_test_lock();

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let _hermes_stepfun = EnvVarGuard::remove("HERMES_STEPFUN_API_KEY");
        let _stepfun = EnvVarGuard::set("STEPFUN_API_KEY", "stepfun-secret");
        let resolved = agent.resolve_runtime_api_key("stepfun", None, None);
        assert_eq!(resolved.as_deref(), Some("stepfun-secret"));

        let base = agent.resolve_runtime_base_url("stepfun", None);
        assert_eq!(base.as_deref(), Some("https://api.stepfun.ai/step_plan/v1"));
    }

    #[test]
    fn test_runtime_provider_copilot_env_aliases_and_base_url_default() {
        use futures::stream::BoxStream;
        let _guard = env_test_lock();

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let _copilot_github = EnvVarGuard::remove("COPILOT_GITHUB_TOKEN");
        let _gh = EnvVarGuard::remove("GH_TOKEN");
        let _github_token = EnvVarGuard::remove("GITHUB_TOKEN");
        let _legacy = EnvVarGuard::remove("GITHUB_COPILOT_TOKEN");
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let _github_token_override = EnvVarGuard::set("GITHUB_TOKEN", "gh-env-secret");
        let resolved = agent.resolve_runtime_api_key("copilot", None, None);
        assert_eq!(resolved.as_deref(), Some("gh-env-secret"));

        let base = agent.resolve_runtime_base_url("copilot", None);
        assert_eq!(base.as_deref(), Some("https://api.githubcopilot.com"));
    }

    #[test]
    fn test_runtime_provider_direct_env_keys_and_base_url_defaults() {
        use futures::stream::BoxStream;
        let _guard = env_test_lock();

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let _gemini = EnvVarGuard::remove("GEMINI_API_KEY");
        let _google = EnvVarGuard::remove("GOOGLE_API_KEY");
        let _gmi = EnvVarGuard::remove("GMI_API_KEY");
        let _arceeai = EnvVarGuard::remove("ARCEEAI_API_KEY");
        let _arcee = EnvVarGuard::remove("ARCEE_API_KEY");
        let _xiaomi = EnvVarGuard::remove("XIAOMI_API_KEY");
        let _tokenhub = EnvVarGuard::remove("TOKENHUB_API_KEY");
        let _nous = EnvVarGuard::remove("NOUS_API_KEY");
        let _nous_base = EnvVarGuard::remove("NOUS_BASE_URL");
        let _kimi_coding = EnvVarGuard::remove("KIMI_CODING_API_KEY");
        let _kimi = EnvVarGuard::remove("KIMI_API_KEY");
        let _moonshot = EnvVarGuard::remove("MOONSHOT_API_KEY");
        let _kimi_base = EnvVarGuard::remove("KIMI_BASE_URL");

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let _google_key = EnvVarGuard::set("GOOGLE_API_KEY", "google-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("google-ai-studio", None, None),
            Some("google-secret".to_string())
        );
        drop(_google_key);

        let _gmi_key = EnvVarGuard::set("GMI_API_KEY", "gmi-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("gmicloud", None, None),
            Some("gmi-secret".to_string())
        );

        let _arcee_key = EnvVarGuard::set("ARCEE_API_KEY", "arcee-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("arcee-ai", None, None),
            Some("arcee-secret".to_string())
        );

        let _xiaomi_key = EnvVarGuard::set("XIAOMI_API_KEY", "xiaomi-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("mimo", None, None),
            Some("xiaomi-secret".to_string())
        );

        let _tokenhub_key = EnvVarGuard::set("TOKENHUB_API_KEY", "tokenhub-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("tencent", None, None),
            Some("tokenhub-secret".to_string())
        );

        let _nous_key = EnvVarGuard::set("NOUS_API_KEY", "nous-secret");
        let _nous_base_override = EnvVarGuard::set("NOUS_BASE_URL", "https://nous.example/v1");
        assert_eq!(
            agent.resolve_runtime_api_key("nous-api", None, None),
            Some("nous-secret".to_string())
        );
        assert_eq!(
            agent
                .resolve_runtime_api_key("nous-portal-api", None, None)
                .as_deref(),
            Some("nous-secret")
        );
        assert_eq!(
            agent.resolve_runtime_base_url("nous-api", None).as_deref(),
            Some("https://nous.example/v1")
        );
        assert!(
            agent
                .build_runtime_provider("nous-api", "openai/gpt-5.5", None, None, None, None, None)
                .is_ok(),
            "nous-api direct-key runtime provider should build"
        );
        drop(_nous_key);
        drop(_nous_base_override);

        let kimi_code_key = EnvVarGuard::set("KIMI_CODING_API_KEY", "sk-kimi-code-secret");
        assert_eq!(
            agent.resolve_runtime_api_key("kimi-coding", None, None),
            Some("sk-kimi-code-secret".to_string())
        );
        let auto_kimi_code_base = agent.resolve_kimi_runtime_base_url_for_key(
            "kimi-coding",
            None,
            "sk-kimi-code-secret",
            agent.resolve_runtime_base_url("kimi-coding", None),
        );
        assert_eq!(
            auto_kimi_code_base.as_deref(),
            Some(crate::provider_profiles::KIMI_CODE_BASE_URL)
        );
        assert!(
            agent
                .build_runtime_provider("kimi-coding", "kimi-k2.6", None, None, None, None, None)
                .is_ok(),
            "kimi-coding runtime provider should build through the Kimi provider"
        );
        drop(kimi_code_key);

        let kimi_legacy_key = EnvVarGuard::set("KIMI_API_KEY", "sk-legacy-secret");
        let legacy_base = agent.resolve_kimi_runtime_base_url_for_key(
            "kimi-coding",
            None,
            "sk-legacy-secret",
            agent.resolve_runtime_base_url("kimi-coding", None),
        );
        assert_eq!(
            legacy_base.as_deref(),
            Some(crate::provider_profiles::KIMI_LEGACY_BASE_URL)
        );
        drop(kimi_legacy_key);

        let kimi_code_key = EnvVarGuard::set("KIMI_CODING_API_KEY", "sk-kimi-code-secret");
        let kimi_override = EnvVarGuard::set("KIMI_BASE_URL", "https://kimi.override.test/v1");
        let override_base = agent.resolve_kimi_runtime_base_url_for_key(
            "kimi-coding",
            None,
            "sk-kimi-code-secret",
            agent.resolve_runtime_base_url("kimi-coding", None),
        );
        assert_eq!(
            override_base.as_deref(),
            Some("https://kimi.override.test/v1")
        );
        drop(kimi_override);
        drop(kimi_code_key);

        assert_eq!(
            agent
                .resolve_runtime_base_url("google-gemini", None)
                .as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai")
        );
        assert_eq!(
            agent.resolve_runtime_base_url("gmi-cloud", None).as_deref(),
            Some("https://api.gmi-serving.com/v1")
        );
        assert_eq!(
            agent.resolve_runtime_base_url("arceeai", None).as_deref(),
            Some("https://api.arcee.ai/api/v1")
        );
        assert_eq!(
            agent
                .resolve_runtime_base_url("xiaomi-mimo", None)
                .as_deref(),
            Some("https://api.xiaomimimo.com/v1")
        );
        assert_eq!(
            agent
                .resolve_runtime_base_url("tencentmaas", None)
                .as_deref(),
            Some("https://tokenhub.tencentmaas.com/v1")
        );
    }

    #[test]
    fn runtime_provider_config_lookup_supports_delegation_aliases() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
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
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "kimi-coding".to_string(),
            RuntimeProviderConfig {
                api_key: Some("kimi-config-key".to_string()),
                base_url: Some("https://api.moonshot.ai/v1".to_string()),
                request_timeout_seconds: Some(41.0),
                ..RuntimeProviderConfig::default()
            },
        );
        runtime_providers.insert(
            "zai".to_string(),
            RuntimeProviderConfig {
                api_key_env: Some("ZAI_CONFIG_KEY".to_string()),
                base_url: Some("https://api.z.ai/api/paas/v4".to_string()),
                ..RuntimeProviderConfig::default()
            },
        );

        let agent = AgentLoop::new(
            AgentConfig {
                runtime_providers,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        assert_eq!(
            agent.resolve_runtime_api_key("kimi", None, None).as_deref(),
            Some("kimi-config-key")
        );
        assert_eq!(
            agent.resolve_runtime_base_url("moonshot", None).as_deref(),
            Some("https://api.moonshot.ai/v1")
        );
        assert_eq!(
            agent.resolve_runtime_request_timeout_seconds("kimi"),
            Some(41.0)
        );
        assert_eq!(
            agent.resolve_runtime_base_url("z-ai", None).as_deref(),
            Some("https://api.z.ai/api/paas/v4")
        );
    }
