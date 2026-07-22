fn gateway_status_callback_for_context(
    gateway: Arc<Gateway>,
    ctx: &GatewayRuntimeContext,
) -> Arc<dyn Fn(&str, &str) + Send + Sync> {
    let platform = ctx.platform.clone();
    let chat_id = ctx.chat_id.clone();
    let thread_id = ctx.thread_id.clone();
    Arc::new(move |event_type: &str, message: &str| {
        if message.trim().is_empty() {
            return;
        }
        let gateway = Arc::clone(&gateway);
        let platform = platform.clone();
        let chat_id = chat_id.clone();
        let thread_id = thread_id.clone();
        let status_key = event_type.to_string();
        let message = message.to_string();
        tokio::spawn(async move {
            let _ = gateway
                .send_or_update_status_threaded(
                    &platform,
                    &chat_id,
                    &status_key,
                    &message,
                    None,
                    thread_id.as_deref(),
                )
                .await;
        });
    })
}

/// Handle `hermes gateway [action]`.
#[allow(clippy::too_many_arguments)]
async fn run_gateway(
    cli: Cli,
    action: Option<String>,
    _platform: Option<String>,
    _system: bool,
    all: bool,
    force: bool,
    _run_as_user: Option<String>,
    _replace: bool,
    dry_run: bool,
    yes: bool,
    _deep: bool,
) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        Some("install") => {
            install_gateway_service(force, dry_run)?;
            return Ok(());
        }
        Some("uninstall") => {
            uninstall_gateway_service(dry_run)?;
            return Ok(());
        }
        Some("migrate-legacy") => {
            migrate_legacy_gateway_services(dry_run, yes)?;
            return Ok(());
        }
        Some("restart") => {
            let pid_path = gateway_pid_path_for_cli(&cli);
            let (stopped, already_exited) = stop_gateway_metadata_pids(&pid_path).await?;
            if stopped.is_empty() && already_exited.is_empty() {
                if try_restart_gateway_service()? {
                    println!("Gateway service restarted.");
                    return Ok(());
                }
            } else if !stopped.is_empty() {
                println!(
                    "Stopped existing gateway process(es): {}.",
                    stopped
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            ensure_gateway_start_slot_available(&pid_path)?;
            if try_start_gateway_service()? {
                println!("Gateway service started.");
                return Ok(());
            }
            return Box::pin(run_gateway(
                cli,
                Some("run".to_string()),
                None,
                false,
                all,
                force,
                None,
                false,
                false,
                yes,
                false,
            ))
            .await;
        }
        Some("setup") => {
            run_gateway_setup(&cli).await?;
        }
        None | Some("run") | Some("start") => {
            // Register before initialization so a SIGTERM received during
            // startup is retained and takes the same teardown path as Ctrl-C.
            let shutdown_listener = install_gateway_shutdown_signal_listener()?;
            if matches!(action.as_deref(), Some("start")) {
                let pid_path = gateway_pid_path_for_cli(&cli);
                ensure_gateway_start_slot_available(&pid_path)?;
                if try_start_gateway_service()? {
                    println!("Gateway service started.");
                    return Ok(());
                }
            }
            let execution_mode = hermes_config::resolve_runtime_execution_mode(
                &config,
                cli.execution_mode.as_deref(),
            )
            .map_err(|error| AgentError::Config(error.to_string()))?;
            let execution_backend =
                hermes_cli::terminal_backend::build_execution_backend_binding(
                    &config,
                    execution_mode,
                )?;
            println!("Starting Hermes Gateway...");
            run_sessions_db_auto_maintenance(&config);

            // List enabled platforms
            let enabled: Vec<&String> = config
                .platforms
                .iter()
                .filter(|(_, pc)| pc.enabled)
                .map(|(name, _)| name)
                .collect();

            if enabled.is_empty() {
                println!(
                    "Note: no chat platforms enabled in config.yaml — gateway still runs cron + HTTP webhooks."
                );
            } else if gateway_allowlist_startup_would_warn(&config) {
                tracing::warn!(
                    "No gateway user allowlist or allow-all override configured; set platform *_ALLOWED_USERS or explicit *_ALLOW_ALL_USERS to silence this warning"
                );
                println!(
                    "Warning: no gateway user allowlist configured. Set platform *_ALLOWED_USERS or explicit *_ALLOW_ALL_USERS=true if this is intentional."
                );
            }
            let requirement_issues = gateway_requirement_issues(&config);
            if !requirement_issues.is_empty() {
                let mut msg = String::from("Gateway requirement check failed:\n");
                for issue in requirement_issues {
                    msg.push_str("  - ");
                    msg.push_str(&issue);
                    msg.push('\n');
                }
                msg.push_str("请先执行 `hermes gateway setup` 或 `hermes auth login <provider>` 修复后再启动。");
                return Err(AgentError::Config(msg));
            }

            let pid_path = gateway_pid_path_for_cli(&cli);
            ensure_gateway_start_slot_available(&pid_path)?;
            let run_runtime = Arc::new(
                RunRuntime::open(hermes_state_root(&cli)).map_err(|error| {
                    AgentError::Io(format!("run-record startup failed: {error}"))
                })?,
            );

            if !enabled.is_empty() {
                println!(
                    "Enabled platforms: {}",
                    enabled
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }

            // Build gateway runtime and context-aware message handler.
            let runtime_gateway_config = RuntimeGatewayConfig {
                model: config.model.clone(),
                model_switch_persist_by_default: config.model_switch.persist_switch_by_default,
                model_switch_config_path: Some(
                    hermes_state_root(&cli)
                        .join("config.yaml")
                        .to_string_lossy()
                        .to_string(),
                ),
                streaming_enabled: config.streaming.enabled,
                display: config.display.clone(),
                service_tier: config.agent.normalized_service_tier(),
                quick_commands: config.quick_commands.clone(),
                resource_limits_enabled: config.resource_limits_enabled,
                kanban_dispatch_in_gateway: config.kanban.dispatch_in_gateway,
                ..RuntimeGatewayConfig::default()
            };
            let session_manager = Arc::new(SessionManager::with_resource_limits(
                config.session.clone(),
                config.resource_limits_enabled,
            ));
            let dm_manager = build_gateway_dm_manager_with_grant_store(
                &config,
                gateway_dm_grant_store_path(&cli),
            )?;
            let gateway = Arc::new(Gateway::new(
                session_manager,
                dm_manager,
                runtime_gateway_config,
            ));
            gateway
                .set_platform_access_policies(build_gateway_platform_access_policies(&config))
                .await;
            let mut hook_registry = HookRegistry::new();
            hook_registry.register_builtins();
            hook_registry.discover_and_load(&hermes_home().join("hooks"));
            gateway.set_hook_registry(Arc::new(hook_registry)).await;
            gateway
                .emit_hook_event(
                    "gateway:startup",
                    serde_json::json!({
                        "enabled_platforms": enabled.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                    }),
                )
                .await;

            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = Arc::clone(&execution_backend.terminal_backend);
            let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            let clarify_dispatcher = ClarifyDispatcher::new();
            let tool_registry_for_msg = tool_registry.clone();
            let tool_registry_for_stream = tool_registry.clone();
            let agent_tools_for_cron = Arc::new(bridge_tool_registry(&tool_registry));
            agent_tools_for_cron.set_execution_containment_context(Arc::clone(
                &execution_backend.containment_context,
            ));
            let clarify_for_msg = clarify_dispatcher.clone();
            let clarify_for_stream = clarify_dispatcher.clone();
            let config_arc = Arc::new(config.clone());
            let config_arc_stream = config_arc.clone();
            let run_runtime_for_msg = run_runtime.clone();
            let run_runtime_for_stream = run_runtime.clone();
            let gateway_for_review = gateway.clone();
            let gateway_for_review_stream = gateway.clone();
            let execution_mode_for_message_handler = execution_mode;
            let execution_mode_for_stream_handler = execution_mode;
            let config_arc_memory_forget = config_arc.clone();
            let tool_registry_for_memory_forget = tool_registry.clone();
            let run_runtime_for_memory_forget = run_runtime.clone();
            let execution_mode_for_memory_forget = execution_mode;
            let config_arc_memory_export = config_arc.clone();
            let tool_registry_for_memory_export = tool_registry.clone();
            let run_runtime_for_memory_export = run_runtime.clone();
            let execution_mode_for_memory_export = execution_mode;
            let config_arc_memory_export_open = config_arc.clone();
            let tool_registry_for_memory_export_open = tool_registry.clone();
            let run_runtime_for_memory_export_open = run_runtime.clone();
            let execution_mode_for_memory_export_open = execution_mode;
            gateway
                .set_memory_forget_handler(Arc::new(move |ctx| {
                    let config = config_arc_memory_forget.clone();
                    let runtime_tools = tool_registry_for_memory_forget.clone();
                    let run_runtime = run_runtime_for_memory_forget.clone();
                    Box::pin(async move {
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let response = match build_agent_for_gateway_context(
                            config.as_ref(),
                            &ctx,
                            agent_tools,
                            run_runtime,
                            execution_mode_for_memory_forget,
                        ) {
                            Ok(agent) => gateway_memory_forget_response(agent.forget_bound_memory()),
                            Err(_) => GatewayMemoryForgetResponse {
                                erased: false,
                                providers: Vec::new(),
                                remediation: Some(GatewayMemoryForgetRemediation::Reconfigure),
                            },
                        };
                        Ok(response)
                    })
                }))
                .await;
            gateway
                .set_memory_export_handler(Arc::new(move |ctx| {
                    let config = config_arc_memory_export.clone();
                    let runtime_tools = tool_registry_for_memory_export.clone();
                    let run_runtime = run_runtime_for_memory_export.clone();
                    Box::pin(async move {
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let response = match build_agent_for_gateway_context(
                            config.as_ref(),
                            &ctx,
                            agent_tools,
                            run_runtime,
                            execution_mode_for_memory_export,
                        ) {
                            Ok(agent) => gateway_memory_export_response(agent.export_bound_memory()),
                            Err(_) => GatewayMemoryExportResponse {
                                artifact: None,
                                remediation: Some(GatewayMemoryExportRemediation::Reconfigure),
                            },
                        };
                        Ok(response)
                    })
                }))
                .await;
            gateway
                .set_memory_export_open_handler(Arc::new(move |ctx, artifact| {
                    let config = config_arc_memory_export_open.clone();
                    let runtime_tools = tool_registry_for_memory_export_open.clone();
                    let run_runtime = run_runtime_for_memory_export_open.clone();
                    Box::pin(async move {
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let agent = build_agent_for_gateway_context(
                            config.as_ref(),
                            &ctx,
                            agent_tools,
                            run_runtime,
                            execution_mode_for_memory_export_open,
                        )
                        .map_err(|_| {
                            hermes_gateway::GatewayError::Auth(
                                "memory export download is unavailable".to_string(),
                            )
                        })?;
                        let artifact = hermes_core::MemoryExportArtifact::new(
                            artifact.artifact_ref,
                            artifact.manifest_digest,
                            artifact.record_count,
                            artifact.page_count,
                            artifact.provider_count,
                        );
                        agent.open_bound_memory_export(&artifact).map_err(|_| {
                            hermes_gateway::GatewayError::Auth(
                                "memory export download is unavailable".to_string(),
                            )
                        })
                    })
                }))
                .await;
            gateway
                .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                    let config = config_arc.clone();
                    let runtime_tools = tool_registry_for_msg.clone();
                    let run_runtime = run_runtime_for_msg.clone();
                    let gateway_for_review = gateway_for_review.clone();
                    let clarify = clarify_for_msg.clone();
                    Box::pin(async move {
                        if let Some(pending) = clarify.take_next().await {
                            let answer = messages
                                .iter()
                                .rev()
                                .find_map(|m| {
                                    (m.role == MessageRole::User)
                                        .then(|| m.content.clone())
                                        .flatten()
                                })
                                .unwrap_or_default();
                            let _ = pending.respond(answer);
                            return Ok(
                                "Clarification received. Continuing task execution...".to_string()
                            );
                        }
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-5.5"),
                            &ctx,
                        );
                        let tool_schemas = resolve_platform_tool_schemas(
                            config.as_ref(),
                            &ctx.platform,
                            &runtime_tools,
                        );
                        // Gateway progress is emitted only through the request/response
                        // stream. Out-of-band status, hook, and background notifications
                        // require a durable outbox route and are disabled here.
                        let callbacks = AgentCallbacks::default();
                        let agent = build_agent_for_gateway_context(
                            config.as_ref(),
                            &ctx,
                            agent_tools,
                            run_runtime,
                            execution_mode_for_message_handler,
                        )?
                        .with_callbacks(callbacks);
                        if let Some(registration) = ctx.busy_control.clone() {
                            let _ = registration
                                .attach(Arc::new(GatewayAgentBusyControl::new(
                                    agent.interrupt.clone(),
                                )))
                                .await;
                        }
                        let result = agent
                            .run(messages, Some(tool_schemas))
                            .await
                            .map_err(|e| hermes_gateway::GatewayError::Platform(e.to_string()))?;
                        let home = ctx
                            .home
                            .as_deref()
                            .or(config.home_dir.as_deref())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if let Some(h) = home {
                            if !ctx.session_key.trim().is_empty() {
                                let sp = SessionPersistence::new(Path::new(h));
                                let sys = leading_system_prompt_for_persist(&result.messages);
                                let _ = sp.persist_session(
                                    &ctx.session_key,
                                    &result.messages,
                                    Some(&effective_model),
                                    Some(ctx.platform.as_str()),
                                    None,
                                    sys.as_deref(),
                                );
                            }
                        }
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;
            gateway
                .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
                    let config = config_arc_stream.clone();
                    let runtime_tools = tool_registry_for_stream.clone();
                    let run_runtime = run_runtime_for_stream.clone();
                    let gateway_for_review = gateway_for_review_stream.clone();
                    let clarify = clarify_for_stream.clone();
                    Box::pin(async move {
                        if let Some(pending) = clarify.take_next().await {
                            let answer = messages
                                .iter()
                                .rev()
                                .find_map(|m| {
                                    (m.role == MessageRole::User)
                                        .then(|| m.content.clone())
                                        .flatten()
                                })
                                .unwrap_or_default();
                            let _ = pending.respond(answer);
                            return Ok(
                                "Clarification received. Continuing task execution...".to_string()
                            );
                        }
                        let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-5.5"),
                            &ctx,
                        );
                        let tool_schemas = resolve_platform_tool_schemas(
                            config.as_ref(),
                            &ctx.platform,
                            &runtime_tools,
                        );
                        // Out-of-band progress requires a durable gateway outbox route.
                        let callbacks = AgentCallbacks::default();
                        let agent = build_agent_for_gateway_context(
                            config.as_ref(),
                            &ctx,
                            agent_tools,
                            run_runtime,
                            execution_mode_for_stream_handler,
                        )?
                        .with_callbacks(callbacks);
                        if let Some(registration) = ctx.busy_control.clone() {
                            let _ = registration
                                .attach(Arc::new(GatewayAgentBusyControl::new(
                                    agent.interrupt.clone(),
                                )))
                                .await;
                        }
                        let emit = on_chunk.clone();
                        let muted = Arc::new(Mutex::new(false));
                        let muted_cb = muted.clone();
                        let (observer_sink, mut observer_rx) =
                            hermes_agent::AgentObserverSink::channel(256);
                        let mut observer_task = tokio::spawn(async move {
                            while let Some(event) = observer_rx.recv().await {
                                let hermes_agent::AgentObserverEvent::RenderDelta(delta) = event
                                else {
                                    continue;
                                };
                                if let Some(enabled) = delta.mute_post_response {
                                    if let Ok(mut state) = muted_cb.lock() {
                                        *state = enabled;
                                    }
                                }
                                let is_muted =
                                    muted_cb.lock().map(|state| *state).unwrap_or(false);
                                if is_muted {
                                    continue;
                                }
                                if let Some(text) = delta.text {
                                    if delta.break_before {
                                        emit("\n\n".to_string());
                                    }
                                    emit(text);
                                }
                            }
                        });

                        let result = agent
                            .run_stream(messages, Some(tool_schemas), observer_sink.clone())
                            .await
                            .map_err(|e| {
                                hermes_gateway::GatewayError::Platform(e.to_string())
                            });
                        let flushed = observer_sink.flush().await;
                        drop(observer_sink);
                        if !flushed
                            || tokio::time::timeout(
                                Duration::from_millis(100),
                                &mut observer_task,
                            )
                            .await
                            .is_err()
                        {
                            observer_task.abort();
                        }
                        let result = result?;
                        let home = ctx
                            .home
                            .as_deref()
                            .or(config.home_dir.as_deref())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if let Some(h) = home {
                            if !ctx.session_key.trim().is_empty() {
                                let sp = SessionPersistence::new(Path::new(h));
                                let sys = leading_system_prompt_for_persist(&result.messages);
                                let _ = sp.persist_session(
                                    &ctx.session_key,
                                    &result.messages,
                                    Some(&effective_model),
                                    Some(ctx.platform.as_str()),
                                    None,
                                    sys.as_deref(),
                                );
                            }
                        }
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;

            // Cron: same on-disk dir as `hermes cron` + real LLM/tools as the gateway agent.
            let cron_dir = hermes_state_root(&cli).join("cron");
            std::fs::create_dir_all(&cron_dir)
                .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_dir.display(), e)))?;
            let default_model = config
                .model
                .clone()
                .unwrap_or_else(|| "gpt-5.5".to_string());
            let cron_persistence = Arc::new(FileJobPersistence::with_dir(cron_dir.clone()));
            let cron_llm = build_provider(&config, &default_model);
            let cron_runner = Arc::new(
                CronRunner::new(
                    cron_llm,
                    agent_tools_for_cron,
                    run_runtime.clone(),
                    execution_mode,
                    Arc::clone(&execution_backend.containment_context),
                    &config,
                )
                .map_err(|error| AgentError::Config(error.to_string()))?,
            );
            let mut cron_scheduler = CronScheduler::new(cron_persistence, cron_runner);
            let (cron_tx, cron_rx) = broadcast::channel::<CronCompletionEvent>(64);
            let cron_platform_rx = cron_tx.subscribe();
            cron_scheduler.set_completion_broadcast(cron_tx);
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            let cron_scheduler = Arc::new(cron_scheduler);
            wire_cron_scheduler_backend(&tool_registry, cron_scheduler.clone());
            wire_gateway_messaging_backend(&tool_registry, gateway.clone());
            wire_gateway_clarify_backend(&tool_registry, clarify_dispatcher);
            let webhooks_path = hermes_state_root(&cli).join("webhooks.json");
            let discord_route_spool_path = discord_route_spool_path_for_gateway(&cli, &config)?;
            tracing::info!(
                cron_dir = %cron_dir.display(),
                webhooks = %webhooks_path.display(),
                "gateway cron scheduler + HTTP webhook fan-out"
            );
            println!(
                "Cron jobs: {}  |  Webhook registry: {}",
                cron_dir.display(),
                webhooks_path.display()
            );

            let mut sidecar_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            let webhooks_path_clone = webhooks_path.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_cron_webhook_delivery_loop(cron_rx, webhooks_path_clone).await;
            }));

            let discord_ingress_shutdown = register_gateway_adapters(
                &config,
                gateway.clone(),
                &mut sidecar_tasks,
                &discord_route_spool_path,
                cron_scheduler.clone(),
                execution_mode,
            )
            .await?;

            if gateway.adapter_names().await.is_empty() {
                if enabled.is_empty() {
                    println!("No chat adapters enabled; cron + webhooks still active.");
                } else {
                    return Err(AgentError::Config(
                        "Gateway startup failed: platforms are enabled but no adapters registered."
                            .to_string(),
                    ));
                }
            }

            if let Err(start_error) = gateway.start_all().await {
                cron_scheduler.stop().await;
                let terminal_error = cleanup_gateway_after_start_failure(
                    gateway.as_ref(),
                    discord_ingress_shutdown,
                    start_error.into(),
                )
                .await;
                for task in sidecar_tasks {
                    task.abort();
                }
                return Err(terminal_error);
            }
            let own_pid = std::process::id();
            if let Err(error) = std::fs::write(&pid_path, format!("{}\n", own_pid)) {
                cron_scheduler.stop().await;
                let terminal_error = cleanup_gateway_after_start_failure(
                    gateway.as_ref(),
                    discord_ingress_shutdown,
                    AgentError::Io(format!("failed to write {}: {}", pid_path.display(), error)),
                )
                .await;
                for task in sidecar_tasks {
                    task.abort();
                }
                return Err(terminal_error);
            }
            {
                let gw_cron_delivery = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    run_cron_gateway_delivery_loop(cron_platform_rx, gw_cron_delivery).await;
                }));
            }
            let reconnect_task = {
                let gw_reconnect = gateway.clone();
                tokio::spawn(async move {
                    gw_reconnect.platform_reconnect_watcher(20).await;
                })
            };
            {
                let gw_expiry = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    gw_expiry.session_expiry_watcher(300).await;
                }));
            }
            println!("Gateway runtime initialized with context-aware model/provider routing.");
            println!("Gateway is ready. Press Ctrl+C or send SIGTERM to stop.");
            let shutdown_signal = shutdown_listener.wait().await?;

            println!(
                "\nShutting down gateway after {}...",
                shutdown_signal.as_str()
            );
            cron_scheduler.stop().await;
            reconnect_task.abort();
            let _ = reconnect_task.await;
            let discord_drain_error =
                if let Some(discord_ingress_shutdown) = discord_ingress_shutdown {
                    discord_ingress_shutdown.drain().await.err()
                } else {
                    None
                };
            let gateway_stop_result = gateway.stop_all().await;
            if gateway_stop_result.is_ok() {
                cleanup_gateway_metadata_for_pid(&pid_path, own_pid);
            }
            for task in sidecar_tasks {
                task.abort();
            }
            gateway_shutdown_result(discord_drain_error, gateway_stop_result)?;
            println!("Gateway stopped.");
        }
        Some("status") => {
            if let Some(service_state) = gateway_service_status()? {
                println!("{service_state}");
            }
            let pid_path = gateway_pid_path_for_cli(&cli);
            if !pid_path.exists() && !gateway_lock_path_for_pid_path(&pid_path).exists() {
                println!(
                    "Gateway status: not running (no PID/lock metadata; start with `hermes gateway start`)"
                );
                return Ok(());
            }
            match read_gateway_pid_or_lock(&pid_path) {
                Some(pid) => match gateway_pid_state(pid) {
                    GatewayPidState::Gateway => {
                        println!(
                            "Gateway status: running (PID {}, file {})",
                            pid,
                            pid_path.display()
                        );
                    }
                    GatewayPidState::Dead => {
                        cleanup_gateway_metadata_for_pid(&pid_path, pid);
                        println!(
                            "Gateway status: not running (stale metadata for PID {} in {})",
                            pid,
                            pid_path.display()
                        );
                    }
                    GatewayPidState::LiveUnowned => {
                        println!(
                            "Gateway status: PID {} is alive but does not identify as a Hermes gateway; retained PID/lock metadata at {}",
                            pid,
                            pid_path.display()
                        );
                    }
                    GatewayPidState::Unverifiable => {
                        println!(
                            "Gateway status: cannot verify live PID {}; retained PID/lock metadata at {}",
                            pid,
                            pid_path.display()
                        );
                    }
                },
                None => {
                    println!(
                        "Gateway status: invalid or unreadable PID/lock metadata retained at {}",
                        pid_path.display()
                    );
                }
            }
        }
        Some("stop") => {
            let pid_path = gateway_pid_path_for_cli(&cli);
            let (stopped, already_exited) = stop_gateway_metadata_pids(&pid_path).await?;
            if stopped.is_empty() && already_exited.is_empty() {
                return Err(AgentError::Gateway(
                    "refusing to stop an unverified gateway service: no PID/lock metadata was available to prove process identity. Inspect the service, restore verified metadata, or stop it through its owning supervisor."
                        .to_string(),
                ));
            }

            if stopped.is_empty() {
                println!(
                    "Gateway stop: process(es) {} not running; removed only their stale PID/lock metadata.",
                    already_exited
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            } else {
                println!(
                    "Gateway stopped after process(es) {} exited.",
                    stopped
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Some(other) => {
            println!(
                "Unknown gateway action: {}. Use 'run', 'start', 'stop', 'restart', 'status', 'install', 'uninstall', 'setup', or 'migrate-legacy'.",
                other
            );
        }
    }
    Ok(())
}
async fn stop_gateway_metadata_pids(pid_path: &Path) -> Result<(Vec<u32>, Vec<u32>), AgentError> {
    let mut stopped = Vec::new();
    let mut already_exited = Vec::new();
    let mut service_stop_requested = false;
    for pid in read_gateway_metadata_pids(pid_path)? {
        match validate_gateway_pid_for_stop(pid)? {
            GatewayStopOutcome::AlreadyExited => {
                cleanup_gateway_metadata_for_pid(pid_path, pid);
                already_exited.push(pid);
            }
            GatewayStopOutcome::Stopped => {
                if !service_stop_requested && try_stop_gateway_service()? {
                    service_stop_requested = true;
                    wait_for_gateway_pid_exit(pid, GATEWAY_STOP_WAIT_TIMEOUT).await?;
                    cleanup_gateway_metadata_for_pid(pid_path, pid);
                } else {
                    stop_gateway_pid_gracefully(pid_path, pid).await?;
                }
                stopped.push(pid);
            }
        }
    }
    Ok((stopped, already_exited))
}

/// Resolves the Discord ingress spool under the same non-secret identity
/// namespace as Discord adapter state. A legacy unscoped spool cannot be
/// attributed safely after token/application rotation, so it is quarantined
/// rather than replayed by whichever bot starts next.
fn discord_route_spool_path_for_gateway(
    cli: &Cli,
    config: &GatewayConfig,
) -> Result<PathBuf, AgentError> {
    let gateway_root = hermes_state_root(cli).join("gateway");
    let legacy_path = gateway_root.join("discord-route-spool.json");

    #[cfg(feature = "gateway-discord")]
    if let Some(platform_cfg) = config
        .platforms
        .get("discord")
        .filter(|platform_cfg| platform_cfg.enabled)
    {
        if let Some(token) = platform_token_or_extra(platform_cfg) {
            let discord_config = build_discord_config(platform_cfg, config.proxy.as_ref(), token);
            let scope = DiscordAdapter::state_scope_for_config(&discord_config);
            quarantine_legacy_discord_route_spool(&legacy_path)?;
            return Ok(gateway_root
                .join("discord")
                .join(scope)
                .join("route-spool.json"));
        }
    }

    // No configured Discord identity can consume this fallback path. It
    // preserves the adapter registration contract for disabled/misconfigured
    // Discord without ever adopting an ambiguous legacy spool.
    Ok(legacy_path)
}

fn quarantine_legacy_discord_route_spool(legacy_path: &Path) -> Result<(), AgentError> {
    if !legacy_path.exists() {
        return Ok(());
    }

    let parent = legacy_path.parent().ok_or_else(|| {
        AgentError::Io(format!(
            "legacy Discord spool has no parent directory: {}",
            legacy_path.display()
        ))
    })?;
    let quarantine_dir = parent.join("discord-route-spool-quarantine");
    std::fs::create_dir_all(&quarantine_dir).map_err(|error| {
        AgentError::Io(format!(
            "failed to create Discord spool quarantine {}: {error}",
            quarantine_dir.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&quarantine_dir, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                AgentError::Io(format!(
                    "failed to make Discord spool quarantine directory {} private: {error}",
                    quarantine_dir.display()
                ))
            },
        )?;
    }
    let unique = format!(
        "legacy-unscoped-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let quarantine_path = quarantine_dir.join(unique);
    std::fs::rename(legacy_path, &quarantine_path).map_err(|error| {
        AgentError::Io(format!(
            "failed to quarantine ambiguous legacy Discord spool {}: {error}",
            legacy_path.display()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&quarantine_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|error| {
                AgentError::Io(format!(
                    "failed to make quarantined Discord spool {} private: {error}",
                    quarantine_path.display()
                ))
            })?;
    }
    tracing::warn!(
        legacy = %legacy_path.display(),
        quarantine = %quarantine_path.display(),
        "quarantined unscoped legacy Discord ingress spool; it will not be replayed by the current bot"
    );
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatewayShutdownSignal {
    CtrlC,
    Sigterm,
}

impl GatewayShutdownSignal {
    fn as_str(self) -> &'static str {
        match self {
            Self::CtrlC => "Ctrl+C",
            Self::Sigterm => "SIGTERM",
        }
    }
}

struct GatewayShutdownSignalListener {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

fn install_gateway_shutdown_signal_listener() -> Result<GatewayShutdownSignalListener, AgentError> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        Ok(GatewayShutdownSignalListener {
            sigint: signal(SignalKind::interrupt())
                .map_err(|error| AgentError::Io(format!("failed to listen for Ctrl+C: {error}")))?,
            sigterm: signal(SignalKind::terminate()).map_err(|error| {
                AgentError::Io(format!("failed to listen for SIGTERM: {error}"))
            })?,
        })
    }

    #[cfg(not(unix))]
    {
        Ok(GatewayShutdownSignalListener {})
    }
}

impl GatewayShutdownSignalListener {
    async fn wait(mut self) -> Result<GatewayShutdownSignal, AgentError> {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.sigint.recv() => Ok(GatewayShutdownSignal::CtrlC),
                _ = self.sigterm.recv() => Ok(GatewayShutdownSignal::Sigterm),
            }
        }

        #[cfg(not(unix))]
        {
            let _ = self;
            tokio::signal::ctrl_c()
                .await
                .map_err(|error| AgentError::Io(format!("failed to listen for Ctrl+C: {error}")))?;
            Ok(GatewayShutdownSignal::CtrlC)
        }
    }
}

fn gateway_shutdown_result(
    discord_drain_error: Option<hermes_core::errors::GatewayError>,
    gateway_stop_result: Result<(), hermes_core::errors::GatewayError>,
) -> Result<(), AgentError> {
    match (discord_drain_error, gateway_stop_result) {
        (None, Ok(())) => Ok(()),
        (Some(discord_error), Ok(())) => Err(AgentError::Gateway(format!(
            "Discord ingress drain failed after gateway cleanup: {discord_error}"
        ))),
        (None, Err(gateway_error)) => Err(gateway_error.into()),
        (Some(discord_error), Err(gateway_error)) => Err(AgentError::Gateway(format!(
            "Discord ingress drain failed: {discord_error}; gateway stop also failed: {gateway_error}"
        ))),
    }
}

async fn cleanup_gateway_after_start_failure(
    gateway: &Gateway,
    discord_ingress_shutdown: Option<DiscordIngressShutdown>,
    start_error: AgentError,
) -> AgentError {
    let discord_drain_error = if let Some(discord_ingress_shutdown) = discord_ingress_shutdown {
        discord_ingress_shutdown.drain().await.err()
    } else {
        None
    };
    let gateway_stop_result = gateway.stop_all().await;
    match gateway_shutdown_result(discord_drain_error, gateway_stop_result) {
        Ok(()) => start_error,
        Err(cleanup_error) => AgentError::Gateway(format!(
            "gateway adapter startup failed: {start_error}; startup cleanup also failed: {cleanup_error}"
        )),
    }
}

fn run_sessions_db_auto_maintenance(config: &GatewayConfig) {
    if !config.sessions.auto_prune {
        return;
    }
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let sp = SessionPersistence::new(&home);
    let result = sp.maybe_auto_prune_and_vacuum(
        config.sessions.retention_days,
        config.sessions.min_interval_hours,
        config.sessions.vacuum_after_prune,
    );
    if let Some(err) = result.error {
        tracing::debug!("sessions db auto-maintenance skipped: {}", err);
    } else if !result.skipped && result.pruned > 0 {
        tracing::info!(
            "sessions db auto-maintenance pruned {} session(s){}",
            result.pruned,
            if result.vacuumed { " + vacuum" } else { "" }
        );
    }
}

/// One-command always-on runtime UX.
///
/// This intentionally composes the existing gateway service manager instead of
/// creating a parallel daemon path. On platforms without service-install
/// support, it still prints the gateway service/status contract so operators
/// know the fallback command.
async fn run_up(force: bool, dry_run: bool) -> Result<(), AgentError> {
    println!("Hermes Agent Ultra up");
    if dry_run {
        install_gateway_service(force, true)?;
        if let Some(service_state) = gateway_service_status()? {
            println!("{service_state}");
        } else {
            println!(
                "Gateway service: service install/start is not implemented for this platform."
            );
        }
        println!(
            "Dry-run: would ensure the gateway service is installed, start it, then print status."
        );
        return Ok(());
    }

    install_gateway_service(force, false)?;
    if try_start_gateway_service()? {
        println!("Gateway service started.");
    } else {
        println!(
            "Gateway service start is not available on this platform; use `hermes-ultra gateway start` for foreground runtime."
        );
    }
    if let Some(service_state) = gateway_service_status()? {
        println!("{service_state}");
    }
    println!("Use `hermes-ultra logs --follow` for runtime logs.");
    Ok(())
}

fn gateway_memory_notifications_enabled(config: &GatewayConfig) -> bool {
    config.display.memory_notifications_enabled()
}

async fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool, AgentError> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let ans = prompt_line(format!("{question} {hint}: ")).await?;
    if ans.trim().is_empty() {
        return Ok(default_yes);
    }
    let v = ans.trim().to_ascii_lowercase();
    Ok(matches!(v.as_str(), "y" | "yes" | "1" | "true" | "on"))
}

fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn enabled_flag(platform: Option<&PlatformConfig>) -> &'static str {
    if platform.map(|p| p.enabled).unwrap_or(false) {
        "enabled"
    } else {
        "disabled"
    }
}

fn set_extra_string_if_nonempty(platform: &mut PlatformConfig, key: &str, value: &str) {
    let v = value.trim();
    if !v.is_empty() {
        platform
            .extra
            .insert(key.to_string(), serde_json::Value::String(v.to_string()));
    }
}

async fn configure_platform_basic_prompts(
    disk: &mut hermes_config::GatewayConfig,
    key: &str,
) -> Result<(), AgentError> {
    let p = disk
        .platforms
        .entry(key.to_string())
        .or_insert_with(PlatformConfig::default);
    p.enabled = true;

    match key {
        "discord" => {
            let token = prompt_line("Discord bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_id = prompt_line("Discord application_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "application_id", &app_id);
            let allowed =
                prompt_line("Discord allowed users (comma-separated, optional): ").await?;
            if !allowed.trim().is_empty() {
                p.allowed_users = parse_csv_list(&allowed);
            }
            let home = prompt_line("Discord home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "slack" => {
            let token = prompt_line("Slack bot token (xoxb-...): ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let app_token = prompt_line("Slack app token (xapp-..., optional): ").await?;
            set_extra_string_if_nonempty(p, "app_token", &app_token);
            let socket_mode = prompt_yes_no("Slack use socket_mode?", true).await?;
            p.extra.insert(
                "socket_mode".to_string(),
                serde_json::Value::Bool(socket_mode),
            );
        }
        "matrix" => {
            let homeserver =
                prompt_line("Matrix homeserver_url (e.g. https://matrix.org): ").await?;
            set_extra_string_if_nonempty(p, "homeserver_url", &homeserver);
            let user_id = prompt_line("Matrix user_id (e.g. @bot:matrix.org): ").await?;
            set_extra_string_if_nonempty(p, "user_id", &user_id);
            let token = prompt_line("Matrix access token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let room = prompt_line("Matrix home room_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "room_id", &room);
        }
        "mattermost" => {
            let server_url = prompt_line("Mattermost server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let token = prompt_line("Mattermost bot token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let team_id = prompt_line("Mattermost team_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "team_id", &team_id);
            let home = prompt_line("Mattermost home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "signal" => {
            let account = prompt_line("Signal phone_number/account (e.g. +15551234567): ").await?;
            set_extra_string_if_nonempty(p, "phone_number", &account);
            let api_url = prompt_line("Signal api_url (default http://localhost:8080): ").await?;
            set_extra_string_if_nonempty(p, "api_url", &api_url);
        }
        "whatsapp" => {
            let token = prompt_line("WhatsApp Cloud API token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let phone_id = prompt_line("WhatsApp phone_number_id: ").await?;
            set_extra_string_if_nonempty(p, "phone_number_id", &phone_id);
            let verify = prompt_line("WhatsApp verify_token (optional): ").await?;
            set_extra_string_if_nonempty(p, "verify_token", &verify);
            let home = prompt_line("WhatsApp home channel (optional): ").await?;
            if !home.trim().is_empty() {
                p.home_channel = Some(home.trim().to_string());
            }
        }
        "dingtalk" => {
            let client_id = prompt_line("DingTalk client_id/appkey: ").await?;
            set_extra_string_if_nonempty(p, "client_id", &client_id);
            let client_secret = prompt_line("DingTalk client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &client_secret);
        }
        "feishu" => {
            let app_id = prompt_line("Feishu/Lark app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let app_secret = prompt_line("Feishu/Lark app_secret: ").await?;
            set_extra_string_if_nonempty(p, "app_secret", &app_secret);
            let verify = prompt_line("Feishu verification_token (optional): ").await?;
            set_extra_string_if_nonempty(p, "verification_token", &verify);
            let encrypt_key = prompt_line("Feishu encrypt_key (optional): ").await?;
            set_extra_string_if_nonempty(p, "encrypt_key", &encrypt_key);
        }
        "wecom" => {
            let corp_id = prompt_line("WeCom corp_id: ").await?;
            set_extra_string_if_nonempty(p, "corp_id", &corp_id);
            let agent_id = prompt_line("WeCom agent_id: ").await?;
            set_extra_string_if_nonempty(p, "agent_id", &agent_id);
            let secret = prompt_line("WeCom secret: ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
        }
        "wecom_callback" => {
            let corp_id = prompt_line("WeCom callback corp_id: ").await?;
            set_extra_string_if_nonempty(p, "corp_id", &corp_id);
            let corp_secret = prompt_line("WeCom callback corp_secret: ").await?;
            set_extra_string_if_nonempty(p, "corp_secret", &corp_secret);
            let agent_id = prompt_line("WeCom callback agent_id: ").await?;
            set_extra_string_if_nonempty(p, "agent_id", &agent_id);
            let token = prompt_line("WeCom callback token: ").await?;
            set_extra_string_if_nonempty(p, "token", &token);
            let aes = prompt_line("WeCom callback encoding_aes_key: ").await?;
            set_extra_string_if_nonempty(p, "encoding_aes_key", &aes);
            let host = prompt_line("WeCom callback host (default 0.0.0.0): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = prompt_line("WeCom callback port (default 8645): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path = prompt_line("WeCom callback path (default /wecom/callback): ").await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "qqbot" => {
            let app_id = prompt_line("QQBot app_id: ").await?;
            set_extra_string_if_nonempty(p, "app_id", &app_id);
            let secret = prompt_line("QQBot client_secret: ").await?;
            set_extra_string_if_nonempty(p, "client_secret", &secret);
            let markdown = prompt_yes_no("QQBot markdown_support?", true).await?;
            p.extra.insert(
                "markdown_support".to_string(),
                serde_json::Value::Bool(markdown),
            );
        }
        "bluebubbles" => {
            let server_url = prompt_line("BlueBubbles server_url: ").await?;
            set_extra_string_if_nonempty(p, "server_url", &server_url);
            let password = prompt_line("BlueBubbles password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
        }
        "email" => {
            let username = prompt_line("Email username/address: ").await?;
            set_extra_string_if_nonempty(p, "username", &username);
            let password = prompt_line("Email password/app password: ").await?;
            set_extra_string_if_nonempty(p, "password", &password);
            let imap_host = prompt_line("Email imap_host: ").await?;
            set_extra_string_if_nonempty(p, "imap_host", &imap_host);
            let smtp_host = prompt_line("Email smtp_host: ").await?;
            set_extra_string_if_nonempty(p, "smtp_host", &smtp_host);
            let imap_port = prompt_line("Email imap_port (default 993): ").await?;
            if let Ok(v) = imap_port.trim().parse::<u16>() {
                p.extra
                    .insert("imap_port".to_string(), serde_json::Value::from(v));
            }
            let smtp_port = prompt_line("Email smtp_port (default 587): ").await?;
            if let Ok(v) = smtp_port.trim().parse::<u16>() {
                p.extra
                    .insert("smtp_port".to_string(), serde_json::Value::from(v));
            }
        }
        "sms" => {
            let sid = prompt_line("Twilio account_sid: ").await?;
            set_extra_string_if_nonempty(p, "account_sid", &sid);
            let auth = prompt_line("Twilio auth_token: ").await?;
            set_extra_string_if_nonempty(p, "auth_token", &auth);
            let from = prompt_line("Twilio from_number (E.164): ").await?;
            set_extra_string_if_nonempty(p, "from_number", &from);
        }
        "homeassistant" => {
            let base_url =
                prompt_line("HomeAssistant base_url (e.g. http://127.0.0.1:8123): ").await?;
            set_extra_string_if_nonempty(p, "base_url", &base_url);
            let token = prompt_line("HomeAssistant long_lived_token: ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
            let webhook_id = prompt_line("HomeAssistant webhook_id (optional): ").await?;
            set_extra_string_if_nonempty(p, "webhook_id", &webhook_id);
        }
        "ntfy" => {
            let topic = prompt_line("ntfy subscribe topic: ").await?;
            set_extra_string_if_nonempty(p, "topic", &topic);
            let server = prompt_line("ntfy server URL (default https://ntfy.sh): ").await?;
            set_extra_string_if_nonempty(p, "server", &server);
            let publish_topic = prompt_line("ntfy publish topic (optional): ").await?;
            set_extra_string_if_nonempty(p, "publish_topic", &publish_topic);
            let token = prompt_line("ntfy auth token (optional): ").await?;
            if !token.trim().is_empty() {
                p.token = Some(token.trim().to_string());
            }
        }
        "webhook" => {
            let secret = prompt_line("Webhook secret: ").await?;
            set_extra_string_if_nonempty(p, "secret", &secret);
            let port = prompt_line("Webhook port (default 9000): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let path = prompt_line("Webhook path (default /webhook): ").await?;
            set_extra_string_if_nonempty(p, "path", &path);
        }
        "api_server" => {
            let host = prompt_line("API server host (default 127.0.0.1): ").await?;
            set_extra_string_if_nonempty(p, "host", &host);
            let port = prompt_line("API server port (default 8090): ").await?;
            if let Ok(v) = port.trim().parse::<u16>() {
                p.extra
                    .insert("port".to_string(), serde_json::Value::from(v));
            }
            let token =
                prompt_line("API server auth_token (required for non-loopback host): ").await?;
            set_extra_string_if_nonempty(p, "auth_token", &token);
        }
        _ => {}
    }
    Ok(())
}

async fn run_gateway_setup(cli: &Cli) -> Result<(), AgentError> {
    println!("Gateway setup wizard");
    println!("--------------------");
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let platform_catalog: &[(&str, &str)] = &[
        ("weixin", "Weixin"),
        ("qqbot", "QQBot"),
        ("telegram", "Telegram"),
        ("discord", "Discord"),
        ("slack", "Slack"),
        ("matrix", "Matrix"),
        ("mattermost", "Mattermost"),
        ("whatsapp", "WhatsApp"),
        ("signal", "Signal"),
        ("dingtalk", "DingTalk"),
        ("feishu", "Feishu"),
        ("wecom", "WeCom"),
        ("wecom_callback", "WeCom Callback"),
        ("bluebubbles", "BlueBubbles"),
        ("email", "Email"),
        ("sms", "SMS"),
        ("homeassistant", "HomeAssistant"),
        ("ntfy", "ntfy"),
        ("webhook", "Webhook"),
        ("api_server", "API Server"),
    ];
    println!("This wizard configures messaging platforms in config.yaml.");
    println!("Current platform status:");
    for (k, label) in platform_catalog {
        println!("  - {:<13} {}", label, enabled_flag(disk.platforms.get(*k)));
    }
    println!();
    println!("Use SPACE to toggle platforms and ENTER to confirm.");
    let mut pre_selected: HashSet<usize> = HashSet::new();
    for (idx, (key, _)) in platform_catalog.iter().enumerate() {
        if disk
            .platforms
            .get(*key)
            .map(|cfg| cfg.enabled)
            .unwrap_or(false)
        {
            pre_selected.insert(idx);
        }
    }
    let selection_items: Vec<String> = platform_catalog
        .iter()
        .map(|(key, label)| format!("{:<13} {}", label, enabled_flag(disk.platforms.get(*key))))
        .collect();
    let selected_result = hermes_cli::curses_checklist(
        "Select platforms to configure",
        &selection_items,
        &pre_selected,
        Some(&|selected| {
            if selected.is_empty() {
                "none selected".to_string()
            } else {
                format!("{} selected", selected.len())
            }
        }),
    );
    if !selected_result.confirmed {
        println!("Gateway setup cancelled.");
        return Ok(());
    }
    let mut selected: Vec<String> = selected_result
        .selected
        .iter()
        .copied()
        .filter_map(|idx| platform_catalog.get(idx).map(|(key, _)| key.to_string()))
        .collect();
    selected.sort();
    selected.dedup();
    if selected.is_empty() {
        println!("No valid platforms selected.");
        return Ok(());
    }

    for key in selected {
        println!();
        println!("Configuring {}...", key);
        match key.as_str() {
            "weixin" => {
                run_auth(
                    cli.clone(),
                    Some("login".to_string()),
                    Some("weixin".to_string()),
                    None,
                    None,
                    None,
                    None,
                    true,
                )
                .await?;
                disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let wx = disk
                    .platforms
                    .entry("weixin".to_string())
                    .or_insert_with(PlatformConfig::default);
                wx.enabled = true;
                println!("Direct message policy: 1)pairing 2)open 3)allowlist 4)disabled");
                let dm_choice = prompt_line("Choose [1-4] (default 1): ").await?;
                match dm_choice.trim() {
                    "2" => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("open"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                    "3" => {
                        let ids = parse_csv_list(
                            &prompt_line("Allowed Weixin user IDs (comma-separated): ").await?,
                        );
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("allowlist"));
                        wx.extra.insert(
                            "allow_from".to_string(),
                            serde_json::Value::Array(
                                ids.into_iter().map(serde_json::Value::String).collect(),
                            ),
                        );
                    }
                    "4" => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("disabled"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                    _ => {
                        wx.extra
                            .insert("dm_policy".to_string(), serde_json::json!("pairing"));
                        wx.extra
                            .insert("allow_from".to_string(), serde_json::json!([]));
                    }
                }
                println!("Group policy: 1)disabled 2)open 3)allowlist");
                let group_choice = prompt_line("Choose [1-3] (default 1): ").await?;
                match group_choice.trim() {
                    "2" => {
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("open"));
                        wx.extra
                            .insert("group_allow_from".to_string(), serde_json::json!([]));
                    }
                    "3" => {
                        let ids = parse_csv_list(
                            &prompt_line("Allowed Weixin group IDs (comma-separated): ").await?,
                        );
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("allowlist"));
                        wx.extra.insert(
                            "group_allow_from".to_string(),
                            serde_json::Value::Array(
                                ids.into_iter().map(serde_json::Value::String).collect(),
                            ),
                        );
                    }
                    _ => {
                        wx.extra
                            .insert("group_policy".to_string(), serde_json::json!("disabled"));
                        wx.extra
                            .insert("group_allow_from".to_string(), serde_json::json!([]));
                    }
                }
                let home = prompt_line("Weixin home channel (optional): ").await?;
                if !home.trim().is_empty() {
                    wx.home_channel = Some(home.trim().to_string());
                }
            }
            "telegram" => {
                run_auth(
                    cli.clone(),
                    Some("login".to_string()),
                    Some("telegram".to_string()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await?;
                disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let tg = disk
                    .platforms
                    .entry("telegram".to_string())
                    .or_insert_with(PlatformConfig::default);
                tg.enabled = true;
                let polling = prompt_yes_no("Telegram use polling mode?", true).await?;
                tg.extra
                    .insert("polling".to_string(), serde_json::Value::Bool(polling));
                if !polling {
                    let webhook_url = prompt_line("Telegram webhook URL: ").await?;
                    if !webhook_url.trim().is_empty() {
                        tg.webhook_url = Some(webhook_url.trim().to_string());
                    }
                }
                let home = prompt_line("Telegram home channel (optional): ").await?;
                if !home.trim().is_empty() {
                    tg.home_channel = Some(home.trim().to_string());
                }
            }
            other => configure_platform_basic_prompts(&mut disk, other).await?,
        }
    }

    validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;

    println!();
    println!("Gateway setup complete.");
    println!("Config saved: {}", cfg_path.display());
    println!("Next step: `hermes gateway start`");
    Ok(())
}
