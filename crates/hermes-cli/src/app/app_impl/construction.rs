impl App {
    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let state_root = state_dir(cli.config_dir.as_deref().map(std::path::Path::new));
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        apply_cli_runtime_overrides(&mut config, &cli);
        Self::apply_explore_first_runtime_defaults(&config);

        if config.sessions.auto_prune {
            let resolved_home = config
                .home_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var("HERMES_HOME")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(hermes_home_dir);
            let sp = SessionPersistence::new(&resolved_home);
            let maintenance = sp.maybe_auto_prune_and_vacuum(
                config.sessions.retention_days,
                config.sessions.min_interval_hours,
                config.sessions.vacuum_after_prune,
            );
            if let Some(err) = maintenance.error {
                tracing::debug!("sessions db auto-maintenance skipped: {}", err);
            } else if !maintenance.skipped && maintenance.pruned > 0 {
                tracing::info!(
                    "sessions db auto-maintenance pruned {} session(s){}",
                    maintenance.pruned,
                    if maintenance.vacuumed {
                        " + vacuum"
                    } else {
                        ""
                    }
                );
            }
        }

        let configured_model = config
            .model
            .clone()
            .unwrap_or_else(|| "gpt-5.5".to_string());
        let current_model = resolve_startup_model(&config, &configured_model);
        let current_model = select_startup_model_with_fallback_and_auth_resolver(
            &config,
            &current_model,
            Some(&provider_oauth_token_from_auth_state),
        )
        .selected_model;
        let current_personality = config.personality.clone();

        sync_runtime_model_env(&config, &current_model);

        let tool_registry = Arc::new(ToolRegistry::new());
        if default_rtk_raw_mode() {
            tool_registry.set_raw_mode(true);
        }
        let stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>> =
            Arc::new(StdMutex::new(None));
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_data_dir.display(), e)))?;
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            &config,
            &current_model,
            cron_data_dir,
            &tool_registry,
        ));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(&tool_registry, cron_scheduler.clone());
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let tool_schemas =
            hermes_tool_planning::resolve_platform_tool_schemas(&config, "cli", &tool_registry);

        let session_id = Uuid::new_v4().to_string();
        let mut agent_config = build_agent_config(&config, &current_model);
        agent_config.session_id = Some(session_id.clone());
        let provider = build_provider(&config, &current_model);

        let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
            agent_config,
            agent_tool_registry,
            provider,
        ))
        .with_callbacks(Self::stream_callbacks(stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        let recovered_background_jobs = recover_queued_background_jobs(8);
        if recovered_background_jobs > 0 {
            tracing::info!(
                "Recovered {} queued background job(s) from durable status queue",
                recovered_background_jobs
            );
        }

        let app = Self {
            state_root,
            config: Arc::new(config),
            agent,
            tool_registry,
            cron_scheduler,
            tool_schemas,
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id,
            running: true,
            current_model,
            last_usage: None,
            session_usage: None,
            session_cost_usd: 0.0,
            current_personality,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared,
            mouse_enabled: default_mouse_enabled(),
            pending_theme: None,
            pending_image_hint: None,
            session_objective: None,
            pending_input_prefill: None,
            pending_system_notes: Vec::new(),
            quorum_armed_once: false,
            pet_settings: load_pet_settings(),
            #[cfg(test)]
            fail_model_rebuild_for: None,
        };
        app.ensure_session_stub_snapshot();
        Ok(app)
    }

}
