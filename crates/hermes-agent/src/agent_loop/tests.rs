use super::*;
use crate::agent_config::is_stream_not_supported_error;
use crate::governor::{GovernorRuntimeState, TurnGovernor};
use crate::hooks::spill_hook_context_if_oversized;
use crate::llm_caller::{
    collect_stream_llm_response, session_disable_streaming, use_streaming_llm_transport,
};
use crate::message_sanitization::budget_pressure_text;
use crate::plugins::HookResult;
use crate::replay::{ReplayState, RouteLearningStats, redact_json_value};
use crate::route_learning::{
    now_unix_ms, route_learning_effective_stats, route_learning_state_path,
};
use crate::tool_executor::{
    coerce_textual_tool_calls, deduplicate_tool_calls, hydrate_session_search_args,
};
use futures::StreamExt;
use futures::stream::BoxStream;
use hermes_core::JsonSchema;
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_test_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("env test lock poisoned")
}

struct IsolatedHermesHome {
    _dir: tempfile::TempDir,
}

impl IsolatedHermesHome {
    fn new() -> Self {
        Self {
            _dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn path(&self) -> std::path::PathBuf {
        self._dir.path().to_path_buf()
    }
}

fn session_search_call(args: &str) -> ToolCall {
    ToolCall {
        id: "call_session".into(),
        function: hermes_core::FunctionCall {
            name: "session_search".into(),
            arguments: args.into(),
        },
        extra_content: None,
    }
}

#[test]
fn session_search_query_guard_detects_missing_query() {
    assert!(!session_search_has_query(&session_search_call("{}")));
    assert!(!session_search_has_query(&session_search_call(
        r#"{"query":"   "}"#
    )));
}

#[test]
fn session_search_query_guard_allows_concrete_query() {
    assert!(session_search_has_query(&session_search_call(
        r#"{"query":"上次的项目进展"}"#
    )));
}

#[test]
fn restore_primary_runtime_at_turn_start_after_fallback() {
    use crate::test_support::ErrNoopProvider as NoopProvider;

    let config = AgentConfig::default();
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(NoopProvider),
    );
    agent.activate_runtime_fallback(PrimaryRuntime {
        model: "anthropic/claude-sonnet-4".to_string(),
        provider: Some("openrouter".to_string()),
        base_url: None,
        api_mode: ApiMode::ChatCompletions,
        command: None,
        args: Vec::new(),
        credential_pool: None,
    });
    assert_eq!(
        crate::runtime_provider::active_model(&agent),
        "anthropic/claude-sonnet-4"
    );
    assert_eq!(agent.config().model, "anthropic/claude-sonnet-4");
    agent.restore_primary_runtime_at_turn_start();
    assert_eq!(crate::runtime_provider::active_model(&agent), "gpt-4o");
    assert_eq!(agent.config().model, "gpt-4o");
    assert!(
        !agent
            .state
            .lock()
            .expect("lock")
            .turn_fallback
            .is_fallback_activated()
    );
}

#[tokio::test]
async fn preprocess_user_message_context_references_expands_at_file() {
    let _guard = env_test_lock();
    let td = tempfile::tempdir().expect("tempdir");
    std::fs::write(td.path().join("note.txt"), "hello context\n").expect("write");
    let prev_cwd = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(td.path()).expect("chdir");

    use crate::test_support::ErrNoopProvider as NoopProvider;

    let loop_engine = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(NoopProvider),
    );
    let mut messages = vec![Message::user("summarize @file:note.txt")];
    loop_engine
        .preprocess_user_message_context_references(&mut messages)
        .await;

    std::env::set_current_dir(prev_cwd).expect("restore cwd");
    let content = messages[0].content.as_deref().expect("content");
    assert!(content.contains("Attached Context"));
    assert!(content.contains("hello context"));
}

#[test]
fn test_agent_config_default() {
    let config = AgentConfig::default();
    assert_eq!(config.max_turns, 250);
    assert_eq!(config.model, "gpt-4o");
    assert!(!config.stream);
    assert_eq!(config.max_concurrent_delegates, 1);
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
}

#[test]
fn delegation_spawning_paused_honors_env_toggle() {
    hermes_core::test_env::remove_var("HERMES_DELEGATION_PAUSED");
    assert!(!delegation_spawning_paused());
    hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "1");
    assert!(delegation_spawning_paused());
    hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "true");
    assert!(delegation_spawning_paused());
    hermes_core::test_env::set_var("HERMES_DELEGATION_PAUSED", "0");
    assert!(!delegation_spawning_paused());
}

