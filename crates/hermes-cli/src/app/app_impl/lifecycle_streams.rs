impl App {
    fn stream_callbacks(shared: Arc<StdMutex<Option<StreamHandle>>>) -> AgentCallbacks {
        let (observer_sink, mut observer_rx) =
            hermes_agent::AgentObserverSink::channel(256);
        let _ = std::thread::Builder::new()
            .name("hermes-app-observer".to_string())
            .spawn(move || {
                while let Some(event) = observer_rx.blocking_recv() {
                    match event {
                        hermes_agent::AgentObserverEvent::Thinking(thinking) => {
                            let preview = App::preview_for_status(&thinking, 220);
                            if !preview.is_empty() {
                                App::push_stream_ui_event(
                                    &shared,
                                    crate::tui::StreamUiEvent::Thinking(preview),
                                );
                            }
                        }
                        hermes_agent::AgentObserverEvent::ToolStarted {
                            name,
                            args_preview,
                        } => {
                            let arg_preview = App::preview_for_status(&args_preview, 140);
                            App::push_stream_ui_event(
                                &shared,
                                crate::tui::StreamUiEvent::ToolStarted {
                                    name,
                                    args_preview: arg_preview,
                                },
                            );
                        }
                        hermes_agent::AgentObserverEvent::ToolCompleted {
                            name,
                            result_preview,
                        } => {
                            let preview = App::preview_for_status(&result_preview, 160);
                            App::push_stream_ui_event(
                                &shared,
                                crate::tui::StreamUiEvent::ToolCompleted {
                                    name,
                                    result_preview: preview,
                                },
                            );
                        }
                        hermes_agent::AgentObserverEvent::Status { kind, message } => {
                            let preview = App::preview_for_status(&message, 200);
                            if !preview.is_empty() {
                                App::push_stream_ui_event(
                                    &shared,
                                    crate::tui::StreamUiEvent::Status {
                                        kind,
                                        message: preview,
                                    },
                                );
                            }
                        }
                        hermes_agent::AgentObserverEvent::StepCompleted(_)
                        | hermes_agent::AgentObserverEvent::RenderDelta(_)
                        | hermes_agent::AgentObserverEvent::Usage(_) => {}
                    }
                }
            });
        AgentCallbacks {
            observer_sink: Some(observer_sink),
        }
    }

    fn emit_lifecycle_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        message: impl AsRef<str>,
    ) {
        let preview = App::preview_for_status(message.as_ref(), 220);
        if preview.is_empty() {
            return;
        }
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[lifecycle] {}", preview);
        }
        App::push_stream_ui_event(
            shared,
            crate::tui::StreamUiEvent::Lifecycle(preview),
        );
    }

    fn emit_phase_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        phase: &str,
        label: &str,
        progress_pct: u8,
    ) {
        let phase = phase.trim();
        let label = App::preview_for_status(label, 220);
        if phase.is_empty() || label.is_empty() {
            return;
        }
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[phase {:>3}%] {}: {}", progress_pct.min(100), phase, label);
        }
        App::push_stream_ui_event(
            shared,
            crate::tui::StreamUiEvent::Phase {
                phase: phase.to_string(),
                label,
                progress_pct: progress_pct.min(100),
            },
        );
    }

    fn emit_moa_reference_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        index: usize,
        count: usize,
        model: &str,
        text: &str,
    ) {
        let label = App::preview_for_status(model, 96);
        let text = App::preview_for_status(text, 1200);
        if label.is_empty() || text.is_empty() {
            return;
        }
        if App::oneshot_lifecycle_stdout_enabled(shared) {
            println!("[moa reference {}/{}] {}: {}", index, count, label, text);
        }
        App::push_stream_ui_event(
            shared,
            crate::tui::StreamUiEvent::MoaReference {
                index: u64::try_from(index).unwrap_or(u64::MAX),
                count: u64::try_from(count).unwrap_or(u64::MAX),
                label,
                text,
            },
        );
    }

    fn oneshot_lifecycle_stdout_enabled(shared: &Arc<StdMutex<Option<StreamHandle>>>) -> bool {
        let stream_attached = shared
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|_| ()))
            .is_some();
        if stream_attached {
            return false;
        }
        matches!(
            std::env::var("HERMES_ONESHOT_LIFECYCLE_STDOUT")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "1" | "true" | "yes" | "on")
        )
    }

    fn objective_context_autopin_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_OBJECTIVE_CONTEXT_AUTOPIN")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn sanitize_topic_path_segment(raw: &str) -> String {
        let mut out = String::with_capacity(raw.len());
        for ch in raw.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/') {
                out.push(ch);
            } else {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }

    fn maybe_autopin_contextlattice_topic_from_objective(&self) {
        if !Self::objective_context_autopin_enabled() {
            return;
        }
        let Ok(Some(contract)) = load_objective_contract() else {
            return;
        };
        let objective_id = Self::sanitize_topic_path_segment(contract.id.trim());
        if objective_id.is_empty() {
            return;
        }
        let target_topic = format!("runbooks/objective/{}", objective_id);
        let current_topic = std::env::var("CONTEXTLATTICE_TOPIC_PATH")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let should_override = match current_topic.as_deref() {
            None => true,
            Some("runbooks/hermes") => true,
            Some(existing)
                if existing.eq_ignore_ascii_case(target_topic.as_str())
                    || !existing
                        .to_ascii_lowercase()
                        .starts_with("runbooks/objective/") =>
            {
                false
            }
            Some(_) => true,
        };
        if should_override {
            std::env::set_var("CONTEXTLATTICE_TOPIC_PATH", &target_topic);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "ContextLattice objective autopin set topic_path={} (objective_id={})",
                    target_topic, contract.id
                ),
            );
            Self::emit_phase_event(
                &self.stream_handle_shared,
                "context",
                "objective context autopin",
                8,
            );
        }
    }

}
