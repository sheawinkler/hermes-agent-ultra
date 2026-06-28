#[test]
fn coding_context_prunes_non_coding_skill_categories_in_prompt_only() {
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    let _lock = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[package]\nname='x'\nversion='0.1.0'\n",
    )
    .expect("write manifest");
    let home = tmp.path().join("home");
    let social = home.join("skills").join("social-media").join("tweet-stuff");
    let github = home.join("skills").join("github").join("pr-review");
    std::fs::create_dir_all(&social).expect("social skill dir");
    std::fs::create_dir_all(&github).expect("github skill dir");
    std::fs::write(
        social.join("SKILL.md"),
        "---\nname: tweet-stuff\ndescription: Draft social posts\n---\nBody",
    )
    .expect("write social skill");
    std::fs::write(
        github.join("SKILL.md"),
        "---\nname: pr-review\ndescription: Review pull requests\n---\nBody",
    )
    .expect("write github skill");
    let _cwd = EnvVarGuard::set("TERMINAL_CWD", tmp.path().to_str().unwrap());

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

    let config = AgentConfig {
        platform: Some("cli".to_string()),
        coding_context: "auto".to_string(),
        hermes_home: Some(home.to_string_lossy().to_string()),
        skip_memory: true,
        skip_context_files: true,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let tools = vec![
        ToolSchema::new("terminal", "Terminal", JsonSchema::new("object")),
        ToolSchema::new("skills_list", "List skills", JsonSchema::new("object")),
        ToolSchema::new("skill_view", "View skill", JsonSchema::new("object")),
    ];
    let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-sonnet-4");
    assert!(prompt.contains("pr-review"));
    assert!(!prompt.contains("tweet-stuff"));
    assert!(prompt.contains("skills_prompt_pruned"));
    assert!(
        prompt.contains("full catalog remains available through skills_list and skill_view")
    );
    assert!(prompt.contains("mode='replace'"));
}

#[tokio::test]
async fn mid_turn_steer_interrupt_is_appended_to_last_tool_result() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCall, JsonSchema};

    #[derive(Default)]
    struct RecordingProvider {
        calls: std::sync::Mutex<u32>,
        second_call_messages: std::sync::Mutex<Vec<Message>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RecordingProvider {
        async fn chat_completion(
            &self,
            messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            if *calls == 1 {
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant_with_tool_calls(
                        None,
                        vec![ToolCall {
                            id: "call_1".to_string(),
                            function: FunctionCall {
                                name: "steer_test".to_string(),
                                arguments: "{}".to_string(),
                            },
                            extra_content: None,
                        }],
                    ),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("tool_calls".into()),
                });
            }

            *self.second_call_messages.lock().expect("messages lock") = messages.to_vec();
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

    let interrupt = InterruptController::new();
    let interrupt_for_tool = interrupt.clone();
    let mut registry = ToolRegistry::new();
    registry.register(
        "steer_test",
        ToolSchema::new("steer_test", "Steer test", JsonSchema::new("object")),
        Arc::new(move |_args| {
            interrupt_for_tool.interrupt(Some(crate::steer::format_steer_marker(
                "prefer the simpler fix",
            )));
            Ok("tool output".to_string())
        }),
    );
    let provider = Arc::new(RecordingProvider::default());
    let agent = AgentLoop::with_interrupt(
        AgentConfig::default(),
        Arc::new(registry),
        provider.clone(),
        interrupt,
    );

    let result = agent
        .run(vec![Message::user("use the tool")], None)
        .await
        .expect("agent run");

    assert!(!result.interrupted);
    let second_call_messages = provider
        .second_call_messages
        .lock()
        .expect("messages lock")
        .clone();
    let tool_message = second_call_messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("tool result in second call");
    assert_eq!(tool_message.name.as_deref(), Some("steer_test"));
    let content = tool_message.content.as_deref().expect("tool content");
    assert!(content.contains("tool output"));
    assert!(content.contains(crate::steer::STEER_MARKER_OPEN));
    assert!(content.contains("prefer the simpler fix"));
    assert!(content.contains(crate::steer::STEER_MARKER_CLOSE));
    assert!(!content.contains("User guidance:"));

    let persisted_tool_message = result
        .messages
        .iter()
        .find(|message| message.role == MessageRole::Tool)
        .expect("persisted tool result");
    assert_eq!(persisted_tool_message.name.as_deref(), Some("steer_test"));
}

#[test]
fn test_smart_model_routing_cheap_route_for_simple_turn() {
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
            base_url: None,
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
    let messages = vec![Message::user("帮我总结一下今天要做什么")];
    let selected = agent.resolve_smart_runtime_route(&messages);
    assert_eq!(
        selected.as_ref().map(|r| r.model.as_str()),
        Some("gpt-4o-mini")
    );
}

#[test]
fn test_smart_model_routing_online_learning_prefers_primary_when_cheap_unstable() {
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
            base_url: None,
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
    if let Ok(mut m) = agent.route_learning.lock() {
        m.insert(
            "openai:gpt-4o".to_string(),
            RouteLearningStats {
                samples: 12,
                success_rate: 0.98,
                avg_latency_ms: 900.0,
                consecutive_failures: 0,
                updated_at_unix_ms: now_unix_ms(),
            },
        );
        m.insert(
            "openai:gpt-4o-mini".to_string(),
            RouteLearningStats {
                samples: 12,
                success_rate: 0.35,
                avg_latency_ms: 3800.0,
                consecutive_failures: 3,
                updated_at_unix_ms: now_unix_ms(),
            },
        );
    }
    let selected =
        agent.resolve_smart_runtime_route(&[Message::user("summarize today's work")]);
    assert!(
        selected.is_none(),
        "online learning should keep primary when cheap route is unstable"
    );
}

#[test]
fn test_route_learning_updates_after_outcomes() {
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

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentConfig::default();
    cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());
    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
    agent.update_route_learning(None, Some("openai:gpt-4o"), 2100, false);
    agent.update_route_learning(None, Some("openai:gpt-4o"), 900, true);
    let snapshot = agent.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["enabled"], true);
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert_eq!(snapshot["stats"]["samples"], 2);
    assert!(snapshot["stats"]["success_rate"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(snapshot["stats"]["avg_latency_ms"].as_f64().unwrap_or(0.0) > 0.0);
}

#[test]
fn test_route_learning_persists_across_agent_restarts() {
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

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentConfig::default();
    cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());

    let agent = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    agent.update_route_learning(None, Some("openai:gpt-4o"), 1200, true);
    let persisted_path = route_learning_state_path(&cfg);
    assert!(
        persisted_path.exists(),
        "route-learning state file must exist"
    );

    let reloaded = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    );
    let snapshot = reloaded.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert!(snapshot["stats"]["samples"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn test_route_learning_malformed_file_is_safe_fallback() {
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

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentConfig::default();
    cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());
    let state_path = route_learning_state_path(&cfg);
    std::fs::create_dir_all(
        state_path
            .parent()
            .expect("route-learning path should have a parent"),
    )
    .expect("create route-learning dir");
    std::fs::write(&state_path, "{ this-is-invalid-json")
        .expect("write malformed route-learning file");

    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
    let snapshot = agent.route_learning_snapshot(None, Some("openai:gpt-4o"));
    assert!(
        snapshot["stats"].is_null(),
        "malformed state must fall back cleanly"
    );
}