#[test]
fn tool_enforcement_prompt_gate_matches_python_model_patterns() {
    assert!(should_inject_tool_enforcement_for_model("openai:gpt-5"));
    assert!(should_inject_tool_enforcement_for_model("xai:grok-4-fast"));
    assert!(should_inject_tool_enforcement_for_model("zhipu:glm-4.5"));
    assert!(!should_inject_tool_enforcement_for_model(
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
        preferred_tool_payload_fallback_model("openrouter", "openai/gpt-5.5"),
        None
    );
    hermes_core::test_env::set_var(
        "HERMES_TOOL_PAYLOAD_FALLBACK_MODEL",
        "nousresearch/hermes-4-405b",
    );
    assert_eq!(
        preferred_tool_payload_fallback_model("nous", "openai/gpt-5.5"),
        Some("nousresearch/hermes-4-405b".to_string())
    );
    hermes_core::test_env::remove_var("HERMES_TOOL_PAYLOAD_FALLBACK_MODEL");
}

#[test]
fn maybe_nous_401_diagnostic_returns_hint_for_nous_auth_failures() {
    let diag = maybe_nous_401_diagnostic(
        "nous",
        "HTTP 401 Unauthorized: token expired",
        Some("/tmp/hermes-home"),
    )
    .expect("nous 401 should produce diagnostics");
    assert!(diag.contains("Nous 401 - Portal authentication failed."));
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
    assert!(out.starts_with("\u{1F9E0} "));
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

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let prev = std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS").ok();
    hermes_core::test_env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", "1024");
    let tmp = tempfile::tempdir().expect("tempdir");
    let cfg = AgentConfig {
        hermes_home: Some(tmp.path().display().to_string()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let large = "x".repeat(2_048);
    let spilled = spill_hook_context_if_oversized(&agent, &large).expect("spill should write file");
    assert!(spilled.exists(), "spill file should exist");
    let read_back = std::fs::read_to_string(&spilled).expect("read spill file");
    assert_eq!(read_back.len(), large.len());
    assert!(
        spill_hook_context_if_oversized(&agent, "small payload").is_none(),
        "small payload must not spill"
    );
    if let Some(v) = prev {
        hermes_core::test_env::set_var("HERMES_HOOK_CONTEXT_SPILL_CHARS", v);
    } else {
        hermes_core::test_env::remove_var("HERMES_HOOK_CONTEXT_SPILL_CHARS");
    }
}

#[test]
fn post_llm_transform_hook_rewrites_assistant_content() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let mut content = Some("before".to_string());
    crate::hooks::apply_hook_output_transforms(
        &[HookResult::TransformLlmOutput("after".to_string())],
        &mut content,
    );
    assert_eq!(content.as_deref(), Some("after"));
}

#[test]
fn set_runtime_session_id_updates_config() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    agent.set_runtime_session_id("session-abc");
    assert_eq!(agent.runtime_session_id().as_deref(), Some("session-abc"));
}

#[tokio::test]
async fn compress_messages_short_transcript_is_noop() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let messages = vec![
        Message::system("sys"),
        Message::user("hi"),
        Message::assistant("hello"),
    ];
    let (out, compressed) = agent
        .compress_messages(messages.clone(), "sid-1", "gpt-4o")
        .await;
    assert!(!compressed);
    assert_eq!(out.len(), messages.len());
}

#[tokio::test]
async fn preflight_compression_status_reports_skipped_when_under_threshold() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
        Arc::new(DummyProvider::default()),
    )
    .with_callbacks(callbacks);
    let mut ctx = ContextManager::new(100);
    ctx.add_message(Message::user("small"));
    agent.preflight_context_compress_with_status(&mut ctx).await;

    let rows = captured.lock().expect("captured lock");
    assert!(
        rows.iter()
            .any(|(kind, msg)| { kind == "lifecycle" && msg.contains("no compression needed") })
    );
}

#[tokio::test]
async fn preflight_compression_status_reports_when_compressing() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
        Arc::new(DummyProvider::default()),
    )
    .with_callbacks(callbacks);
    let mut ctx = ContextManager::new(100);
    ctx.add_message(Message::user("x".repeat(95)));
    agent.preflight_context_compress_with_status(&mut ctx).await;

    let rows = captured.lock().expect("captured lock");
    assert!(rows.iter().any(|(kind, msg)| {
        kind == "lifecycle" && msg.contains("compressing before first turn")
    }));
    assert!(
        rows.iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("compression complete"))
    );
}

#[tokio::test]
async fn status_callback_receives_context_pressure_messages() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
        Arc::new(DummyProvider::default()),
    )
    .with_callbacks(callbacks);

    let mut ctx = ContextManager::new(100);
    ctx.add_message(Message::user("x".repeat(90)));
    agent.auto_compress_if_over_threshold(&mut ctx).await;

    let rows = captured.lock().expect("captured lock");
    assert!(
        rows.iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression"))
    );
}

#[tokio::test]
async fn call_llm_with_retry_strips_provider_prefix_for_primary_and_fallback_models() {
    use futures::stream::BoxStream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

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
                ..Default::default()
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

    let mut api_call_count = 0u32;
    let resp = agent
        .call_llm_with_retry_inner(&mut ctx, &[], None, None, &mut api_call_count)
        .await
        .expect("fallback should recover");
    assert_eq!(resp.message.content.as_deref(), Some("ok"));

    let seen = seen_models.lock().expect("seen model lock").clone();
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
                ..Default::default()
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
    {
        let _guard = env_test_lock();
        unsafe {
            std::env::remove_var("OPENROUTER_API_KEY");
        }
    }

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

    let mut api_call_count = 0u32;
    match agent
        .call_llm_with_retry_inner(&mut ctx, &[], None, None, &mut api_call_count)
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
                ..Default::default()
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
                ..Default::default()
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

        fn prefers_non_streaming_transport(&self) -> bool {
            true
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
        kind == "lifecycle" && msg.contains("Empty assistant response - retrying")
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
                ..Default::default()
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

        fn prefers_non_streaming_transport(&self) -> bool {
            true
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
        kind == "lifecycle" && msg.contains("Empty assistant response - retrying")
    }));
}

