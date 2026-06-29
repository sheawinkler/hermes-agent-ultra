    use super::*;
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    fn env_test_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn isolate_route_learning_home(config: &mut AgentConfig) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        config.hermes_home = Some(tmp.path().to_string_lossy().to_string());
        tmp
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 250);
        assert_eq!(config.model, "gpt-5.5");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.max_delegate_depth, 4);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(!config.skip_context_files);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
        assert_eq!(config.stream_read_max_retries, 2);
        assert!(config.prefill_messages.is_empty());
    }

    #[tokio::test]
    async fn prefill_messages_are_model_visible_but_not_returned_for_persistence() {
        #[derive(Default)]
        struct CapturingProvider {
            seen_messages: Mutex<Vec<Message>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                *self.seen_messages.lock().expect("seen messages lock") = messages.to_vec();
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "test".to_string(),
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
                futures::stream::empty().boxed()
            }
        }

        let provider = std::sync::Arc::new(CapturingProvider::default());
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            prefill_messages: vec![
                Message::system("prefill system"),
                Message::user("prefill user example"),
            ],
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            std::sync::Arc::new(ToolRegistry::new()),
            provider.clone(),
        );

        let result = agent
            .run(vec![Message::user("real question")], Some(Vec::new()))
            .await
            .expect("agent run");

        let seen = provider.seen_messages.lock().expect("seen messages lock");
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill system")));
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill user example")));
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));

        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill system")));
        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("prefill user example")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("done")));
    }

    #[tokio::test]
    async fn ephemeral_system_prompt_is_model_visible_but_not_returned_for_persistence() {
        #[derive(Default)]
        struct CapturingProvider {
            seen_messages: Mutex<Vec<Message>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CapturingProvider {
            async fn chat_completion(
                &self,
                messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                *self.seen_messages.lock().expect("seen messages lock") = messages.to_vec();
                Ok(LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "test".to_string(),
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
                futures::stream::empty().boxed()
            }
        }

        let provider = std::sync::Arc::new(CapturingProvider::default());
        let mut config = AgentConfig {
            skip_memory: true,
            skip_context_files: true,
            ephemeral_system_prompt: Some("ephemeral test-only system prompt".to_string()),
            ..AgentConfig::default()
        };
        let _home = isolate_route_learning_home(&mut config);
        let agent = AgentLoop::new(
            config,
            std::sync::Arc::new(ToolRegistry::new()),
            provider.clone(),
        );

        let result = agent
            .run(vec![Message::user("real question")], Some(Vec::new()))
            .await
            .expect("agent run");

        let seen = provider.seen_messages.lock().expect("seen messages lock");
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("ephemeral test-only system prompt")));
        assert!(seen
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));

        assert!(!result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("ephemeral test-only system prompt")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("real question")));
        assert!(result
            .messages
            .iter()
            .any(|m| m.content.as_deref() == Some("done")));
    }

    #[test]
    fn delegate_depth_parser_floors_without_legacy_ceiling() {
        assert_eq!(parse_delegate_depth("99"), Some(99));
        assert_eq!(parse_delegate_depth(" 12 "), Some(12));
        assert_eq!(parse_delegate_depth("1"), Some(1));
        assert_eq!(parse_delegate_depth("0"), Some(1));
        assert_eq!(parse_delegate_depth("-7"), Some(1));
        assert_eq!(parse_delegate_depth(""), None);
        assert_eq!(parse_delegate_depth("not-a-number"), None);
    }

    #[test]
    fn max_delegate_depth_resolves_env_then_config_with_floor() {
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

        let _env = EnvVarGuard::set("HERMES_MAX_DELEGATE_DEPTH", "99");
        let config = AgentConfig {
            max_delegate_depth: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config.clone(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        assert_eq!(agent.resolve_max_delegate_depth(), 99);
        drop(_env);

        let _env = EnvVarGuard::set("HERMES_MAX_DELEGATE_DEPTH", "0");
        assert_eq!(agent.resolve_max_delegate_depth(), 1);
        drop(_env);

        let _env = EnvVarGuard::remove("HERMES_MAX_DELEGATE_DEPTH");
        let config = AgentConfig {
            max_delegate_depth: 0,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        assert_eq!(agent.resolve_max_delegate_depth(), 1);
    }

    #[test]
    fn delegation_spawning_paused_honors_env_toggle() {
        std::env::remove_var("HERMES_DELEGATION_PAUSED");
        assert!(!delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "1");
        assert!(delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "true");
        assert!(delegation_spawning_paused());
        std::env::set_var("HERMES_DELEGATION_PAUSED", "0");
        assert!(!delegation_spawning_paused());
    }

    #[test]
    fn tool_enforcement_prompt_gate_applies_to_non_openai_model_names() {
        assert!(should_inject_tool_enforcement_for_model(
            "nous:nousresearch/hermes-4-70b"
        ));
        assert!(should_inject_tool_enforcement_for_model(
            "openrouter:moonshotai/kimi-k2.6"
        ));
        assert!(should_inject_tool_enforcement_for_model(
            "anthropic:claude-3-7-sonnet"
        ));
    }

    #[test]
    fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    fn classify_error_404_generic_is_retryable() {
        assert_eq!(classify_error("HTTP 404 Not Found"), ErrorClass::Retryable);
        assert_eq!(
            classify_error("gateway returned not found"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_404_model_not_found_is_fatal() {
        assert_eq!(
            classify_error("404 model not found: foo/bar"),
            ErrorClass::Fatal
        );
        assert_eq!(
            classify_error("invalid model: gpt-unknown"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_openrouter_privacy_guardrail_is_fatal() {
        assert_eq!(
            classify_error("HTTP 404: OpenRouter privacy guardrail blocked this endpoint"),
            ErrorClass::Fatal
        );
    }

    #[test]
    fn classify_error_ssl_bad_record_mac_is_retryable() {
        assert_eq!(
            classify_error("[SSL: BAD_RECORD_MAC] sslv3 alert bad record mac (_ssl.c:2580)"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_ssl_openssl_token_form_is_retryable() {
        assert_eq!(
            classify_error("ERR_SSL_SSL/TLS_ALERT_BAD_RECORD_MAC during streaming"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_plain_disconnect_stays_retryable() {
        assert_eq!(
            classify_error("Server disconnected without sending a response"),
            ErrorClass::Retryable
        );
    }

    #[test]
    fn classify_error_context_overflow_contracts_include_413_and_generic_400_text() {
        for err in [
            "HTTP 413 payload too large",
            "API error 400: request too large: max tokens per request is 200000",
            "prompt is too long: context length 300000 exceeds max of 200000",
            "Please reduce the length of the messages",
            "The input exceeds the context window",
        ] {
            assert_eq!(classify_error(err), ErrorClass::ContextOverflow, "{err}");
        }
    }

    #[test]
    fn tool_payload_validation_error_detector_matches_known_provider_signatures() {
        let strict_shape = "API error 400 Bad Request: Invalid input: expected \"function\"";
        assert!(is_tool_payload_validation_error(strict_shape));
        let provider_generic = "API error 400 Bad Request: This request is not valid. Check the model name and other parameters. Additional info: Provider returned error";
        assert!(is_tool_payload_validation_error(provider_generic));
        let no_choices_provider_shape = "No choices in response (status=400; message=This request is not valid. Check the model name and other parameters. Additional info: Provider returned error)";
        assert!(is_tool_payload_validation_error(no_choices_provider_shape));
        let unprocessable_payload =
            "API error 422 Unprocessable Entity: Check that you're sending a valid payload";
        assert!(is_tool_payload_validation_error(unprocessable_payload));
        assert!(!is_tool_payload_validation_error(
            "API error 400 Bad Request: max_tokens must be positive"
        ));
    }

    #[test]
    fn preferred_tool_payload_fallback_model_defaults_and_override() {
        assert_eq!(
            preferred_tool_payload_fallback_model("nous", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-70b".to_string())
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("nous-api", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-70b".to_string())
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("openrouter", "openai/gpt-5.5"),
            None
        );
        std::env::set_var(
            "HERMES_TOOL_PAYLOAD_FALLBACK_MODEL",
            "nousresearch/hermes-4-405b",
        );
        assert_eq!(
            preferred_tool_payload_fallback_model("nous-portal-api", "openai/gpt-5.5"),
            Some("nousresearch/hermes-4-405b".to_string())
        );
        std::env::remove_var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL");
    }

    #[test]
    fn maybe_nous_401_diagnostic_returns_hint_for_nous_auth_failures() {
        let diag = maybe_nous_401_diagnostic(
            "nous",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        )
        .expect("nous 401 should produce diagnostics");
        assert!(diag.contains("Nous 401 — Portal authentication failed."));
        assert!(diag.contains("hermes auth add nous"));
        assert!(diag.contains("portal.nousresearch.com"));
        assert!(diag.contains("/tmp/hermes-home/auth.json"));
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_nous_provider() {
        let diag = maybe_nous_401_diagnostic(
            "openrouter",
            "HTTP 401 Unauthorized: token expired",
            Some("/tmp/hermes-home"),
        );
        assert!(diag.is_none());
    }

    #[test]
    fn maybe_nous_401_diagnostic_ignores_non_auth_errors() {
        let diag = maybe_nous_401_diagnostic("nous", "HTTP 500 upstream timeout", None);
        assert!(diag.is_none());
    }

    #[test]
    fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}"),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[test]
    fn summarize_background_review_filters_status_and_secret_like_text() {
        let msgs = vec![
            Message::tool_result(
                "tc_safe",
                "{\"success\":true,\"message\":\"created docs/repo-review-notes.md\"}",
            ),
            Message::tool_result(
                "tc_status",
                "{\"success\":true,\"message\":\"status=ok token refreshed\"}",
            ),
            Message::tool_result(
                "tc_obj",
                "{\"success\":true,\"message\":\"{\\\"message\\\":\\\"updated config\\\"}\"}",
            ),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.contains("created docs/repo-review-notes.md"));
        assert!(!out.contains("status=ok token refreshed"));
        assert!(!out.contains("{\"message\""));
    }

    #[test]
    fn exploratory_hint_enabled_for_repo_exploration_intent() {
        let msgs = vec![Message::user(
            "Deeply audit /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/crates/hermes-agent/src/agent_loop.rs and diagnose root cause.",
        )];
        let hint = exploratory_problem_solving_system_hint(&msgs).expect("hint should exist");
        assert!(hint.contains("Exploratory problem-solving protocol active"));
        assert!(hint.contains("workstream=<name>"));
    }

    #[test]
    fn exploratory_hint_disabled_for_non_exploratory_repo_intent() {
        let msgs = vec![Message::user(
            "Implement this fix directly in /Users/sheawinkler/Documents/Projects/hermes-agent-ultra/src/main.rs.",
        )];
        assert!(exploratory_problem_solving_system_hint(&msgs).is_none());
    }

    #[test]
    fn hook_context_spill_writes_file_for_oversized_payload() {
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

        let prev = std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS").ok();
        std::env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", "1024");
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = AgentConfig {
            hermes_home: Some(tmp.path().display().to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider));
        let large = "x".repeat(2_048);
        let spilled = agent
            .spill_hook_context_if_oversized(&large)
            .expect("spill should write file");
        assert!(spilled.exists(), "spill file should exist");
        let read_back = std::fs::read_to_string(&spilled).expect("read spill file");
        assert_eq!(read_back.len(), large.len());
        assert!(
            agent
                .spill_hook_context_if_oversized("small payload")
                .is_none(),
            "small payload must not spill"
        );
        if let Some(v) = prev {
            std::env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", v);
        } else {
            std::env::remove_var("HERMES_HOOK_CONTEXT_SPILL_CHARS");
        }
    }

    #[test]
    fn post_llm_transform_hook_rewrites_assistant_content() {
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
        let mut content = Some("before".to_string());
        agent.apply_hook_output_transforms(
            &[HookResult::TransformLlmOutput("after".to_string())],
            &mut content,
        );
        assert_eq!(content.as_deref(), Some("after"));
    }

    #[test]
    fn preflight_compression_status_reports_skipped_when_under_threshold() {
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("small"));
        agent.preflight_context_compress_with_status(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| { kind == "lifecycle" && msg.contains("no compression needed") }));
    }

    #[test]
    fn preflight_compression_status_reports_when_compressing() {
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(95)));
        agent.preflight_context_compress_with_status(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("compressing before first turn")
        }));
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("compression complete")));
    }

    #[test]
    fn status_callback_receives_context_pressure_messages() {
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    include!("tests/llm_retry_stream.rs");

    #[test]
    fn quiet_mode_suppresses_status_callback() {
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

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx);

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    fn test_builtin_personality_injected_into_system_prompt() {
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

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    fn test_unknown_personality_name_does_not_add_overlay_block() {
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

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn test_default_personality_name_does_not_add_overlay_block() {
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

        let config = AgentConfig {
            personality: Some("default".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (default)"));
        assert!(prompt.contains("You are Hermes Agent"));
    }

    #[test]
    fn steer_channel_note_is_gated_on_tool_availability() {
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
        let without_tools = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!without_tools.contains("## Mid-turn user steering"));

        let tool_schemas = vec![ToolSchema::new(
            "terminal",
            "Terminal",
            JsonSchema::new("object"),
        )];
        let with_tools = agent.build_system_prompt("", &tool_schemas, "gpt-4o");
        assert!(with_tools.contains(STEER_CHANNEL_NOTE));
        assert!(with_tools.contains(crate::steer::STEER_MARKER_OPEN));
        assert!(with_tools.contains(crate::steer::STEER_MARKER_CLOSE));
    }

    #[test]
    fn coding_context_prompt_injected_for_cli_workspace_with_tools() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write manifest");
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
            ToolSchema::new("read_file", "Read", JsonSchema::new("object")),
            ToolSchema::new("write_file", "Write", JsonSchema::new("object")),
            ToolSchema::new("patch", "Patch", JsonSchema::new("object")),
            ToolSchema::new("search_files", "Search", JsonSchema::new("object")),
        ];
        let prompt = agent.build_system_prompt("", &tools, "openai/gpt-5.4");
        assert!(
            prompt.contains("You are a coding agent pairing with the user inside their codebase")
        );
        assert!(prompt.contains("Workspace"));
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("cargo test"));
        assert!(prompt.contains("mode='patch'"));

        let without_tools = agent.build_system_prompt("", &[], "openai/gpt-5.4");
        assert!(!without_tools.contains("You are a coding agent pairing"));
    }

    #[test]
    fn coding_context_prompt_respects_platform_and_off_mode() {
        use futures::stream::BoxStream;
        use hermes_core::JsonSchema;

        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("Cargo.toml"),
            "[package]\nname='x'\nversion='0.1.0'\n",
        )
        .expect("write manifest");
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

        let tools = vec![ToolSchema::new(
            "terminal",
            "Terminal",
            JsonSchema::new("object"),
        )];
        for config in [
            AgentConfig {
                platform: Some("telegram".to_string()),
                coding_context: "auto".to_string(),
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
            AgentConfig {
                platform: Some("cli".to_string()),
                coding_context: "off".to_string(),
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
        ] {
            let agent = AgentLoop::new(
                config,
                Arc::new(ToolRegistry::new()),
                Arc::new(DummyProvider),
            );
            let prompt = agent.build_system_prompt("", &tools, "openai/gpt-5.4");
            assert!(!prompt.contains("You are a coding agent pairing"));
        }
    }

    include!("tests/coding_route_learning.rs");
