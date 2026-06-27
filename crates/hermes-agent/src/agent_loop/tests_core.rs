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
        assert!(diag.contains("hermes auth login nous"));
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

    #[tokio::test]
    async fn call_llm_with_retry_strips_provider_prefix_for_primary_and_fallback_models() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct RecordingProvider {
            seen_models: Arc<std::sync::Mutex<Vec<String>>>,
            call_count: AtomicUsize,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                self.seen_models
                    .lock()
                    .expect("seen model lock")
                    .push(model.unwrap_or_default().to_string());
                let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
                if idx == 0 {
                    return Err(AgentError::LlmApi(
                        "API error 429: synthetic retry".to_string(),
                    ));
                }
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "ok".to_string(),
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

        let seen_models = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:primary-model".to_string();
        cfg.retry.max_retries = 0;
        cfg.retry.fallback_model = Some("openrouter:backup-model".to_string());

        let provider = Arc::new(RecordingProvider {
            seen_models: seen_models.clone(),
            call_count: AtomicUsize::new(0),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hello"));

        let resp = agent
            .call_llm_with_retry_inner(&ctx, &[], None, None)
            .await
            .expect("fallback should recover");
        assert_eq!(resp.message.content.as_deref(), Some("ok"));

        let seen = seen_models.lock().expect("seen model lock").clone();
        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    struct RateLimitFallbackProvider {
        seen_models: Arc<std::sync::Mutex<Vec<String>>>,
        primary_calls_before_success: std::sync::atomic::AtomicUsize,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RateLimitFallbackProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let model = model.unwrap_or_default().to_string();
            self.seen_models
                .lock()
                .expect("seen model lock")
                .push(model.clone());
            if model == "backup-model" {
                return Ok(hermes_core::LlmResponse {
                    message: Message::assistant("fallback-ok"),
                    usage: None,
                    model: "backup-model".to_string(),
                    finish_reason: Some("stop".to_string()),
                });
            }
            let remaining = self
                .primary_calls_before_success
                .fetch_update(
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                    |value| value.checked_sub(1),
                )
                .unwrap_or(0);
            if remaining > 0 {
                return Err(AgentError::LlmApi(
                    "API error 429: synthetic rate limit".to_string(),
                ));
            }
            Ok(hermes_core::LlmResponse {
                message: Message::assistant("primary-ok"),
                usage: None,
                model: "primary-model".to_string(),
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

    async fn run_rate_limit_fallback_probe(
        provider: &str,
        base_url: Option<&str>,
        pool: Arc<CredentialPool>,
        primary_failures_before_success: usize,
    ) -> Vec<String> {
        let seen_models = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut cfg = AgentConfig::default();
        cfg.model = "primary-model".to_string();
        cfg.provider = Some(provider.to_string());
        cfg.retry.max_retries = 3;
        cfg.retry.base_delay_ms = 0;
        cfg.retry.max_delay_ms = 0;
        cfg.retry.fallback_model = Some("openrouter:backup-model".to_string());
        if let Some(base_url) = base_url {
            cfg.runtime_providers.insert(
                provider.to_string(),
                RuntimeProviderConfig {
                    base_url: Some(base_url.to_string()),
                    request_timeout_seconds: None,
                    api_mode: None,
                    ..RuntimeProviderConfig::default()
                },
            );
        }

        let provider = Arc::new(RateLimitFallbackProvider {
            seen_models: seen_models.clone(),
            primary_calls_before_success: std::sync::atomic::AtomicUsize::new(
                primary_failures_before_success,
            ),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider)
            .with_primary_credential_pool(pool);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hello"));

        let _ = agent
            .call_llm_with_retry_inner(&ctx, &[], None, None)
            .await
            .expect("probe should recover");
        let seen = seen_models.lock().expect("seen model lock").clone();
        seen
    }

    #[tokio::test]
    async fn cloudcode_rate_limit_uses_fallback_without_pool_retry() {
        let seen = run_rate_limit_fallback_probe(
            "google-gemini-cli",
            None,
            Arc::new(CredentialPool::new(vec![
                "oauth-a".to_string(),
                "oauth-b".to_string(),
                "oauth-c".to_string(),
            ])),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[tokio::test]
    async fn cloudcode_base_url_uses_fallback_even_for_alias_provider() {
        let seen = run_rate_limit_fallback_probe(
            "custom-provider",
            Some("cloudcode-pa://google"),
            Arc::new(CredentialPool::new(vec![
                "oauth-a".to_string(),
                "oauth-b".to_string(),
                "oauth-c".to_string(),
            ])),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[tokio::test]
    async fn non_cloudcode_multi_key_pool_retries_primary_before_fallback() {
        let seen = run_rate_limit_fallback_probe(
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            Arc::new(CredentialPool::new(vec![
                "key-a".to_string(),
                "key-b".to_string(),
                "key-c".to_string(),
            ])),
            1,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "primary-model".to_string()]
        );
    }

    #[tokio::test]
    async fn single_entry_pool_rate_limit_uses_fallback_without_retry() {
        let seen = run_rate_limit_fallback_probe(
            "openrouter",
            Some("https://openrouter.ai/api/v1"),
            Arc::new(CredentialPool::single("key-a")),
            10,
        )
        .await;

        assert_eq!(
            seen,
            vec!["primary-model".to_string(), "backup-model".to_string()]
        );
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessStep {
        kind: String,
        message: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
    struct ChaosHarnessExpectation {
        outcome: String,
        attempts: usize,
        fallback_calls: usize,
        error_contains: Option<String>,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct ChaosHarnessScenario {
        id: String,
        seed: u64,
        max_retries: u32,
        fallback_model: Option<String>,
        #[serde(default)]
        include_tool_schema: bool,
        steps: Vec<ChaosHarnessStep>,
        expected: ChaosHarnessExpectation,
    }

    #[derive(Debug, serde::Deserialize)]
    struct ChaosHarnessFixture {
        schema_version: u32,
        scenarios: Vec<ChaosHarnessScenario>,
    }

    #[derive(Debug)]
    struct ChaosHarnessRun {
        outcome: &'static str,
        attempts: usize,
        fallback_calls: usize,
        error: Option<String>,
    }

    fn load_chaos_harness_fixture() -> ChaosHarnessFixture {
        serde_json::from_str(include_str!("../testdata/adapter_chaos_profiles.json"))
            .expect("parse adapter chaos fixture")
    }

    struct ChaosHarnessProvider {
        scenario_id: String,
        steps: Vec<ChaosHarnessStep>,
        call_index: std::sync::atomic::AtomicUsize,
        seen_models: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl ChaosHarnessProvider {
        fn new(scenario: &ChaosHarnessScenario) -> Self {
            Self {
                scenario_id: scenario.id.clone(),
                steps: scenario.steps.clone(),
                call_index: std::sync::atomic::AtomicUsize::new(0),
                seen_models: Arc::new(std::sync::Mutex::new(Vec::new())),
            }
        }

        fn attempts(&self) -> usize {
            self.call_index.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn fallback_calls(&self, fallback_model: Option<&str>) -> usize {
            let Some(fallback) = fallback_model else {
                return 0;
            };
            let fallback_name = fallback
                .split_once(':')
                .map(|(_, model)| model)
                .unwrap_or(fallback);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .iter()
                .filter(|m| m.as_str() == fallback_name)
                .count()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ChaosHarnessProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            let idx = self
                .call_index
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.seen_models
                .lock()
                .expect("seen model lock")
                .push(model.unwrap_or_default().to_string());

            let step = self
                .steps
                .get(idx)
                .cloned()
                .or_else(|| self.steps.last().cloned())
                .unwrap_or(ChaosHarnessStep {
                    kind: "success".to_string(),
                    message: Some("ok-default".to_string()),
                });
            match step.kind.as_str() {
                "success" => Ok(hermes_core::LlmResponse {
                    message: Message::assistant(
                        step.message
                            .unwrap_or_else(|| format!("ok-{}", self.scenario_id)),
                    ),
                    usage: None,
                    model: "chaos".to_string(),
                    finish_reason: Some("stop".to_string()),
                }),
                "timeout" => Err(AgentError::LlmApi(
                    step.message
                        .unwrap_or_else(|| "request timeout".to_string()),
                )),
                "http_5xx" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 500: synthetic upstream fault".to_string()
                    })))
                }
                "rate_limit" => {
                    Err(AgentError::LlmApi(step.message.unwrap_or_else(|| {
                        "API error 429: synthetic rate limit".to_string()
                    })))
                }
                "connection_reset" => Err(AgentError::LlmApi(
                    step.message
                        .unwrap_or_else(|| "connection reset by peer".to_string()),
                )),
                "auth_expired" => Err(AgentError::AuthFailed(
                    step.message
                        .unwrap_or_else(|| "HTTP 401 Unauthorized: token expired".to_string()),
                )),
                "malformed_tool_payload" => Err(AgentError::LlmApi(
                    step.message.unwrap_or_else(|| {
                        "Provider returned error: This request is not valid because tools payload is invalid"
                            .to_string()
                    }),
                )),
                other => Err(AgentError::LlmApi(format!(
                    "unsupported chaos step '{}' in scenario '{}'",
                    other, self.scenario_id
                ))),
            }
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

    async fn run_chaos_harness_scenario(scenario: &ChaosHarnessScenario) -> ChaosHarnessRun {
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:primary-model".to_string();
        cfg.retry.max_retries = scenario.max_retries;
        cfg.retry.base_delay_ms = 0;
        cfg.retry.max_delay_ms = 0;
        cfg.retry.fallback_model = scenario.fallback_model.clone();

        let provider = Arc::new(ChaosHarnessProvider::new(scenario));
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user(format!(
            "chaos scenario {} seed {}",
            scenario.id, scenario.seed
        )));
        let tool_schemas = if scenario.include_tool_schema {
            vec![ToolSchema::new(
                "sota_fault_probe",
                "SOTA fault-injection probe tool",
                hermes_core::JsonSchema::new("object"),
            )]
        } else {
            Vec::new()
        };

        match agent
            .call_llm_with_retry_inner(&ctx, &tool_schemas, None, None)
            .await
        {
            Ok(_) => ChaosHarnessRun {
                outcome: "success",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: None,
            },
            Err(err) => ChaosHarnessRun {
                outcome: "error",
                attempts: provider.attempts(),
                fallback_calls: provider.fallback_calls(scenario.fallback_model.as_deref()),
                error: Some(err.to_string()),
            },
        }
    }

    #[test]
    fn chaos_harness_fixture_is_seeded_and_unique() {
        let fixture = load_chaos_harness_fixture();
        assert_eq!(fixture.schema_version, 1, "unexpected fixture schema");
        assert!(
            !fixture.scenarios.is_empty(),
            "chaos fixture must not be empty"
        );
        let mut ids = std::collections::HashSet::new();
        let mut seeds = std::collections::HashSet::new();
        for scenario in fixture.scenarios {
            assert!(
                ids.insert(scenario.id.clone()),
                "duplicate chaos scenario id: {}",
                scenario.id
            );
            assert!(
                seeds.insert(scenario.seed),
                "duplicate chaos scenario seed: {}",
                scenario.seed
            );
        }
    }

    #[tokio::test]
    async fn chaos_harness_profiles_verify_retry_and_fallback() {
        let fixture = load_chaos_harness_fixture();
        let mut diagnostics = Vec::new();
        let mut runs = Vec::new();
        for scenario in fixture.scenarios {
            let run = run_chaos_harness_scenario(&scenario).await;
            runs.push(serde_json::json!({
                "scenario": scenario.id,
                "seed": scenario.seed,
                "actual": {
                    "outcome": run.outcome,
                    "attempts": run.attempts,
                    "fallback_calls": run.fallback_calls,
                    "error": run.error,
                }
            }));
            let mut mismatches = Vec::new();

            if run.outcome != scenario.expected.outcome {
                mismatches.push(format!(
                    "outcome mismatch expected={} actual={}",
                    scenario.expected.outcome, run.outcome
                ));
            }
            if run.attempts != scenario.expected.attempts {
                mismatches.push(format!(
                    "attempt mismatch expected={} actual={}",
                    scenario.expected.attempts, run.attempts
                ));
            }
            if run.fallback_calls != scenario.expected.fallback_calls {
                mismatches.push(format!(
                    "fallback_calls mismatch expected={} actual={}",
                    scenario.expected.fallback_calls, run.fallback_calls
                ));
            }
            if let Some(expect_fragment) = scenario.expected.error_contains.as_ref() {
                let got_error = run.error.as_deref().unwrap_or("");
                if !got_error.contains(expect_fragment) {
                    mismatches.push(format!(
                        "error fragment missing expected='{}' actual='{}'",
                        expect_fragment, got_error
                    ));
                }
            }

            if !mismatches.is_empty() {
                diagnostics.push(serde_json::json!({
                    "scenario": scenario.id,
                    "seed": scenario.seed,
                    "expected": scenario.expected,
                    "actual": {
                        "outcome": run.outcome,
                        "attempts": run.attempts,
                        "fallback_calls": run.fallback_calls,
                        "error": run.error,
                    },
                    "mismatches": mismatches,
                }));
            }
        }

        println!(
            "adapter chaos harness results: {}",
            serde_json::to_string(&runs).expect("serialize chaos runs")
        );

        assert!(
            diagnostics.is_empty(),
            "adapter chaos harness mismatches:\n{}",
            serde_json::to_string_pretty(&diagnostics).expect("serialize diagnostics")
        );
    }

    #[tokio::test]
    async fn handle_max_iterations_uses_provider_native_model_id() {
        use futures::stream::BoxStream;

        struct RecordingProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecordingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen model lock") =
                    Some(model.unwrap_or_default().to_string());
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("summary"),
                    usage: None,
                    model: "ok".to_string(),
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

        let seen_model = Arc::new(std::sync::Mutex::new(None::<String>));
        let mut cfg = AgentConfig::default();
        cfg.model = "nous:moonshotai/kimi-k2.6".to_string();

        let provider = Arc::new(RecordingProvider {
            seen_model: seen_model.clone(),
        });
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::new(32);
        ctx.add_message(Message::user("hit turn limit"));

        let _ = agent
            .handle_max_iterations(&mut ctx)
            .await
            .expect("max iterations summary should succeed");
        let seen = seen_model.lock().expect("seen model lock").clone();
        assert_eq!(seen.as_deref(), Some("moonshotai/kimi-k2.6"));
    }

    #[tokio::test]
    async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                let finish_reason = if *n == 1 {
                    None
                } else {
                    Some("stop".to_string())
                };
                Ok(hermes_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason,
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
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    async fn empty_stop_response_is_accepted_without_retry() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct EmptyStopProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for EmptyStopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant(""),
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

        let provider = Arc::new(EmptyStopProvider::default());
        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 3,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone())
            .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        let rows = captured.lock().expect("captured lock");
        assert!(!rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    fn stream_chunk_content(text: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: Some(text.to_string()),
                tool_calls: None,
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_name(index: u32, id: &str, name: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: Some(id.to_string()),
                    function: Some(hermes_core::FunctionCallDelta {
                        name: Some(name.to_string()),
                        arguments: None,
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_tool_args(index: u32, args: &str) -> StreamChunk {
        StreamChunk {
            delta: Some(hermes_core::StreamDelta {
                content: None,
                tool_calls: Some(vec![hermes_core::ToolCallDelta {
                    index,
                    id: None,
                    function: Some(hermes_core::FunctionCallDelta {
                        name: None,
                        arguments: Some(args.to_string()),
                    }),
                }]),
                extra: None,
            }),
            finish_reason: None,
            usage: None,
        }
    }

    fn stream_chunk_finish(reason: &str) -> StreamChunk {
        StreamChunk {
            delta: None,
            finish_reason: Some(reason.to_string()),
            usage: None,
        }
    }

    #[derive(Clone, Copy)]
    enum StreamRetryScenario {
        RecoverOnSecondAttempt,
        AlwaysFailMidToolCall,
        TextOnlyDrop,
        MemoryContextLeak,
        MalformedToolArgs,
    }

    struct StreamRetryProvider {
        scenario: StreamRetryScenario,
        calls: std::sync::Mutex<u32>,
    }

    impl StreamRetryProvider {
        fn new(scenario: StreamRetryScenario) -> Self {
            Self {
                scenario,
                calls: std::sync::Mutex::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for StreamRetryProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, AgentError> {
            Err(AgentError::LlmApi("unused".to_string()))
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
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            let attempt = *calls;

            let events: Vec<Result<StreamChunk, AgentError>> = match self.scenario {
                StreamRetryScenario::RecoverOnSecondAttempt => {
                    if attempt == 1 {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/x\",")),
                            Err(AgentError::LlmApi(
                                "Stream read error: peer closed connection".to_string(),
                            )),
                        ]
                    } else {
                        vec![
                            Ok(stream_chunk_content("Let me write the audit: ")),
                            Ok(stream_chunk_tool_name(0, "call_1", "write_file")),
                            Ok(stream_chunk_tool_args(
                                0,
                                "{\"path\":\"/tmp/x\",\"content\":\"hi\"}",
                            )),
                            Ok(stream_chunk_finish("tool_calls")),
                        ]
                    }
                }
                StreamRetryScenario::AlwaysFailMidToolCall => vec![
                    Ok(stream_chunk_content("Working...")),
                    Ok(stream_chunk_tool_name(0, "call_2", "write_file")),
                    Ok(stream_chunk_tool_args(0, "{\"path\":\"/tmp/y\",")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection reset by peer".to_string(),
                    )),
                ],
                StreamRetryScenario::TextOnlyDrop => vec![
                    Ok(stream_chunk_content("Partial text")),
                    Err(AgentError::LlmApi(
                        "Stream read error: connection lost".to_string(),
                    )),
                ],
                StreamRetryScenario::MemoryContextLeak => vec![
                    Ok(stream_chunk_content("Hello\n")),
                    Ok(stream_chunk_content("<memory-context>\nsecret ")),
                    Ok(stream_chunk_content("payload\n")),
                    Ok(stream_chunk_content("</memory-context> world")),
                    Ok(stream_chunk_finish("stop")),
                ],
                StreamRetryScenario::MalformedToolArgs => vec![
                    Ok(stream_chunk_tool_name(0, "call_repair", "terminal")),
                    Ok(stream_chunk_tool_args(
                        0,
                        "{\"command\":\"ls -la\",\"timeout\":30",
                    )),
                    Ok(stream_chunk_finish("tool_calls")),
                ],
            };

            futures::stream::iter(events).boxed()
        }
    }

    #[test]
    fn normalize_tool_call_arguments_repairs_malformed_json_without_retry_error() {
        let mut tc = ToolCall {
            id: "call_1".to_string(),
            function: hermes_core::FunctionCall {
                name: "terminal".to_string(),
                arguments: "{\"command\":\"ls -la\",\"timeout\":30".to_string(),
            },
            extra_content: None,
        };

        AgentLoop::normalize_tool_call_arguments(&mut tc).expect("repairable arguments");
        let parsed: Value = serde_json::from_str(&tc.function.arguments).expect("valid JSON");
        assert_eq!(parsed["command"], "ls -la");
        assert_eq!(parsed["timeout"], 30);
    }

    #[tokio::test]
    async fn stream_tool_call_arguments_are_repaired_before_truncation_retry_gate() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::MalformedToolArgs,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 0,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider);
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &|_| {})
            .await
            .expect("stream should collect");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete response");
        };
        assert_eq!(resp.finish_reason.as_deref(), Some("tool_calls"));
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("tool call");
        let parsed: Value = serde_json::from_str(&tc.function.arguments).expect("valid JSON");
        assert_eq!(parsed["command"], "ls -la");
        assert_eq!(parsed["timeout"], 30);
    }

    #[tokio::test]
    async fn stream_mid_tool_call_silent_retry_recovers_tool_call() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::RecoverOnSecondAttempt,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let StreamCollectOutcome::Complete(resp) = out.expect("stream should recover") else {
            panic!("expected complete response");
        };
        let tc = resp
            .message
            .tool_calls
            .as_ref()
            .and_then(|calls| calls.first())
            .expect("missing tool call");
        assert_eq!(tc.function.name, "write_file");
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_mid_tool_call_exhausted_retries_returns_error() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::AlwaysFailMidToolCall,
        ));
        let cfg = AgentConfig {
            stream_read_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("should fail after retries"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
        assert!(seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_text_only_drop_does_not_retry() {
        let provider = Arc::new(StreamRetryProvider::new(StreamRetryScenario::TextOnlyDrop));
        let cfg = AgentConfig {
            stream_read_max_retries: 2,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await;

        let err = match out {
            Err(err) => err,
            Ok(_) => panic!("text-only stream drops should not retry silently"),
        };
        assert!(err.to_string().contains("Stream read error"));
        assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
        assert!(!seen.lock().expect("seen lock").iter().any(|text| {
            text.to_lowercase()
                .contains("connection dropped mid tool-call; reconnecting")
        }));
    }

    #[tokio::test]
    async fn stream_memory_context_blocks_are_scrubbed_from_callbacks_and_final_message() {
        let provider = Arc::new(StreamRetryProvider::new(
            StreamRetryScenario::MemoryContextLeak,
        ));
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            provider.clone(),
        );
        let mut ctx = ContextManager::default_budget();
        ctx.add_message(Message::system("system"));
        ctx.add_message(Message::user("run"));
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_ref = seen.clone();

        let out = agent
            .collect_stream_llm_response(&ctx, &[], None, "dummy-model", None, &move |chunk| {
                if let Some(delta) = chunk.delta {
                    if let Some(text) = delta.content {
                        seen_ref.lock().expect("seen lock").push(text);
                    }
                }
            })
            .await
            .expect("stream should complete");

        let StreamCollectOutcome::Complete(resp) = out else {
            panic!("expected complete response");
        };
        assert_eq!(resp.message.content.as_deref(), Some("Hello\n world"));
        let joined = seen.lock().expect("seen lock").join("");
        assert_eq!(joined, "Hello\n world");
        assert!(!joined.contains("secret"));
        assert!(!joined.contains("memory-context"));
    }

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