#[tokio::test]
async fn run_truncated_tool_call_retries_before_tool_execution() {
    use futures::stream::BoxStream;
    use hermes_core::{FunctionCall, ToolCall};

    #[derive(Default)]
    struct TruncatedThenOkProvider {
        calls: std::sync::Mutex<u32>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for TruncatedThenOkProvider {
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
            if *n == 1 {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant_with_tool_calls(
                        None,
                        vec![ToolCall {
                            id: "call_trunc".to_string(),
                            function: FunctionCall {
                                name: "echo".to_string(),
                                arguments: "{\"path\":\"/tmp/x\",".to_string(),
                            },
                            extra_content: None,
                        }],
                    ),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            } else {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
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

        fn prefers_non_streaming_transport(&self) -> bool {
            true
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

    let provider = Arc::new(TruncatedThenOkProvider::default());
    let mut registry = ToolRegistry::new();
    registry.register(
        "echo",
        hermes_core::tool_schema("echo", "Echo input", hermes_core::JsonSchema::new("object")),
        Arc::new(|_| Ok("{}".to_string())),
    );
    let cfg = AgentConfig {
        max_turns: 3,
        truncated_tool_call_max_retries: 1,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(registry), provider.clone()).with_callbacks(callbacks);

    let result = agent.run(vec![Message::user("hello")], None).await;
    assert!(result.is_ok());
    assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
    let rows = captured.lock().expect("captured lock");
    assert!(
        rows.iter()
            .any(|(kind, msg)| { kind == "lifecycle" && msg.contains("Truncated tool arguments") })
    );
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
        };

        futures::stream::iter(events).boxed()
    }
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
    let mut api_call_count = 0u32;

    let out = collect_stream_llm_response(
        &agent,
        &mut ctx,
        &[],
        None,
        "dummy-model",
        None,
        &move |chunk| {
            if let Some(delta) = chunk.delta {
                if let Some(text) = delta.content {
                    seen_ref.lock().expect("seen lock").push(text);
                }
            }
        },
        &mut api_call_count,
        None,
    )
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
async fn stream_mid_tool_call_exhausted_retries_returns_partial_stub() {
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
    let mut api_call_count = 0u32;

    let out = collect_stream_llm_response(
        &agent,
        &mut ctx,
        &[],
        None,
        "dummy-model",
        None,
        &move |chunk| {
            if let Some(delta) = chunk.delta {
                if let Some(text) = delta.content {
                    seen_ref.lock().expect("seen lock").push(text);
                }
            }
        },
        &mut api_call_count,
        None,
    )
    .await
    .expect("partial stub should recover instead of hard error");

    let StreamCollectOutcome::Complete(resp) = out else {
        panic!("expected complete partial-stream stub");
    };
    assert_eq!(
        resp.response_id.as_deref(),
        Some(hermes_core::PARTIAL_STREAM_STUB_ID)
    );
    assert_eq!(resp.finish_reason.as_deref(), Some("length"));
    assert!(
        resp.message
            .tool_calls
            .as_ref()
            .map_or(true, |calls| calls.is_empty())
    );
    assert_eq!(
        resp.dropped_tool_names.as_deref(),
        Some(["write_file".to_string()].as_slice())
    );
    let body = resp.message.content.as_deref().unwrap_or_default();
    assert!(body.contains("Working..."));
    assert!(body.contains("Stream stalled mid tool-call"));
    assert!(body.contains("write_file"));
    assert_eq!(*provider.calls.lock().expect("calls lock"), 2);
    assert!(seen.lock().expect("seen lock").iter().any(|text| {
        text.to_lowercase()
            .contains("connection dropped mid tool-call; reconnecting")
    }));
    assert!(
        seen.lock()
            .expect("seen lock")
            .iter()
            .any(|text| { text.contains("Stream stalled mid tool-call") })
    );
}

#[tokio::test]
async fn stream_text_only_drop_returns_partial_stub_without_retry() {
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
    let mut api_call_count = 0u32;

    let out = collect_stream_llm_response(
        &agent,
        &mut ctx,
        &[],
        None,
        "dummy-model",
        None,
        &move |chunk| {
            if let Some(delta) = chunk.delta {
                if let Some(text) = delta.content {
                    seen_ref.lock().expect("seen lock").push(text);
                }
            }
        },
        &mut api_call_count,
        None,
    )
    .await
    .expect("text-only partial stream should return stub");

    let StreamCollectOutcome::Complete(resp) = out else {
        panic!("expected partial-stream stub");
    };
    assert_eq!(
        resp.response_id.as_deref(),
        Some(hermes_core::PARTIAL_STREAM_STUB_ID)
    );
    assert_eq!(resp.finish_reason.as_deref(), Some("length"));
    assert_eq!(resp.message.content.as_deref(), Some("Partial text"));
    assert_eq!(*provider.calls.lock().expect("calls lock"), 1);
    assert!(!seen.lock().expect("seen lock").iter().any(|text| {
        text.to_lowercase()
            .contains("connection dropped mid tool-call; reconnecting")
    }));
    assert_eq!(
        crate::message_sanitization::continuation_prompt_for_response(&resp),
        crate::message_sanitization::get_continuation_prompt(true, None)
    );
}

#[tokio::test]
async fn quiet_mode_suppresses_status_callback() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    )
    .with_callbacks(callbacks);
    let mut ctx = ContextManager::new(100);
    ctx.add_message(Message::user("x".repeat(90)));
    agent.auto_compress_if_over_threshold(&mut ctx).await;

    assert!(captured.lock().expect("captured lock").is_empty());
}

#[test]
fn test_builtin_personality_injected_into_system_prompt() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        personality: Some("coder".to_string()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let prompt = agent.build_system_prompt("", &[], "gpt-4o");
    assert!(prompt.contains("## Active Personality (coder)"));
    assert!(prompt.contains("`coder` persona"));
}

#[test]
fn test_unknown_personality_name_does_not_add_overlay_block() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        personality: Some("unknown_persona".to_string()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let prompt = agent.build_system_prompt("", &[], "gpt-4o");
    assert!(!prompt.contains("## Active Personality (unknown_persona)"));
    assert!(prompt.contains("You are Hermes Agent"));
}

#[test]
fn test_default_personality_name_does_not_add_overlay_block() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        personality: Some("default".to_string()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let prompt = agent.build_system_prompt("", &[], "gpt-4o");
    assert!(!prompt.contains("## Active Personality (default)"));
    assert!(prompt.contains("You are Hermes Agent"));
}

#[test]
fn test_task_completion_guidance_default_injects_when_tools_present() {
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let tools = vec![ToolSchema::new(
        "terminal",
        "Execute commands",
        JsonSchema::new("object"),
    )];
    let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-opus-4.8");
    assert!(prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
}

#[test]
fn test_task_completion_guidance_false_disables_injection() {
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        task_completion_guidance: false,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let tools = vec![ToolSchema::new(
        "terminal",
        "Execute commands",
        JsonSchema::new("object"),
    )];
    let prompt = agent.build_system_prompt("", &tools, "anthropic/claude-opus-4.8");
    assert!(!prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
}

#[test]
fn test_task_completion_guidance_not_injected_without_tools() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let prompt = agent.build_system_prompt("", &[], "anthropic/claude-opus-4.8");
    assert!(!prompt.contains(crate::prompt_builder::TASK_COMPLETION_GUIDANCE));
}

#[test]
fn test_smart_model_routing_cheap_route_for_simple_turn() {
    use futures::stream::BoxStream;

    let home = IsolatedHermesHome::new();
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "openai".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        hermes_home: Some(home.path().to_string_lossy().into_owned()),
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let messages = vec![Message::user("帮我总结一下今天要做什么")];
    let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
    assert_eq!(
        selected.as_ref().map(|r| r.model.as_str()),
        Some("gpt-4o-mini")
    );
}

#[test]
fn test_smart_model_routing_online_learning_prefers_primary_when_cheap_unstable() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "openai".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );
    let config = AgentConfig {
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    if let Ok(mut m) = agent.router.route_learning.lock() {
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
    let selected = crate::route_learning::resolve_smart_runtime_route(
        &agent,
        &[Message::user("summarize today's work")],
    );
    assert!(
        selected.is_none(),
        "online learning should keep primary when cheap route is unstable"
    );
}

#[test]
fn test_route_learning_updates_after_outcomes() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentConfig::default();
    cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    crate::route_learning::update_route_learning(&agent, None, Some("openai:gpt-4o"), 2100, false);
    crate::route_learning::update_route_learning(&agent, None, Some("openai:gpt-4o"), 900, true);
    let snapshot =
        crate::route_learning::route_learning_snapshot(&agent, None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["enabled"], true);
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert_eq!(snapshot["stats"]["samples"], 2);
    assert!(snapshot["stats"]["success_rate"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(snapshot["stats"]["avg_latency_ms"].as_f64().unwrap_or(0.0) > 0.0);
}

#[test]
fn test_route_learning_persists_across_agent_restarts() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cfg = AgentConfig::default();
    cfg.hermes_home = Some(tmp.path().to_string_lossy().to_string());

    let agent = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    crate::route_learning::update_route_learning(&agent, None, Some("openai:gpt-4o"), 1200, true);
    let persisted_path = route_learning_state_path(&cfg);
    assert!(
        persisted_path.exists(),
        "route-learning state file must exist"
    );

    let reloaded = AgentLoop::new(
        cfg.clone(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let snapshot =
        crate::route_learning::route_learning_snapshot(&reloaded, None, Some("openai:gpt-4o"));
    assert_eq!(snapshot["key"], "openai:gpt-4o");
    assert!(snapshot["stats"]["samples"].as_u64().unwrap_or(0) >= 1);
}

#[test]
fn test_route_learning_malformed_file_is_safe_fallback() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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

    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let snapshot =
        crate::route_learning::route_learning_snapshot(&agent, None, Some("openai:gpt-4o"));
    assert!(
        snapshot["stats"].is_null(),
        "malformed state must fall back cleanly"
    );
}

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
        route_learning_effective_stats(&stale, now_ms).is_none(),
        "stale route entries must expire by ttl"
    );

    let recent = RouteLearningStats {
        samples: 10,
        success_rate: 0.20,
        avg_latency_ms: 4000.0,
        consecutive_failures: 4,
        updated_at_unix_ms: now_ms - (12 * 60 * 60 * 1000),
    };
    let adjusted =
        route_learning_effective_stats(&recent, now_ms).expect("recent entry should not expire");
    assert!(adjusted.success_rate > recent.success_rate);
    assert!(adjusted.avg_latency_ms < recent.avg_latency_ms);
    assert!(adjusted.samples <= recent.samples);
}

#[test]
fn test_runtime_provider_command_args_override_primary_acp_metadata() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "openai".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: Some("https://api.openai.com/v1".to_string()),
            command: Some("copilot-language-server".to_string()),
            args: vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string(),
            ],
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
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
        Arc::new(DummyProvider::default()),
    );
    let primary = crate::route_learning::primary_runtime_snapshot(&agent);
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

    let home = IsolatedHermesHome::new();
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "codex".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-test-key".to_string()),
            api_key_env: None,
            base_url: Some("https://api.openai.com/v1".to_string()),
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        hermes_home: Some(home.path().to_string_lossy().into_owned()),
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let messages = vec![Message::user("总结一下这个需求")];
    let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
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

    let home = IsolatedHermesHome::new();
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "qwen-oauth".to_string(),
        RuntimeProviderConfig {
            api_key: Some("sk-qwen-oauth".to_string()),
            api_key_env: None,
            base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        hermes_home: Some(home.path().to_string_lossy().into_owned()),
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let selected = crate::route_learning::resolve_smart_runtime_route(
        &agent,
        &[Message::user("给我一段简短总结")],
    );
    assert_eq!(
        selected.as_ref().and_then(|r| r.provider.as_deref()),
        Some("qwen-oauth")
    );
}

