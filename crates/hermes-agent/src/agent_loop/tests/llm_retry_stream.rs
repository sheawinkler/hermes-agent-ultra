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
        serde_json::from_str(include_str!("../../testdata/adapter_chaos_profiles.json"))
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
