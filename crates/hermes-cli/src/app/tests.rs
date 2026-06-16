use super::actors::AuthLane;
use super::provider::{
    NoBackendProvider, allow_no_api_key, clear_provider_cache, normalize_runtime_provider_name,
    provider_cache_key, resolve_provider_and_model, resolve_startup_model,
};
use super::quorum::{QUORUM_DEFAULT_VOTER_PASSES, QUORUM_HINT_PREFIX};
use super::*;
use crate::alpha_runtime::QuorumPolicy;
use crate::alpha_runtime::{
    load_quorum_policy, set_objective_contract_behavior_mode, set_quorum_policy,
    upsert_objective_contract,
};
use crate::test_env_lock;
use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::LlmProvider;
use hermes_gateway::tool_backends::ClarifyDispatcher;
use std::collections::HashMap;

fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    test_env_lock::lock()
}

fn build_minimal_test_app() -> App {
    let config = Arc::new(GatewayConfig::default());
    let tool_registry = Arc::new(ToolRegistry::new());
    let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
    let agent_config = build_agent_config(config.as_ref(), "openai:gpt-4o");
    let provider: Arc<dyn LlmProvider> = Arc::new(NoBackendProvider {
        model: "openai:gpt-4o".to_string(),
    });
    let agent_inner = hermes_agent::attach_agent_runtime(AgentLoop::new(
        agent_config,
        agent_tool_registry,
        provider,
    ))
    .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
    let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
        &agent_inner,
        hermes_home_dir(),
    ));
    let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

    let stream_handle_shared = Arc::new(StdMutex::new(None));
    App {
        state_root: hermes_home_dir(),
        core: AgentCore {
            config,
            agent,
            tool_registry,
            tool_schemas: Vec::new(),
            interrupt_controller: InterruptController::new(),
        },
        session: SessionState::new("test-session".to_string()),
        model: ModelState {
            current_model: "openai:gpt-4o".to_string(),
            current_personality: None,
        },
        stream: StreamState::new(stream_handle_shared, true),
        runtime: RuntimeFlags::new(),
        chrome: ChromeState::new(PetSettings::default()),
        acp: AcpState::new(),
        clarify_dispatcher: ClarifyDispatcher::new(),
        snapshot_gate: SnapshotPersistGate::new(),
        persist_lane: PersistLane::spawn(),
        auth_lane: AuthLane::spawn(),
    }
}

fn build_minimal_test_app_with_state_root(state_root: PathBuf) -> App {
    let mut app = build_minimal_test_app();
    app.state_root = state_root;
    app
}

#[test]
fn test_switch_model_updates_existing_session_db_row() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
    let persistence = SessionPersistence::new(tmp.path());
    persistence
        .persist_session(
            &app.session.session_id,
            &[hermes_core::Message::user("hello")],
            &mut hermes_agent::session_persistence::SessionFlushCursor::new(),
            Some("openai:gpt-4o"),
            Some("cli"),
            None,
            None,
        )
        .unwrap();

    app.switch_model("anthropic:claude-sonnet-4-6");

    assert_eq!(
        persistence
            .get_session_model(&app.session.session_id)
            .unwrap(),
        Some("anthropic:claude-sonnet-4-6".to_string())
    );
}

#[test]
fn test_undo_last_n_soft_rewinds_and_sets_prefill() {
    let tmp = tempfile::tempdir().unwrap();
    let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
    app.session.messages = vec![
        hermes_core::Message::system("sys"),
        hermes_core::Message::user("question 1"),
        hermes_core::Message::assistant("answer 1"),
        hermes_core::Message::user("question 2"),
        hermes_core::Message::assistant("answer 2"),
        hermes_core::Message::user("question 3"),
        hermes_core::Message::assistant("answer 3"),
    ];
    let persistence = SessionPersistence::new(tmp.path());
    persistence
        .persist_session(
            &app.session.session_id,
            &app.session.messages,
            &mut hermes_agent::session_persistence::SessionFlushCursor::new(),
            None,
            Some("cli"),
            None,
            None,
        )
        .unwrap();

    let prefill = app.undo_last_n(2).expect("undo");

    assert_eq!(prefill, "question 2");
    assert_eq!(
        app.take_pending_input_prefill().as_deref(),
        Some("question 2")
    );
    assert_eq!(
        app.session
            .messages
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["sys", "question 1", "answer 1"]
    );
    assert_eq!(
        persistence
            .load_session(&app.session.session_id)
            .unwrap()
            .len(),
        3
    );
    let recent = persistence
        .list_recent_user_messages(&app.session.session_id, 5)
        .unwrap();
    assert_eq!(
        recent
            .iter()
            .filter_map(|row| row.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["question 1"]
    );
}

#[test]
fn test_session_info_serialization() {
    let info = SessionInfo {
        session_id: "test-123".to_string(),
        model: "gpt-4o".to_string(),
        personality: Some("helpful".to_string()),
        message_count: 5,
        created_at: "2025-01-01T00:00:00Z".to_string(),
    };
    let json = serde_json::to_string(&info).unwrap();
    let back: SessionInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(back.session_id, "test-123");
    assert_eq!(back.model, "gpt-4o");
}

#[test]
fn test_collect_quorum_models_dedup_and_limit() {
    let policy = QuorumPolicy {
        enabled: true,
        voters: 3,
        models: vec![
            "nous:openai/gpt-5.5-pro".to_string(),
            "nous:openai/gpt-5.5-pro".to_string(),
            "nous:anthropic/claude-opus-4.7".to_string(),
            "nous:deepseek/deepseek-v4-pro".to_string(),
        ],
        mode: "balanced".to_string(),
        updated_at: "2026-01-01T00:00:00Z".to_string(),
    };
    let models = App::collect_quorum_models(&policy, "nous:openai/gpt-5.5-pro");
    assert_eq!(
        models,
        vec![
            "nous:openai/gpt-5.5-pro".to_string(),
            "nous:anthropic/claude-opus-4.7".to_string(),
            "nous:deepseek/deepseek-v4-pro".to_string()
        ]
    );
}

#[test]
fn test_provider_cache_key_changes_with_base_url() {
    let a = provider_cache_key("openai", "gpt-4o", Some("https://a.example/v1"), "k");
    let b = provider_cache_key("openai", "gpt-4o", Some("https://b.example/v1"), "k");
    assert_ne!(a, b);
}

#[test]
fn test_build_provider_reuses_cached_provider_instance() {
    let mut cfg = GatewayConfig::default();
    cfg.model = Some("openai:gpt-4o".to_string());
    cfg.llm_providers.insert(
        "openai".to_string(),
        LlmProviderConfig {
            api_key: Some("test-key".to_string()),
            ..Default::default()
        },
    );
    clear_provider_cache();
    let p1 = build_provider(&cfg, "openai:gpt-4o");
    let p2 = build_provider(&cfg, "openai:gpt-4o");
    assert!(Arc::ptr_eq(&p1, &p2));
}

#[test]
fn test_extract_last_assistant_output_prefers_non_empty_assistant_text() {
    let messages = vec![
        hermes_core::Message::user("hello"),
        hermes_core::Message::assistant(""),
        hermes_core::Message::assistant("final answer"),
    ];
    let output = App::extract_last_assistant_output(&messages);
    assert_eq!(output, "final answer");
}

#[test]
fn test_required_quorum_success_majority() {
    assert_eq!(App::required_quorum_success(1), 1);
    assert_eq!(App::required_quorum_success(2), 2);
    assert_eq!(App::required_quorum_success(3), 2);
    assert_eq!(App::required_quorum_success(4), 3);
    assert_eq!(App::required_quorum_success(5), 3);
}

#[test]
fn test_quorum_mode_armed_once_triggers_without_system_hint() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());

    let _ = set_quorum_policy(
        true,
        Some(3),
        Some(vec![
            "nous:openai/gpt-5.5-pro".to_string(),
            "nous:anthropic/claude-opus-4.7".to_string(),
        ]),
    )
    .expect("set quorum policy");
    let policy = load_quorum_policy().expect("load quorum policy");
    assert!(
        policy.enabled,
        "quorum policy should be enabled in test home"
    );

    let mut app = build_minimal_test_app();
    app.session.messages = vec![hermes_core::Message::user("run quorum now")];
    app.runtime.quorum_armed_once = true;
    let has_hint = app.session.messages.iter().any(|message| {
        message.role == hermes_core::MessageRole::System
            && message
                .content
                .as_deref()
                .unwrap_or_default()
                .starts_with(QUORUM_HINT_PREFIX)
    });
    let has_user_turn = app
        .session
        .messages
        .iter()
        .any(|m| m.role == hermes_core::MessageRole::User);

    assert!(
        app.quorum_mode_armed_for_turn().is_some(),
        "one-shot quorum arm should trigger fan-out without relying on stale system hints (enabled={}, armed_once={}, has_hint={}, has_user_turn={})",
        policy.enabled,
        app.runtime.quorum_armed_once,
        has_hint,
        has_user_turn
    );

    match prev_home {
        Some(v) => crate::env_vars::set_var("HERMES_HOME", v),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}