#[test]
fn test_runtime_provider_stepfun_build_supported() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "stepfun".to_string(),
        RuntimeProviderConfig {
            api_key: Some("stepfun-test-key".to_string()),
            api_key_env: None,
            base_url: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
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
        Arc::new(DummyProvider::default()),
    );

    let built = crate::runtime_provider::build_runtime_provider(
        &agent,
        "stepfun",
        "step-3.5-flash",
        None,
        None,
        None,
        None,
        None,
    );
    assert!(built.is_ok(), "stepfun runtime provider should build");
}

#[test]
fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
    use futures::stream::BoxStream;
    use std::time::{SystemTime, UNIX_EPOCH};

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
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
        Arc::new(DummyProvider::default()),
    );
    let selected = crate::route_learning::resolve_smart_runtime_route(
        &agent,
        &[Message::user("帮我总结这段话")],
    );
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

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
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
        Arc::new(DummyProvider::default()),
    );
    let resolved = agent.resolve_runtime_api_key("openai", None, None);
    assert_eq!(resolved.as_deref(), Some("openai-oauth-token"));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
fn test_self_evolution_skill_counter_ticks_each_iteration() {
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
    let agent = AgentLoop::new(
        config,
        Arc::new(registry),
        Arc::new(DummyProvider::default()),
    );
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let _ = rt
        .block_on(agent.run(vec![Message::user("hello")], None))
        .expect("agent run should succeed");

    let counters = &agent.state.lock().expect("counter lock").evolution_counters;
    assert_eq!(counters.iters_since_skill, 1);
}

