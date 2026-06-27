#[cfg(test)]
mod tests {
    use super::*;
    use crate::alpha_runtime::{
        load_quorum_policy, set_objective_contract_behavior_mode, set_quorum_policy,
        upsert_objective_contract,
    };
    use crate::test_env_lock;
    use hermes_agent::plugins::{HookResult, Plugin, PluginContext, PluginManager};
    use hermes_config::LlmProviderConfig;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    fn block_on_test<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    struct TestToolHandler {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl hermes_core::ToolHandler for TestToolHandler {
        async fn execute(&self, _params: Value) -> Result<String, hermes_core::ToolError> {
            Ok(format!("{} ok", self.name))
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema::new(
                self.name,
                "test tool",
                hermes_core::JsonSchema::new("object"),
            )
        }
    }

    fn register_test_tool_in_toolset(
        tools: &ToolRegistry,
        name: &'static str,
        toolset: &'static str,
    ) {
        let handler: Arc<dyn hermes_core::ToolHandler> = Arc::new(TestToolHandler { name });
        tools.register(
            name,
            toolset,
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "test tool",
            "T",
            None,
        );
    }

    fn register_test_tool(tools: &ToolRegistry, name: &'static str) {
        register_test_tool_in_toolset(tools, name, "test");
    }

    fn build_minimal_test_app() -> App {
        build_minimal_test_app_with_state_root(hermes_home_dir())
    }

    fn build_minimal_test_app_with_state_root(state_root: PathBuf) -> App {
        let config = Arc::new(GatewayConfig::default());
        let tool_registry = Arc::new(ToolRegistry::new());
        let cron_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_dir).expect("create cron test dir");
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            config.as_ref(),
            "dynamic",
            cron_dir,
            &tool_registry,
        ));
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let session_id = "test-session".to_string();
        let mut agent_config = build_agent_config(config.as_ref(), "dynamic");
        agent_config.session_id = Some(session_id.clone());
        let provider: Arc<dyn LlmProvider> = Arc::new(hermes_provider_runtime::NoBackendProvider {
            model: "dynamic".to_string(),
        });
        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        App {
            state_root,
            config,
            agent,
            tool_registry,
            cron_scheduler,
            tool_schemas: Vec::new(),
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id,
            running: true,
            current_model: "dynamic".to_string(),
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
            current_personality: None,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared: Arc::new(StdMutex::new(None)),
            mouse_enabled: true,
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pending_input_prefill: None,
            pending_system_notes: Vec::new(),
            quorum_armed_once: false,
            pet_settings: PetSettings::default(),
            fail_model_rebuild_for: None,
        }
    }

    #[test]
    fn refresh_agent_tool_snapshot_adds_late_registered_tool() {
        let mut app = build_minimal_test_app();

        assert!(app.agent.tool_registry.get("mcp_srv_late").is_none());
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_late"));

        register_test_tool(&app.tool_registry, "mcp_srv_late");
        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 1);
        assert_eq!(refresh.added, vec!["mcp_srv_late".to_string()]);
        assert!(refresh.removed.is_empty());
        assert!(app.agent.tool_registry.get("mcp_srv_late").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_late"));
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );
    }

    #[test]
    fn refresh_agent_tool_snapshot_detects_equal_size_replacement() {
        let mut app = build_minimal_test_app();
        register_test_tool(&app.tool_registry, "mcp_srv_old");
        let first_refresh = app.refresh_agent_tool_snapshot();
        assert_eq!(first_refresh.added, vec!["mcp_srv_old".to_string()]);
        assert!(app.agent.tool_registry.get("mcp_srv_old").is_some());

        assert!(app.tool_registry.deregister("mcp_srv_old"));
        register_test_tool(&app.tool_registry, "mcp_srv_new");
        let second_refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(second_refresh.before_count, 1);
        assert_eq!(second_refresh.after_count, 1);
        assert_eq!(second_refresh.added, vec!["mcp_srv_new".to_string()]);
        assert_eq!(second_refresh.removed, vec!["mcp_srv_old".to_string()]);
        assert!(app.agent.tool_registry.get("mcp_srv_old").is_none());
        assert!(app.agent.tool_registry.get("mcp_srv_new").is_some());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_new"));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_old"));
    }

    #[test]
    fn refresh_agent_tool_snapshot_reports_no_changes_when_current() {
        let mut app = build_minimal_test_app();
        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 0);
        assert!(!refresh.changed());
    }

    #[test]
    fn refresh_agent_tool_snapshot_reports_advertised_surface_only() {
        let mut app = build_minimal_test_app();
        Arc::make_mut(&mut app.config)
            .platform_toolsets
            .insert("cli".to_string(), vec!["test".to_string()]);
        register_test_tool(&app.tool_registry, "allowed_tool");
        register_test_tool_in_toolset(&app.tool_registry, "mcp_srv_hidden", "mcp-srv");

        let refresh = app.refresh_agent_tool_snapshot();

        assert_eq!(refresh.before_count, 0);
        assert_eq!(refresh.after_count, 1);
        assert_eq!(refresh.added, vec!["allowed_tool".to_string()]);
        assert!(refresh.removed.is_empty());
        assert!(app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "allowed_tool"));
        assert!(!app
            .tool_schemas
            .iter()
            .any(|schema| schema.name == "mcp_srv_hidden"));
    }

    #[test]
    fn test_switch_model_updates_existing_session_db_row() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        app.switch_model("anthropic:claude-sonnet-4-6");

        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some("anthropic:claude-sonnet-4-6".to_string())
        );
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn try_switch_model_failure_is_noop_for_session_state() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        let old_model = "anthropic:claude-sonnet-4-6";
        app.try_switch_model(old_model).expect("baseline switch");
        let old_agent_model = app.agent.config.model.clone();
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some(old_model.to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some(old_model)
        );

        let broken_model = "openrouter:zai/glm-5.2";
        app.force_model_rebuild_failure_for_test(broken_model);
        let err = app
            .try_switch_model(broken_model)
            .expect_err("forced rebuild failure");

        assert!(err.to_string().contains("test forced rebuild failure"));
        assert_eq!(app.current_model, old_model);
        assert_eq!(app.agent.config.model, old_agent_model);
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some(old_model.to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some(old_model)
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some(old_model)
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn moa_virtual_model_switch_updates_session_without_rebuilding_agent() {
        let _guard = env_test_lock();
        let env_keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let saved_env: Vec<(&str, Option<String>)> = env_keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();

        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        let original_agent_model = app.agent.config.model.clone();
        let persistence = SessionPersistence::new(tmp.path());
        persistence
            .persist_session(
                &app.session_id,
                &[hermes_core::Message::user("hello")],
                Some(&app.current_model),
                Some("cli"),
                None,
                None,
            )
            .unwrap();

        app.try_switch_model("mixture-of-agents:default")
            .expect("virtual moa switch");

        assert_eq!(app.current_model, "moa:default");
        assert_eq!(
            app.agent.config.model, original_agent_model,
            "virtual model selection should not rebuild the current concrete agent"
        );
        assert_eq!(
            persistence.get_session_model(&app.session_id).unwrap(),
            Some("moa:default".to_string())
        );
        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("moa:default")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("moa")
        );

        let err = app
            .try_switch_model("moa:unknown")
            .expect_err("unsupported preset should fail closed");
        assert!(err.to_string().contains("unsupported MoA preset"));
        assert_eq!(app.current_model, "moa:default");

        for (key, value) in saved_env {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn model_switch_preflight_warning_is_ui_only_and_keeps_messages_clean() {
        let mut app = build_minimal_test_app();
        app.current_model = "anthropic:claude-sonnet-4-6".to_string();
        app.messages = vec![hermes_core::Message::user("abcd".repeat(90_000))];

        let warning = app
            .model_switch_preflight_warning("deepseek-chat")
            .expect("large transcript should warn");

        assert!(warning.contains("Context warning"));
        assert!(warning.contains("preflight compression"));
        assert_eq!(
            app.messages.len(),
            1,
            "warning must not mutate model context"
        );
        assert!(
            app.ui_messages.is_empty(),
            "warning calculation must not add UI transcript rows"
        );
    }

    #[test]
    fn input_history_prev_next_restores_empty_draft_state() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.input_history = vec![
            "first prompt".to_string(),
            "second prompt".to_string(),
            "third prompt".to_string(),
        ];
        app.history_index = app.input_history.len();

        assert_eq!(app.history_prev(), Some("third prompt"));
        assert_eq!(app.history_prev(), Some("second prompt"));
        assert_eq!(app.history_prev(), Some("first prompt"));
        assert_eq!(app.history_prev(), None);

        assert_eq!(app.history_next(), Some("second prompt"));
        assert_eq!(app.history_next(), Some("third prompt"));
        assert_eq!(app.history_next(), None);
        assert_eq!(app.history_index, app.input_history.len());
        assert_eq!(app.history_next(), None);
    }

    #[test]
    fn composer_drafts_persist_per_session_without_touching_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.session_id = "session-a".to_string();

        app.persist_current_composer_draft("alpha draft").unwrap();
        app.session_id = "session-b".to_string();
        app.persist_current_composer_draft("beta draft").unwrap();

        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("beta draft")
        );
        app.session_id = "session-a".to_string();
        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("alpha draft")
        );

        app.clear_current_composer_draft().unwrap();
        assert_eq!(app.load_current_composer_draft().unwrap(), None);
        app.session_id = "session-b".to_string();
        assert_eq!(
            app.load_current_composer_draft().unwrap().as_deref(),
            Some("beta draft")
        );

        let persistence = SessionPersistence::new(tmp.path());
        assert!(persistence.load_session("session-a").unwrap().is_empty());
        assert!(persistence.load_session("session-b").unwrap().is_empty());
    }

    #[test]
    fn composer_drafts_keep_mru_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());

        for idx in 0..(MAX_COMPOSER_DRAFTS + 3) {
            app.session_id = format!("session-{idx}");
            app.persist_current_composer_draft(&format!("draft-{idx}"))
                .unwrap();
        }

        let raw = std::fs::read_to_string(app.composer_drafts_path()).unwrap();
        let store: ComposerDraftStore = serde_json::from_str(&raw).unwrap();
        assert_eq!(store.drafts.len(), MAX_COMPOSER_DRAFTS);
        assert_eq!(store.drafts.first().unwrap().session_id, "session-3");
        assert_eq!(
            store.drafts.last().unwrap().session_id,
            format!("session-{}", MAX_COMPOSER_DRAFTS + 2)
        );
    }

    #[test]
    fn test_undo_last_n_soft_rewinds_and_sets_prefill() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = build_minimal_test_app_with_state_root(tmp.path().to_path_buf());
        app.messages = vec![
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
                &app.session_id,
                &app.messages,
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
            app.messages
                .iter()
                .filter_map(|m| m.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["sys", "question 1", "answer 1"]
        );
        assert_eq!(persistence.load_session(&app.session_id).unwrap().len(), 3);
        let recent = persistence
            .list_recent_user_messages(&app.session_id, 5)
            .unwrap();
        assert_eq!(
            recent
                .iter()
                .filter_map(|row| row.content.as_deref())
                .collect::<Vec<_>>(),
            vec!["question 1"]
        );
    }

    struct LifecycleHookPlugin {
        seen: Arc<StdMutex<Vec<(String, String)>>>,
    }

    #[async_trait::async_trait]
    impl Plugin for LifecycleHookPlugin {
        fn meta(&self) -> hermes_agent::plugins::PluginMeta {
            hermes_agent::plugins::PluginMeta {
                name: "lifecycle-recorder".to_string(),
                version: "0.1.0".to_string(),
                description: "Lifecycle recorder".to_string(),
                author: None,
            }
        }

        async fn initialize(&self) -> Result<(), AgentError> {
            Ok(())
        }

        async fn shutdown(&self) -> Result<(), AgentError> {
            Ok(())
        }

        fn register(&self, ctx: &mut PluginContext) {
            let finalize_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionFinalize,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    finalize_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_finalize".to_string(), session_id));
                    HookResult::Ok
                }),
            );
            let reset_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionReset,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    reset_seen
                        .lock()
                        .unwrap()
                        .push(("on_session_reset".to_string(), session_id));
                    HookResult::Ok
                }),
            );
            let end_seen = self.seen.clone();
            ctx.on(
                HookType::OnSessionEnd,
                Arc::new(move |value| {
                    let session_id = value
                        .get("session_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let interrupted = value
                        .get("interrupted")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let reason = value
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    end_seen.lock().unwrap().push((
                        format!("on_session_end:interrupted={interrupted}:reason={reason}"),
                        session_id,
                    ));
                    HookResult::Ok
                }),
            );
        }
    }

    fn attach_lifecycle_recorder(app: &mut App, seen: Arc<StdMutex<Vec<(String, String)>>>) {
        let mut plugin_manager = PluginManager::new();
        plugin_manager.register(Arc::new(LifecycleHookPlugin { seen }));
        let agent = AgentLoop::new(
            app.agent.config.clone(),
            app.agent.tool_registry.clone(),
            app.agent.llm_provider.clone(),
        )
        .with_plugins(Arc::new(StdMutex::new(plugin_manager)));
        app.agent = Arc::new(agent);
    }

    #[test]
    fn app_new_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let old_session_id = app.session_id.clone();

        app.new_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            ("on_session_finalize".to_string(), old_session_id)
        );
        assert_eq!(events[1].0, "on_session_reset");
        assert_eq!(events[1].1, app.session_id);
        assert_ne!(events[0].1, events[1].1);
        assert_eq!(
            app.agent.config.session_id.as_deref(),
            Some(app.session_id.as_str())
        );
    }

    #[test]
    fn app_reset_session_invokes_session_lifecycle_hooks() {
        let mut app = build_minimal_test_app();
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());
        let session_id = app.session_id.clone();

        app.reset_session();

        let events = seen.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                ("on_session_finalize".to_string(), session_id.clone()),
                ("on_session_reset".to_string(), session_id)
            ]
        );
        assert_eq!(app.session_id, "test-session");
        assert_eq!(app.agent.config.session_id.as_deref(), Some("test-session"));
    }

    #[test]
    fn finalize_interrupted_tui_session_invokes_session_end_hook() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "hooked-force-quit".to_string();
        app.messages = vec![hermes_core::Message::user("still save this")];
        let seen = Arc::new(StdMutex::new(Vec::new()));
        attach_lifecycle_recorder(&mut app, seen.clone());

        app.finalize_interrupted_tui_session(None, "shutdown_signal")
            .expect("finalize interrupted session");

        assert_eq!(
            seen.lock().unwrap().clone(),
            vec![(
                "on_session_end:interrupted=true:reason=shutdown_signal".to_string(),
                "hooked-force-quit".to_string()
            )]
        );
    }

    #[test]
    fn active_subagent_count_reads_started_lineage_records() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var("HERMES_HOME").ok();
        // SAFETY: serialized by env_test_lock.
        unsafe { std::env::set_var("HERMES_HOME", tmp.path()) };
        let subagents = tmp.path().join("subagents");
        std::fs::create_dir_all(&subagents).expect("subagents dir");
        std::fs::write(
            subagents.join("a.json"),
            r#"{"sub_agent_id":"a","status":"started"}"#,
        )
        .expect("write active");
        std::fs::write(
            subagents.join("b.json"),
            r#"{"sub_agent_id":"b","status":"completed"}"#,
        )
        .expect("write complete");
        std::fs::write(subagents.join("bad.json"), "{not json").expect("write bad");

        let app = build_minimal_test_app();
        assert_eq!(app.active_subagent_count(), 1);

        // SAFETY: serialized by env_test_lock.
        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HERMES_HOME", value),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            session_id: "test-123".to_string(),
            model: "dynamic".to_string(),
            personality: Some("helpful".to_string()),
            message_count: 5,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "test-123");
        assert_eq!(back.model, "dynamic");
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
        std::env::set_var("HERMES_HOME", tmp.path());

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
        app.messages = vec![hermes_core::Message::user("run quorum now")];
        app.quorum_armed_once = true;
        let has_hint = app.messages.iter().any(|message| {
            message.role == hermes_core::MessageRole::System
                && message
                    .content
                    .as_deref()
                    .unwrap_or_default()
                    .starts_with(QUORUM_HINT_PREFIX)
        });
        let has_user_turn = app
            .messages
            .iter()
            .any(|m| m.role == hermes_core::MessageRole::User);

        assert!(
            app.quorum_mode_armed_for_turn().is_some(),
            "one-shot quorum arm should trigger fan-out without relying on stale system hints (enabled={}, armed_once={}, has_hint={}, has_user_turn={})",
            policy.enabled,
            app.quorum_armed_once,
            has_hint,
            has_user_turn
        );

        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn moa_virtual_model_arms_quorum_without_global_policy() {
        let mut app = build_minimal_test_app();
        app.current_model = "moa:default".to_string();
        app.messages = vec![hermes_core::Message::user("solve with moa")];

        let policy = app
            .quorum_mode_armed_for_turn()
            .expect("moa model should arm quorum fan-out");

        assert!(policy.enabled);
        assert_eq!(policy.voters, 2);
        assert_eq!(
            policy.models,
            vec![
                "openai-codex:gpt-5.5".to_string(),
                "openrouter:deepseek/deepseek-v4-pro".to_string()
            ]
        );
        assert_eq!(policy.mode, "moa-default");
        assert_eq!(
            App::quorum_synthesis_model_for_original("moa:default"),
            "openrouter:anthropic/claude-opus-4.7"
        );
        assert_eq!(
            App::quorum_synthesis_model_for_original("anthropic:claude-sonnet-4-6"),
            "anthropic:claude-sonnet-4-6"
        );
    }

    #[test]
    fn test_clear_quorum_system_hints_inplace_preserves_other_system_messages() {
        let mut app = build_minimal_test_app();
        app.messages = vec![
            hermes_core::Message::system("[QUORUM_MODE] quorum armed"),
            hermes_core::Message::system("normal system context"),
            hermes_core::Message::user("hello"),
        ];

        app.clear_quorum_system_hints_inplace();

        assert_eq!(app.messages.len(), 2);
        assert!(app.messages.iter().all(|message| !message
            .content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[QUORUM_MODE] ")));
        assert!(app
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("normal system context")));
    }

    #[test]
    fn test_run_agent_quorum_arm_persists_artifact_even_on_voter_failures() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_passes = std::env::var("HERMES_QUORUM_VOTER_PASSES").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "1");

        let _ = set_quorum_policy(
            true,
            Some(3),
            Some(vec![
                "custom:quorum-voter-a".to_string(),
                "custom:quorum-voter-b".to_string(),
                "custom:quorum-voter-c".to_string(),
            ]),
        )
        .expect("set quorum policy");

        let mut app = build_minimal_test_app();
        app.session_id = "quorum-test-session".to_string();
        app.messages = vec![hermes_core::Message::user(
            "no tools, just verify quorum fan-out branch",
        )];
        app.quorum_armed_once = true;

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
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match prev_passes {
            Some(v) => std::env::set_var("HERMES_QUORUM_VOTER_PASSES", v),
            None => std::env::remove_var("HERMES_QUORUM_VOTER_PASSES"),
        }
    }

    #[test]
    fn test_persist_session_snapshot_writes_default_session_file() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());

        let mut app = build_minimal_test_app();
        app.session_id = "resume-test".to_string();
        app.messages = vec![
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
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_persist_session_snapshot_respects_app_state_root() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "state-root-test".to_string();
        app.messages = vec![hermes_core::Message::user("ping")];

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
    fn app_tracks_actual_usage_from_agent_results_and_resets() {
        let mut app = build_minimal_test_app();
        let first = hermes_core::UsageStats {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            estimated_cost: Some(0.0015),
        };
        let second = hermes_core::UsageStats {
            prompt_tokens: 7,
            completion_tokens: 3,
            total_tokens: 10,
            estimated_cost: None,
        };

        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![hermes_core::Message::assistant("first")],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(first.clone()),
            interrupted: false,
            session_cost_usd: Some(0.002),
            session_started_hooks_fired: false,
        });
        app.apply_agent_result(hermes_core::AgentResult {
            messages: vec![hermes_core::Message::assistant("second")],
            finished_naturally: true,
            total_turns: 1,
            tool_errors: Vec::new(),
            usage: Some(second.clone()),
            interrupted: false,
            session_cost_usd: None,
            session_started_hooks_fired: false,
        });

        assert_eq!(app.last_usage, Some(second));
        let session = app.session_usage.as_ref().expect("session usage");
        assert_eq!(session.prompt_tokens, 17);
        assert_eq!(session.completion_tokens, 8);
        assert_eq!(session.total_tokens, 25);
        assert_eq!(session.estimated_cost, Some(0.0015));
        assert!((app.session_cost_usd - 0.002).abs() < f64::EPSILON);

        app.reset_session();
        assert!(app.last_usage.is_none());
        assert!(app.session_usage.is_none());
        assert_eq!(app.session_cost_usd, 0.0);
    }

    #[test]
    fn test_apply_agent_result_and_persist_writes_updated_messages() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "persist-after-run".to_string();

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

    #[test]
    fn finalize_interrupted_tui_session_persists_user_and_partial_stream() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "force-quit-session".to_string();
        app.messages = vec![hermes_core::Message::user("diagnose this")];

        app.finalize_interrupted_tui_session(Some("partial answer"), "ctrl_c")
            .expect("finalize interrupted session");

        let path = app
            .state_root
            .join("sessions")
            .join("force-quit-session.json");
        let raw = std::fs::read_to_string(path).expect("read snapshot");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("parse snapshot");
        let contents = value
            .get("messages")
            .and_then(|v| v.as_array())
            .expect("messages")
            .iter()
            .filter_map(|message| message.get("content").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(contents, vec!["diagnose this", "partial answer"]);
    }

    #[test]
    fn finalize_interrupted_tui_session_does_not_duplicate_partial_tail() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "force-quit-dedupe".to_string();
        app.messages = vec![
            hermes_core::Message::user("question"),
            hermes_core::Message::assistant("partial answer"),
        ];

        app.finalize_interrupted_tui_session(Some("partial answer"), "shutdown_signal")
            .expect("finalize interrupted session");

        assert_eq!(app.messages.len(), 2);
        let path = app
            .state_root
            .join("sessions")
            .join("force-quit-dedupe.json");
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

    #[test]
    fn test_new_session_persists_startup_stub_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        std::fs::create_dir_all(app.state_root.join("sessions")).expect("create sessions dir");
        let old_session_id = app.session_id.clone();

        app.new_session();

        assert_ne!(app.session_id, old_session_id);
        let snapshot_path = app
            .state_root
            .join("sessions")
            .join(format!("{}.json", app.session_id));
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
    fn new_session_discards_previous_empty_stub_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "old-empty".to_string();
        app.messages.clear();
        let old_path = app
            .persist_session_snapshot(None)
            .expect("persist old empty snapshot");
        assert!(old_path.exists());

        app.new_session();

        assert!(!old_path.exists());
        assert!(app
            .state_root
            .join("sessions")
            .join(format!("{}.json", app.session_id))
            .exists());
    }

    #[test]
    fn new_session_keeps_previous_nonempty_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "old-nonempty".to_string();
        app.messages = vec![hermes_core::Message::user("keep this")];
        let old_path = app
            .persist_session_snapshot(None)
            .expect("persist old nonempty snapshot");

        app.new_session();

        assert!(old_path.exists());
    }

    #[test]
    fn discard_current_session_if_empty_removes_current_stub_snapshot() {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let mut app = build_minimal_test_app();
        app.state_root = tmp.path().join("custom-state-root");
        app.session_id = "current-empty".to_string();
        app.messages.clear();
        let snapshot_path = app
            .persist_session_snapshot(None)
            .expect("persist current empty snapshot");

        assert!(app.discard_current_session_if_empty());

        assert!(!snapshot_path.exists());
    }

    #[test]
    fn test_persist_session_snapshot_prunes_old_files_by_count_limit() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_max_files = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_FILES").ok();
        let prev_max_total = std::env::var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES").ok();
        let prev_min_free = std::env::var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", "2");
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", "999999999");
        std::env::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", "0");

        let mut app = build_minimal_test_app();
        app.session_id = "snap-prune".to_string();
        app.messages = vec![hermes_core::Message::user("snapshot payload")];

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
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MIN_FREE_BYTES"),
        }
        match prev_max_total {
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MAX_TOTAL_BYTES"),
        }
        match prev_max_files {
            Some(v) => std::env::set_var("HERMES_SESSION_SNAPSHOT_MAX_FILES", v),
            None => std::env::remove_var("HERMES_SESSION_SNAPSHOT_MAX_FILES"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_apply_cli_runtime_overrides_applies_provider_to_prefixed_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("openai:dynamic".to_string());
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
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("nous:dynamic"));
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
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("anthropic:moonshotai/kimi-k2.6"));
    }

    #[test]
    fn explore_first_defaults_do_not_shadow_configured_delegation_depth() {
        let _guard = env_test_lock();
        let keys = [
            "HERMES_SKILL_GUARD_MODE",
            "HERMES_GUARD_MODE",
            "HERMES_TOOL_POLICY_PRESET",
            "HERMES_TOOL_POLICY_MODE",
            "HERMES_REPO_REVIEW_BUDGET_PROFILE",
            "HERMES_MAX_ITERATIONS",
            "HERMES_TOOL_CALL_MAX_CONCURRENCY",
            "HERMES_MAX_DELEGATE_DEPTH",
        ];
        let _snapshot = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        let mut cfg = GatewayConfig::default();
        cfg.delegation.max_spawn_depth = Some(99);
        App::apply_explore_first_runtime_defaults(&cfg);

        assert!(std::env::var("HERMES_MAX_DELEGATE_DEPTH").is_err());

        cfg.delegation.max_spawn_depth = None;
        App::apply_explore_first_runtime_defaults(&cfg);
        assert_eq!(
            std::env::var("HERMES_MAX_DELEGATE_DEPTH").ok().as_deref(),
            Some("4")
        );
    }

    #[tokio::test]
    async fn runtime_cron_scheduler_uses_configured_provider_not_minimal_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "chatcmpl-live-cron-test",
                "object": "chat.completion",
                "created": 0,
                "model": "gpt-live-cron",
                "choices": [
                    {
                        "index": 0,
                        "message": {
                            "role": "assistant",
                            "content": "live-cron-provider-ok"
                        },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            })))
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.model = Some("openai:gpt-live-cron".to_string());
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("test-key".to_string()),
                base_url: Some(server.uri()),
                model: Some("gpt-live-cron".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let temp = tempfile::tempdir().expect("cron tempdir");
        let tools = ToolRegistry::new();
        let scheduler = build_runtime_cron_scheduler(
            &config,
            "openai:gpt-live-cron",
            temp.path().to_path_buf(),
            &tools,
        );
        let job_id = scheduler
            .create_job(hermes_cron::CronJob::new(
                "0 * * * *",
                "prove live cron provider wiring",
            ))
            .await
            .expect("create cron job");
        let result = scheduler.run_job(&job_id).await.expect("run cron job");
        let final_text = result
            .messages
            .iter()
            .rev()
            .find_map(|message| message.content.as_deref())
            .unwrap_or_default();

        assert!(final_text.contains("live-cron-provider-ok"));
        assert!(!final_text.contains("fallback LLM path"));
        server.verify().await;
    }

    #[test]
    fn runtime_cron_scheduler_bridge_excludes_recursive_cronjob_tool() {
        let tools = ToolRegistry::new();
        register_test_tool(&tools, "cronjob");
        register_test_tool(&tools, "terminal");

        let agent_registry = bridge_tool_registry_excluding(&tools, &["cronjob"]);
        let names = agent_registry.names();

        assert!(!names.contains(&"cronjob".to_string()));
        assert!(names.contains(&"terminal".to_string()));
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
        let _guard = env_test_lock();
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let previous: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

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

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_sync_runtime_model_env_sets_tui_provider_when_absent() {
        let _guard = env_test_lock();
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom-xuanji".to_string(),
            LlmProviderConfig {
                model: Some("deepseek-v4-pro".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        let previous: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }

        sync_runtime_model_env(&cfg, "custom-xuanji:deepseek-v4-pro");

        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("custom-xuanji")
        );

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_startup_model_env_sync_uses_config_provider_not_stale_env() {
        let _guard = env_test_lock();
        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
        ];
        let previous: Vec<(&str, Option<String>)> = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect();
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_INFERENCE_PROVIDER", "openrouter");

        let mut cfg = GatewayConfig::default();
        cfg.model = Some("anthropic:claude-sonnet-4-6".to_string());
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let configured_model = cfg.model.as_deref().expect("model should be set");
        let startup = resolve_startup_model(&cfg, configured_model);
        sync_runtime_model_env(&cfg, &startup);

        assert_eq!(startup, "anthropic:claude-sonnet-4-6");
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );

        for (key, value) in previous {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }

    #[test]
    fn test_default_mouse_enabled_respects_env_override() {
        std::env::remove_var("HERMES_TUI_MOUSE");
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "off");
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "1");
        assert!(default_mouse_enabled());

        std::env::remove_var("HERMES_TUI_MOUSE");
    }

    #[test]
    fn test_contextlattice_orchestrator_url_prefers_contextlattice_env_then_memmcp() {
        let _lock = env_test_lock();
        std::env::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
        std::env::remove_var("MEMMCP_ORCHESTRATOR_URL");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:8075"
        );

        std::env::set_var("MEMMCP_ORCHESTRATOR_URL", "http://127.0.0.1:9999/");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:9999"
        );

        std::env::set_var("CONTEXTLATTICE_ORCHESTRATOR_URL", "http://127.0.0.1:7777/");
        assert_eq!(
            App::contextlattice_orchestrator_url(),
            "http://127.0.0.1:7777"
        );

        std::env::remove_var("CONTEXTLATTICE_ORCHESTRATOR_URL");
        std::env::remove_var("MEMMCP_ORCHESTRATOR_URL");
    }

    #[test]
    fn test_build_inference_messages_injects_runtime_reformulation() {
        let _lock = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK", "1");
        std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", "focus");
        std::env::set_var(
            "CONTEXTLATTICE_TOPIC_PATH",
            "runbooks/objective/test-objective",
        );
        let contract =
            upsert_objective_contract("Grow SOL with controlled risk", true).expect("obj");

        let mut app = build_minimal_test_app();
        app.messages.push(hermes_core::Message::user(
            "provide 3 more ideas with contextlattice being one",
        ));
        let (messages, injected) = app.build_inference_messages();
        assert!(injected);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, hermes_core::MessageRole::System);
        let injected_text = messages[0].content.as_deref().unwrap_or_default();
        assert!(injected_text.contains(hermes_app_runtime::RUNTIME_REFORMULATION_PREFIX));
        assert!(injected_text.contains("tool-profile(mode): focus"));
        assert!(injected_text.contains("contextlattice(topic): runbooks/objective/test-objective"));
        assert!(injected_text.contains(contract.id.as_str()));
        assert!(injected_text.contains("UNPROVEN/CONTRADICTORY"));
        assert!(injected_text.contains("execute at least one concrete action"));
        assert!(injected_text.contains("iterative objective momentum"));
        assert!(injected_text.contains("objective behavior directives:"));
        assert!(injected_text.contains("objective success criteria:"));
        assert!(injected_text.contains("objective loop protocol:"));
        assert!(injected_text.contains("Hermes intelligence kernel:"));
        assert!(injected_text.contains("context firewall:"));
        assert!(injected_text.contains("evidence compiler:"));
        assert!(injected_text.contains("adaptive tool planner:"));
        assert!(injected_text.contains("ContextLattice memory cycle:"));
        assert!(injected_text.contains("self-audit finalizer:"));
        assert!(injected_text.contains("user-request(routing-preview):"));
        assert!(
            injected_text.contains("full user request remains available as the next user message")
        );
        assert_eq!(messages[1].role, hermes_core::MessageRole::User);

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
        std::env::remove_var("HERMES_RUNTIME_CONTRADICTION_SELF_CHECK");
        std::env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
        std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH");
    }

    #[test]
    fn test_runtime_reformulation_caps_long_prompt_preview_without_losing_user_message() {
        let _lock = env_test_lock();
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "1");
        std::env::set_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS", "48");

        let long_prompt =
            "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu".repeat(12);
        let mut app = build_minimal_test_app();
        app.messages
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

        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
        std::env::remove_var("HERMES_RUNTIME_REFORMULATION_PROMPT_PREVIEW_CHARS");
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
        std::env::set_var("HERMES_RUNTIME_PROMPT_REFORMULATION", "off");
        let mut app = build_minimal_test_app();
        app.messages
            .push(hermes_core::Message::user("plain request"));
        let (messages, injected) = app.build_inference_messages();
        assert!(!injected);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, hermes_core::MessageRole::User);
        std::env::remove_var("HERMES_RUNTIME_PROMPT_REFORMULATION");
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
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER", "1");

        let mut app = build_minimal_test_app();
        app.messages.push(hermes_core::Message::user(
            "Proceed with objective and improve outcomes continuously.",
        ));
        upsert_objective_contract(
            "Run this assignment in perpetuity and continuously improve output quality",
            false,
        )
        .expect("set objective");
        set_objective_contract_behavior_mode("mission").expect("set mission mode");

        let baseline_len = app.messages.len();
        let mut result_messages = app.messages.clone();
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
        let reason = block_on_test(reason);
        assert!(reason.is_some());

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
        std::env::remove_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER");
    }

    #[test]
    fn test_objective_wait_timer_parks_continuation_enforcer() {
        let _lock = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER", "1");

        let mut app = build_minimal_test_app();
        app.messages.push(hermes_core::Message::user(
            "Proceed with objective and improve outcomes continuously.",
        ));
        upsert_objective_contract(
            "Run this assignment in perpetuity and continuously improve output quality",
            false,
        )
        .expect("set objective");
        set_objective_contract_behavior_mode("mission").expect("set mission mode");
        crate::alpha_runtime::set_objective_contract_wait_seconds(60, Some("CI cooldown"))
            .expect("set wait");

        let baseline_len = app.messages.len();
        let mut result_messages = app.messages.clone();
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
        let reason = block_on_test(reason);
        assert!(reason.is_none());

        match prev_home {
            Some(val) => std::env::set_var("HERMES_HOME", val),
            None => std::env::remove_var("HERMES_HOME"),
        }
        std::env::remove_var("HERMES_OBJECTIVE_EXECUTION_ENFORCER");
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
        std::env::set_var("HERMES_HOME", tmp.path());
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
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn test_default_rtk_raw_mode_respects_env_override() {
        std::env::remove_var("HERMES_RTK_RAW");
        assert!(!default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "on");
        assert!(default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "0");
        assert!(!default_rtk_raw_mode());

        std::env::remove_var("HERMES_RTK_RAW");
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
        std::env::remove_var("HERMES_QUORUM_VOTER_PASSES");
        assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "0");
        assert_eq!(App::quorum_voter_passes(), QUORUM_DEFAULT_VOTER_PASSES);
        std::env::set_var("HERMES_QUORUM_VOTER_PASSES", "max");
        assert_eq!(App::quorum_voter_passes(), 16);
        std::env::remove_var("HERMES_QUORUM_VOTER_PASSES");
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
        let timeout =
            AgentError::LlmApi("request timed out while waiting for provider".to_string());
        let rate_limit = AgentError::LlmApi("HTTP 429 Too Many Requests".to_string());
        let model_missing =
            AgentError::LlmApi("API error 404 Not Found: model missing".to_string());
        assert!(App::is_transient_retryable_error(&timeout));
        assert!(App::is_transient_retryable_error(&rate_limit));
        assert!(!App::is_transient_retryable_error(&model_missing));
    }

    #[test]
    fn test_auth_error_requires_nous_login_detects_missing_login_shape() {
        let err = AgentError::AuthFailed(
            "Hermes is not logged into Nous Portal. Run `hermes portal`.".to_string(),
        );
        assert!(App::auth_error_requires_nous_login(&err));
        let legacy = AgentError::AuthFailed(
            "Stored Nous auth state is invalid; re-run `hermes auth nous`.".to_string(),
        );
        assert!(App::auth_error_requires_nous_login(&legacy));
        let unrelated = AgentError::AuthFailed("rate limited".to_string());
        assert!(!App::auth_error_requires_nous_login(&unrelated));
    }

    #[test]
    fn test_auto_nous_reauth_toggle_defaults_on() {
        let _guard = env_test_lock();
        std::env::remove_var("HERMES_AUTO_NOUS_REAUTH");
        assert!(App::auto_nous_reauth_enabled());
        std::env::set_var("HERMES_AUTO_NOUS_REAUTH", "0");
        assert!(!App::auto_nous_reauth_enabled());
        std::env::remove_var("HERMES_AUTO_NOUS_REAUTH");
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
        let claude = App::resolve_quorum_catalog_candidate(
            "anthropic/claude-4.7-opus-fast-20260512",
            &catalog,
        );
        let qwen =
            App::resolve_quorum_catalog_candidate("qwen/qwen3.6-max-preview-20260420", &catalog);

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
        app.messages
            .push(hermes_core::Message::user("hello before objective"));

        app.set_session_objective(Some(
            "Ship parity with upstream plus stronger UX".to_string(),
        ));
        assert_eq!(
            app.session_objective.as_deref(),
            Some("Ship parity with upstream plus stronger UX")
        );
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[0].role, hermes_core::MessageRole::System);
        let system = app.messages[0].content.clone().unwrap_or_default();
        assert!(system.starts_with("[SESSION_OBJECTIVE] "));
        assert!(system.contains("Ship parity with upstream plus stronger UX"));

        app.set_session_objective(Some("Minimize latency regressions".to_string()));
        let system_count = app
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
            app.session_objective.as_deref(),
            Some("Minimize latency regressions")
        );

        app.set_session_objective(None);
        assert!(app.session_objective.is_none());
        assert!(app.messages.iter().all(|m| {
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
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/hermes");
        std::env::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", "1");

        let contract = upsert_objective_contract("grow wallet safely", true).expect("objective");
        let app = build_minimal_test_app();
        app.maybe_autopin_contextlattice_topic_from_objective();
        let expected = format!("runbooks/objective/{}", contract.id);
        assert_eq!(
            std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
            Some(expected.as_str())
        );

        match prev_toggle {
            Some(v) => std::env::set_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN", v),
            None => std::env::remove_var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN"),
        }
        match prev_topic {
            Some(v) => std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
            None => std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }

    #[test]
    fn test_objective_context_autopin_respects_custom_topic_pin() {
        let _guard = env_test_lock();
        let prev_home = std::env::var("HERMES_HOME").ok();
        let prev_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok();

        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", "runbooks/custom/keep-me");

        let _contract =
            upsert_objective_contract("objective override regression test", false).expect("obj");
        let app = build_minimal_test_app();
        app.maybe_autopin_contextlattice_topic_from_objective();
        assert_eq!(
            std::env::var("CONTEXTLATTICE_TOPIC_PATH").ok().as_deref(),
            Some("runbooks/custom/keep-me")
        );

        match prev_topic {
            Some(v) => std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", v),
            None => std::env::remove_var("CONTEXTLATTICE_TOPIC_PATH"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }
}