#[test]
fn test_clear_quorum_system_hints_inplace_preserves_other_system_messages() {
    let mut app = build_minimal_test_app();
    app.session.messages = vec![
        hermes_core::Message::system("[QUORUM_MODE] quorum armed"),
        hermes_core::Message::system("normal system context"),
        hermes_core::Message::user("hello"),
    ];

    app.clear_quorum_system_hints_inplace();

    assert_eq!(app.session.messages.len(), 2);
    assert!(app.session.messages.iter().all(|message| {
        !message
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[QUORUM_MODE] ")
    }));
    assert!(
        app.session
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("normal system context"))
    );
}

#[test]
fn test_run_agent_quorum_arm_persists_artifact_even_on_voter_failures() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());

    let _ = set_quorum_policy(
        true,
        Some(3),
        Some(vec![
            "openai:gpt-4o".to_string(),
            "anthropic:claude-3-5-sonnet".to_string(),
            "nous:openai/gpt-5.5-pro".to_string(),
        ]),
    )
    .expect("set quorum policy");

    let mut app = build_minimal_test_app();
    app.session.session_id = "quorum-test-session".to_string();
    app.session.messages = vec![hermes_core::Message::user(
        "no tools, just verify quorum fan-out branch",
    )];
    app.runtime.quorum_armed_once = true;

    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let result = runtime.block_on(app.run_agent());
    assert!(
        result.is_err(),
        "NoBackendProvider should fail voter inference, but quorum artifact must still persist"
    );

    let quorum_dir = app.state_root.join("quorum");
    let artifacts: Vec<_> = std::fs::read_dir(&quorum_dir)
        .expect("read quorum artifact dir")
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some("json"))
        .collect();
    assert!(
        !artifacts.is_empty(),
        "quorum run should write at least one artifact file"
    );
    let latest = artifacts
        .iter()
        .max_by_key(|entry| entry.metadata().and_then(|m| m.modified()).ok())
        .expect("latest quorum artifact");
    let raw = std::fs::read_to_string(latest.path()).expect("read quorum artifact");
    let doc: serde_json::Value = serde_json::from_str(&raw).expect("parse quorum artifact");
    assert_eq!(
        doc.get("session_id").and_then(|v| v.as_str()),
        Some("quorum-test-session")
    );
    assert!(
        doc.get("voters")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| !arr.is_empty()),
        "artifact should contain per-voter outcomes"
    );

    match prev_home {
        Some(v) => crate::env_vars::set_var("HERMES_HOME", v),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}

#[test]
fn test_persist_session_snapshot_writes_default_session_file() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());

    let mut app = build_minimal_test_app();
    app.session.session_id = "resume-test".to_string();
    app.session.messages = vec![
        hermes_core::Message::system("[SESSION_OBJECTIVE] Preserve context"),
        hermes_core::Message::user("hello"),
        hermes_core::Message::assistant("world"),
    ];

    let path = app
        .persist_session_snapshot(None)
        .expect("persist session snapshot");
    assert!(path.ends_with("resume-test.json"));
    assert!(path.exists());
    let content = std::fs::read_to_string(&path).expect("read snapshot");
    let value: serde_json::Value = serde_json::from_str(&content).expect("parse snapshot");
    assert_eq!(
        value
            .get("session_info")
            .and_then(|v| v.get("session_id"))
            .and_then(|v| v.as_str()),
        Some("resume-test")
    );
    assert_eq!(
        value
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|v| v.len()),
        Some(3)
    );

    match prev_home {
        Some(val) => crate::env_vars::set_var("HERMES_HOME", val),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}