#[test]
fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
    use futures::stream::BoxStream;
    use hermes_core::JsonSchema;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

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
        let agent = AgentLoop::new(
            config,
            Arc::new(registry),
            Arc::new(DummyProvider::default()),
        );
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        for _ in 0..case.runs {
            let _ = rt
                .block_on(agent.run(vec![Message::user("hello")], None))
                .expect("agent run should succeed");
        }
        let counters = &agent.state.lock().expect("counter lock").evolution_counters;
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
                ..Default::default()
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

    let counters = &agent.state.lock().expect("counter lock").evolution_counters;
    // Iteration #1 increments then skill_manage resets to 0.
    // Iteration #2 (final assistant turn) increments again to 1.
    // Python follows the same cadence because `_iters_since_skill += 1`
    // happens at each loop iteration before the tool/reset branch.
    assert_eq!(counters.iters_since_skill, 1);
}

#[test]
fn test_use_streaming_llm_transport_matches_python_gates() {
    use futures::stream::BoxStream;

    struct HealthCheckProvider;
    #[async_trait::async_trait]
    impl LlmProvider for HealthCheckProvider {
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
                message: Message::assistant("ok"),
                usage: None,
                model: "t".into(),
                finish_reason: Some("stop".into()),
                ..Default::default()
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

        fn prefers_non_streaming_transport(&self) -> bool {
            true
        }
    }

    struct OpenProvider;
    #[async_trait::async_trait]
    impl LlmProvider for OpenProvider {
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
                message: Message::assistant("ok"),
                usage: None,
                model: "t".into(),
                finish_reason: Some("stop".into()),
                ..Default::default()
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

    let registry = Arc::new(ToolRegistry::new());
    let mock_like = AgentLoop::new(
        AgentConfig::default(),
        registry.clone(),
        Arc::new(HealthCheckProvider),
    );
    assert!(!use_streaming_llm_transport(&mock_like, false, 0, None));
    assert!(use_streaming_llm_transport(&mock_like, true, 0, None));

    let open = AgentLoop::new(
        AgentConfig::default(),
        registry.clone(),
        Arc::new(OpenProvider),
    );
    assert!(use_streaming_llm_transport(&open, false, 0, None));

    let acp_cfg = AgentConfig {
        provider: Some("copilot-acp".to_string()),
        ..AgentConfig::default()
    };
    let acp = AgentLoop::new(acp_cfg, registry.clone(), Arc::new(OpenProvider));
    assert!(!use_streaming_llm_transport(&acp, true, 0, None));

    let acp_url_cfg = AgentConfig {
        provider: Some("custom".to_string()),
        runtime_providers: [(
            "custom".to_string(),
            RuntimeProviderConfig {
                base_url: Some("acp://copilot".to_string()),
                ..RuntimeProviderConfig::default()
            },
        )]
        .into_iter()
        .collect(),
        ..AgentConfig::default()
    };
    let acp_url = AgentLoop::new(acp_url_cfg, registry, Arc::new(OpenProvider));
    assert!(!use_streaming_llm_transport(&acp_url, true, 0, None));

    session_disable_streaming(&open);
    assert!(!use_streaming_llm_transport(&open, true, 0, None));
    assert!(!use_streaming_llm_transport(&open, true, 1, None));
}

#[test]
fn test_is_stream_not_supported_error_detects_provider_message() {
    let err = AgentError::LlmApi("Streaming is not supported for this model".into());
    assert!(is_stream_not_supported_error(&err));
    let transient = AgentError::LlmApi("connection reset".into());
    assert!(!is_stream_not_supported_error(&transient));
}

#[test]
fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "copilot-acp".to_string(),
        RuntimeProviderConfig {
            api_key: None,
            api_key_env: None,
            base_url: Some("acp://copilot".to_string()),
            command: Some("definitely-not-installed-copilot-cli".to_string()),
            args: vec!["--acp".to_string(), "--stdio".to_string()],
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let selected = crate::route_learning::resolve_smart_runtime_route(
        &agent,
        &[Message::user("帮我总结这段话")],
    );
    assert!(
        selected.is_none(),
        "missing ACP CLI should fail cheap-route and fall back"
    );
}

#[test]
fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
    use futures::stream::BoxStream;

    let home = IsolatedHermesHome::new();
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "copilot-acp".to_string(),
        RuntimeProviderConfig {
            api_key: None,
            api_key_env: None,
            base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
            command: Some("definitely-not-installed-copilot-cli".to_string()),
            args: vec!["--acp".to_string(), "--stdio".to_string()],
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
        model: "openai:gpt-4o".to_string(),
        hermes_home: Some(home.path().to_string_lossy().into_owned()),
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let selected = crate::route_learning::resolve_smart_runtime_route(
        &agent,
        &[Message::user("帮我总结这段话")],
    );
    assert_eq!(
        selected.as_ref().and_then(|r| r.provider.as_deref()),
        Some("copilot-acp")
    );
}

#[test]
fn test_smart_model_routing_skips_complex_turn() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
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
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
    let selected = crate::route_learning::resolve_smart_runtime_route(&agent, &messages);
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
    let deduped = deduplicate_tool_calls(&calls);
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
            arguments: r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#
                .into(),
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
            arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#.into(),
        },
        extra_content: None,
    };
    let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
    assert_eq!(event.0, "remove");
    assert_eq!(event.1, "memory");
    assert_eq!(event.2, "obsolete fact");
}

