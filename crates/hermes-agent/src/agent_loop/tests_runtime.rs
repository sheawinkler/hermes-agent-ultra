
    #[test]
    fn test_route_learning_decay_and_ttl() {
        let now_ms = now_unix_ms();
        let stale = RouteLearningStats {
            samples: 10,
            success_rate: 0.05,
            avg_latency_ms: 9900.0,
            consecutive_failures: 9,
            updated_at_unix_ms: now_ms - (8 * 24 * 60 * 60 * 1000),
        };
        assert!(
            AgentLoop::route_learning_effective_stats(&stale, now_ms).is_none(),
            "stale route entries must expire by ttl"
        );

        let recent = RouteLearningStats {
            samples: 10,
            success_rate: 0.20,
            avg_latency_ms: 4000.0,
            consecutive_failures: 4,
            updated_at_unix_ms: now_ms - (12 * 60 * 60 * 1000),
        };
        let adjusted = AgentLoop::route_learning_effective_stats(&recent, now_ms)
            .expect("recent entry should not expire");
        assert!(adjusted.success_rate > recent.success_rate);
        assert!(adjusted.avg_latency_ms < recent.avg_latency_ms);
        assert!(adjusted.samples <= recent.samples);
    }

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

    #[test]
    fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

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
                    message: Message::assistant("done"),
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

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

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
                    message: Message::assistant("done"),
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

        // Fixture-style cases distilled from Python v2026.4.13:
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = agent.evolution_counters.lock().expect("counter lock");
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermes_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
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

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn hidden_registered_tool_call_is_rejected_when_not_advertised() {
        use std::sync::atomic::{AtomicU32, Ordering};

        struct HiddenToolProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for HiddenToolProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let message = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![ToolCall {
                            id: "tc_hidden".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "web_search".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermes_core::LlmResponse {
                    message,
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

        let terminal_schema =
            ToolSchema::new("terminal", "Terminal tool", JsonSchema::new("object"));
        let hidden_executions = Arc::new(AtomicU32::new(0));
        let hidden_executions_for_tool = hidden_executions.clone();
        let mut registry = ToolRegistry::new();
        registry.register(
            "terminal",
            terminal_schema.clone(),
            Arc::new(|_args| Ok("terminal ran".to_string())),
        );
        registry.register(
            "web_search",
            ToolSchema::new("web_search", "Web search", JsonSchema::new("object")),
            Arc::new(move |_args| {
                hidden_executions_for_tool.fetch_add(1, Ordering::SeqCst);
                Ok("web ran".to_string())
            }),
        );

        let config = AgentConfig {
            max_turns: 4,
            invalid_tool_call_max_retries: 2,
            ..AgentConfig::default()
        };
        let provider = Arc::new(HiddenToolProvider {
            calls: Arc::new(AtomicU32::new(0)),
        });
        let agent = AgentLoop::new(config, Arc::new(registry), provider);
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let result = rt
            .block_on(agent.run(vec![Message::user("hello")], Some(vec![terminal_schema])))
            .expect("agent run should succeed");

        assert!(result.finished_naturally);
        assert_eq!(hidden_executions.load(Ordering::SeqCst), 0);
        let transcript = result
            .messages
            .iter()
            .filter_map(|msg| msg.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(transcript.contains("Tool 'web_search' is not enabled in this session"));
        assert!(transcript.contains("Available tools: terminal"));
        assert!(!transcript.contains("web ran"));
    }

    #[test]
    fn plugin_tool_middleware_runs_inside_agent_loop() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct ToolThenDoneProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenDoneProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let message = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![ToolCall {
                            id: "tc_echo".to_string(),
                            function: hermes_core::FunctionCall {
                                name: "echo".to_string(),
                                arguments: r#"{"value":"original"}"#.to_string(),
                            },
                            extra_content: None,
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermes_core::LlmResponse {
                    message,
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

        struct MiddlewarePlugin;

        #[async_trait::async_trait]
        impl crate::plugins::Plugin for MiddlewarePlugin {
            fn meta(&self) -> crate::plugins::PluginMeta {
                crate::plugins::PluginMeta {
                    name: "middleware_test".to_string(),
                    version: "0.1.0".to_string(),
                    description: "Middleware test".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut crate::plugins::PluginContext) {
                ctx.on_tool_request(Arc::new(|request| {
                    let mut args = request.args.clone();
                    args["value"] = Value::String("rewritten".to_string());
                    Some(crate::plugins::ToolRequestMiddlewareUpdate::new(args))
                }));
                ctx.on_tool_execution(Arc::new(|_request, next_call| {
                    let mut result = next_call(None);
                    result.content = format!("wrapped: {}", result.content);
                    result
                }));
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "echo",
            ToolSchema::new("echo", "Echo tool", JsonSchema::new("object")),
            Arc::new(|args| Ok(args["value"].as_str().unwrap_or_default().to_string())),
        );
        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(MiddlewarePlugin));
        let agent = AgentLoop::new(
            AgentConfig {
                max_turns: 4,
                ..AgentConfig::default()
            },
            Arc::new(registry),
            Arc::new(ToolThenDoneProvider {
                calls: Arc::new(AtomicU32::new(0)),
            }),
        )
        .with_plugins(Arc::new(std::sync::Mutex::new(plugin_manager)));
        let rt = tokio::runtime::Runtime::new().expect("runtime");

        let result = rt
            .block_on(agent.run(vec![Message::user("use echo")], None))
            .expect("agent run should succeed");
        let transcript = result
            .messages
            .iter()
            .filter_map(|msg| msg.content.as_deref())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(result.finished_naturally);
        assert!(transcript.contains("wrapped: rewritten"), "{transcript}");
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
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
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp://copilot".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
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
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
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
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
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
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                api_key_env: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                request_timeout_seconds: None,
                api_mode: None,
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
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
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
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
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    fn test_smart_model_routing_skips_complex_turn() {
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

        let mut config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
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
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert!(selected.is_none());
    }

    #[test]
    fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".into(),
                function: hermes_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
                extra_content: None,
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermes_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
            extra_content: None,
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    fn memory_tool_result_succeeded_fails_closed_on_unclear_shapes() {
        assert!(AgentLoop::memory_tool_result_succeeded(
            r#"{"success":true,"message":"Entry added."}"#
        ));
        for content in [
            r#"{"success":false,"message":"failed"}"#,
            r#"{"success":true,"staged":true}"#,
            r#"{"message":"Entry added."}"#,
            "not json",
            "[]",
        ] {
            assert!(
                !AgentLoop::memory_tool_result_succeeded(content),
                "{content} should not be mirrored"
            );
        }
    }

    #[test]
    fn test_hydrate_session_search_args_injects_current_session_id() {
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
            ) -> BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
            extra_content: None,
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
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
            ) -> BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermes_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
            extra_content: None,
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
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

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        let max = agent.config.max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost: Some(0.01),
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: Some(0.02),
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: None,
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            estimated_cost: None,
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

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

    #[test]
    fn test_governor_reduces_budget_under_high_pressure() {
        let mut ctx = ContextManager::default_budget();
        let payload = "x".repeat(((ctx.max_context_chars() as f64) * 0.9) as usize);
        ctx.add_message(Message::user(payload));
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let gov = governor_for_turn(&config, &ctx, 12, None);
        assert!(gov.pressure >= 0.9);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 4);
    }

    #[test]
    fn test_governor_reduces_budget_under_latency_degradation() {
        let ctx = ContextManager::default_budget();
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let runtime = GovernorRuntimeState {
            avg_llm_latency_ms: Some(7000.0),
            avg_tool_error_rate: 0.0,
            consecutive_error_turns: 0,
        };
        let gov = governor_for_turn(&config, &ctx, 6, Some(&runtime));
        assert!(gov.latency_degraded);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 2);
    }

    #[test]
    fn test_governor_reduces_budget_under_error_degradation() {
        let ctx = ContextManager::default_budget();
        let config = AgentConfig {
            max_tokens: Some(1200),
            ..AgentConfig::default()
        };
        let runtime = GovernorRuntimeState {
            avg_llm_latency_ms: Some(1000.0),
            avg_tool_error_rate: 0.55,
            consecutive_error_turns: 3,
        };
        let gov = governor_for_turn(&config, &ctx, 10, Some(&runtime));
        assert!(gov.error_degraded);
        assert!(gov.max_tokens.unwrap_or(1200) < 1200);
        assert!(gov.tool_concurrency <= 2);
    }

    #[test]
    fn test_tool_loop_guard_trips_on_consecutive_full_failure_turns() {
        assert!(!should_trip_tool_loop_guard_with_config(
            2, 2, 2, true, 3, 1
        ));
        assert!(should_trip_tool_loop_guard_with_config(3, 2, 2, true, 3, 1));
        assert!(!should_trip_tool_loop_guard_with_config(
            3, 2, 2, false, 3, 1
        ));
    }

    #[test]
    fn test_tool_loop_guard_ignores_partial_success_turns() {
        assert!(!should_trip_tool_loop_guard_with_config(
            4, 3, 2, true, 2, 1
        ));
    }

    #[test]
    fn test_looks_like_tool_error_output_detects_json_error_envelope() {
        assert!(looks_like_tool_error_output(
            r#"{"error":"Invalid tool parameters: Missing 'platform' parameter"}"#
        ));
        assert!(looks_like_tool_error_output(
            r#"{"success":false,"message":"failed"}"#
        ));
        assert!(!looks_like_tool_error_output(
            r#"{"success":true,"result":"ok"}"#
        ));
    }

    #[test]
    fn test_looks_like_tool_error_output_detects_text_error_signatures() {
        assert!(looks_like_tool_error_output("error: invalid request"));
        assert!(looks_like_tool_error_output(
            "Invalid tool parameters: Missing 'platform' parameter"
        ));
        assert!(!looks_like_tool_error_output("all good"));
    }

    #[test]
    fn test_redact_json_value_masks_sensitive_fields() {
        let mut payload = serde_json::json!({
            "api_key": "abc",
            "nested": { "token": "def", "safe": "ok" },
            "list": [{"password":"x"}, {"value":"y"}],
            "text": "Authorization: Bearer sk-secretvalue12345"
        });
        redact_json_value(&mut payload);
        assert_eq!(payload["api_key"], "[redacted]");
        assert_eq!(payload["nested"]["token"], "[redacted]");
        assert_eq!(payload["nested"]["safe"], "ok");
        assert_eq!(payload["list"][0]["password"], "[redacted]");
        assert_eq!(payload["text"], "Authorization: Bearer [redacted]");
    }

    #[test]
    fn test_replay_recorder_adds_hash_chain_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let replay_path = tmp.path().join("trace.jsonl");
        let recorder = ReplayRecorder {
            path: Some(replay_path.clone()),
            state: Some(Arc::new(Mutex::new(ReplayState {
                seq: 0,
                prev_hash: short_sha256_hex("seed"),
                trace_root: short_sha256_hex("trace-seed"),
            }))),
        };

        recorder.record("turn_start", serde_json::json!({"token":"abc"}));
        recorder.record("tool_call", serde_json::json!({"cmd":"echo ok"}));

        let body = std::fs::read_to_string(&replay_path).expect("replay file");
        let mut lines = body.lines();
        let first: serde_json::Value =
            serde_json::from_str(lines.next().expect("line1")).expect("json line1");
        let second: serde_json::Value =
            serde_json::from_str(lines.next().expect("line2")).expect("json line2");

        assert_eq!(first["seq"], 1);
        assert_eq!(second["seq"], 2);
        assert!(first.get("trace_id").is_some());
        assert!(second.get("trace_id").is_some());
        assert_eq!(first["payload"]["token"], "[redacted]");
        assert_eq!(second["prev_hash"], first["event_hash"]);
        assert_ne!(first["event_hash"], second["event_hash"]);
    }

    #[test]
    fn test_detect_contextlattice_connect_intent() {
        let msgs = vec![Message::user(
            "please confirm and connect to contextlattice, then harden it",
        )];
        assert!(detect_contextlattice_connect_intent(&msgs));

        let msgs = vec![Message::user("explain contextlattice architecture only")];
        assert!(!detect_contextlattice_connect_intent(&msgs));
    }

    #[test]
    fn test_contextlattice_connect_system_hint_emitted() {
        let msgs = vec![Message::user("connect to contextlattice and verify health")];
        let hint = contextlattice_connect_system_hint(&msgs).expect("expected hint");
        assert!(hint.contains("contextlattice_search"));
        assert!(hint.contains("HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH"));
        assert!(hint.contains("Never use terminal command `contextlattice`"));
    }

    #[test]
    fn test_contextlattice_intelligence_system_hint_requires_tools_and_intent() {
        let msgs = vec![Message::user(
            "perform deep repo audit and objective verification on /tmp/repo",
        )];
        let tools = vec![
            ToolSchema::new("contextlattice_search", "search", JsonSchema::new("object")),
            ToolSchema::new(
                "contextlattice_context_pack",
                "pack",
                JsonSchema::new("object"),
            ),
        ];
        let hint = contextlattice_intelligence_system_hint(&msgs, &tools).expect("expected hint");
        assert!(hint.contains("ContextLattice-first intelligence policy active"));
        assert!(hint.contains("scoped retrieval"));
        assert!(hint.contains("Copy numeric facts verbatim"));
    }

    #[test]
    fn test_contextlattice_intelligence_system_hint_skips_without_tools() {
        let msgs = vec![Message::user(
            "perform deep repo audit and objective verification on /tmp/repo",
        )];
        let tools = vec![ToolSchema::new(
            "terminal",
            "terminal",
            JsonSchema::new("object"),
        )];
        assert!(contextlattice_intelligence_system_hint(&msgs, &tools).is_none());
    }

    #[test]
    fn test_contextlattice_shell_invocation_detector() {
        assert!(is_contextlattice_shell_invocation(
            r#"{"command":"contextlattice"}"#
        ));
        assert!(is_contextlattice_shell_invocation(
            r#"{"command":"contextlattice status"}"#
        ));
        assert!(!is_contextlattice_shell_invocation(
            r#"{"command":"which contextlattice"}"#
        ));
        assert!(!is_contextlattice_shell_invocation(r#"{"command":"ls"}"#));
    }

    #[test]
    fn test_repo_review_tool_profile_keeps_todo_filters_messaging() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "balanced");
        let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
        let mut calls = vec![
            ToolCall {
                id: "a".to_string(),
                function: hermes_core::FunctionCall {
                    name: "todo".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "b".to_string(),
                function: hermes_core::FunctionCall {
                    name: "telegram_send".to_string(),
                    arguments: r#"{"text":"status"}"#.to_string(),
                },
                extra_content: None,
            },
        ];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_some());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "todo");
    }

    #[test]
    fn test_repo_review_tool_profile_escape_hatch_disables_filtering() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        let msgs = vec![Message::user(
            "review repo at /tmp/app and diagnose issue; allow all tools",
        )];
        let mut calls = vec![ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        }];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_some());
        assert_eq!(calls.len(), 1, "escape hatch should bypass filtering");
    }

    #[test]
    fn test_repo_review_tool_profile_off_mode_disables_filtering() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "off");
        let msgs = vec![Message::user("review repo at /tmp/app and diagnose issue")];
        let mut calls = vec![ToolCall {
            id: "b".to_string(),
            function: hermes_core::FunctionCall {
                name: "telegram_send".to_string(),
                arguments: r#"{"text":"status"}"#.to_string(),
            },
            extra_content: None,
        }];
        let note = apply_repo_review_tool_profile_narrowing(&mut calls, &msgs);
        assert!(note.is_none());
        assert_eq!(calls.len(), 1, "off mode should keep all calls");
    }

    #[test]
    fn test_repo_review_discovery_policy_trims_repeated_loops() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "enforce");
        let msgs = vec![Message::user(
            "inspect repo /tmp/app and review codebase deeply",
        )];
        let mut state = RepoReviewBudgetState::default();
        let make_calls = || {
            vec![
                ToolCall {
                    id: "1".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
                ToolCall {
                    id: "2".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
                ToolCall {
                    id: "3".to_string(),
                    function: hermes_core::FunctionCall {
                        name: "terminal".to_string(),
                        arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                    },
                    extra_content: None,
                },
            ]
        };
        let mut first = make_calls();
        assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
        let mut second = make_calls();
        assert!(
            apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state).is_none()
        );
        let mut third = make_calls();
        let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
        assert!(note.is_some());
        assert!(third.len() < 3);
    }

    #[test]
    fn test_repo_review_discovery_policy_advisory_keeps_calls() {
        let _guard = env_test_lock();
        let _env = EnvVarGuard::set("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "advisory");
        let msgs = vec![Message::user(
            "inspect repo /tmp/app and review codebase deeply",
        )];
        let mut state = RepoReviewBudgetState::default();
        let mut first = vec![
            ToolCall {
                id: "1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "2".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "3".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"rg -n TODO src"}"#.to_string(),
                },
                extra_content: None,
            },
        ];
        assert!(apply_repo_review_discovery_budget_policy(&mut first, &msgs, &mut state).is_none());
        let mut second = first.clone();
        let _ = apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state);
        let mut third = first.clone();
        let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
        assert!(note.is_some());
        assert_eq!(third.len(), 3, "advisory mode must not trim tool calls");
    }

    #[test]
    fn test_repo_review_intent_includes_path_scoped_read_only_research() {
        let msgs = vec![Message::user(
            "Conduct READ-ONLY local research in /tmp/algotraderV2_rust and report back on how to improve profitability.",
        )];
        assert!(detect_repo_review_intent(&msgs));
        assert!(detect_research_evidence_intent(&msgs));
        let hint = exploratory_problem_solving_system_hint(&msgs).expect("research hint");
        assert!(hint.contains("Exploratory problem-solving protocol active"));
    }

    #[test]
    fn test_finalizer_claim_retry_for_research_without_explicit_evidence() {
        let msgs = vec![Message::user(
            "Conduct read-only research in /tmp/algotraderV2_rust and report back with evidence-rich recommendations.",
        )];
        let answer = "Profitability can be improved by 60.2% based on local research.";
        assert!(finalizer_claim_requires_evidence_retry(&msgs, answer, 0));

        let grounded =
            "confidence=medium\nfile=Cargo.toml\ncmd=rg -n profit src\nObserved facts only.";
        assert!(!finalizer_claim_requires_evidence_retry(&msgs, grounded, 0));
    }

    #[test]
    fn test_finalizer_claim_retry_for_missing_evidence_path() {
        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("src")).expect("create src");
        std::fs::write(tmp.path().join("src/lib.rs"), "fn main() {}\n").expect("write file");

        assert!(!assistant_references_missing_evidence_paths_from_base(
            "confidence=high\nfile=src/lib.rs:1\ncmd=rg -n main src/lib.rs",
            tmp.path()
        ));
        assert!(assistant_references_missing_evidence_paths_from_base(
            "confidence=high\nfile=src/missing.rs;cmd=rg -n main src/missing.rs",
            tmp.path()
        ));
    }

    #[test]
    fn test_repo_research_plan_finalizer_requires_workstream_evidence() {
        let msgs = vec![Message::user(
            "Conduct read-only repo research in crates/hermes-agent/src/agent_loop.rs and report how to improve planning.",
        )];
        assert!(detect_exploratory_repo_research_intent(&msgs));

        let shallow = "confidence=medium\nfile=Cargo.toml\ncmd=rg -n planning crates";
        assert!(finalizer_repo_research_plan_requires_retry(
            &msgs, shallow, 0
        ));

        let grounded = "REPO_RESEARCH_PLAN: complete\n\
confidence=medium\n\
- workstream=web status=complete file=Cargo.toml cmd=rg -n web Cargo.toml\n\
- workstream=agent-loop status=complete file=src/agent_loop.rs cmd=rg -n finalizer src/agent_loop.rs\n\
- workstream=tests status=unproven file=Cargo.toml cmd=cargo test -p hermes-agent finalizer";
        assert!(!finalizer_repo_research_plan_requires_retry(
            &msgs, grounded, 0
        ));
    }

    #[test]
    fn test_repo_research_plan_finalizer_accepts_explicit_blocker() {
        let msgs = vec![Message::user(
            "Research the missing repo path /tmp/does-not-exist and report blockers.",
        )];
        let blocked =
            "REPO_RESEARCH_PLAN: blocked\nblocker=path missing\ncmd=rg --files /tmp/does-not-exist";
        assert!(!finalizer_repo_research_plan_requires_retry(
            &msgs, blocked, 0
        ));
    }

    #[test]
    fn test_tool_result_signal_score_rewards_workstream_evidence() {
        let score = tool_result_signal_score(
            "workstream=agent status=complete file=Cargo.toml path=crates/hermes-agent/src/agent_loop.rs cmd=rg -n finalizer crates command=cargo test",
            false,
        );
        assert!(score > 0.75, "score={score}");
    }

    #[test]
    fn test_web_research_finalizer_requires_web_tool_and_urls() {
        let msgs = vec![Message::user(
            "Search the web for Solana trading strategies and cite concrete URLs.",
        )];
        assert!(detect_web_research_intent(&msgs));
        assert!(finalizer_web_research_requires_retry(
            &msgs,
            "WEB_SEARCH_USED: yes\nNo URLs found.",
            0
        ));

        let mut grounded = msgs.clone();
        grounded.push(Message::tool_result_with_name(
            "web1",
            "web_search",
            r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
        ));
        let search_only_answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
        assert!(finalizer_web_research_requires_retry(
            &grounded,
            search_only_answer,
            0
        ));
        grounded.push(Message::tool_result_with_name(
            "web2",
            "web_extract",
            "Extracted Jito low-latency transaction send docs.",
        ));
        let answer = "WEB_SEARCH_USED: yes\nSOURCE_QUALITY: primary=1 community=1 secondary=0\nObserved:\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
        assert!(!finalizer_web_research_requires_retry(&grounded, answer, 0));
    }

    #[test]
    fn test_web_research_finalizer_requires_source_quality_counts() {
        let mut msgs = vec![Message::user(
            "Search the web for Solana trading strategies and cite concrete URLs.",
        )];
        msgs.push(Message::tool_result_with_name(
            "web1",
            "web_search",
            r#"{"results":[{"url":"https://docs.jito.wtf/lowlatencytxnsend/"}]}"#,
        ));
        msgs.push(Message::tool_result_with_name(
            "web2",
            "web_extract",
            "Extracted source",
        ));
        let missing_quality = "WEB_SEARCH_USED: yes\n- https://docs.jito.wtf/lowlatencytxnsend/\n- https://www.helius.dev/blog/solana-local-fee-markets";
        assert!(finalizer_web_research_requires_retry(
            &msgs,
            missing_quality,
            0
        ));
    }

    #[test]
    fn test_web_research_system_hint_reports_tool_availability() {
        let msgs = vec![Message::user(
            "Do online research across the web and cite URLs.",
        )];
        let tools = vec![ToolSchema::new(
            "web_search",
            "Search the web",
            JsonSchema::new("object"),
        )];
        let hint = web_research_system_hint(&msgs, &tools).expect("web hint");
        assert!(hint.contains("Web research contract active"));
        assert!(hint.contains("web_search"));
        assert!(hint.contains("SOURCE_QUALITY"));
    }

    #[test]
    fn test_task_focus_finalizer_retries_when_explicit_anchors_disappear() {
        let msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        assert!(finalizer_task_focus_requires_retry(
            &msgs,
            "Here is a generic repository analysis with no email evidence.",
            0
        ));
        assert!(!finalizer_task_focus_requires_retry(
            &msgs,
            "Gmail is blocked: not authenticated for sheawinkler@gmail.com.",
            0
        ));
    }

    #[test]
    fn test_google_workspace_finalizer_retries_absent_skill_claim() {
        let mut msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        msgs.push(Message::tool_result_with_name(
            "skill1",
            "skill_view",
            "# Google Workspace\nGmail, Calendar, Drive.",
        ));

        assert!(finalizer_google_workspace_requires_retry(
            &msgs,
            "No Google Workspace tools exist, so this is blocked.",
            0
        ));
    }

    #[test]
    fn test_google_workspace_finalizer_requires_status_marker() {
        let msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        assert!(finalizer_google_workspace_requires_retry(
            &msgs,
            "Here is an unrelated repo analysis.",
            0
        ));
    }

    #[test]
    fn test_google_workspace_finalizer_accepts_setup_probe_blocker() {
        let mut msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        msgs.push(Message::assistant_with_tool_calls(
            None,
            vec![hermes_core::ToolCall {
                id: "call_setup".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
                },
                extra_content: None,
            }],
        ));
        msgs.push(Message::tool_result_with_name(
            "call_setup",
            "terminal",
            r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
        ));

        assert!(!finalizer_google_workspace_requires_retry(
            &msgs,
            "GOOGLE_WORKSPACE_USED: no\ncmd=python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check\nerror=NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json",
            0
        ));
    }

    #[test]
    fn test_google_workspace_finalizer_retries_success_after_auth_blocker() {
        let mut msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        msgs.push(Message::assistant_with_tool_calls(
            None,
            vec![hermes_core::ToolCall {
                id: "call_setup".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3.12 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --check"}"#.to_string(),
                },
                extra_content: None,
            }],
        ));
        msgs.push(Message::tool_result_with_name(
            "call_setup",
            "terminal",
            r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
        ));

        assert!(finalizer_google_workspace_requires_retry(
            &msgs,
            "GOOGLE_WORKSPACE_USED: yes\n20 important emails were found. Gmail search and reading were successful.",
            0
        ));
    }

    #[test]
    fn test_google_workspace_auth_blocker_guard_blocks_setup_mutation() {
        let mut msgs = vec![Message::user(
            "Use Gmail to summarize important emails from sheawinkler@gmail.com.",
        )];
        msgs.push(Message::tool_result_with_name(
            "call_setup",
            "terminal",
            r#"{"result":"NOT_AUTHENTICATED: No token at /Users/me/.hermes-agent-ultra/google_token.json"}"#,
        ));
        let calls = vec![hermes_core::ToolCall {
            id: "write_fake".to_string(),
            function: hermes_core::FunctionCall {
                name: "write_file".to_string(),
                arguments: r#"{"path":"/tmp/simulated_clients.json","content":"{}"}"#.to_string(),
            },
            extra_content: None,
        }];

        assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &calls).is_some());
        let auth_url_calls = vec![hermes_core::ToolCall {
            id: "auth_url".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: r#"{"command":"env HERMES_HOME=/Users/me/.hermes-agent-ultra python3 /Users/me/.hermes-agent-ultra/skills/productivity/google-workspace/scripts/setup.py --auth-url --services email"}"#.to_string(),
            },
            extra_content: None,
        }];
        assert!(google_workspace_auth_blocker_mutation_guard(&msgs, &auth_url_calls).is_some());
    }

    #[test]
    fn test_terminal_command_system_hint_warns_against_shell_wrappers() {
        let tools = vec![ToolSchema::new(
            "terminal",
            "Execute command",
            JsonSchema::new("object"),
        )];
        let hint = terminal_command_system_hint(&tools).expect("terminal hint");
        assert!(hint.contains("bash -lc"));
        assert!(hint.contains("direct commands"));
    }

    #[test]
    fn test_finalizer_output_quality_retry_detects_placeholders() {
        let templated =
            "**Title:** Example\n**Authors:** pack of authors\n(Full text available at [URL](URL))";
        assert!(finalizer_output_quality_requires_retry(templated, 0));
    }

    #[test]
    fn test_finalizer_output_quality_retry_detects_fake_attachments() {
        let answer = "The full evidence is attached separately; proposed calibration: redacted.";
        assert!(finalizer_output_quality_requires_retry(answer, 0));
        assert!(finalizer_output_quality_requires_retry(
            r#"{"name":"terminal","arguments":{}}</tool_call>"#,
            0
        ));
    }

    #[test]
    fn test_finalizer_output_quality_retry_detects_duplicate_lines() {
        let duplicated =
            "- **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management\n\
            - **Title:** Bayesian Learning for Dive State Prediction and Management";
        assert!(finalizer_output_quality_requires_retry(duplicated, 0));
        assert!(!finalizer_output_quality_requires_retry(duplicated, 2));
    }

    #[test]
    fn test_finalizer_action_execution_retry_detects_intent_narration() {
        let msgs = vec![Message::user(
            "proceed with deep repo review for /tmp/app and implement patches",
        )];
        assert!(finalizer_action_execution_requires_retry(
            &msgs,
            "I will proceed now and report back shortly.",
            0
        ));
        assert!(!finalizer_action_execution_requires_retry(
            &msgs,
            "I will proceed now and report back shortly.",
            2
        ));
    }

    #[test]
    fn test_finalizer_action_execution_retry_skips_when_evidence_present() {
        let msgs = vec![Message::user(
            "proceed with deep repo review for /tmp/app and implement patches",
        )];
        assert!(!finalizer_action_execution_requires_retry(
            &msgs,
            "cmd=rg -n TODO src\nfile=/tmp/app/src/main.rs\nobjective_state=advancing",
            0
        ));
    }

    #[test]
    fn test_objective_guard_requires_sections_for_trading_objective() {
        let msgs = vec![
            Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
            Message::user("review repo /tmp/algotraderv2_rust and produce patch plan"),
        ];
        let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
        assert!(active);
        assert!(needs_analytics);
        assert!(!deep_audit_required);
        assert!(!objective_guard_satisfied("plain response", true, false));
        assert!(objective_guard_satisfied(
            "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL",
            true,
            false
        ));
    }

    #[test]
    fn test_deep_objective_guard_requires_deep_audit_section() {
        let msgs = vec![
            Message::system("[SESSION_OBJECTIVE] Exponentiate Solana wallet via trading."),
            Message::user(
                "deep end-to-end review repo /tmp/algotraderv2_rust and produce complete patch plan",
            ),
        ];
        let (active, needs_analytics, deep_audit_required) = objective_guard_policy(&msgs);
        assert!(active);
        assert!(needs_analytics);
        assert!(deep_audit_required);

        let shallow = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.42 SOL";
        assert!(!objective_guard_satisfied(shallow, true, true));

        let numeric_only = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- verified_files=8\n- commands_run=5\n- unknowns=1\n- blockers=none";
        assert!(!objective_guard_satisfied(numeric_only, true, true));

        let deep = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=complete evidence(file=/tmp/b.rs cmd=sed -n 1,220p src/strategy.rs)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test -p hermes-agent objective_guard)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=none";
        assert!(objective_guard_satisfied(deep, true, true));
    }

    #[test]
    fn test_deep_objective_retry_prompt_contains_audit_requirements() {
        let prompt = objective_guard_retry_prompt(true, true);
        assert!(prompt.contains(OBJECTIVE_DEEP_AUDIT_TAG));
        assert!(prompt.contains("file=<verified_path_1>"));
        assert!(prompt.contains("cmd=<command_1>"));
        assert!(prompt.contains("workstream=<name> status=<complete|blocked|unproven>"));
    }

    #[test]
    fn test_deep_objective_scope_complete_rejects_non_complete_streams() {
        let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nANALYTICS_VERIFIED:\n- objective_state=flat metric=+0.00 SOL\nDEEP_AUDIT_VERIFIED:\n- scope_complete=true\n- workstream=ingestion status=complete evidence(file=/tmp/a.rs cmd=rg -n ingest src)\n- workstream=strategy status=blocked evidence(file=/tmp/b.rs cmd=rg -n strategy src)\n- workstream=execution status=complete evidence(file=/tmp/c.rs cmd=cargo test)\n- file=/tmp/a.rs\n- file=/tmp/b.rs\n- file=/tmp/c.rs\n- file=/tmp/d.rs\n- file=/tmp/e.rs\n- cmd=rg -n objective src\n- cmd=sed -n 1,220p src/main.rs\n- cmd=cargo test -p hermes-agent objective_guard\n- unknowns=1\n- blockers=rpc unavailable";
        assert!(!objective_guard_satisfied(text, true, true));
    }

    #[test]
    fn test_coerce_textual_tool_calls_extracts_and_cleans_message() {
        let msg = Message::assistant(
            "Proceeding with discovery now.\n<tool_call name=\"skill_view\">\n<argument name=\"skill\">contextlattice-master-router</argument>\n</tool_call>",
        );
        let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
        assert!(parsed_textual);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "skill_view");
        assert_eq!(
            coerced.content.as_deref(),
            Some("Proceeding with discovery now.")
        );
    }

    #[test]
    fn test_coerce_textual_tool_calls_keeps_declared_calls() {
        let msg = Message::assistant_with_tool_calls(
            Some("Running tool.".to_string()),
            vec![ToolCall {
                id: "id1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "terminal".to_string(),
                    arguments: r#"{"command":"pwd"}"#.to_string(),
                },
                extra_content: None,
            }],
        );
        let (coerced, calls, parsed_textual) = AgentLoop::coerce_textual_tool_calls(msg);
        assert!(!parsed_textual);
        assert_eq!(calls.len(), 1);
        assert_eq!(coerced.tool_calls.as_ref().map(|v| v.len()), Some(1));
        assert_eq!(coerced.content.as_deref(), Some("Running tool."));
    }

    #[test]
    fn test_extract_objective_state_marker_prefers_explicit_marker() {
        let text = "ANALYTICS_VERIFIED:\n- objective_state=advancing metric=+0.12 SOL";
        assert_eq!(extract_objective_state_marker(text), "advancing");
        let colon_text = "ANALYTICS_VERIFIED:\n- objective_state: regressing metric=-0.30 SOL";
        assert_eq!(extract_objective_state_marker(colon_text), "regressing");
    }

    #[test]
    fn test_extract_marker_values_collects_unique_paths_and_cmds() {
        let text = "PATCH_VERIFIED:\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/a.rs exists_now=true\n- path=/tmp/b.rs exists_now=true\nDEEP_AUDIT_VERIFIED:\n- cmd=rg -n objective src\n- cmd=cargo test -p hermes-agent objective_guard";
        let files = extract_marker_values(text, "path=", 8);
        let cmds = extract_marker_values(text, "cmd=", 8);
        assert_eq!(
            files,
            vec!["/tmp/a.rs".to_string(), "/tmp/b.rs".to_string()]
        );
        assert_eq!(cmds, vec!["rg".to_string(), "cargo".to_string()]);
    }