#[test]
fn test_persist_session_snapshot_respects_app_state_root() {
    let _guard = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut app = build_minimal_test_app();
    app.state_root = tmp.path().join("custom-state-root");
    app.session.session_id = "state-root-test".to_string();
    app.session.messages = vec![hermes_core::Message::user("ping")];

    let path = app
        .persist_session_snapshot(None)
        .expect("persist session snapshot");
    assert_eq!(
        path,
        app.state_root.join("sessions").join("state-root-test.json")
    );
    assert!(path.exists());
}

#[test]
fn test_apply_agent_result_and_persist_writes_updated_messages() {
    let _guard = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut app = build_minimal_test_app();
    app.state_root = tmp.path().join("custom-state-root");
    app.session.session_id = "persist-after-run".to_string();

    let result = hermes_core::AgentResult {
        messages: vec![
            hermes_core::Message::user("hello"),
            hermes_core::Message::assistant("world"),
        ],
        finished_naturally: true,
        interrupted: false,
        total_turns: 1,
        ..Default::default()
    };

    app.apply_agent_result_and_persist(result)
        .expect("persist updated messages");

    let path = app
        .state_root
        .join("sessions")
        .join("persist-after-run.json");
    assert!(path.exists());
    let raw = std::fs::read_to_string(path).expect("read snapshot");
    let value: serde_json::Value = serde_json::from_str(&raw).expect("parse snapshot");
    assert_eq!(
        value
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.len()),
        Some(2)
    );
}

#[tokio::test]
async fn compress_conversation_context_rejects_short_transcript() {
    let _guard = env_test_lock();
    let mut app = build_minimal_test_app();
    app.session.messages = vec![
        hermes_core::Message::system("sys"),
        hermes_core::Message::user("hi"),
    ];
    let (pre, post, compressed) = app.compress_conversation_context().await.expect("compress");
    assert_eq!(pre, 2);
    assert_eq!(post, 2);
    assert!(!compressed);
}

#[tokio::test]
async fn test_new_session_persists_startup_stub_snapshot() {
    let _guard = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut app = build_minimal_test_app();
    app.state_root = tmp.path().join("custom-state-root");
    std::fs::create_dir_all(app.state_root.join("sessions")).expect("create sessions dir");
    let old_session_id = app.session.session_id.clone();

    app.new_session();

    assert_ne!(app.session.session_id, old_session_id);
    let snapshot_path = app
        .state_root
        .join("sessions")
        .join(format!("{}.json", app.session.session_id));
    assert!(snapshot_path.exists());

    let content = std::fs::read_to_string(&snapshot_path).expect("read snapshot");
    let value: serde_json::Value = serde_json::from_str(&content).expect("parse snapshot");
    assert_eq!(
        value
            .get("messages")
            .and_then(|v| v.as_array())
            .map(|arr| arr.len()),
        Some(0)
    );
}

#[test]
fn test_persist_session_snapshot_prunes_old_files_by_count_limit() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let prev_max_files = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_FILES").ok();
    let prev_max_total = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES").ok();
    let prev_min_free = std::env::var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", "2");
    crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", "999999999");
    crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", "0");

    let mut app = build_minimal_test_app();
    app.session.session_id = "snap-prune".to_string();
    app.session.messages = vec![hermes_core::Message::user("snapshot payload")];

    let p1 = app
        .persist_session_snapshot(Some("older-1"))
        .expect("persist snapshot 1");
    let p2 = app
        .persist_session_snapshot(Some("older-2"))
        .expect("persist snapshot 2");
    let p3 = app
        .persist_session_snapshot(Some("newest"))
        .expect("persist snapshot 3");
    assert!(!p1.exists(), "oldest snapshot should be pruned");
    assert!(p2.exists(), "middle snapshot should remain");
    assert!(p3.exists(), "newest snapshot should remain");

    let sessions_dir = app.state_root.join("sessions");
    let remaining: Vec<_> = std::fs::read_dir(&sessions_dir)
        .expect("read sessions dir")
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some("json"))
        .collect();
    assert_eq!(remaining.len(), 2, "snapshot file count should be capped");

    match prev_min_free {
        Some(v) => crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", v),
        None => crate::env_vars::remove_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES"),
    }
    match prev_max_total {
        Some(v) => crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", v),
        None => crate::env_vars::remove_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES"),
    }
    match prev_max_files {
        Some(v) => crate::env_vars::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", v),
        None => crate::env_vars::remove_var("HERMES_SESSION_SNAPSHOT_MAX_FILES"),
    }
    match prev_home {
        Some(v) => crate::env_vars::set_var("HERMES_HOME", v),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}

#[test]
fn test_apply_cli_runtime_overrides_applies_provider_to_prefixed_model() {
    let mut cfg = GatewayConfig::default();
    cfg.model = Some("openai:gpt-4o".to_string());
    let cli = Cli {
        command: None,
        verbose: false,
        config_dir: None,
        model: None,
        provider: Some("nous".to_string()),
        oneshot: None,
        allow_tools: false,
        personality: None,
        ignore_user_config: false,
        ignore_rules: false,
        accept_hooks: false,
    };

    apply_cli_runtime_overrides(&mut cfg, &cli);
    assert_eq!(cfg.model.as_deref(), Some("nous:gpt-4o"));
}

#[test]
fn test_apply_cli_runtime_overrides_applies_provider_to_bare_model() {
    let mut cfg = GatewayConfig::default();
    cfg.model = Some("moonshotai/kimi-k2.6".to_string());
    let cli = Cli {
        command: None,
        verbose: false,
        config_dir: None,
        model: None,
        provider: Some("anthropic".to_string()),
        oneshot: None,
        allow_tools: false,
        personality: None,
        ignore_user_config: false,
        ignore_rules: false,
        accept_hooks: false,
    };

    apply_cli_runtime_overrides(&mut cfg, &cli);
    assert_eq!(cfg.model.as_deref(), Some("anthropic:moonshotai/kimi-k2.6"));
}

