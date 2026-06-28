fn render_header(frame: &mut Frame, app: &App, area: Rect, colors: &crate::theme::RatatuiColors) {
    let session_short = &app.session_id[..8.min(app.session_id.len())];
    let chrome = format!(
        "  •  session {}  •  Enter send  •  Shift+Enter/Ctrl+J newline  •  / commands  •  Ctrl+L lane  •  Ctrl+O cockpit  •  Ctrl+G refresh-tail",
        session_short
    );
    let available = area.width.saturating_sub(28) as usize;
    let text = Text::from(vec![Line::from(vec![
        Span::styled(
            " ▓ HERMES ",
            Style::default()
                .fg(colors.status_bar_strong)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "AGENT ULTRA",
            Style::default()
                .fg(colors.accent)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            truncate_chars(&chrome, available),
            Style::default()
                .fg(colors.status_bar_text)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ])]);
    let title = Paragraph::new(text)
        .block(Block::default().style(Style::default().bg(colors.status_bar_bg)));
    frame.render_widget(title, area);
}

fn render_live_details(
    frame: &mut Frame,
    app: &App,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lane_title = match state.activity_lane_mode {
        ActivityLaneMode::Live => " Activity Lane ",
        ActivityLaneMode::Cockpit => " Ops Cockpit ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(lane_title)
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_dim));
    let mut rows: Vec<Line<'static>> = Vec::new();

    if matches!(state.activity_lane_mode, ActivityLaneMode::Cockpit) {
        rows.push(Line::from(vec![
            Span::styled(
                " mode: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                if state.processing {
                    format!(
                        "processing ({:.1}s)",
                        state.processing_elapsed().as_secs_f64()
                    )
                } else {
                    "idle".to_string()
                },
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        rows.push(Line::from(vec![
            Span::styled(
                " model: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                truncate_chars(&app.current_model, area.width.saturating_sub(10) as usize),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
        rows.push(Line::from(vec![
            Span::styled(
                " planner: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                std::env::var("HERMES_PLAN_CAPABILITY_ROUTER")
                    .unwrap_or_else(|_| "off".to_string()),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
            Span::styled(
                "  compaction: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                std::env::var("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE")
                    .unwrap_or_else(|_| "advisory".to_string()),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
        rows.push(Line::from(vec![
            Span::styled(
                " policy: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                format!(
                    "preset={} mode={} skills={}",
                    std::env::var("HERMES_TOOL_POLICY_PRESET")
                        .unwrap_or_else(|_| "balanced".to_string()),
                    std::env::var("HERMES_TOOL_POLICY_MODE")
                        .unwrap_or_else(|_| "enforce".to_string()),
                    std::env::var("HERMES_SKILLS_EXECUTION_TIER")
                        .unwrap_or_else(|_| "balanced".to_string()),
                ),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
        if let Some((prompt, completion, total)) = state.last_usage {
            rows.push(Line::from(vec![
                Span::styled(
                    " usage: ",
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                ),
                Span::styled(
                    format!("in={} out={} total={}", prompt, completion, total),
                    Style::default()
                        .fg(colors.status_bar_text)
                        .bg(colors.background),
                ),
            ]));
        }
        rows.push(Line::from(vec![Span::styled(
            " Ctrl+O live lane",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));

        let paragraph = Paragraph::new(Text::from(rows))
            .block(block)
            .wrap(Wrap { trim: true });
        frame.render_widget(Clear, area);
        frame.render_widget(paragraph, area);
        return;
    }

    if state.processing {
        let elapsed = state.processing_elapsed().as_secs_f64();
        rows.push(Line::from(vec![
            Span::styled(
                " ⟳ processing ",
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{elapsed:.1}s"),
                Style::default()
                    .fg(colors.accent)
                    .bg(colors.background)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" • {}", state.processing_stage_label()),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
        rows.push(Line::from(vec![Span::styled(
            format!(
                " [{}] chunks:{} chars:{} phase:{}% {}",
                animated_processing_bar(state.spinner_frame, 18),
                state.stream_chunk_count,
                state.stream_char_count,
                state.processing_phase_progress,
                truncate_chars(
                    if state.processing_phase_label.is_empty() {
                        state.processing_phase.as_str()
                    } else {
                        state.processing_phase_label.as_str()
                    },
                    38
                )
            ),
            Style::default().fg(colors.accent).bg(colors.background),
        )]));
        if state.processing_degraded {
            rows.push(Line::from(vec![Span::styled(
                format!(
                    " ⚠ fallback active: {}",
                    truncate_chars(&state.degraded_notes.join(" | "), 120)
                ),
                Style::default()
                    .fg(colors.status_bar_warn)
                    .bg(colors.background),
            )]));
        }
    }

    if !state.active_tools.is_empty() {
        rows.push(Line::from(vec![
            Span::styled(
                " tools: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                truncate_chars(&state.active_tools.join(", "), 120),
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    } else if state.processing {
        rows.push(Line::from(vec![Span::styled(
            " tools: awaiting tool events…",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }

    if !state.live_thinking.is_empty() {
        rows.push(Line::from(vec![
            Span::styled(
                " thinking: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                if crate::commands::reasoning_full_enabled() {
                    state.live_thinking.clone()
                } else {
                    truncate_chars(&state.live_thinking, 140)
                },
                Style::default().fg(colors.accent).bg(colors.background),
            ),
        ]));
    }

    if let Some((prompt, completion, total)) = state.last_usage {
        rows.push(Line::from(vec![
            Span::styled(
                " usage: ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ),
            Span::styled(
                format!("in={} out={} total={}", prompt, completion, total),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
    }

    let recent_cap = area.height.saturating_sub(rows.len() as u16 + 3) as usize;
    for event in state
        .recent_activity
        .iter()
        .rev()
        .take(recent_cap.max(2))
        .rev()
    {
        rows.push(Line::from(vec![
            Span::styled(
                " • ",
                Style::default().fg(colors.accent).bg(colors.background),
            ),
            Span::styled(
                truncate_chars(event, area.width.saturating_sub(8) as usize),
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.background),
            ),
        ]));
    }

    if rows.is_empty() {
        rows.push(Line::from(vec![Span::styled(
            " waiting for activity…",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    rows.push(Line::from(vec![Span::styled(
        " Ctrl+L toggle lane • Ctrl+O cockpit",
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background)
            .add_modifier(Modifier::ITALIC),
    )]));

    let paragraph = Paragraph::new(Text::from(rows))
        .block(block)
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

