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
            if try_restart_gateway_service()? {
                println!("Gateway service restarted.");
                return Ok(());
            }
            let pid_path = gateway_pid_path_for_cli(&cli);
            if let Some(pid) = read_gateway_pid(&pid_path) {
                if gateway_pid_is_alive(pid) {
                    let _ = gateway_pid_terminate(pid);
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Stopped existing gateway process {}.", pid);
                }
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
            if matches!(action.as_deref(), Some("start")) && try_start_gateway_service()? {
                println!("Gateway service started.");
                return Ok(());
            }
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
            if let Some(pid) = read_gateway_pid(&pid_path) {
                if gateway_pid_is_alive(pid) {
                    println!(
                        "Gateway already appears to be running (PID {}, file {}). Stop it first or remove a stale PID file.",
                        pid,
                        pid_path.display()
                    );
                    return Ok(());
                }
                cleanup_stale_gateway_metadata(&pid_path);
            }

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
                kanban_dispatch_in_gateway: config.kanban.dispatch_in_gateway,
                ..RuntimeGatewayConfig::default()
            };
            let session_manager = Arc::new(SessionManager::new(config.session.clone()));
            let dm_manager = build_gateway_dm_manager(&config);
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
            let terminal_backend = build_terminal_backend(&config);
            let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            let clarify_dispatcher = ClarifyDispatcher::new();
            let tool_registry_for_msg = tool_registry.clone();
            let tool_registry_for_stream = tool_registry.clone();
            let agent_tools_for_cron = Arc::new(bridge_tool_registry(&tool_registry));
            let clarify_for_msg = clarify_dispatcher.clone();
            let clarify_for_stream = clarify_dispatcher.clone();
            let config_arc = Arc::new(config.clone());
            let config_arc_stream = config_arc.clone();
            let gateway_for_review = gateway.clone();
            let gateway_for_review_stream = gateway.clone();
            gateway
                .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                    let config = config_arc.clone();
                    let runtime_tools = tool_registry_for_msg.clone();
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
                        let tool_defs = tool_definition_summary(&tool_schemas);
                        gateway_for_review
                            .emit_hook_event(
                                "agent:tool_definitions",
                                serde_json::json!({
                                    "platform": ctx.platform,
                                    "chat_id": ctx.chat_id,
                                    "user_id": ctx.user_id,
                                    "session_id": ctx.session_key,
                                    "streaming": false,
                                    "tools": tool_defs
                                }),
                            )
                            .await;
                        let platform_for_review = ctx.platform.clone();
                        let chat_for_review = ctx.chat_id.clone();
                        let thread_for_review = ctx.thread_id.clone();
                        let deferred_queue = ctx.deferred_post_delivery_messages.clone();
                        let deferred_released = ctx.deferred_post_delivery_released.clone();
                        let gateway_for_review_cb = gateway_for_review.clone();
                        let review_cb: Arc<dyn Fn(&str) + Send + Sync> =
                            Arc::new(move |text: &str| {
                                if let (Some(queue), Some(released)) =
                                    (deferred_queue.as_ref(), deferred_released.as_ref())
                                {
                                    if !released.load(Ordering::Acquire) {
                                        if let Ok(mut guard) = queue.lock() {
                                            guard.push(text.to_string());
                                            return;
                                        }
                                    }
                                }
                                let gw = gateway_for_review_cb.clone();
                                let platform = platform_for_review.clone();
                                let chat_id = chat_for_review.clone();
                                let thread_id = thread_for_review.clone();
                                let msg = text.to_string();
                                tokio::spawn(async move {
                                    let _ = gw
                                        .send_notify_message_threaded(
                                            &platform,
                                            &chat_id,
                                            &msg,
                                            None,
                                            thread_id.as_deref(),
                                        )
                                        .await;
                                });
                            });
                        let background_review_callback =
                            gateway_memory_notifications_enabled(config.as_ref())
                                .then_some(review_cb);
                        let gateway_for_status = gateway_for_review.clone();
                        let gateway_for_status_hook = gateway_for_review.clone();
                        let platform_for_status = ctx.platform.clone();
                        let chat_for_status = ctx.chat_id.clone();
                        let platform_for_status_hook = ctx.platform.clone();
                        let user_for_status_hook = ctx.user_id.clone();
                        let session_for_status_hook = ctx.session_key.clone();
                        let status_cb = Arc::new(move |event_type: &str, message: &str| {
                            if message.trim().is_empty() {
                                return;
                            }
                            let gw = gateway_for_status.clone();
                            let platform = platform_for_status.clone();
                            let chat_id = chat_for_status.clone();
                            let status_key = event_type.to_string();
                            let msg = message.to_string();
                            tokio::spawn(async move {
                                let _ = gw
                                    .send_or_update_status(
                                        &platform,
                                        &chat_id,
                                        &status_key,
                                        &msg,
                                        None,
                                    )
                                    .await;
                            });
                            let gw_hook = gateway_for_status_hook.clone();
                            let platform = platform_for_status_hook.clone();
                            let user_id = user_for_status_hook.clone();
                            let session_id = session_for_status_hook.clone();
                            let event_type = event_type.to_string();
                            let message = message.to_string();
                            tokio::spawn(async move {
                                gw_hook
                                    .emit_hook_event(
                                        "agent:status",
                                        serde_json::json!({
                                            "platform": platform,
                                            "user_id": user_id,
                                            "session_id": session_id,
                                            "event_type": event_type,
                                            "message": message
                                        }),
                                    )
                                    .await;
                            });
                        });
                        let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
                        let tool_events_for_start = tool_events.clone();
                        let tool_progress_mode = resolve_gateway_tool_progress_mode(
                            config.as_ref(),
                            &ctx.platform,
                            ctx.tool_progress.as_deref(),
                        );
                        let friendly_tool_labels =
                            config.display.friendly_tool_labels_enabled();
                        let tool_progress_seen = Arc::new(Mutex::new(HashSet::<String>::new()));
                        let gateway_for_tool_progress = gateway_for_review.clone();
                        let platform_for_tool_progress = ctx.platform.clone();
                        let chat_for_tool_progress = ctx.chat_id.clone();
                        let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
                            Box::new(move |name: &str, args: &serde_json::Value| {
                                let preview =
                                    build_tool_label_from_value(name, args, 60, friendly_tool_labels)
                                        .unwrap_or_default();
                                let mut event = serde_json::json!({
                                    "phase": "start",
                                    "name": name,
                                    "emoji": tool_emoji(name)
                                });
                                if !preview.is_empty() {
                                    event["preview"] = serde_json::json!(preview);
                                }
                                if let Ok(mut guard) = tool_events_for_start.lock() {
                                    guard.push(event);
                                }
                                if should_emit_gateway_tool_progress(
                                    &tool_progress_mode,
                                    name,
                                    &tool_progress_seen,
                                ) {
                                    if let Some(message) =
                                        build_gateway_tool_progress_message_with_labels(
                                        &platform_for_tool_progress,
                                        name,
                                        args,
                                        &tool_progress_mode,
                                        60,
                                        friendly_tool_labels,
                                    ) {
                                        let gw = gateway_for_tool_progress.clone();
                                        let platform = platform_for_tool_progress.clone();
                                        let chat_id = chat_for_tool_progress.clone();
                                        let parse_mode =
                                            gateway_tool_progress_parse_mode(&platform, &message);
                                        tokio::spawn(async move {
                                            let _ = gw
                                                .send_message(
                                                    &platform, &chat_id, &message, parse_mode,
                                                )
                                                .await;
                                        });
                                    }
                                }
                            });
                        let tool_events_for_complete = tool_events.clone();
                        let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
                            Box::new(move |name: &str, result: &str| {
                                if let Ok(mut guard) = tool_events_for_complete.lock() {
                                    guard.push(serde_json::json!({
                                        "phase": "complete",
                                        "name": name,
                                        "emoji": tool_emoji(name),
                                        "result": truncate_hook_tool_result(result)
                                    }));
                                }
                            });
                        let tool_events_for_step = tool_events.clone();
                        let gateway_for_step_hook = gateway_for_review.clone();
                        let platform_for_step_hook = ctx.platform.clone();
                        let user_for_step_hook = ctx.user_id.clone();
                        let session_for_step_hook = ctx.session_key.clone();
                        let on_step_complete: Box<dyn Fn(u32) + Send + Sync> =
                            Box::new(move |iteration: u32| {
                                let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
                                    std::mem::take(&mut *guard)
                                } else {
                                    Vec::new()
                                };
                                let tool_names: Vec<String> = tools
                                    .iter()
                                    .filter_map(|v| {
                                        v.get("name")
                                            .and_then(|n| n.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect();
                                let gw_hook = gateway_for_step_hook.clone();
                                let platform = platform_for_step_hook.clone();
                                let user_id = user_for_step_hook.clone();
                                let session_id = session_for_step_hook.clone();
                                tokio::spawn(async move {
                                    gw_hook
                                        .emit_hook_event(
                                            "agent:step",
                                            serde_json::json!({
                                                "platform": platform,
                                                "user_id": user_id,
                                                "session_id": session_id,
                                                "iteration": iteration,
                                                "tool_names": tool_names,
                                                "tools": tools
                                            }),
                                        )
                                        .await;
                                });
                            });
                        let callbacks = AgentCallbacks {
                            background_review_callback,
                            status_callback: Some(status_cb),
                            on_tool_start: Some(on_tool_start),
                            on_tool_complete: Some(on_tool_complete),
                            on_step_complete: Some(on_step_complete),
                            ..Default::default()
                        };
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools)
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
                        let tool_defs = tool_definition_summary(&tool_schemas);
                        gateway_for_review
                            .emit_hook_event(
                                "agent:tool_definitions",
                                serde_json::json!({
                                    "platform": ctx.platform,
                                    "chat_id": ctx.chat_id,
                                    "user_id": ctx.user_id,
                                    "session_id": ctx.session_key,
                                    "streaming": true,
                                    "tools": tool_defs
                                }),
                            )
                            .await;
                        let platform_for_review = ctx.platform.clone();
                        let chat_for_review = ctx.chat_id.clone();
                        let thread_for_review = ctx.thread_id.clone();
                        let deferred_queue = ctx.deferred_post_delivery_messages.clone();
                        let deferred_released = ctx.deferred_post_delivery_released.clone();
                        let gateway_for_review_cb = gateway_for_review.clone();
                        let review_cb: Arc<dyn Fn(&str) + Send + Sync> =
                            Arc::new(move |text: &str| {
                                if let (Some(queue), Some(released)) =
                                    (deferred_queue.as_ref(), deferred_released.as_ref())
                                {
                                    if !released.load(Ordering::Acquire) {
                                        if let Ok(mut guard) = queue.lock() {
                                            guard.push(text.to_string());
                                            return;
                                        }
                                    }
                                }
                                let gw = gateway_for_review_cb.clone();
                                let platform = platform_for_review.clone();
                                let chat_id = chat_for_review.clone();
                                let thread_id = thread_for_review.clone();
                                let msg = text.to_string();
                                tokio::spawn(async move {
                                    let _ = gw
                                        .send_notify_message_threaded(
                                            &platform,
                                            &chat_id,
                                            &msg,
                                            None,
                                            thread_id.as_deref(),
                                        )
                                        .await;
                                });
                            });
                        let background_review_callback =
                            gateway_memory_notifications_enabled(config.as_ref())
                                .then_some(review_cb);
                        let gateway_for_status = gateway_for_review.clone();
                        let gateway_for_status_hook = gateway_for_review.clone();
                        let platform_for_status = ctx.platform.clone();
                        let chat_for_status = ctx.chat_id.clone();
                        let platform_for_status_hook = ctx.platform.clone();
                        let user_for_status_hook = ctx.user_id.clone();
                        let session_for_status_hook = ctx.session_key.clone();
                        let status_cb = Arc::new(move |event_type: &str, message: &str| {
                            if message.trim().is_empty() {
                                return;
                            }
                            let gw = gateway_for_status.clone();
                            let platform = platform_for_status.clone();
                            let chat_id = chat_for_status.clone();
                            let status_key = event_type.to_string();
                            let msg = message.to_string();
                            tokio::spawn(async move {
                                let _ = gw
                                    .send_or_update_status(
                                        &platform,
                                        &chat_id,
                                        &status_key,
                                        &msg,
                                        None,
                                    )
                                    .await;
                            });
                            let gw_hook = gateway_for_status_hook.clone();
                            let platform = platform_for_status_hook.clone();
                            let user_id = user_for_status_hook.clone();
                            let session_id = session_for_status_hook.clone();
                            let event_type = event_type.to_string();
                            let message = message.to_string();
                            tokio::spawn(async move {
                                gw_hook
                                    .emit_hook_event(
                                        "agent:status",
                                        serde_json::json!({
                                            "platform": platform,
                                            "user_id": user_id,
                                            "session_id": session_id,
                                            "event_type": event_type,
                                            "message": message
                                        }),
                                    )
                                    .await;
                            });
                        });
                        let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
                        let tool_events_for_start = tool_events.clone();
                        let tool_progress_mode = resolve_gateway_tool_progress_mode(
                            config.as_ref(),
                            &ctx.platform,
                            ctx.tool_progress.as_deref(),
                        );
                        let friendly_tool_labels =
                            config.display.friendly_tool_labels_enabled();
                        let tool_progress_seen = Arc::new(Mutex::new(HashSet::<String>::new()));
                        let gateway_for_tool_progress = gateway_for_review.clone();
                        let platform_for_tool_progress = ctx.platform.clone();
                        let chat_for_tool_progress = ctx.chat_id.clone();
                        let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
                            Box::new(move |name: &str, args: &serde_json::Value| {
                                let preview =
                                    build_tool_label_from_value(name, args, 60, friendly_tool_labels)
                                        .unwrap_or_default();
                                let mut event = serde_json::json!({
                                    "phase": "start",
                                    "name": name,
                                    "emoji": tool_emoji(name)
                                });
                                if !preview.is_empty() {
                                    event["preview"] = serde_json::json!(preview);
                                }
                                if let Ok(mut guard) = tool_events_for_start.lock() {
                                    guard.push(event);
                                }
                                if should_emit_gateway_tool_progress(
                                    &tool_progress_mode,
                                    name,
                                    &tool_progress_seen,
                                ) {
                                    if let Some(message) =
                                        build_gateway_tool_progress_message_with_labels(
                                        &platform_for_tool_progress,
                                        name,
                                        args,
                                        &tool_progress_mode,
                                        60,
                                        friendly_tool_labels,
                                    ) {
                                        let gw = gateway_for_tool_progress.clone();
                                        let platform = platform_for_tool_progress.clone();
                                        let chat_id = chat_for_tool_progress.clone();
                                        let parse_mode =
                                            gateway_tool_progress_parse_mode(&platform, &message);
                                        tokio::spawn(async move {
                                            let _ = gw
                                                .send_message(
                                                    &platform, &chat_id, &message, parse_mode,
                                                )
                                                .await;
                                        });
                                    }
                                }
                            });
                        let tool_events_for_complete = tool_events.clone();
                        let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
                            Box::new(move |name: &str, result: &str| {
                                if let Ok(mut guard) = tool_events_for_complete.lock() {
                                    guard.push(serde_json::json!({
                                        "phase": "complete",
                                        "name": name,
                                        "emoji": tool_emoji(name),
                                        "result": truncate_hook_tool_result(result)
                                    }));
                                }
                            });
                        let tool_events_for_step = tool_events.clone();
                        let gateway_for_step_hook = gateway_for_review.clone();
                        let platform_for_step_hook = ctx.platform.clone();
                        let user_for_step_hook = ctx.user_id.clone();
                        let session_for_step_hook = ctx.session_key.clone();
                        let on_step_complete: Box<dyn Fn(u32) + Send + Sync> =
                            Box::new(move |iteration: u32| {
                                let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
                                    std::mem::take(&mut *guard)
                                } else {
                                    Vec::new()
                                };
                                let tool_names: Vec<String> = tools
                                    .iter()
                                    .filter_map(|v| {
                                        v.get("name")
                                            .and_then(|n| n.as_str())
                                            .map(|s| s.to_string())
                                    })
                                    .collect();
                                let gw_hook = gateway_for_step_hook.clone();
                                let platform = platform_for_step_hook.clone();
                                let user_id = user_for_step_hook.clone();
                                let session_id = session_for_step_hook.clone();
                                tokio::spawn(async move {
                                    gw_hook
                                        .emit_hook_event(
                                            "agent:step",
                                            serde_json::json!({
                                                "platform": platform,
                                                "user_id": user_id,
                                                "session_id": session_id,
                                                "iteration": iteration,
                                                "tool_names": tool_names,
                                                "tools": tools
                                            }),
                                        )
                                        .await;
                                });
                            });
                        let callbacks = AgentCallbacks {
                            background_review_callback,
                            status_callback: Some(status_cb),
                            on_tool_start: Some(on_tool_start),
                            on_tool_complete: Some(on_tool_complete),
                            on_step_complete: Some(on_step_complete),
                            ..Default::default()
                        };
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools)
                                .with_callbacks(callbacks);
                        if let Some(registration) = ctx.busy_control.clone() {
                            let _ = registration
                                .attach(Arc::new(GatewayAgentBusyControl::new(
                                    agent.interrupt.clone(),
                                )))
                                .await;
                        }
                        let emit = on_chunk.clone();
                        let ui_state = Arc::new(Mutex::new((false, false))); // (muted, needs_break)
                        let ui_state_cb = ui_state.clone();
                        let stream_cb: Box<dyn Fn(StreamChunk) + Send + Sync> =
                            Box::new(move |chunk: StreamChunk| {
                                if let Some(delta) = chunk.delta {
                                    if let Some(extra) = delta.extra.as_ref() {
                                        if let Some(control) =
                                            extra.get("control").and_then(|v| v.as_str())
                                        {
                                            if control == "mute_post_response" {
                                                let enabled = extra
                                                    .get("enabled")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false);
                                                if let Ok(mut st) = ui_state_cb.lock() {
                                                    st.0 = enabled;
                                                }
                                            } else if control == "stream_break" {
                                                if let Ok(mut st) = ui_state_cb.lock() {
                                                    st.1 = true;
                                                }
                                            }
                                        }
                                    }
                                    if let Some(text) = delta.content {
                                        if let Ok(mut st) = ui_state_cb.lock() {
                                            if st.0 {
                                                return;
                                            }
                                            if st.1 {
                                                emit("\n\n".to_string());
                                                st.1 = false;
                                            }
                                        }
                                        emit(text);
                                    }
                                }
                            });

                        let result = agent
                            .run_stream(messages, Some(tool_schemas), Some(stream_cb))
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
            let cron_runner = Arc::new(CronRunner::new(cron_llm, agent_tools_for_cron));
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

            register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks).await?;

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

            gateway.start_all().await?;
            {
                let gw_cron_delivery = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    run_cron_gateway_delivery_loop(cron_platform_rx, gw_cron_delivery).await;
                }));
            }
            {
                let gw_reconnect = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    gw_reconnect.platform_reconnect_watcher(20).await;
                }));
                let gw_expiry = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    gw_expiry.session_expiry_watcher(300).await;
                }));
            }
            let own_pid = std::process::id();
            std::fs::write(&pid_path, format!("{}\n", own_pid)).map_err(|e| {
                AgentError::Io(format!("failed to write {}: {}", pid_path.display(), e))
            })?;
            println!("Gateway runtime initialized with context-aware model/provider routing.");
            println!("Gateway is ready. Press Ctrl+C to stop.");
            // Keep gateway alive for future adapter/event wiring.
            // Wait for Ctrl+C
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to listen for Ctrl+C: {}", e)))?;

            println!("\nShutting down gateway...");
            cron_scheduler.stop().await;
            gateway.stop_all().await?;
            let _ = std::fs::remove_file(&pid_path);
            for task in sidecar_tasks {
                task.abort();
            }
            println!("Gateway stopped.");
        }
        Some("status") => {
            if let Some(service_state) = gateway_service_status()? {
                println!("{service_state}");
            }
            let pid_path = gateway_pid_path_for_cli(&cli);
            if !pid_path.exists() {
                println!(
                    "Gateway status: not running (no PID file; start with `hermes gateway start`)"
                );
                return Ok(());
            }
            match read_gateway_pid(&pid_path) {
                Some(pid) if gateway_pid_is_alive(pid) => {
                    println!(
                        "Gateway status: running (PID {}, file {})",
                        pid,
                        pid_path.display()
                    );
                }
                Some(pid) => {
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!(
                        "Gateway status: not running (stale metadata for PID {} in {})",
                        pid,
                        pid_path.display()
                    );
                }
                None => {
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Gateway status: invalid PID file at {}", pid_path.display());
                }
            }
        }
        Some("stop") => {
            if try_stop_gateway_service()? {
                println!("Gateway service stopped.");
                return Ok(());
            }
            let pid_path = gateway_pid_path_for_cli(&cli);
            let Some(pid) = read_gateway_pid(&pid_path) else {
                println!("Gateway stop: no PID file (nothing to stop).");
                return Ok(());
            };
            if !gateway_pid_is_alive(pid) {
                cleanup_stale_gateway_metadata(&pid_path);
                println!(
                    "Gateway stop: process {} not running; removed stale PID/lock metadata for {}.",
                    pid,
                    pid_path.display()
                );
                return Ok(());
            }
            match gateway_pid_terminate(pid) {
                Ok(()) => {
                    println!("Sent SIGTERM to gateway PID {}.", pid);
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Removed {}.", pid_path.display());
                }
                Err(e) => println!("Gateway stop: failed to signal PID {}: {}", pid, e),
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
            println!("Gateway service: service install/start is not implemented for this platform.");
        }
        println!("Dry-run: would ensure the gateway service is installed, start it, then print status.");
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