#[test]
fn test_build_agent_config_maps_runtime_provider_api_key_env() {
    let mut cfg = GatewayConfig::default();
    let mut providers = HashMap::new();
    providers.insert(
        "custom".to_string(),
        LlmProviderConfig {
            api_key: None,
            api_key_env: Some("MY_FALLBACK_KEY".to_string()),
            ..LlmProviderConfig::default()
        },
    );
    cfg.llm_providers = providers;

    let agent_cfg = build_agent_config(&cfg, "custom:some-model");
    let runtime = agent_cfg
        .runtime_providers
        .get("custom")
        .expect("runtime provider should exist");
    assert_eq!(runtime.api_key_env.as_deref(), Some("MY_FALLBACK_KEY"));
}

#[test]
fn test_build_agent_config_forwards_provider_extra_body() {
    let mut cfg = GatewayConfig::default();
    cfg.llm_providers.insert(
        "nous".to_string(),
        LlmProviderConfig {
            extra_body: Some(serde_json::json!({
                "reasoning_effort": "high",
                "reasoning": { "effort": "high" }
            })),
            ..LlmProviderConfig::default()
        },
    );
    let agent_cfg = build_agent_config(&cfg, "nous:moonshotai/kimi-k2.6");
    assert_eq!(
        agent_cfg
            .extra_body
            .as_ref()
            .and_then(|body| body.get("reasoning_effort"))
            .and_then(|value| value.as_str()),
        Some("high")
    );
}

#[test]
fn test_build_agent_config_infers_provider_for_bare_model() {
    let mut cfg = GatewayConfig::default();
    cfg.model = Some("claude-opus-4-6".to_string());
    cfg.llm_providers.insert(
        "anthropic".to_string(),
        LlmProviderConfig {
            model: Some("claude-opus-4-6".to_string()),
            ..LlmProviderConfig::default()
        },
    );

    let agent_cfg = build_agent_config(&cfg, "claude-opus-4-6");
    assert_eq!(agent_cfg.provider.as_deref(), Some("anthropic"));
}

#[test]
fn test_build_agent_config_maps_failover_chain_from_env() {
    crate::env_vars::set_var(
        "HERMES_FALLBACK_MODELS",
        "nous:moonshotai/kimi-k2.6,openai:gpt-4o-mini",
    );
    crate::env_vars::remove_var("HERMES_FALLBACK_MODEL");
    let cfg = GatewayConfig::default();
    let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
    assert_eq!(
        agent_cfg.retry.fallback_model.as_deref(),
        Some("nous:moonshotai/kimi-k2.6")
    );
    assert_eq!(
        agent_cfg.retry.fallback_models,
        vec![
            "nous:moonshotai/kimi-k2.6".to_string(),
            "openai:gpt-4o-mini".to_string()
        ]
    );
    crate::env_vars::remove_var("HERMES_FALLBACK_MODELS");
}

#[test]
fn test_build_agent_config_maps_single_failover_model_from_env() {
    let _guard = env_test_lock();
    crate::env_vars::remove_var("HERMES_FALLBACK_MODELS");
    crate::env_vars::set_var("HERMES_FALLBACK_MODEL", "anthropic:claude-3-5-sonnet");
    let cfg = GatewayConfig::default();
    let agent_cfg = build_agent_config(&cfg, "nous:openai/gpt-5.5");
    assert_eq!(
        agent_cfg.retry.fallback_model.as_deref(),
        Some("anthropic:claude-3-5-sonnet")
    );
    assert_eq!(
        agent_cfg.retry.fallback_models,
        vec!["anthropic:claude-3-5-sonnet".to_string()]
    );
    crate::env_vars::remove_var("HERMES_FALLBACK_MODEL");
}

#[test]
fn test_resolve_provider_and_model_uses_single_provider_fallback() {
    let mut cfg = GatewayConfig::default();
    cfg.llm_providers
        .insert("stepfun".to_string(), LlmProviderConfig::default());
    let (provider, model) = resolve_provider_and_model(&cfg, "step-3.5-flash");
    assert_eq!(provider, "stepfun");
    assert_eq!(model, "step-3.5-flash");
}

#[test]
fn test_resolve_startup_model_prefers_provider_runtime_model_for_provider_slug() {
    let mut cfg = GatewayConfig::default();
    cfg.llm_providers.insert(
        "nous".to_string(),
        LlmProviderConfig {
            model: Some("moonshotai/kimi-k2.6".to_string()),
            ..LlmProviderConfig::default()
        },
    );
    let startup = resolve_startup_model(&cfg, "nous");
    assert_eq!(startup, "nous:moonshotai/kimi-k2.6");
}

#[test]
fn test_sync_runtime_model_env_sets_model_and_provider_values() {
    let mut cfg = GatewayConfig::default();
    cfg.llm_providers
        .insert("anthropic".to_string(), LlmProviderConfig::default());

    let keys = [
        "HERMES_MODEL",
        "HERMES_INFERENCE_MODEL",
        "HERMES_INFERENCE_PROVIDER",
        "HERMES_TUI_PROVIDER",
    ];
    for key in keys {
        crate::env_vars::remove_var(key);
    }
    crate::env_vars::set_var("HERMES_TUI_PROVIDER", "openai");

    sync_runtime_model_env(&cfg, "anthropic:claude-sonnet-4-6");

    assert_eq!(
        std::env::var("HERMES_MODEL").ok().as_deref(),
        Some("anthropic:claude-sonnet-4-6")
    );
    assert_eq!(
        std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
        Some("anthropic:claude-sonnet-4-6")
    );
    assert_eq!(
        std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
        Some("anthropic")
    );
    assert_eq!(
        std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
        Some("anthropic")
    );

    for key in keys {
        crate::env_vars::remove_var(key);
    }
}

#[test]
fn test_provider_api_key_from_env_supports_stepfun() {
    let hermes_var = "HERMES_STEPFUN_API_KEY";
    let stepfun_var = "STEPFUN_API_KEY";
    crate::env_vars::remove_var(hermes_var);
    crate::env_vars::remove_var(stepfun_var);

    crate::env_vars::set_var(stepfun_var, "stepfun-direct");
    assert_eq!(
        provider_api_key_from_env("stepfun").as_deref(),
        Some("stepfun-direct")
    );

    crate::env_vars::set_var(hermes_var, "stepfun-hermes");
    assert_eq!(
        provider_api_key_from_env("stepfun").as_deref(),
        Some("stepfun-hermes")
    );

    crate::env_vars::remove_var(hermes_var);
    crate::env_vars::remove_var(stepfun_var);
}

