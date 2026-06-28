    #[test]
    fn test_runtime_provider_command_args_override_primary_acp_metadata() {
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
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                request_timeout_seconds: Some(45.0),
                api_mode: None,
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                request_timeout_seconds: Some(75.0),
                ..RuntimeProviderConfig::default()
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = agent.primary_runtime_snapshot();
        assert_eq!(
            agent.resolve_runtime_request_timeout_seconds("openai"),
            Some(45.0)
        );
        assert_eq!(
            agent.resolve_runtime_request_timeout_seconds("codex"),
            Some(75.0)
        );
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_runtime_wire_model_remaps_openai_dynamic_only() {
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai", "dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("codex", "openai:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai-codex", "codex:dynamic"),
            OPENAI_CODEX_DYNAMIC_WIRE_MODEL
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openrouter", "dynamic"),
            "dynamic"
        );
        assert_eq!(
            AgentLoop::runtime_wire_model_for_provider("openai", "gpt-5.5"),
            "gpt-5.5"
        );
    }

    #[test]
    fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
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
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                api_key_env: None,
                base_url: Some("https://api.openai.com/v1".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
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
                api_key: Some("sk-qwen-oauth".to_string()),
                api_key_env: None,
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    fn test_runtime_provider_stepfun_build_supported() {
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
            "stepfun".to_string(),
            RuntimeProviderConfig {
                api_key: Some("stepfun-test-key".to_string()),
                api_key_env: None,
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
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        let built =
            agent.build_runtime_provider("stepfun", "step-3.5-flash", None, None, None, None, None);
        assert!(built.is_ok(), "stepfun runtime provider should build");
    }

    #[test]
    fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
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
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_openai_runtime_reads_openai_oauth_token_store_entry() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

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

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-openai-auth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai": {
    "provider": "openai",
    "access_token": "openai-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
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
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let resolved = agent.resolve_runtime_api_key("openai", None, None);
        assert_eq!(resolved.as_deref(), Some("openai-oauth-token"));
        let _ = std::fs::remove_dir_all(home);
    }
