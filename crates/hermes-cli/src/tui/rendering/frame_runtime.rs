fn env_usize_with_bounds(key: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .map(|v| v.clamp(min, max))
        .unwrap_or(default)
}

fn max_assistant_render_lines() -> usize {
    env_usize_with_bounds(
        "HERMES_TUI_MAX_ASSISTANT_RENDER_LINES",
        DEFAULT_MAX_ASSISTANT_RENDER_LINES,
        40,
        4000,
    )
}

fn max_tool_output_lines() -> usize {
    env_usize_with_bounds(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINES",
        DEFAULT_TOOL_OUTPUT_MAX_LINES,
        20,
        5000,
    )
}

fn max_tool_output_line_chars() -> usize {
    env_usize_with_bounds(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS",
        DEFAULT_TOOL_OUTPUT_MAX_LINE_CHARS,
        120,
        4000,
    )
}

fn max_tool_output_total_chars() -> usize {
    env_usize_with_bounds(
        "HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS",
        DEFAULT_TOOL_OUTPUT_MAX_TOTAL_CHARS,
        2000,
        500_000,
    )
}

fn transcript_wrap_width(viewport_width: u16) -> u16 {
    viewport_width.min(TRANSCRIPT_HARD_WRAP_COLS).max(1)
}

fn stream_lane_budget(processing: bool, chunk_count: usize) -> (usize, Duration) {
    let profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
        .ok()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "balanced".to_string());
    let mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
        .ok()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "advisory".to_string());

    stream_lane_budget_from(mode.as_str(), profile.as_str(), processing, chunk_count)
}

fn stream_lane_budget_from(
    mode: &str,
    profile: &str,
    processing: bool,
    chunk_count: usize,
) -> (usize, Duration) {
    if mode == "off" {
        return (96, Duration::from_millis(6));
    }

    let mut cap = 96usize;
    let mut budget_ms = 6u64;

    match profile {
        "throughput" => {
            cap = 320;
            budget_ms = 16;
        }
        "quality" => {
            cap = 120;
            budget_ms = 8;
        }
        "reliability" => {
            cap = 192;
            budget_ms = 12;
        }
        "safety" => {
            cap = 96;
            budget_ms = 8;
        }
        _ => {}
    }

    if processing && chunk_count > 40 {
        cap = cap.max(224);
        budget_ms = budget_ms.max(12);
    }

    (cap, Duration::from_millis(budget_ms))
}

fn find_anchor_line_index(
    lines: &[Line<'static>],
    anchor_text: &str,
    expected_index: usize,
) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let len = lines.len();
    let center = expected_index.min(len.saturating_sub(1));
    let radius = OFFSET_ANCHOR_SEARCH_RADIUS.min(len.saturating_sub(1));
    let start = center.saturating_sub(radius);
    let end = (center + radius).min(len.saturating_sub(1));

    for (idx, line) in lines.iter().enumerate().take(end + 1).skip(start) {
        if line.to_string() == anchor_text {
            return Some(idx);
        }
    }
    lines
        .iter()
        .position(|line| line.to_string() == anchor_text)
}

/// Render the full TUI frame.
pub fn render(frame: &mut Frame, app: &App, state: &mut TuiState, theme: &Theme) {
    let resolved = theme.resolved_styles();
    let colors = theme.colors.to_ratatui_colors();

    let size = frame.area();
    if size.width == 0 || size.height == 0 {
        return;
    }
    frame.render_widget(
        Block::default().style(Style::default().bg(colors.background)),
        size,
    );

    // Layout: header, body, input, status bar
    let header_height = 1;
    let composer_lines = state.input.matches('\n').count() as u16 + 1;
    let input_height = (composer_lines + 2).clamp(3, 12);
    let status_height = 1;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height), // header
            Constraint::Min(5),                // body
            Constraint::Length(input_height),  // input
            Constraint::Length(status_height), // status
        ])
        .split(size);

    let header_area = vertical[0];
    let body_area = vertical[1];
    let input_area = vertical[2];
    let status_area = vertical[3];

    let details_enabled = state.activity_lane_open && body_area.width >= 86;
    let body_split = if details_enabled {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20), Constraint::Length(38)])
            .split(body_area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(20)])
            .split(body_area)
    };
    let messages_area = body_split[0];
    let details_area = if details_enabled {
        Some(body_split[1])
    } else {
        None
    };

    render_header(frame, app, header_area, &colors);

    // --- Render message history ---
    render_messages(frame, app, state, messages_area, &resolved, &colors);

    if let Some(details_area) = details_area {
        render_live_details(frame, app, state, details_area, &colors);
    }

    // --- Render input area ---
    if let Some(pos) = render_input(frame, state, input_area, &colors) {
        frame.set_cursor_position(pos);
    }

    // --- Render completions as popup above composer ---
    if should_render_completions_popup(state) {
        render_completions_popup(
            frame,
            &state.completions,
            state.completion_index,
            messages_area,
            input_area,
            &colors,
        );
    }

    if let Some(modal) = state.modal.as_ref() {
        render_picker_modal(frame, modal, &colors);
    }

    // --- Render status bar ---
    render_status(frame, app, state, status_area, &colors);
}

fn draw_frame_now(tui: &mut Tui, app: &App, state: &mut TuiState) -> Result<(), AgentError> {
    state.refresh_sticky_prompt(app);
    let active_theme = tui.theme().clone();
    tui.terminal
        .draw(|f| render(f, app, state, &active_theme))
        .map(|_| ())
        .map_err(|e| AgentError::Config(e.to_string()))
}

fn stream_event_completes_background_task(event: &Event) -> bool {
    matches!(
        event,
        Event::AgentRunComplete { .. } | Event::ManagedAppRunComplete { .. }
    )
}

fn should_render_completions_popup(state: &TuiState) -> bool {
    state.mode != InputMode::Normal
        && !state.processing
        && state.modal.is_none()
        && state.input.starts_with('/')
        && !state.input.contains('\n')
        && !state.history_search_active
        && !state.completions.is_empty()
}

fn restore_tui_composer_draft(app: &App, state: &mut TuiState) -> bool {
    match app.load_current_composer_draft() {
        Ok(Some(draft)) if !draft.trim().is_empty() => {
            state.input = draft;
            state.cursor_position = state.input.len();
            state.refresh_completions_for_app(Some(app));
            state.status_message = "Restored unsent composer draft for this session.".to_string();
            true
        }
        Ok(_) => false,
        Err(err) => {
            tracing::warn!(error = %err, "failed to restore composer draft");
            false
        }
    }
}

fn persist_tui_composer_draft(app: &App, state: &TuiState) {
    if let Err(err) = app.persist_current_composer_draft(&state.input) {
        tracing::warn!(error = %err, "failed to persist composer draft");
    }
}

fn clear_tui_composer_draft(app: &App) {
    if let Err(err) = app.clear_current_composer_draft() {
        tracing::warn!(error = %err, "failed to clear composer draft");
    }
}

fn should_route_prompt_via_managed_agent(quorum_armed_once: bool, messages: &[Message]) -> bool {
    if quorum_armed_once {
        return true;
    }
    messages.iter().any(|message| {
        message.role == hermes_core::MessageRole::System
            && message
                .content
                .as_deref()
                .unwrap_or_default()
                .starts_with("[QUORUM_MODE] ")
    })
}