#[test]
fn test_provider_api_key_from_env_supports_openai_codex() {
    let var = "HERMES_OPENAI_CODEX_API_KEY";
    crate::env_vars::remove_var(var);
    crate::env_vars::set_var(var, "codex-oauth-token");
    assert_eq!(
        provider_api_key_from_env("openai-codex").as_deref(),
        Some("codex-oauth-token")
    );
    crate::env_vars::remove_var(var);
}

#[test]
fn test_provider_api_key_from_env_supports_anthropic_aliases() {
    let primary = "ANTHROPIC_API_KEY";
    let secondary = "ANTHROPIC_TOKEN";
    let tertiary = "CLAUDE_CODE_OAUTH_TOKEN";
    crate::env_vars::remove_var(primary);
    crate::env_vars::remove_var(secondary);
    crate::env_vars::remove_var(tertiary);

    crate::env_vars::set_var(tertiary, "claude-oauth-token");
    assert_eq!(
        provider_api_key_from_env("anthropic").as_deref(),
        Some("claude-oauth-token")
    );

    crate::env_vars::set_var(secondary, "anthropic-token");
    assert_eq!(
        provider_api_key_from_env("anthropic").as_deref(),
        Some("anthropic-token")
    );

    crate::env_vars::set_var(primary, "anthropic-api-key");
    assert_eq!(
        provider_api_key_from_env("anthropic").as_deref(),
        Some("anthropic-api-key")
    );

    crate::env_vars::remove_var(primary);
    crate::env_vars::remove_var(secondary);
    crate::env_vars::remove_var(tertiary);
}

#[test]
fn test_provider_api_key_from_env_supports_qwen_oauth() {
    let oauth_var = "HERMES_QWEN_OAUTH_API_KEY";
    let fallback_var = "DASHSCOPE_API_KEY";
    crate::env_vars::remove_var(oauth_var);
    crate::env_vars::remove_var(fallback_var);

    crate::env_vars::set_var(fallback_var, "dashscope-fallback");
    assert_eq!(
        provider_api_key_from_env("qwen-oauth").as_deref(),
        Some("dashscope-fallback")
    );

    crate::env_vars::set_var(oauth_var, "qwen-oauth-token");
    assert_eq!(
        provider_api_key_from_env("qwen-oauth").as_deref(),
        Some("qwen-oauth-token")
    );

    crate::env_vars::remove_var(oauth_var);
    crate::env_vars::remove_var(fallback_var);
}

#[test]
fn test_provider_api_key_from_env_supports_google_gemini_cli() {
    let var = "HERMES_GEMINI_OAUTH_API_KEY";
    crate::env_vars::remove_var(var);
    crate::env_vars::set_var(var, "google-gemini-oauth-token");
    assert_eq!(
        provider_api_key_from_env("google-gemini-cli").as_deref(),
        Some("google-gemini-oauth-token")
    );
    crate::env_vars::remove_var(var);
}

