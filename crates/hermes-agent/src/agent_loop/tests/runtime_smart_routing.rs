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
    fn test_smart_model_routing_selects_openai_codex_provider_override() {
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


        let state = tempfile::tempdir().expect("route state");
        let home = state.path().join("home");
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
    }

    #[tokio::test]
    async fn test_openai_runtime_reads_encrypted_cli_oauth_token_store_entry() {
        use futures::stream::BoxStream;
        use hermes_auth::{FileTokenStore, OAuthCredential};
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
        let token_path = home.join("auth").join("tokens.json");
        let cli_store = FileTokenStore::new(&token_path)
            .await
            .expect("CLI token store");
        cli_store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "openai-oauth-token".to_string(),
                refresh_token: None,
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() + chrono::Duration::days(1)),
            })
            .await
            .expect("CLI credential");
        assert!(!std::fs::read_to_string(&token_path)
            .expect("encrypted token store")
            .contains("openai-oauth-token"));

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


    #[tokio::test]
    async fn smart_route_provider_override_refreshes_its_vault_not_the_model_prefix_vault() {
        use futures::stream::BoxStream;
        use hermes_auth::{FileTokenStore, OAuthCredential};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        struct NoCallProvider {
            calls: Arc<std::sync::atomic::AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for NoCallProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("unexpected"),
                    usage: None,
                    model: "unexpected".to_string(),
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
                self.calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                futures::stream::empty().boxed()
            }
        }

        let state = tempfile::tempdir().expect("route OAuth state");
        let home = state.path().join("home");
        let token_path = home.join("auth").join("tokens.json");
        let store = FileTokenStore::new(&token_path)
            .await
            .expect("encrypted OAuth vault");
        store
            .upsert(OAuthCredential {
                provider: "qwen-oauth".to_string(),
                access_token: "qwen-expired-token".to_string(),
                refresh_token: Some("qwen-refresh-token".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            })
            .await
            .expect("expired routed credential");
        store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "openai-must-remain-untouched".to_string(),
                refresh_token: Some("openai-refresh-token".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            })
            .await
            .expect("expired model-prefix credential");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("route OAuth fixture listener");
        let address = listener.local_addr().expect("route OAuth fixture address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("one refresh request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read refresh request");
            let body = r#"{"access_token":"qwen-refreshed-token","refresh_token":"qwen-rotated-refresh","token_type":"Bearer","expires_in":3600}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("write refresh response");
        });

        let endpoint = format!("http://{address}/token");
        let mut runtime_providers = HashMap::new();
        for provider in ["qwen-oauth", "openai"] {
            runtime_providers.insert(
                provider.to_string(),
                RuntimeProviderConfig {
                    oauth_token_url: Some(endpoint.clone()),
                    oauth_client_id: Some("route-oauth-client".to_string()),
                    ..RuntimeProviderConfig::default()
                },
            );
        }
        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermes_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    // The route model deliberately retains the wrong prefix:
                    // provider override, not model parsing, owns the vault key.
                    model: Some("openai:gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("route OAuth runtime"),
        );
        let agent = AgentLoop::new(
            config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(NoCallProvider {
                calls: calls.clone(),
            }),
        )
        .with_run_runtime(runtime.clone());
        let route = agent
            .resolve_smart_runtime_route(&[Message::user("帮我总结这段话")])
            .expect("smart route");
        assert_eq!(route.provider.as_deref(), Some("qwen-oauth"));
        let effective_provider =
            agent.effective_provider_for_model_call(route.model.as_str(), Some(&route));
        assert_eq!(effective_provider, "qwen-oauth");

        let containment =
            hermes_environments::ExecutionContainmentContext::manual(config.execution_mode)
                .expect("manual route OAuth test context");
        let active = runtime
            .start_run(
                &config,
                &containment,
                &ToolRegistry::new(),
                crate::run_runtime::PreparedRunStartEvidence::new(
                    &[],
                    &[],
                    crate::run_runtime::PreparedSystemPromptEvidence::Stored {
                        content: "route OAuth test",
                    },
                ),
                &crate::run_runtime::RunInvocationContext::default(),
                None,
            )
            .expect("route OAuth run starts");
        agent
            .resolve_governed_oauth_before_model_call(Some(&active), &effective_provider)
            .await
            .expect("routed vault refresh");
        tokio::time::timeout(Duration::from_secs(1), server)
            .await
            .expect("routed provider reached refresh endpoint")
            .expect("refresh fixture task");

        // Refresh uses a separate governed writable handle; reopen the
        // encrypted vault to assert the durable provider-specific result.
        let refreshed_store =
            FileTokenStore::open_read_only(&token_path).expect("reopen refreshed OAuth vault");
        assert_eq!(
            refreshed_store
                .get("qwen-oauth")
                .await
                .expect("routed credential")
                .access_token,
            "qwen-refreshed-token"
        );
        assert_eq!(
            refreshed_store
                .get("openai")
                .await
                .expect("model-prefix credential")
                .access_token,
            "openai-must-remain-untouched"
        );
        let inspection = runtime
            .inspect_run(active.run_id())
            .expect("inspect routed refresh run");
        let refresh_intents = inspection
            .events
            .iter()
            .filter_map(|event| {
                serde_json::from_str::<serde_json::Value>(event.public_payload.as_str()).ok()
            })
            .filter(|payload| payload["schema"] == "hermes.oauth-refresh-intent/v1")
            .collect::<Vec<_>>();
        assert_eq!(refresh_intents.len(), 1);
        assert_eq!(refresh_intents[0]["provider"], "qwen-oauth");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        active
            .finish(TerminalReason::Completed)
            .await
            .expect("route OAuth run finish");
    }

    #[tokio::test]
    async fn test_expired_encrypted_oauth_credential_fails_closed_with_remediation() {
        use futures::stream::BoxStream;
        use hermes_auth::{FileTokenStore, OAuthCredential};
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
        let home = std::env::temp_dir().join(format!("hermes-expired-auth-fixture-{}", nonce));
        let token_path = home.join("auth").join("tokens.json");
        let store = FileTokenStore::new(&token_path)
            .await
            .expect("CLI token store");
        store
            .upsert(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "expired-runtime-sentinel".to_string(),
                refresh_token: Some("refresh-runtime-sentinel".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            })
            .await
            .expect("expired CLI credential");
        let store_before = std::fs::read(&token_path).expect("read encrypted token store");


        let agent = AgentLoop::new(
            AgentConfig {
                hermes_home: Some(home.to_string_lossy().to_string()),
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let error = match agent.build_runtime_provider(
            "openai",
            "gpt-4o",
            None,
            None,
            None,
            None,
            None,
        ) {
            Ok(_) => panic!("expired credential must not build a provider"),
            Err(error) => error,
        };
        assert!(matches!(error, AgentError::AuthFailed(_)));
        assert!(error.to_string().contains("hermes auth refresh openai"));
        assert!(!error.to_string().contains("expired-runtime-sentinel"));
        assert!(!format!("{error:?}").contains("refresh-runtime-sentinel"));
        assert_eq!(
            std::fs::read(&token_path).expect("read encrypted token store"),
            store_before,
            "runtime credential resolution must not refresh or rewrite the token store"
        );
        let _ = std::fs::remove_dir_all(home);
    }