#[test]
fn test_hydrate_session_search_args_injects_current_session_id() {
    use futures::stream::BoxStream;
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        session_id: Some("sess-auto-1".into()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let mut tc = ToolCall {
        id: "s1".into(),
        function: hermes_core::FunctionCall {
            name: "session_search".into(),
            arguments: r#"{"query":"previous issue","limit":3}"#.into(),
        },
        extra_content: None,
    };
    hydrate_session_search_args(&agent, &mut tc);
    let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
    assert_eq!(
        args.get("current_session_id").and_then(|v| v.as_str()),
        Some("sess-auto-1")
    );
}

#[test]
fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
    use futures::stream::BoxStream;
    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig {
        session_id: Some("sess-outer".into()),
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let mut tc = ToolCall {
        id: "s2".into(),
        function: hermes_core::FunctionCall {
            name: "session_search".into(),
            arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
        },
        extra_content: None,
    };
    hydrate_session_search_args(&agent, &mut tc);
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

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider::default()));

    let max = agent.config().max_turns;
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
        cache_read_tokens: 10,
        estimated_cost: Some(0.01),
        ..Default::default()
    };
    let b = UsageStats {
        prompt_tokens: 200,
        completion_tokens: 100,
        total_tokens: 300,
        cache_read_tokens: 20,
        estimated_cost: Some(0.02),
        ..Default::default()
    };
    let merged = merge_usage(Some(a), &b);
    assert_eq!(merged.prompt_tokens, 300);
    assert_eq!(merged.completion_tokens, 150);
    assert_eq!(merged.total_tokens, 450);
    assert_eq!(merged.cache_read_tokens, 30);
    assert_eq!(merged.estimated_cost, Some(0.03));
}

#[test]
fn test_merge_usage_none() {
    let b = UsageStats {
        prompt_tokens: 200,
        completion_tokens: 100,
        total_tokens: 300,
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "qwen-oauth".to_string(),
        RuntimeProviderConfig {
            api_key: None,
            api_key_env: None,
            base_url: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: Some("https://cfg.example.com/token".to_string()),
            oauth_client_id: Some("cfg-client".to_string()),
            ..Default::default()
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
            command: None,
            args: Vec::new(),
            oauth_token_url: Some("https://cfg.example.com/custom-token".to_string()),
            oauth_client_id: Some("custom-client".to_string()),
            ..Default::default()
        },
    );

    let config = AgentConfig {
        runtime_providers,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );

    // Set conflicting env values - config must win.
    hermes_core::test_env::set_var("HERMES_QWEN_OAUTH_TOKEN_URL", "https://env.example.com/tok");
    hermes_core::test_env::set_var("HERMES_QWEN_OAUTH_CLIENT_ID", "env-client");

    let (token_url, client_id) = agent.oauth_refresh_config("qwen-oauth").unwrap();
    assert_eq!(token_url, "https://cfg.example.com/token");
    assert_eq!(client_id, "cfg-client");

    // Unknown-provider path still resolves when config centre supplies both.
    let (token_url, client_id) = agent.oauth_refresh_config("custom-oauth").unwrap();
    assert_eq!(token_url, "https://cfg.example.com/custom-token");
    assert_eq!(client_id, "custom-client");

    hermes_core::test_env::remove_var("HERMES_QWEN_OAUTH_TOKEN_URL");
    hermes_core::test_env::remove_var("HERMES_QWEN_OAUTH_CLIENT_ID");
}

#[test]
fn test_runtime_provider_api_key_env_is_resolved() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let mut runtime_providers = HashMap::new();
    runtime_providers.insert(
        "custom".to_string(),
        RuntimeProviderConfig {
            api_key: None,
            api_key_env: Some("MY_FALLBACK_KEY".to_string()),
            base_url: None,
            command: None,
            args: Vec::new(),
            oauth_token_url: None,
            oauth_client_id: None,
            ..Default::default()
        },
    );

    let config = AgentConfig {
        runtime_providers,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );

    hermes_core::test_env::set_var("MY_FALLBACK_KEY", "env-secret");
    let resolved = agent.resolve_runtime_api_key("custom", None, None);
    assert_eq!(resolved.as_deref(), Some("env-secret"));
    hermes_core::test_env::remove_var("MY_FALLBACK_KEY");
}

#[test]
fn test_runtime_provider_api_key_env_supports_anthropic_aliases_and_gemini_oauth() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );

    hermes_core::test_env::remove_var("ANTHROPIC_API_KEY");
    hermes_core::test_env::remove_var("ANTHROPIC_TOKEN");
    hermes_core::test_env::set_var("CLAUDE_CODE_OAUTH_TOKEN", "claude-code-token");
    assert_eq!(
        agent
            .resolve_runtime_api_key("anthropic", None, None)
            .as_deref(),
        Some("claude-code-token")
    );
    hermes_core::test_env::remove_var("CLAUDE_CODE_OAUTH_TOKEN");

    hermes_core::test_env::set_var("HERMES_GEMINI_OAUTH_API_KEY", "gemini-oauth-token");
    assert_eq!(
        agent
            .resolve_runtime_api_key("google-gemini-cli", None, None)
            .as_deref(),
        Some("gemini-oauth-token")
    );
    hermes_core::test_env::remove_var("HERMES_GEMINI_OAUTH_API_KEY");
}