#[test]
fn test_provider_api_key_from_env_supports_extended_registry() {
    let checks = [
        ("AI_GATEWAY_API_KEY", "ai-gateway"),
        ("DEEPSEEK_API_KEY", "deepseek"),
        ("HF_TOKEN", "huggingface"),
        ("KILOCODE_API_KEY", "kilocode"),
        ("NVIDIA_API_KEY", "nvidia"),
        ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
        ("LLAMA_CPP_API_KEY", "llama-cpp"),
        ("VLLM_API_KEY", "vllm"),
        ("MLX_API_KEY", "mlx"),
        ("APPLE_ANE_API_KEY", "apple-ane"),
        ("SGLANG_API_KEY", "sglang"),
        ("TGI_API_KEY", "tgi"),
        ("OPENCODE_GO_API_KEY", "opencode-go"),
        ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
        ("XAI_API_KEY", "xai"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("GLM_API_KEY", "zai"),
    ];
    for (env_var, provider) in checks {
        crate::env_vars::remove_var(env_var);
        let expected = format!("token-for-{provider}");
        crate::env_vars::set_var(env_var, expected.clone());
        assert_eq!(
            provider_api_key_from_env(provider).as_deref(),
            Some(expected.as_str())
        );
        crate::env_vars::remove_var(env_var);
    }
}

#[test]
fn test_normalize_runtime_provider_name_covers_aliases() {
    assert_eq!(
        normalize_runtime_provider_name("gemini-cli"),
        "google-gemini-cli"
    );
    assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
    assert_eq!(
        normalize_runtime_provider_name("alibaba-coding-plan"),
        "qwen"
    );
    assert_eq!(normalize_runtime_provider_name("opencode"), "opencode-zen");
    assert_eq!(normalize_runtime_provider_name("ollama"), "ollama-local");
    assert_eq!(normalize_runtime_provider_name("llama.cpp"), "llama-cpp");
    assert_eq!(normalize_runtime_provider_name("ollvm"), "vllm");
    assert_eq!(normalize_runtime_provider_name("llvm"), "vllm");
    assert_eq!(normalize_runtime_provider_name("mlx-lm"), "mlx");
    assert_eq!(normalize_runtime_provider_name("ane"), "apple-ane");
}

#[test]
fn test_allow_no_api_key_for_local_backends_and_private_base_urls() {
    assert!(allow_no_api_key("ollama-local", "ollama-local", None));
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

#[test]
fn test_default_mouse_enabled_respects_env_override() {
    crate::env_vars::remove_var("HERMES_TUI_MOUSE");
    assert!(!default_mouse_enabled());

    crate::env_vars::set_var("HERMES_TUI_MOUSE", "off");
    assert!(!default_mouse_enabled());

    crate::env_vars::set_var("HERMES_TUI_MOUSE", "1");
    assert!(default_mouse_enabled());

    crate::env_vars::remove_var("HERMES_TUI_MOUSE");
}

#[test]
fn test_contextlattice_orchestrator_url_prefers_contextlattice_env_then_memmcp() {
    let _lock = env_test_lock();
    crate::env_vars::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
    crate::env_vars::remove_var("MEMMCP_ORCHESTRATOR_URL");
    assert_eq!(
        App::contextlattice_orchestrator_url(),
        "http://127.0.0.1:8075"
    );

    crate::env_vars::set_var("MEMMCP_ORCHESTRATOR_URL", "http://127.0.0.1:9999/");
    assert_eq!(
        App::contextlattice_orchestrator_url(),
        "http://127.0.0.1:9999"
    );

    crate::env_vars::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://127.0.0.1:7777/");
    assert_eq!(
        App::contextlattice_orchestrator_url(),
        "http://127.0.0.1:7777"
    );

    crate::env_vars::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
    crate::env_vars::remove_var("MEMMCP_ORCHESTRATOR_URL");
}

#[test]
fn test_build_inference_messages_injects_runtime_reformulation() {
    let _lock = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    crate::env_vars::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
    crate::env_vars::set_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK", "1");
    crate::env_vars::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
    crate::env_vars::set_var(
        "CONTEXTLATTICE_TOPIC_PATH",
        "runbooks/objective/test-objective",
    );
    let contract = upsert_objective_contract("Grow SOL with controlled risk", true).expect("obj");

    let mut app = build_minimal_test_app();
    app.session.messages.push(hermes_core::Message::user(
        "provide 3 more ideas with contextlattice being one",
    ));
    let (messages, injected) = app.build_inference_messages();
    assert!(injected);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, hermes_core::MessageRole::System);
    let injected_text = messages[0].content.as_deref().unwrap_or_default();
    assert!(injected_text.contains(App::RUNTIME_REFORMULATION_PREFIX));
    assert!(injected_text.contains("tool-profile(mode): focus"));
    assert!(injected_text.contains("contextlattice(topic): runbooks/objective/test-objective"));
    assert!(injected_text.contains(contract.id.as_str()));
    assert!(injected_text.contains("UNPROVEN/CONTRADICTORY"));
    assert!(injected_text.contains("execute at least one concrete action"));
    assert!(injected_text.contains("iterative objective momentum"));
    assert!(injected_text.contains("objective behavior directives:"));
    assert!(injected_text.contains("objective success criteria:"));
    assert!(injected_text.contains("objective loop protocol:"));
    assert!(injected_text.contains("user-request(routing-preview):"));
    assert!(injected_text.contains("full user request remains available as the next user message"));
    assert_eq!(messages[1].role, hermes_core::MessageRole::User);

    match prev_home {
        Some(val) => crate::env_vars::set_var("HERMES_HOME", val),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
    crate::env_vars::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
    crate::env_vars::remove_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK");
    crate::env_vars::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
    crate::env_vars::remove_var("CONTEXTLATTICE_TOPIC_PATH");
}

#[test]
fn test_runtime_reformulation_caps_long_prompt_preview_without_losing_user_message() {
    let _lock = env_test_lock();
    crate::env_vars::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
    crate::env_vars::set_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS", "48");

    let long_prompt =
        "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".repeat(12);
    let mut app = build_minimal_test_app();
    app.session
        .messages
        .push(hermes_core::Message::user(long_prompt.clone()));

    let (messages, injected) = app.build_inference_messages();
    assert!(injected);
    assert_eq!(messages.len(), 2);
    let injected_text = messages[0].content.as_deref().unwrap_or_default();
    assert!(injected_text.contains("user-request(routing-preview):"));
    assert!(injected_text.contains("preview truncated"));
    assert!(!injected_text.contains(&long_prompt));
    assert_eq!(
        messages[1].content.as_deref().unwrap_or_default(),
        long_prompt
    );

    crate::env_vars::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
    crate::env_vars::remove_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS");
}

#[test]
fn test_compose_quorum_messages_coalesces_systems_before_user_messages() {
    let messages = App::compose_quorum_messages(
        vec!["contract rules".to_string(), "voter prompt".to_string()],
        vec![
            hermes_core::Message::system("runtime reformulation"),
            hermes_core::Message::user("mission prompt"),
        ],
        Some("prior draft".to_string()),
    );

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, hermes_core::MessageRole::System);
    assert_eq!(messages[1].role, hermes_core::MessageRole::User);
    assert_eq!(messages[2].role, hermes_core::MessageRole::User);
    assert_eq!(messages[3].role, hermes_core::MessageRole::User);
    let system = messages[0].content.as_deref().unwrap_or_default();
    assert!(system.contains("runtime reformulation"));
    assert!(!system.contains("contract rules"));
    let control = messages[1].content.as_deref().unwrap_or_default();
    assert!(control.contains("[QUORUM_CONTROL]"));
    assert!(control.contains("contract rules"));
    assert!(control.contains("voter prompt"));
    assert_eq!(messages[2].content.as_deref(), Some("mission prompt"));
    assert_eq!(messages[3].content.as_deref(), Some("prior draft"));
}

#[test]
fn test_build_inference_messages_respects_reformulation_toggle_off() {
    let _lock = env_test_lock();
    crate::env_vars::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "off");
    let mut app = build_minimal_test_app();
    app.session
        .messages
        .push(hermes_core::Message::user("plain request"));
    let (messages, injected) = app.build_inference_messages();
    assert!(!injected);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].role, hermes_core::MessageRole::User);
    crate::env_vars::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
}

#[test]
fn test_looks_like_status_only_output_detects_defer_only_language() {
    assert!(App::looks_like_status_only_output(
        "I will proceed with investigation next. Let me know if you'd like me to continue."
    ));
    assert!(!App::looks_like_status_only_output(
        "Implemented patch in path=crates/hermes-cli/src/app.rs and verified with cargo test result: pass."
    ));
}

