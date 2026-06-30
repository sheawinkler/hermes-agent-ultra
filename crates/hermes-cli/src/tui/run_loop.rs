// ---------------------------------------------------------------------------
// Main TUI run loop
// ---------------------------------------------------------------------------

/// Run the interactive TUI with the given App.
///
/// This is the main entry point for the interactive TUI mode.
/// It sets up the terminal, renders frames, and handles events.
pub async fn run(mut app: App) -> Result<(), AgentError> {
    let mut tui = Tui::new().map_err(|e| AgentError::Config(e.to_string()))?;
    let mut state = TuiState::default();
    restore_tui_composer_draft(&app, &mut state);
    let mut last_jobs_refresh = Instant::now()
        .checked_sub(Duration::from_secs(2))
        .unwrap_or_else(Instant::now);
    let mut last_pet_tick = Instant::now();
    app.set_stream_handle(Some(StreamHandle::from(tui.stream_sender())));

    // Spawn crossterm event reader on a dedicated thread. Crossterm polling and
    // reads are synchronous; running them on the Tokio runtime can make abort
    // + await hang during TUI shutdown if the task is inside a terminal read.
    let event_sender = tui.event_sender();
    let event_shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let event_shutdown_for_thread = std::sync::Arc::clone(&event_shutdown);
    let event_thread = std::thread::spawn(move || {
        while !event_shutdown_for_thread.load(std::sync::atomic::Ordering::Relaxed) {
            if crate::checklist::embedded_picker_active() {
                std::thread::sleep(Duration::from_millis(16));
                continue;
            }
            if crossterm::event::poll(Duration::from_millis(16)).unwrap_or(false) {
                if let Ok(event) = crossterm::event::read() {
                    let msg = match event {
                        CrosstermEvent::Key(key) => Some(Event::Key(key)),
                        CrosstermEvent::Resize(w, h) => Some(Event::Resize(w, h)),
                        CrosstermEvent::Mouse(mouse) => Some(Event::Mouse(mouse)),
                        CrosstermEvent::Paste(text) => Some(Event::Paste(text)),
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        if event_sender.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    });
    // Spawn OS signal bridge so terminal close / signal-driven shutdowns
    // unwind the TUI and return control cleanly.
    let signal_sender = tui.event_sender();
    let signal_task = tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint = signal(SignalKind::interrupt()).ok();
            let mut sigterm = signal(SignalKind::terminate()).ok();
            let mut sighup = signal(SignalKind::hangup()).ok();
            tokio::select! {
                _ = async {
                    if let Some(sig) = sigint.as_mut() {
                        let _ = sig.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
                _ = async {
                    if let Some(sig) = sigterm.as_mut() {
                        let _ = sig.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
                _ = async {
                    if let Some(sig) = sighup.as_mut() {
                        let _ = sig.recv().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
        let _ = signal_sender.send(Event::Shutdown);
    });

    let mut frame_tick = tokio::time::interval(Duration::from_millis(60));
    frame_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut needs_redraw = true;
    let mut active_agent_task: Option<JoinHandle<()>> = None;

    // Main event loop
    'main_loop: while app.running {
        tui.set_mouse_capture(app.mouse_enabled())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        if let Some(theme_name) = app.take_pending_theme_change() {
            let applied = crate::skin_engine::resolve_theme(&theme_name);
            tui.set_theme(applied);
            needs_redraw = true;
        }

        if needs_redraw {
            state.refresh_sticky_prompt(&app);
            let active_theme = tui.theme().clone();
            tui.terminal
                .draw(|f| {
                    render(f, &app, &mut state, &active_theme);
                })
                .map_err(|e| AgentError::Config(e.to_string()))?;
            needs_redraw = false;
        }

        tokio::select! {
            biased;
            event = tui.events.recv() => {
                match event {
                    Some(Event::Paste(text)) => {
                        if state.modal_active() {
                            state.status_message = "Paste ignored while picker is open".to_string();
                        } else {
                            let line_count = text.lines().count().max(1);
                            state.insert_paste_at_cursor(&text);
                            persist_tui_composer_draft(&app, &state);
                            state.status_message = format!("Pasted {} line(s)", line_count);
                        }
                        needs_redraw = true;
                        continue;
                    }
                    Some(Event::Key(key)) => {
                        // Ctrl+C always exits back to parent terminal. If work is in flight,
                        // emit interrupt first so in-progress tools can stop gracefully.
                        if is_ctrl_c(&key) {
                            if state.processing {
                                app.interrupt_controller.interrupt(None);
                                abort_and_join_task(&mut active_agent_task).await;
                                finalize_interrupted_tui_session(&mut app, &mut state, "ctrl_c");
                                tui.event_sender().send(Event::Interrupt).ok();
                            }
                            app.running = false;
                            break 'main_loop;
                        }

                        if state.modal_active() {
                            match state.handle_modal_key(key) {
                                ModalAction::Close => {
                                    state.close_modal();
                                    state.status_message = "Picker closed".to_string();
                                }
                                ModalAction::Confirm => {
                                    let input_before_confirm = state.input.clone();
                                    process_modal_confirm(&mut state, &mut app).await?;
                                    if state.input != input_before_confirm {
                                        persist_tui_composer_draft(&app, &state);
                                    }
                                }
                                ModalAction::DisconnectProvider => {
                                    process_modal_disconnect(&mut state, &mut app).await?;
                                }
                                ModalAction::None => {}
                            }
                            needs_redraw = true;
                            continue;
                        }

                        let input_before_key = state.input.clone();
                        let should_quit = state.handle_key(key, &mut app);
                        if should_quit {
                            app.interrupt_controller.interrupt(None);
                            abort_and_join_task(&mut active_agent_task).await;
                            if state.processing {
                                finalize_interrupted_tui_session(
                                    &mut app,
                                    &mut state,
                                    "tui_quit",
                                );
                            }
                            app.running = false;
                            break 'main_loop;
                        }
                        let input_changed_by_key = state.input != input_before_key;

                        let is_submit = is_submit_shortcut(&key, &state.input);

                        if is_submit {
                            if state.processing {
                                state.status_message =
                                    "Still processing previous request… wait for completion."
                                        .to_string();
                                needs_redraw = true;
                                continue;
                            }
                            let input = state.input.clone();
                            state.input.clear();
                            state.cursor_position = 0;
                            state.completions.clear();
                            state.completion_index = None;
                            state.jump_to_latest();
                            clear_tui_composer_draft(&app);

                            if !input.is_empty() {
                                let mut handled_by_tui = false;
                                if let Some((cmd, args)) = parse_slash_parts(&input) {
                                    if cmd.eq_ignore_ascii_case("/ask")
                                        || cmd.eq_ignore_ascii_case("/question")
                                    {
                                        match parse_interactive_question_request(&input) {
                                            Ok(request) => {
                                                open_interactive_question_modal(
                                                    &mut state,
                                                    request,
                                                );
                                                state.status_message = "Interactive question ready. Choose an answer.".to_string();
                                            }
                                            Err(message) => {
                                                state.status_message = message.clone();
                                                app.push_ui_assistant(message);
                                            }
                                        }
                                        handled_by_tui = true;
                                    } else if cmd.eq_ignore_ascii_case("/timestamps")
                                        || cmd.eq_ignore_ascii_case("/ts")
                                    {
                                        let (mut message, persist) =
                                            state.apply_timestamps_command(&args);
                                        if let Some(show_timestamps) = persist {
                                            let value =
                                                if show_timestamps { "true" } else { "false" };
                                            if let Err(err) = hermes_config::set_user_config_value(
                                                &app.state_root,
                                                "display.timestamps",
                                                value,
                                            ) {
                                                message.push_str(&format!(
                                                    "\nFailed to save display.timestamps: {err}"
                                                ));
                                            }
                                        }
                                        app.push_ui_assistant(message);
                                        handled_by_tui = true;
                                    } else if cmd.eq_ignore_ascii_case("/model") {
                                        if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")) {
                                            open_model_provider_modal(&mut state, &app).await;
                                            state.status_message = "Choose provider, then model".to_string();
                                            handled_by_tui = true;
                                        } else if args.len() == 1 {
                                            let providers = crate::model_switch::curated_provider_slugs();
                                            if providers.iter().any(|p| p.eq_ignore_ascii_case(&args[0])) {
                                                open_provider_model_modal(&mut state, &app, &args[0].to_ascii_lowercase()).await;
                                                state.status_message = format!("Choose {} model", args[0].to_ascii_lowercase());
                                                handled_by_tui = true;
                                            }
                                        }
                                    } else if cmd.eq_ignore_ascii_case("/personality")
                                        && (args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")))
                                    {
                                        open_personality_modal(&mut state, &app);
                                        state.status_message = "Choose personality".to_string();
                                        handled_by_tui = true;
                                    } else if (cmd.eq_ignore_ascii_case("/skin")
                                        || cmd.eq_ignore_ascii_case("/skins"))
                                        && (args.is_empty()
                                            || (args.len() == 1
                                                && (args[0].eq_ignore_ascii_case("list")
                                                    || args[0].eq_ignore_ascii_case("status")
                                                    || args[0].eq_ignore_ascii_case("show"))))
                                    {
                                        open_skin_modal(&mut state);
                                        state.status_message = "Choose skin".to_string();
                                        handled_by_tui = true;
                                    } else if cmd.eq_ignore_ascii_case("/toolcards")
                                        && args.first().is_some_and(|a| a.eq_ignore_ascii_case("export"))
                                    {
                                        let export_path = hermes_config::hermes_home().join("logs/toolcards-export.txt");
                                        let mut out = String::new();
                                        for msg in app.transcript_messages().iter().filter(|m| m.role == hermes_core::MessageRole::Tool) {
                                            if let Some(content) = msg.content.as_deref() {
                                                out.push_str(content);
                                                out.push_str("\n\n---\n\n");
                                            }
                                        }
                                        if let Err(err) = std::fs::write(&export_path, out) {
                                            state.status_message = format!("Export failed: {}", err);
                                        } else {
                                            state.status_message = format!("Exported tool cards to {}", export_path.display());
                                            app.push_ui_assistant(format!("Exported tool cards to `{}`.", export_path.display()));
                                        }
                                        handled_by_tui = true;
                                    }
                                }

                                if !handled_by_tui {
                                    let trimmed = input.trim().to_string();
                                    if trimmed.starts_with('/') {
                                        let command_name =
                                            trimmed.split_whitespace().next().unwrap_or("/");
                                        if command_name.eq_ignore_ascii_case("/quit")
                                            || command_name.eq_ignore_ascii_case("/exit")
                                        {
                                            app.push_ui_user(trimmed.clone());
                                            app.push_ui_assistant("Goodbye!");
                                            app.running = false;
                                            state.status_message.clear();
                                            state.completions.clear();
                                            state.completion_index = None;
                                            break 'main_loop;
                                        }
                                        state.begin_processing_cycle(&app.current_model);
                                        state.mark_blocking_action(format!(
                                            "running {command_name} command"
                                        ));
                                        state.status_message =
                                            format!("Running {command_name}…");
                                        draw_frame_now(&mut tui, &app, &mut state)?;
                                        let session_before_command = app.session_id.clone();
                                        match app.handle_input(&input).await {
                                            Ok(_) => {
                                                state.finish_processing_cycle("✔ completed in");
                                                if let Some(prefill) =
                                                    app.take_pending_input_prefill()
                                                {
                                                    state.input = prefill;
                                                    state.cursor_position = state.input.len();
                                                    persist_tui_composer_draft(&app, &state);
                                                    state.status_message =
                                                        "Prompt restored for editing. Press Enter to send.".to_string();
                                                } else {
                                                    let restored_after_session_switch =
                                                        app.session_id != session_before_command
                                                            && restore_tui_composer_draft(
                                                                &app, &mut state,
                                                            );
                                                    if !restored_after_session_switch {
                                                        state.status_message.clear();
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                state.finish_processing_cycle("✖ failed after");
                                                state.status_message = format!("Error: {}", e);
                                                state.push_activity(format!("✖ {}", e));
                                                app.push_ui_assistant(format!("Error: {}", e));
                                            }
                                        }
                                    } else if !trimmed.is_empty() {
                                        let managed_turn_required =
                                            should_route_prompt_via_managed_agent(
                                                app.quorum_armed_once,
                                                &app.messages,
                                            );
                                        if managed_turn_required {
                                            // Quorum/system-hint turns must run through App::run_agent
                                            // so fanout orchestration, artifact persistence, and arm/disarm
                                            // behavior remain correct. Run it on a cloned App so the
                                            // render loop can keep drawing live activity while the
                                            // worker mutates/persists the final session state.
                                            let mut worker_app = app.clone();
                                            app.push_ui_user(trimmed.clone());
                                            state.begin_processing_cycle(&app.current_model);
                                            state.mark_blocking_action(
                                                "running managed quorum/system turn",
                                            );
                                            state.status_message =
                                                "Running managed agent turn…".to_string();
                                            draw_frame_now(&mut tui, &app, &mut state)?;
                                            let stream_tx = tui.stream_sender();
                                            let input_for_task = input.clone();
                                            let task = tokio::spawn(async move {
                                                let started = Instant::now();
                                                let result = worker_app
                                                    .handle_input(&input_for_task)
                                                    .await
                                                    .map(|_| Box::new(worker_app))
                                                    .map_err(|e| e.to_string());
                                                let _ = stream_tx.send(Event::ManagedAppRunComplete {
                                                    result,
                                                    elapsed_secs: started.elapsed().as_secs_f64(),
                                                });
                                            });
                                            active_agent_task = Some(task);
                                        } else {
                                            // Non-slash prompts run in a background task so stream events
                                            // can be consumed/rendered live by this UI loop.
                                            app.input_history.push(trimmed.clone());
                                            app.history_index = app.input_history.len();
                                            let user_message = app.prepare_user_message(&trimmed);
                                            app.messages.push(Message::user(user_message));

                                            state.begin_processing_cycle(&app.current_model);
                                            state.status_message = "Processing...".to_string();

                                            let stream_tx = tui.stream_sender();
                                            let agent = app.agent.clone();
                                            let stream_enabled = app.config.streaming.enabled;
                                            let tool_schemas = app.tool_schemas.clone();
                                            let messages = app.messages.clone();
                                            let stream_handle = app.stream_handle.clone();

                                            let task = tokio::spawn(async move {
                                                let started = Instant::now();
                                                let result = if stream_enabled {
                                                    let stream_cb: Option<
                                                        Box<
                                                            dyn Fn(hermes_core::StreamChunk)
                                                                + Send
                                                                + Sync,
                                                        >,
                                                    > = stream_handle.map(|h| {
                                                        Box::new(
                                                            move |chunk: hermes_core::StreamChunk| {
                                                                h.send_chunk(chunk);
                                                            },
                                                        )
                                                            as Box<
                                                                dyn Fn(hermes_core::StreamChunk)
                                                                    + Send
                                                                    + Sync,
                                                            >
                                                    });
                                                    agent.run_stream(
                                                        messages,
                                                        Some(tool_schemas),
                                                        stream_cb,
                                                    )
                                                    .await
                                                } else {
                                                    agent.run(messages, Some(tool_schemas)).await
                                                };
                                                let _ = stream_tx.send(Event::AgentRunComplete {
                                                    result: result.map_err(|e| e.to_string()),
                                                    elapsed_secs: started.elapsed().as_secs_f64(),
                                                });
                                            });
                                            active_agent_task = Some(task);
                                        }
                                    }
                                }
                            }
                        }
                        else if input_changed_by_key {
                            persist_tui_composer_draft(&app, &state);
                        }
                        needs_redraw = true;
                    }
                    Some(Event::Resize(_, _)) => {
                        let _ = tui.terminal.autoresize();
                        state.transcript_cache = TranscriptCache::default();
                        if state.auto_follow_transcript {
                            state.scroll_offset = 0;
                        }
                        needs_redraw = true;
                    }
                    Some(Event::Mouse(mouse)) => {
                        if !app.mouse_enabled() {
                            continue;
                        }
                        use crossterm::event::MouseEventKind;
                        match mouse.kind {
                            MouseEventKind::ScrollUp => {
                                state.scroll_history_up(1);
                            }
                            MouseEventKind::ScrollDown => {
                                state.scroll_history_down(1);
                            }
                            _ => {}
                        }
                        needs_redraw = true;
                    }
                    Some(Event::Message(msg)) => {
                        state.status_message = msg;
                        needs_redraw = true;
                    }
                    Some(Event::AgentRunComplete {
                        result,
                        elapsed_secs,
                    }) => {
                        active_agent_task = None;
                        handle_agent_run_complete(
                            &mut app,
                            &mut state,
                            result,
                            elapsed_secs,
                        );
                        needs_redraw = true;
                    }
                    Some(Event::ManagedAppRunComplete {
                        result,
                        elapsed_secs,
                    }) => {
                        active_agent_task = None;
                        handle_managed_app_run_complete(
                            &mut app,
                            &mut state,
                            result,
                            elapsed_secs,
                        );
                        needs_redraw = true;
                    }
                    Some(Event::Interrupt) => {
                        abort_and_join_task(&mut active_agent_task).await;
                        finalize_interrupted_tui_session(&mut app, &mut state, "interrupt_event");
                        state.finish_processing_cycle("⏹ interrupted after");
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
                        state.active_tools.clear();
                        app.running = false;
                        break 'main_loop;
                    }
                    Some(Event::Shutdown) => {
                        app.interrupt_controller.interrupt(None);
                        abort_and_join_task(&mut active_agent_task).await;
                        finalize_interrupted_tui_session(&mut app, &mut state, "shutdown_signal");
                        state.finish_processing_cycle("⏹ interrupted after");
                        state.stream_buffer.clear();
                        state.stream_muted = false;
                        state.stream_needs_break = false;
                        state.active_tools.clear();
                        app.running = false;
                        break 'main_loop;
                    }
                    Some(Event::StreamDelta(_)) | Some(Event::StreamChunk(_)) | Some(Event::AgentDone) => {
                        // Stream events are consumed on the dedicated stream lane.
                    }
                    None => {
                        // Channel closed
                        break 'main_loop;
                    }
                }
            }
            stream_event = tui.stream_events.recv() => {
                if let Some(first) = stream_event {
                    let mut task_completed =
                        stream_event_completes_background_task(&first);
                    let mut redraw = process_stream_lane_event(&mut app, &mut state, first);
                    let (drain_cap, drain_budget) =
                        stream_lane_budget(state.processing, state.stream_chunk_count);
                    let drain_started = Instant::now();
                    for _ in 0..drain_cap {
                        match tui.stream_events.try_recv() {
                            Ok(next) => {
                                task_completed |= stream_event_completes_background_task(&next);
                                redraw |= process_stream_lane_event(&mut app, &mut state, next);
                                if drain_started.elapsed() >= drain_budget {
                                    break;
                                }
                            }
                            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => break,
                        }
                    }
                    if task_completed {
                        active_agent_task = None;
                    }
                    if redraw {
                        needs_redraw = true;
                    }
                }
            }
            _ = frame_tick.tick() => {
                let previous_jobs = state.background_jobs_running;
                let previous_subagents = state.active_subagents_running;
                if last_jobs_refresh.elapsed() >= Duration::from_secs(1) {
                    state.background_jobs_running = app.running_background_job_count();
                    state.active_subagents_running = app.active_subagent_count();
                    last_jobs_refresh = Instant::now();
                }
                if state.processing {
                    state.tick_spinner();
                    state.maybe_emit_progress_pulse();
                    needs_redraw = true;
                }
                if app.pet_settings().enabled
                    && last_pet_tick.elapsed()
                        >= Duration::from_millis(app.pet_settings().tick_ms.clamp(120, 2000))
                {
                    state.tick_pet();
                    last_pet_tick = Instant::now();
                    needs_redraw = true;
                }
                if previous_jobs != state.background_jobs_running
                    || previous_subagents != state.active_subagents_running
                {
                    needs_redraw = true;
                }
            }
        }
    }

    app.interrupt_controller.interrupt(None);
    abort_and_join_task(&mut active_agent_task).await;
    app.discard_current_session_if_empty();
    if let Some(summary) = app.tool_registry.disk_cleanup_session_end() {
        tracing::debug!(
            deleted = summary.deleted,
            empty_dirs = summary.empty_dirs,
            freed = summary.freed,
            errors = summary.errors.len(),
            "Rust disk-cleanup session-end quick pass completed"
        );
    }
    event_shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    if event_thread.is_finished() {
        let _ = event_thread.join();
    }
    signal_task.abort();
    let _ = signal_task.await;

    // Restore terminal
    tui.restore()
        .map_err(|e| AgentError::Config(e.to_string()))?;

    Ok(())
}