#[test]
fn test_oauth_refresh_config_anthropic_defaults_available() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    hermes_core::test_env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
    hermes_core::test_env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");
    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let (token_url, client_id) = agent.oauth_refresh_config("anthropic").unwrap();
    assert_eq!(token_url, "https://console.anthropic.com/v1/oauth/token");
    assert_eq!(client_id, "9d1c250a-e61b-44d9-88ed-5944d1962f5e");
}

#[test]
fn test_oauth_refresh_config_openai_defaults_available() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    hermes_core::test_env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
    hermes_core::test_env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
    hermes_core::test_env::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
    hermes_core::test_env::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let (token_url, client_id) = agent.oauth_refresh_config("openai").unwrap();
    assert_eq!(token_url, "https://auth.openai.com/oauth/token");
    assert_eq!(client_id, "app_EMoamEEZ73f0CkXaXp7hrann");
}

#[test]
fn test_oauth_refresh_config_nous_defaults_available() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    hermes_core::test_env::remove_var("HERMES_NOUS_OAUTH_TOKEN_URL");
    hermes_core::test_env::remove_var("HERMES_NOUS_OAUTH_CLIENT_ID");
    hermes_core::test_env::remove_var("NOUS_PORTAL_BASE_URL");
    hermes_core::test_env::remove_var("NOUS_CLIENT_ID");
    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    let (token_url, client_id) = agent.oauth_refresh_config("nous").unwrap();
    assert_eq!(token_url, "https://portal.nousresearch.com/api/oauth/token");
    assert_eq!(client_id, "hermes-cli");
}

#[test]
fn test_runtime_provider_stepfun_env_key_and_base_url_defaults() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let config = AgentConfig::default();
    let agent = AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );

    hermes_core::test_env::remove_var("HERMES_STEPFUN_API_KEY");
    hermes_core::test_env::set_var("STEPFUN_API_KEY", "stepfun-secret");
    let resolved = agent.resolve_runtime_api_key("stepfun", None, None);
    assert_eq!(resolved.as_deref(), Some("stepfun-secret"));
    hermes_core::test_env::remove_var("STEPFUN_API_KEY");

    let base = crate::runtime_provider::resolve_runtime_base_url(&agent, "stepfun", None);
    assert_eq!(base.as_deref(), Some("https://api.stepfun.ai/step_plan/v1"));
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
fn test_reliability_guard_requires_sustained_tool_errors_or_multi_sample_latency() {
    let ctx = ContextManager::default_budget();
    let config = AgentConfig {
        max_tokens: Some(1200),
        ..AgentConfig::default()
    };
    let one_error_turn = GovernorRuntimeState {
        avg_llm_latency_ms: Some(1000.0),
        avg_tool_error_rate: 0.0,
        consecutive_error_turns: 1,
    };
    let gov_one = governor_for_turn(&config, &ctx, 0, Some(&one_error_turn));
    assert!(!should_apply_turn_reliability_guard(
        &one_error_turn,
        &gov_one,
        1
    ));

    let slow_single_sample = GovernorRuntimeState {
        avg_llm_latency_ms: Some(7000.0),
        avg_tool_error_rate: 0.0,
        consecutive_error_turns: 0,
    };
    let gov_slow = governor_for_turn(&config, &ctx, 0, Some(&slow_single_sample));
    assert!(!should_apply_turn_reliability_guard(
        &slow_single_sample,
        &gov_slow,
        1
    ));
    assert!(should_apply_turn_reliability_guard(
        &slow_single_sample,
        &gov_slow,
        2
    ));

    let two_error_turns = GovernorRuntimeState {
        avg_llm_latency_ms: Some(1000.0),
        avg_tool_error_rate: 0.0,
        consecutive_error_turns: 2,
    };
    let gov_two = governor_for_turn(&config, &ctx, 0, Some(&two_error_turns));
    assert!(should_apply_turn_reliability_guard(
        &two_error_turns,
        &gov_two,
        0
    ));
}

#[test]
fn test_resolve_reliability_degrade_model_does_not_hop_to_openai_by_default() {
    use futures::stream::BoxStream;

    type DummyProvider = crate::test_support::FixedAssistantProvider;

    let agent = AgentLoop::new(
        AgentConfig {
            provider: Some("anthropic".to_string()),
            model: "anthropic:claude-sonnet-4-20250514".to_string(),
            ..AgentConfig::default()
        },
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider::default()),
    );
    assert_eq!(
        crate::route_learning::resolve_reliability_degrade_model(
            &agent,
            "anthropic:claude-sonnet-4-20250514",
            None
        ),
        None
    );
}

#[test]
fn test_tool_loop_guard_trips_on_consecutive_full_failure_turns() {
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_ENABLED", "1");
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS", "3");
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS", "1");
    assert!(!should_trip_tool_loop_guard(2, 2, 2));
    assert!(should_trip_tool_loop_guard(3, 2, 2));
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_ENABLED");
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS");
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS");
}