#[test]
fn test_should_force_objective_continuation_for_mission_status_only_turn() {
    let _lock = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    crate::env_vars::set_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER", "1");

    let mut app = build_minimal_test_app();
    app.session.messages.push(hermes_core::Message::user(
        "Proceed with objective and improve outcomes continuously.",
    ));
    upsert_objective_contract(
        "Run this assignment in perpetuity and continuously improve output quality",
        false,
    )
    .expect("set objective");
    set_objective_contract_behavior_mode("mission").expect("set mission mode");

    let baseline_len = app.session.messages.len();
    let mut result_messages = app.session.messages.clone();
    result_messages.push(hermes_core::Message::assistant(
        "I will proceed with the next steps and share updates shortly.",
    ));
    let result = hermes_core::AgentResult {
        messages: result_messages,
        finished_naturally: true,
        total_turns: 1,
        ..Default::default()
    };

    let reason = app.should_force_objective_continuation(&result, baseline_len);
    assert!(reason.is_some());

    match prev_home {
        Some(val) => crate::env_vars::set_var("HERMES_HOME", val),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
    crate::env_vars::remove_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER");
}

#[test]
fn test_pet_settings_normalization_clamps_and_rewrites_invalid_values() {
    let input = PetSettings {
        enabled: true,
        species: "unknown".to_string(),
        mood: "invalid".to_string(),
        dock: PetDock::Left,
        tick_ms: 10,
    };
    let normalized = input.normalized();
    assert!(normalized.enabled);
    assert_eq!(normalized.species, "boba");
    assert_eq!(normalized.mood, "ready");
    assert_eq!(normalized.dock, PetDock::Left);
    assert_eq!(normalized.tick_ms, 120);
}

#[test]
fn test_load_pet_settings_uses_persisted_file_if_present() {
    let _lock = env_test_lock();
    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    std::fs::write(
        tmp.path().join("pet.json"),
        r#"{"enabled":true,"species":"fox","mood":"hyped","dock":"left","tick_ms":180}"#,
    )
    .expect("write pet settings");
    let loaded = load_pet_settings();
    assert!(loaded.enabled);
    assert_eq!(loaded.species, "fox");
    assert_eq!(loaded.mood, "hyped");
    assert_eq!(loaded.dock, PetDock::Left);
    assert_eq!(loaded.tick_ms, 180);
    crate::env_vars::remove_var("HERMES_HOME");
}

#[test]
fn test_default_rtk_raw_mode_respects_env_override() {
    crate::env_vars::remove_var("HERMES_RTK_RAW");
    assert!(!default_rtk_raw_mode());

    crate::env_vars::set_var("HERMES_RTK_RAW", "on");
    assert!(default_rtk_raw_mode());

    crate::env_vars::set_var("HERMES_RTK_RAW", "0");
    assert!(!default_rtk_raw_mode());

    crate::env_vars::remove_var("HERMES_RTK_RAW");
}

#[test]
fn test_is_model_not_found_error_detects_provider_404_shape() {
    let err = AgentError::LlmApi(
        "API error 404 Not Found: model foo/bar not found in OpenRouter catalog".to_string(),
    );
    assert!(App::is_model_not_found_error(&err));
}

#[test]
fn test_is_model_not_found_error_ignores_non_catalog_errors() {
    let err = AgentError::LlmApi("Rate limit exceeded".to_string());
    assert!(!App::is_model_not_found_error(&err));
}

#[test]
fn test_is_provider_auth_or_session_error_detects_auth_failures() {
    let err = AgentError::LlmApi("HTTP 401 Unauthorized: token expired".to_string());
    assert!(App::is_provider_auth_or_session_error(&err));
    let non_auth = AgentError::LlmApi("API error 404 Not Found: model missing".to_string());
    assert!(!App::is_provider_auth_or_session_error(&non_auth));
    let provider_payload = AgentError::LlmApi(
            "API error 400 Bad Request: This request is not valid. Additional info: Provider returned error"
                .to_string(),
        );
    assert!(!App::is_provider_auth_or_session_error(&provider_payload));
}

#[test]
fn test_is_provider_tool_payload_error_detects_schema_rejections() {
    let generic_provider = AgentError::LlmApi(
            "API error 400 Bad Request: This request is not valid. Check the model name and other parameters. Additional info: Provider returned error"
                .to_string(),
        );
    assert!(!App::is_provider_tool_payload_error(&generic_provider));
    let no_choices = AgentError::LlmApi(
            "No choices in response (status=400; message=This request is not valid. Additional info: Provider returned error)"
                .to_string(),
        );
    assert!(!App::is_provider_tool_payload_error(&no_choices));
    let provider_tool = AgentError::LlmApi(
            "API error 400 Bad Request: tools request is not valid. Additional info: Provider returned error"
                .to_string(),
        );
    assert!(App::is_provider_tool_payload_error(&provider_tool));
    let invalid_tool = AgentError::LlmApi("tools schema is invalid".to_string());
    assert!(App::is_provider_tool_payload_error(&invalid_tool));
    let rate_limit = AgentError::LlmApi("HTTP 429 Too Many Requests".to_string());
    assert!(!App::is_provider_tool_payload_error(&rate_limit));
}

#[test]
fn test_quorum_zero_env_no_longer_means_unbounded() {
    let _lock = env_test_lock();
    crate::env_vars::remove_var("HERMES_QUORUM_VOTER_PASSES");
    assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
    crate::env_vars::set_var("HERMES_QUORUM_VOTER_PASSES", "0");
    assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
    crate::env_vars::set_var("HERMES_QUORUM_VOTER_PASSES", "max");
    assert_eq!(App::quorum_voter_passes(), 16);
    crate::env_vars::remove_var("HERMES_QUORUM_VOTER_PASSES");
}

#[test]
fn test_quorum_output_is_degraded_non_answer() {
    assert!(App::quorum_output_is_degraded_non_answer(
        "Objective delivery compromised; reverting to Hermes base model"
    ));
    assert!(App::quorum_output_is_degraded_non_answer(
        "I do not have access to tools in this environment"
    ));
    assert!(!App::quorum_output_is_degraded_non_answer(
        "Strategy table: edge hypothesis, required data, implementation delta"
    ));
}

#[test]
fn test_is_transient_retryable_error_detects_timeout_and_rate_limit() {
    let timeout = AgentError::LlmApi("request timed out while waiting for provider".to_string());
    let rate_limit = AgentError::LlmApi("HTTP 429 Too Many Requests".to_string());
    let model_missing = AgentError::LlmApi("API error 404 Not Found: model missing".to_string());
    assert!(App::is_transient_retryable_error(&timeout));
    assert!(App::is_transient_retryable_error(&rate_limit));
    assert!(!App::is_transient_retryable_error(&model_missing));
}

#[test]
fn test_auth_error_requires_nous_login_detects_missing_login_shape() {
    use super::auth_refresh::auth_error_requires_nous_login;
    let err = AgentError::AuthFailed(
        "Hermes is not logged into Nous Portal. Run `hermes auth nous`.".to_string(),
    );
    assert!(auth_error_requires_nous_login(&err));
    let unrelated = AgentError::AuthFailed("rate limited".to_string());
    assert!(!auth_error_requires_nous_login(&unrelated));
}

#[test]
fn test_auto_nous_reauth_toggle_defaults_on() {
    let _guard = env_test_lock();
    crate::env_vars::remove_var("HERMES_AUTO_NOUS_REAUTH");
    assert!(App::auto_nous_reauth_enabled());
    crate::env_vars::set_var("HERMES_AUTO_NOUS_REAUTH", "0");
    assert!(!App::auto_nous_reauth_enabled());
    crate::env_vars::remove_var("HERMES_AUTO_NOUS_REAUTH");
}

#[test]
fn test_rank_catalog_candidates_prefers_syntactic_nearest() {
    let catalog = vec![
        "qwen/qwen3.6-plus".to_string(),
        "qwen/qwen3.6-max-preview".to_string(),
        "deepseek/deepseek-r1".to_string(),
    ];
    let ranked = App::rank_catalog_candidates("qwen3.6-max", &catalog, 2);
    assert!(!ranked.is_empty());
    assert_eq!(ranked[0], "qwen/qwen3.6-max-preview");
}

#[test]
fn test_resolve_quorum_catalog_candidate_uses_relative_match_when_exact_missing() {
    let catalog = vec![
        "moonshotai/kimi-k2.6".to_string(),
        "qwen/qwen3.6-max-preview".to_string(),
    ];
    let resolved = App::resolve_quorum_catalog_candidate("qwen3.6-max", &catalog);
    assert_eq!(resolved.as_deref(), Some("qwen/qwen3.6-max-preview"));
}

#[test]
fn test_resolve_quorum_catalog_candidate_preserves_version_pinned_miss() {
    let catalog = vec![
        "openai/gpt-5.5-pro".to_string(),
        "anthropic/claude-opus-4.7".to_string(),
        "qwen/qwen3.6-max-preview".to_string(),
    ];

    let gpt = App::resolve_quorum_catalog_candidate("openai/gpt-5.5-pro-20260423", &catalog);
    let claude =
        App::resolve_quorum_catalog_candidate("anthropic/claude-4.7-opus-fast-20260512", &catalog);
    let qwen = App::resolve_quorum_catalog_candidate("qwen/qwen3.6-max-preview-20260420", &catalog);

    assert!(
        gpt.is_none(),
        "version-pinned GPT ID should not fuzzy-remap"
    );
    assert!(
        claude.is_none(),
        "version-pinned Claude ID should not fuzzy-remap"
    );
    assert!(
        qwen.is_none(),
        "version-pinned Qwen ID should not fuzzy-remap"
    );
}

#[test]
fn test_set_session_objective_injects_replaces_and_clears_system_message() {
    let mut app = build_minimal_test_app();
    app.session
        .messages
        .push(hermes_core::Message::user("hello before objective"));

    app.set_session_objective(Some(
        "Ship parity with upstream plus stronger UX".to_string(),
    ));
    assert_eq!(
        app.session.session_objective.as_deref(),
        Some("Ship parity with upstream plus stronger UX")
    );
    assert_eq!(app.session.messages.len(), 2);
    assert_eq!(
        app.session.messages[0].role,
        hermes_core::MessageRole::System
    );
    let system = app.session.messages[0].content.clone().unwrap_or_default();
    assert!(system.starts_with("[SESSION_OBJECTIVE] "));
    assert!(system.contains("Ship parity with upstream plus stronger UX"));

    app.set_session_objective(Some("Minimize latency regressions".to_string()));
    let system_count = app
        .session
        .messages
        .iter()
        .filter(|m| {
            m.role == hermes_core::MessageRole::System
                && m.content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with("[SESSION_OBJECTIVE] ")
        })
        .count();
    assert_eq!(system_count, 1);
    assert_eq!(
        app.session.session_objective.as_deref(),
        Some("Minimize latency regressions")
    );

    app.set_session_objective(None);
    assert!(app.session.session_objective.is_none());
    assert!(app.session.messages.iter().all(|m| {
        !m.content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[SESSION_OBJECTIVE] ")
    }));
}

#[test]
fn test_objective_context_autopin_sets_topic_for_default_path() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let prev_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok();
    let prev_toggle = std::env::var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN").ok();

    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    crate::env_vars::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/hermes");
    crate::env_vars::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", "1");

    let contract = upsert_objective_contract("grow wallet safely", true).expect("objective");
    let app = build_minimal_test_app();
    app.maybe_autopin_contextlattice_topic_from_objective();
    let expected = format!("runbooks/objective/{}", contract.id);
    assert_eq!(
        std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
        Some(expected.as_str())
    );

    match prev_toggle {
        Some(v) => crate::env_vars::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", v),
        None => crate::env_vars::remove_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN"),
    }
    match prev_topic {
        Some(v) => crate::env_vars::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
        None => crate::env_vars::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
    }
    match prev_home {
        Some(v) => crate::env_vars::set_var("HERMES_HOME", v),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}

#[test]
fn app_implements_slash_command_host_trait_bundle() {
    fn assert_host<T: traits::SlashCommandHost>() {}
    assert_host::<App>();
}

#[test]
fn test_objective_context_autopin_respects_custom_topic_pin() {
    let _guard = env_test_lock();
    let prev_home = std::env::var("HERMES_HOME").ok();
    let prev_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok();

    let tmp = tempfile::tempdir().expect("tempdir");
    crate::env_vars::set_var("HERMES_HOME", tmp.path());
    crate::env_vars::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/custom/keep-me");

    let _contract =
        upsert_objective_contract("objective override regression test", false).expect("obj");
    let app = build_minimal_test_app();
    app.maybe_autopin_contextlattice_topic_from_objective();
    assert_eq!(
        std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
        Some("runbooks/custom/keep-me")
    );

    match prev_topic {
        Some(v) => crate::env_vars::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
        None => crate::env_vars::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
    }
    match prev_home {
        Some(v) => crate::env_vars::set_var("HERMES_HOME", v),
        None => crate::env_vars::remove_var("HERMES_HOME"),
    }
}