#[test]
fn test_tool_loop_guard_ignores_partial_success_turns() {
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_ENABLED", "1");
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS", "2");
    hermes_core::test_env::set_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS", "1");
    assert!(!should_trip_tool_loop_guard(4, 3, 2));
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_ENABLED");
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MAX_CONSEC_ERROR_TURNS");
    hermes_core::test_env::remove_var("HERMES_TOOL_LOOP_GUARD_MIN_FAILED_CALLS");
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
    assert!(looks_like_tool_error_output(
        r#"{"stdout":"","stderr":"fail","exit_code":1}"#
    ));
    assert!(looks_like_tool_error_output(
        "command output\n[exit code: 7]"
    ));
    assert!(!looks_like_tool_error_output(
        "command output\n[exit code: 0]"
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
    assert!(hint.contains("scripts/agent_orchestration.py"));
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
    hermes_core::test_env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
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
    hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
}

#[test]
fn test_repo_review_tool_profile_off_mode_disables_filtering() {
    let _guard = env_test_lock();
    hermes_core::test_env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "off");
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
    hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
}

#[test]
fn test_repo_review_discovery_policy_trims_repeated_loops() {
    let _guard = env_test_lock();
    hermes_core::test_env::set_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "enforce");
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
    assert!(apply_repo_review_discovery_budget_policy(&mut second, &msgs, &mut state).is_none());
    let mut third = make_calls();
    let note = apply_repo_review_discovery_budget_policy(&mut third, &msgs, &mut state);
    assert!(note.is_some());
    assert!(third.len() < 3);
    hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE");
}

#[test]
fn test_repo_review_discovery_policy_advisory_keeps_calls() {
    let _guard = env_test_lock();
    hermes_core::test_env::set_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE", "advisory");
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
    hermes_core::test_env::remove_var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE");
}

#[test]
fn test_finalizer_output_quality_retry_detects_placeholders() {
    let templated =
        "**Title:** Example\n**Authors:** pack of authors\n(Full text available at [URL](URL))";
    assert!(finalizer_output_quality_requires_retry(templated, 0));
}

#[test]
fn test_finalizer_output_quality_retry_detects_duplicate_lines() {
    let duplicated = "- **Title:** Bayesian Learning for Dive State Prediction and Management\n\
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
    let (coerced, calls, parsed_textual) = coerce_textual_tool_calls(msg);
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
    let (coerced, calls, parsed_textual) = coerce_textual_tool_calls(msg);
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

#[test]
fn test_format_tool_progress_message_web_and_repeat() {
    let web = vec!["web_search".to_string()];
    assert!(format_tool_progress_message(3, &web, 1).contains("检索网络数据"));
    assert!(format_tool_progress_message(3, &web, 2).contains("仍在进行"));

    let local = vec!["todo".to_string()];
    assert!(format_tool_progress_message(1, &local, 1).contains("todo"));
}

#[test]
fn test_summarize_tool_failure_for_user_web_extract_403() {
    let msg = summarize_tool_failure_for_user(
        "web_extract",
        "HTTP 403 Forbidden when fetching 'https://zhuanlan.zhihu.com/p/1'. This site blocks automated access.",
    )
    .expect("expected user notice");
    assert!(msg.contains("拒绝自动抓取"));
}

#[test]
fn test_summarize_tool_failure_for_user_browser_cdp() {
    let msg = summarize_tool_failure_for_user(
        "browser_navigate",
        "Chrome CDP not reachable. Start Chrome with --remote-debugging-port=9222 or set HERMES_BROWSER_AUTO_START=1",
    )
    .expect("expected user notice");
    assert!(msg.contains("浏览器"));
}

#[test]
fn test_apply_web_tool_budget_caps_web_search_calls() {
    let _guard = env_test_lock();
    hermes_core::test_env::set_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", "2");
    let mut calls = vec![ToolCall {
        id: "s1".to_string(),
        function: hermes_core::FunctionCall {
            name: "web_search".to_string(),
            arguments: r#"{"query":"test"}"#.to_string(),
        },
        extra_content: None,
    }];
    let blocked = apply_web_tool_budget(&mut calls, 0, 2, 0, 1);
    assert_eq!(blocked.len(), 1);
    assert!(calls.is_empty());
    hermes_core::test_env::remove_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS");
}

#[test]
fn test_apply_web_tool_budget_includes_browser_navigate() {
    let mut calls = vec![ToolCall {
        id: "b1".to_string(),
        function: hermes_core::FunctionCall {
            name: "browser_navigate".to_string(),
            arguments: r#"{"url":"https://example.com"}"#.to_string(),
        },
        extra_content: None,
    }];
    let blocked = apply_web_tool_budget(&mut calls, 3, 0, 0, 1);
    assert_eq!(blocked.len(), 1);
    assert!(calls.is_empty());
}

/// Documents the turn-level API message cache contract used by
/// `conversation_loop` (`invalidate_turn_api_messages_cache` each inner iteration).
#[test]
fn turn_api_messages_cache_contract() {
    use crate::test_support::ErrNoopProvider as NoopProvider;

    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(NoopProvider),
    );

    let mut ctx = ContextManager::default_budget();
    ctx.add_message(Message::user("aaa"));
    let first = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
    let second = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
    assert!(
        Arc::ptr_eq(&first, &second),
        "same ctx should return cached Arc"
    );

    ctx.add_message(Message::assistant("draft"));
    let after_assistant = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
    assert!(
        !Arc::ptr_eq(&first, &after_assistant),
        "different ctx should return new Arc"
    );

    let _ = ctx.get_messages_mut().pop();
    let after_pop = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx);
    assert!(
        !after_pop.iter().any(|m| m.role == MessageRole::Assistant),
        "ctx invalidation should recompute after pop"
    );
    let mut ctx_inplace = ContextManager::default_budget();
    ctx_inplace.add_message(Message::user("aaa"));
    let before_edit = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
    ctx_inplace.get_messages_mut()[0].content = Some("xyz".to_string());
    let stale_hit = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
    assert!(
        Arc::ptr_eq(&before_edit, &stale_hit),
        "in-place mutation without invalidation must return stale cache hit"
    );
    agent.invalidate_turn_api_messages_cache();
    let after_invalidate = crate::llm_caller::build_turn_api_messages(&agent, &mut ctx_inplace);
    assert!(!Arc::ptr_eq(&before_edit, &after_invalidate));
    let user_text = after_invalidate
        .iter()
        .find(|m| m.role == MessageRole::User)
        .and_then(|m| m.content.as_deref());
    assert_eq!(user_text, Some("xyz"));
}
