// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const TRANSCRIPT_HARD_WRAP_COLS: u16 = 80;
const TRANSCRIPT_CONTENT_WRAP_COLS: usize = 76;
const OFFSET_ANCHOR_SEARCH_RADIUS: usize = 1200;
const DEFAULT_MAX_ASSISTANT_RENDER_LINES: usize = 260;
const MAX_STREAM_RENDER_LINES: usize = 140;
const DEFAULT_TOOL_OUTPUT_MAX_LINES: usize = 16;
const DEFAULT_TOOL_OUTPUT_MAX_LINE_CHARS: usize = 600;
const DEFAULT_TOOL_OUTPUT_MAX_TOTAL_CHARS: usize = 1_024;

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

/// Render the message history area.
fn role_visuals(
    role: hermes_core::MessageRole,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> (&'static str, &'static str, Style, Style) {
    let role_bg = colors.background;
    match role {
        hermes_core::MessageRole::User => (
            "◆",
            "USER",
            styles.user_input.bg(role_bg),
            styles
                .user_input
                .remove_modifier(Modifier::BOLD)
                .bg(role_bg),
        ),
        hermes_core::MessageRole::Assistant => (
            "●",
            "HERMES",
            styles.assistant_response.bg(role_bg),
            styles.assistant_response.bg(role_bg),
        ),
        hermes_core::MessageRole::System => (
            "◇",
            "SYSTEM",
            styles.system_message.bg(role_bg),
            styles.system_message.bg(role_bg),
        ),
        hermes_core::MessageRole::Tool => (
            "◈",
            "TOOL",
            styles.tool_call.bg(role_bg),
            Style::default().fg(colors.status_bar_text).bg(role_bg),
        ),
    }
}

fn render_inline_with_code(
    prefix: &str,
    text: &str,
    base_style: Style,
    code_style: Style,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), base_style));
    }

    let mut in_code = false;
    let mut current = String::new();
    for ch in text.chars() {
        if ch == '`' {
            if !current.is_empty() {
                spans.push(Span::styled(
                    std::mem::take(&mut current),
                    if in_code { code_style } else { base_style },
                ));
            }
            in_code = !in_code;
            continue;
        }
        current.push(ch);
    }
    if !current.is_empty() {
        spans.push(Span::styled(
            current,
            if in_code { code_style } else { base_style },
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    Line::from(spans)
}

fn parse_markdown_numbered_marker(line: &str) -> Option<(&str, &str)> {
    let digits = line
        .char_indices()
        .take_while(|(_, ch)| ch.is_ascii_digit())
        .last()
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);
    if digits == 0 {
        return None;
    }
    let rest = &line[digits..];
    if let Some(tail) = rest.strip_prefix(". ") {
        return Some((&line[..digits + 1], tail));
    }
    if let Some(tail) = rest.strip_prefix(") ") {
        return Some((&line[..digits + 1], tail));
    }
    None
}

fn keyword_set_for_lang(lang: &str) -> &'static [&'static str] {
    match lang.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => &[
            "fn", "let", "mut", "pub", "impl", "struct", "enum", "match", "if", "else", "for",
            "while", "loop", "return", "async", "await", "use", "mod", "trait", "where",
        ],
        "python" | "py" => &[
            "def", "class", "if", "elif", "else", "for", "while", "return", "import", "from",
            "with", "as", "try", "except", "finally", "lambda", "yield", "async", "await",
        ],
        "javascript" | "js" | "typescript" | "ts" => &[
            "function", "const", "let", "var", "if", "else", "for", "while", "return", "class",
            "import", "export", "await", "async", "switch", "case", "break", "new",
        ],
        "json" => &[],
        "bash" | "sh" | "zsh" => &[
            "if", "then", "else", "fi", "for", "do", "done", "case", "esac", "function", "echo",
            "export",
        ],
        _ => &[],
    }
}

fn render_highlighted_code_line(
    line: &str,
    lang: &str,
    colors: &crate::theme::RatatuiColors,
) -> Line<'static> {
    let default_style = Style::default()
        .fg(colors.status_bar_text)
        .bg(colors.background);
    let keyword_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let string_style = Style::default()
        .fg(colors.status_bar_warn)
        .bg(colors.background);
    let number_style = Style::default()
        .fg(colors.status_bar_good)
        .bg(colors.background);
    let punctuation_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background);
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        "    │ ",
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background),
    )];
    let keywords = keyword_set_for_lang(lang);
    let mut token = String::new();
    let mut in_string = false;
    let mut quote_char = '\0';
    let flush_token =
        |spans: &mut Vec<Span<'static>>, token: &mut String, style: Style, keywords: &[&str]| {
            if token.is_empty() {
                return;
            }
            let tok = std::mem::take(token);
            let tok_style = if keywords.iter().any(|kw| kw.eq_ignore_ascii_case(&tok)) {
                style
            } else if tok.chars().all(|ch| ch.is_ascii_digit()) {
                Style::default()
                    .fg(Color::Cyan)
                    .bg(style.bg.unwrap_or(Color::Reset))
            } else {
                default_style
            };
            spans.push(Span::styled(tok, tok_style));
        };

    for ch in line.chars() {
        if in_string {
            token.push(ch);
            if ch == quote_char {
                spans.push(Span::styled(std::mem::take(&mut token), string_style));
                in_string = false;
                quote_char = '\0';
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            flush_token(&mut spans, &mut token, keyword_style, keywords);
            in_string = true;
            quote_char = ch;
            token.push(ch);
            continue;
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            token.push(ch);
            continue;
        }
        flush_token(&mut spans, &mut token, keyword_style, keywords);
        if ch.is_ascii_digit() {
            spans.push(Span::styled(ch.to_string(), number_style));
        } else if ch.is_whitespace() {
            spans.push(Span::styled(ch.to_string(), default_style));
        } else {
            spans.push(Span::styled(ch.to_string(), punctuation_style));
        }
    }
    flush_token(&mut spans, &mut token, keyword_style, keywords);
    if in_string && !token.is_empty() {
        spans.push(Span::styled(token, string_style));
    }
    Line::from(spans)
}

fn parse_table_cells(line: &str) -> Option<Vec<String>> {
    let trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }
    let cells: Vec<String> = trimmed
        .split('|')
        .map(str::trim)
        .filter(|cell| !cell.is_empty())
        .map(ToString::to_string)
        .collect();
    if cells.len() < 2 {
        return None;
    }
    Some(cells)
}

fn content_width_for_table_row(cells: usize, min_per_cell: usize) -> usize {
    cells.saturating_mul(min_per_cell).max(8)
}

fn message_fingerprint(msg: &hermes_core::Message) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    let role_tag = match msg.role {
        hermes_core::MessageRole::System => "system",
        hermes_core::MessageRole::User => "user",
        hermes_core::MessageRole::Assistant => "assistant",
        hermes_core::MessageRole::Tool => "tool",
    };
    role_tag.hash(&mut hasher);
    msg.content.hash(&mut hasher);
    msg.tool_call_id.hash(&mut hasher);
    msg.reasoning_content.hash(&mut hasher);
    if let Some(calls) = msg.tool_calls.as_ref() {
        for tc in calls {
            tc.id.hash(&mut hasher);
            tc.function.name.hash(&mut hasher);
            tc.function.arguments.hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn transcript_fingerprint(messages: &[hermes_core::Message], state: &TuiState, width: u16) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    width.hash(&mut hasher);
    state.stream_buffer.hash(&mut hasher);
    state.show_timestamps.hash(&mut hasher);
    state.view_density.hash(&mut hasher);
    let mut expanded = state.expanded_tool_cards.iter().collect::<Vec<_>>();
    expanded.sort();
    for key in expanded {
        key.hash(&mut hasher);
    }
    for msg in messages {
        message_fingerprint(msg).hash(&mut hasher);
    }
    hasher.finish()
}

fn transcript_message_fingerprints(messages: &[hermes_core::Message]) -> Vec<u64> {
    messages.iter().map(message_fingerprint).collect()
}

fn count_renderable_messages(messages: &[hermes_core::Message]) -> usize {
    messages
        .iter()
        .filter(|msg| !matches!(msg.role, hermes_core::MessageRole::System))
        .count()
}

fn looks_like_internal_scaffold_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    let lowered = trimmed.to_ascii_lowercase();
    trimmed.starts_with("to=functions.")
        || trimmed.starts_with("to=tools.")
        || trimmed.starts_with("to=memory.")
        || trimmed.starts_with("->functions.")
        || trimmed.contains(" to=functions.")
        || trimmed.starts_with("<tool_call")
        || trimmed.starts_with("</tool_call")
        || trimmed.starts_with("<tool_use")
        || trimmed.starts_with("</tool_use")
        || trimmed.starts_with("<name>")
        || trimmed.starts_with("</name>")
        || trimmed.starts_with("<arguments>")
        || trimmed.starts_with("</arguments>")
        || trimmed.starts_with("<assistant(")
        || trimmed.starts_with("</assistant(")
        || trimmed.contains("(INVOKN_RESULT")
        || lowered.contains("<tool_use>")
        || lowered.contains("</tool_use>")
        || lowered.contains("<tool_call")
        || lowered.contains("</tool_call")
        || lowered.contains("<arguments>")
        || lowered.contains("</arguments>")
        || lowered.contains("<name>")
        || lowered.contains("</name>")
        || lowered.contains("<argument name=")
        || lowered.contains("</argument>")
        || lowered.contains("&lt;tool_use")
        || lowered.contains("&lt;/tool_use")
        || lowered.contains("&lt;tool_call")
        || lowered.contains("&lt;/tool_call")
        || lowered.contains("\\u003ctool_use")
        || lowered.contains("\\u003c/tool_use")
        || lowered.contains("\\u003ctool_call")
        || lowered.contains("\\u003c/tool_call")
        || lowered.contains("invoke_result")
        || lowered.contains("invokn_result")
        || lowered.contains("to=functions.")
        || lowered.contains("to=tools.")
        || lowered.contains("to=memory.")
}

fn strict_default_language_output_enabled() -> bool {
    match std::env::var("HERMES_TUI_STRICT_DEFAULT_LANGUAGE") {
        Ok(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Err(_) => true,
    }
}

fn sanitize_line_to_default_language_ascii(line: &str, compact_ws: bool) -> Option<String> {
    let leading_len = line.len().saturating_sub(line.trim_start().len());
    let leading = &line[..leading_len];
    let mut body = String::new();
    let mut prev_space = false;
    for ch in line[leading_len..].chars() {
        if ch.is_ascii_graphic() {
            body.push(ch);
            prev_space = false;
            continue;
        }

        if ch.is_ascii_whitespace() {
            if compact_ws {
                if !prev_space {
                    body.push(' ');
                    prev_space = true;
                }
            } else {
                body.push(' ');
                prev_space = true;
            }
            continue;
        }

        if !prev_space {
            body.push(' ');
            prev_space = true;
        }
    }
    let body = if compact_ws {
        body.trim().to_string()
    } else {
        body.trim_end().to_string()
    };
    if body.trim().is_empty() {
        return None;
    }
    let ascii_letters = body.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let ascii_graphics = body.chars().filter(|c| c.is_ascii_graphic()).count();
    if ascii_graphics > 0 && ascii_letters == 0 {
        let symbolic_ratio = body.chars().filter(|c| !c.is_ascii_alphanumeric()).count() as f64
            / ascii_graphics as f64;
        if symbolic_ratio > 0.85 {
            return None;
        }
    }
    let lower = body.to_ascii_lowercase();
    if lower.contains("<tool_call")
        || lower.contains("</tool_call")
        || lower.contains("<tool_use>")
        || lower.contains("</tool_use>")
    {
        return None;
    }
    Some(format!("{leading}{body}"))
}

fn render_assistant_markdown_lines(
    content: &str,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    let mut rendered: Vec<Line<'static>> = Vec::new();
    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut hidden_scaffold_lines = 0usize;
    let code_frame_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background);
    let heading_style = Style::default()
        .fg(colors.status_bar_strong)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let bullet_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);
    let quote_style = Style::default()
        .fg(colors.status_bar_dim)
        .bg(colors.background)
        .add_modifier(Modifier::ITALIC);
    let inline_code_style = Style::default()
        .fg(colors.accent)
        .bg(colors.background)
        .add_modifier(Modifier::BOLD);

    let strict_gate = strict_default_language_output_enabled();
    for raw in content.lines() {
        let normalized = if strict_gate {
            sanitize_line_to_default_language_ascii(raw, false).unwrap_or_default()
        } else if looks_like_internal_scaffold_line(raw) {
            sanitize_line_to_default_language_ascii(raw, true).unwrap_or_default()
        } else {
            raw.to_string()
        };
        if normalized.is_empty() {
            continue;
        }
        let raw = normalized.as_str();
        if looks_like_internal_scaffold_line(raw) {
            hidden_scaffold_lines = hidden_scaffold_lines.saturating_add(1);
            continue;
        }
        let trimmed = raw.trim_start();
        let is_fence = trimmed.starts_with("```") || trimmed.starts_with("~~~");
        if is_fence {
            if in_code_block {
                rendered.push(Line::from(vec![Span::styled(
                    "    └─ end code",
                    code_frame_style,
                )]));
                in_code_block = false;
                code_lang.clear();
            } else {
                in_code_block = true;
                code_lang = trimmed
                    .trim_start_matches('`')
                    .trim_start_matches('~')
                    .trim()
                    .to_string();
                let label = if code_lang.is_empty() {
                    "    ┌─ code".to_string()
                } else {
                    format!("    ┌─ code ({})", code_lang)
                };
                rendered.push(Line::from(vec![Span::styled(label, code_frame_style)]));
            }
            continue;
        }

        if in_code_block {
            rendered.push(render_highlighted_code_line(raw, &code_lang, colors));
            continue;
        }

        if trimmed.is_empty() {
            rendered.push(Line::from(String::new()));
            continue;
        }

        let heading_level = trimmed.chars().take_while(|ch| *ch == '#').count();
        if (1..=6).contains(&heading_level) {
            // Avoid byte-index slicing with a char-count offset on multibyte text.
            let rest = trimmed.trim_start_matches('#').trim_start();
            if !rest.is_empty() {
                rendered.push(Line::from(vec![
                    Span::styled(
                        format!("    {} ", "#".repeat(heading_level)),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ),
                    Span::styled(rest.to_string(), heading_style),
                ]));
                continue;
            }
        }

        if let Some(quote) = trimmed.strip_prefix('>').map(str::trim_start) {
            rendered.push(render_inline_with_code(
                "    ▎ ",
                quote,
                quote_style,
                inline_code_style,
            ));
            continue;
        }

        if let Some(body) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            rendered.push(Line::from(vec![
                Span::styled("    • ", bullet_style),
                Span::styled(
                    body.to_string(),
                    styles.assistant_response.bg(colors.background),
                ),
            ]));
            continue;
        }

        if let Some(cells) = parse_table_cells(trimmed) {
            let separator = cells
                .iter()
                .all(|cell| cell.chars().all(|ch| ch == '-' || ch == ':'));
            if separator {
                rendered.push(Line::from(vec![Span::styled(
                    format!(
                        "    ├{}┤",
                        "─".repeat(content_width_for_table_row(cells.len(), 16))
                    ),
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                continue;
            }
            let mut row_spans: Vec<Span<'static>> = vec![Span::styled(
                "    │ ",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            )];
            for (idx, cell) in cells.iter().enumerate() {
                if idx > 0 {
                    row_spans.push(Span::styled(
                        " │ ",
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ));
                }
                row_spans.push(Span::styled(
                    truncate_chars(cell, 24),
                    Style::default()
                        .fg(colors.status_bar_text)
                        .bg(colors.background),
                ));
            }
            row_spans.push(Span::styled(
                " │",
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            ));
            rendered.push(Line::from(row_spans));
            continue;
        }

        if let Some((marker, body)) = parse_markdown_numbered_marker(trimmed) {
            rendered.push(Line::from(vec![
                Span::styled(format!("    {marker} "), bullet_style),
                Span::styled(
                    body.to_string(),
                    styles.assistant_response.bg(colors.background),
                ),
            ]));
            continue;
        }

        for segment in hard_wrap_segments(trimmed, TRANSCRIPT_CONTENT_WRAP_COLS) {
            rendered.push(render_inline_with_code(
                "    ",
                &segment,
                styles.assistant_response,
                inline_code_style,
            ));
        }
    }

    if in_code_block {
        rendered.push(Line::from(vec![Span::styled(
            "    └─ end code",
            code_frame_style,
        )]));
    }
    if hidden_scaffold_lines > 0 {
        rendered.push(Line::from(vec![Span::styled(
            format!(
                "    [internal orchestration scaffold hidden: {} lines]",
                hidden_scaffold_lines
            ),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    rendered
}

fn collapse_render_lines_with_notice(
    lines: Vec<Line<'static>>,
    max_lines: usize,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    if lines.len() <= max_lines.max(1) {
        return lines;
    }
    let cap = max_lines.max(8);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(cap + 2);
    let head = (cap * 2) / 3;
    let tail = cap.saturating_sub(head).saturating_sub(1);
    let total = lines.len();
    out.extend(lines.iter().take(head).cloned());
    out.push(Line::from(vec![Span::styled(
        format!(
            "    … transcript compressed for readability ({} lines hidden)",
            total.saturating_sub(head + tail)
        ),
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background)
            .add_modifier(Modifier::ITALIC),
    )]));
    if tail > 0 {
        out.extend(lines.into_iter().skip(total.saturating_sub(tail)));
    }
    out
}

fn tail_render_lines_with_notice(
    lines: Vec<Line<'static>>,
    max_lines: usize,
    colors: &crate::theme::RatatuiColors,
) -> Vec<Line<'static>> {
    if lines.len() <= max_lines.max(1) {
        return lines;
    }
    let keep = max_lines.max(4);
    let total = lines.len();
    let mut out = Vec::with_capacity(keep + 1);
    out.push(Line::from(vec![Span::styled(
        format!(
            "    … live stream trimmed (showing last {} of {} lines)",
            keep, total
        ),
        Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background)
            .add_modifier(Modifier::ITALIC),
    )]));
    out.extend(lines.into_iter().skip(total.saturating_sub(keep)));
    out
}

fn value_to_display_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(trimmed) {
                    return serde_json::to_string_pretty(&parsed)
                        .unwrap_or_else(|_| raw.to_string());
                }
            }
            raw.to_string()
        }
        _ => serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn push_block(lines: &mut Vec<String>, header: &str, value: &serde_json::Value) {
    let rendered = value_to_display_text(value);
    if rendered.trim().is_empty() {
        return;
    }
    lines.push(format!("[{header}]"));
    for line in rendered.lines() {
        lines.push(line.to_string());
    }
}

fn sanitize_tool_line(raw: &str) -> String {
    let sanitized =
        sanitize_line_to_default_language_ascii(raw, false).unwrap_or_else(|| String::new());
    truncate_chars(&sanitized, max_tool_output_line_chars())
}

fn finalize_tool_message_lines(raw_lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut total_chars = 0usize;
    let mut omitted = 0usize;
    let max_lines = max_tool_output_lines();
    let max_total_chars = max_tool_output_total_chars();
    for line in raw_lines {
        let sanitized = sanitize_tool_line(&line);
        let line_chars = sanitized.chars().count();
        let next_total = total_chars.saturating_add(line_chars);
        if out.len() < max_lines && next_total <= max_total_chars {
            total_chars = next_total;
            out.push(sanitized);
        } else {
            omitted = omitted.saturating_add(1);
        }
    }
    if omitted > 0 {
        out.push(format!(
            "… tool output truncated ({} lines omitted)",
            omitted
        ));
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

fn format_tool_message_lines(content: &str) -> Vec<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }

    let parsed = match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(v) => v,
        Err(_) => {
            return finalize_tool_message_lines(
                content
                    .lines()
                    .map(std::string::ToString::to_string)
                    .collect(),
            );
        }
    };

    if let Some(obj) = parsed.as_object() {
        let mut lines: Vec<String> = Vec::new();

        if let Some(w) = obj.get("_budget_warning").and_then(|v| v.as_str()) {
            lines.push(format!("⚠ {}", w.trim()));
        }

        for key in ["result", "error", "stdout", "stderr", "message"] {
            if let Some(value) = obj.get(key) {
                push_block(&mut lines, key, value);
            }
        }
        if let Some(remediation) = tool_policy_remediation_from_payload(obj) {
            lines.push("[remediation]".to_string());
            for row in remediation {
                lines.push(format!("- {}", row));
            }
        }

        let mut extras = serde_json::Map::new();
        for (k, v) in obj.iter() {
            if k == "_budget_warning"
                || k == "result"
                || k == "error"
                || k == "stdout"
                || k == "stderr"
                || k == "message"
            {
                continue;
            }
            extras.insert(k.clone(), v.clone());
        }
        if !extras.is_empty() {
            push_block(&mut lines, "meta", &serde_json::Value::Object(extras));
        }
        if !lines.is_empty() {
            return finalize_tool_message_lines(lines);
        }
    }

    finalize_tool_message_lines(
        serde_json::to_string_pretty(&parsed)
            .map(|s| s.lines().map(std::string::ToString::to_string).collect())
            .unwrap_or_else(|_| {
                content
                    .lines()
                    .map(std::string::ToString::to_string)
                    .collect()
            }),
    )
}

fn tool_policy_remediation_from_payload(
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Option<Vec<String>> {
    let code = obj
        .get("policy")
        .and_then(|p| p.get("code"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let error_text = obj
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let blocked = error_text.contains("blocked by tool policy")
        || error_text.contains("denied by security policy")
        || !code.is_empty();
    if !blocked {
        return None;
    }

    let mut rows = Vec::new();
    match code.as_str() {
        "params_pattern_denied" => {
            rows.push(
                "Remove secret-like parameter names from tool args; pass secrets via local env/vault.".to_string(),
            );
            rows.push(
                "Retry with sanitized args that reference variable names, not credential material."
                    .to_string(),
            );
        }
        "params_too_large" => {
            rows.push(
                "Reduce payload size and pass only minimal fields required by the tool."
                    .to_string(),
            );
        }
        "tool_denylisted" | "tool_not_allowlisted" => {
            rows.push(
                "Switch to an approved tool surface (`/tools`) for this operation.".to_string(),
            );
        }
        "sandbox_profile_violation" => {
            rows.push(
                "Command matched sandbox denial pattern; use a safer equivalent command path."
                    .to_string(),
            );
            rows.push(
                "If necessary, change runtime sandbox policy explicitly before retrying."
                    .to_string(),
            );
        }
        _ => {
            rows.push(
                "Review policy decision details in `/ops status` and retry with safer parameters."
                    .to_string(),
            );
        }
    }
    Some(rows)
}

fn append_transcript_message_lines(
    lines: &mut Vec<Line<'static>>,
    msg: &hermes_core::Message,
    msg_idx: usize,
    rendered_messages: &mut usize,
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    divider: &str,
) {
    // Hide internal orchestration/system payloads from the chat transcript.
    if matches!(msg.role, hermes_core::MessageRole::System) {
        return;
    }
    if *rendered_messages > 0 && matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(String::new()));
    }
    *rendered_messages += 1;
    let (glyph, label, label_style, body_style) = role_visuals(msg.role, styles, colors);
    let stamp = if state.show_timestamps {
        let fp = message_fingerprint(msg);
        state
            .message_time_labels
            .entry(fp)
            .or_insert_with(|| Local::now().format("%H:%M:%S").to_string())
            .clone()
    } else {
        String::new()
    };
    let label_text = if stamp.is_empty() {
        label.to_string()
    } else {
        format!("{label}  {stamp}")
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" ╭ {} ", glyph),
            label_style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(label_text, label_style.add_modifier(Modifier::BOLD)),
    ]));

    if let Some(content) = msg.content.as_deref() {
        match msg.role {
            hermes_core::MessageRole::Assistant => {
                let assistant_lines = render_assistant_markdown_lines(content, styles, colors);
                lines.extend(collapse_render_lines_with_notice(
                    assistant_lines,
                    max_assistant_render_lines(),
                    colors,
                ));
            }
            hermes_core::MessageRole::Tool => {
                let card_key = format!("tool:{msg_idx}");
                let expanded = state.expanded_tool_cards.contains(&card_key)
                    || state.expanded_tool_cards.contains("__all__");
                let all_lines = format_tool_message_lines(content);
                let shown = if expanded { 20 } else { 4 };
                lines.push(Line::from(vec![Span::styled(
                    format!(
                        "    [tool card: {} | {} lines | Ctrl+E toggles]",
                        if expanded { "expanded" } else { "collapsed" },
                        all_lines.len()
                    ),
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in all_lines.iter().take(shown) {
                    for segment in hard_wrap_segments(line, TRANSCRIPT_CONTENT_WRAP_COLS) {
                        lines.push(render_inline_with_code(
                            "    ",
                            &segment,
                            styles.tool_result,
                            Style::default()
                                .fg(colors.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
                if all_lines.len() > shown {
                    lines.push(Line::from(vec![Span::styled(
                        format!("    … {} more lines", all_lines.len() - shown),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    )]));
                }
            }
            _ => {
                for line in content.lines() {
                    for segment in hard_wrap_segments(line, TRANSCRIPT_CONTENT_WRAP_COLS) {
                        lines.push(render_inline_with_code(
                            "    ",
                            &segment,
                            body_style,
                            Style::default()
                                .fg(colors.accent)
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                }
            }
        }
    }

    if msg.role == hermes_core::MessageRole::Assistant {
        if matches!(state.view_density, ViewDensity::Detailed) {
            if let Some(reasoning) = msg
                .reasoning_content
                .as_ref()
                .filter(|s| !s.trim().is_empty())
            {
                lines.push(Line::from(vec![Span::styled(
                    "    🤔 reasoning",
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.background),
                )]));
                for line in reasoning.lines() {
                    lines.push(Line::from(vec![Span::styled(
                        format!("      {}", line.trim_end()),
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background)
                            .add_modifier(Modifier::ITALIC),
                    )]));
                }
            }
        }
        if let Some(tool_calls) = msg.tool_calls.as_ref() {
            for tc in tool_calls {
                let args = serde_json::from_str::<serde_json::Value>(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Null);
                let preview =
                    build_tool_preview_from_value(&tc.function.name, &args, 44).unwrap_or_default();
                let emoji = tool_emoji(&tc.function.name);
                let summary = if preview.is_empty() {
                    format!("{emoji} {}", tc.function.name)
                } else {
                    format!("{emoji} {} {}", tc.function.name, preview)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        "    ↳ ",
                        Style::default()
                            .fg(colors.status_bar_dim)
                            .bg(colors.background),
                    ),
                    Span::styled(summary, styles.tool_call),
                ]));
            }
        }
    }
    if matches!(state.view_density, ViewDensity::Detailed) {
        lines.push(Line::from(vec![Span::styled(
            divider.to_string(),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }
}

fn build_transcript_lines(
    messages: &[hermes_core::Message],
    state: &mut TuiState,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
    content_width: u16,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut rendered_messages = 0usize;
    let divider = transcript_divider(content_width);

    for (msg_idx, msg) in messages.iter().enumerate() {
        append_transcript_message_lines(
            &mut lines,
            msg,
            msg_idx,
            &mut rendered_messages,
            state,
            styles,
            colors,
            &divider,
        );
    }

    // Streaming buffer (partial assistant response)
    if !state.stream_buffer.is_empty() {
        if !lines.is_empty() {
            lines.push(Line::from(String::new()));
        }
        lines.push(Line::from(vec![
            Span::styled(
                " ╭ ● ",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "HERMES (streaming)",
                styles.assistant_response.add_modifier(Modifier::BOLD),
            ),
        ]));
        let stream_lines = render_assistant_markdown_lines(&state.stream_buffer, styles, colors);
        lines.extend(tail_render_lines_with_notice(
            stream_lines,
            MAX_STREAM_RENDER_LINES,
            colors,
        ));
        lines.push(Line::from(vec![Span::styled(
            "    ▌",
            Style::default()
                .fg(colors.accent)
                .bg(colors.background)
                .add_modifier(Modifier::BOLD),
        )]));
        lines.push(Line::from(vec![Span::styled(
            divider,
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background),
        )]));
    }

    if lines.is_empty() {
        let neon = Style::default()
            .fg(colors.status_bar_strong)
            .bg(colors.background)
            .add_modifier(Modifier::BOLD);
        let dim = Style::default()
            .fg(colors.status_bar_dim)
            .bg(colors.background);
        let accent = Style::default().fg(colors.accent).bg(colors.background);
        let hero = [
            " ╔══════════════════════════════════════════════════════════════════╗",
            " ║  ██╗  ██╗███████╗██████╗ ███╗   ███╗███████╗███████╗          ║",
            " ║  ██║  ██║██╔════╝██╔══██╗████╗ ████║██╔════╝██╔════╝          ║",
            " ║  ███████║█████╗  ██████╔╝██╔████╔██║█████╗  ███████╗          ║",
            " ║  ██╔══██║██╔══╝  ██╔══██╗██║╚██╔╝██║██╔══╝  ╚════██║          ║",
            " ║  ██║  ██║███████╗██║  ██║██║ ╚═╝ ██║███████╗███████║          ║",
            " ║  ╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚═╝     ╚═╝╚══════╝╚══════╝          ║",
            " ║                                                                  ║",
            " ║       AGENT ULTRA  //  SUNBURST OPS  //  LIVE EXECUTION         ║",
            " ║       YELLOW SIGNAL • REDLINE DRIVE • RUST-NATIVE CONTROL       ║",
            " ╚══════════════════════════════════════════════════════════════════╝",
        ];
        for (idx, row) in hero.iter().enumerate() {
            let style = if idx == 0 || idx == hero.len() - 1 {
                accent
            } else if row.contains("AGENT ULTRA") || row.contains("YELLOW SIGNAL") {
                neon
            } else {
                dim
            };
            lines.push(Line::from(vec![Span::styled((*row).to_string(), style)]));
        }
        lines.push(Line::from(String::new()));
        lines.push(Line::from(vec![Span::styled(
            " Start chatting — your messages and Hermes replies will appear here.",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )]));
    }
    lines
}

fn render_messages(
    frame: &mut Frame,
    app: &App,
    state: &mut TuiState,
    area: Rect,
    styles: &crate::theme::ResolvedStyles,
    colors: &crate::theme::RatatuiColors,
) {
    let base_block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_dim));
    let inner = base_block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(Clear, area);
        frame.render_widget(base_block.title(" Conversation "), area);
        return;
    }
    let reserved_scrollbar_col = if inner.width > 1 { 1 } else { 0 };
    let transcript_width = inner.width.saturating_sub(reserved_scrollbar_col).max(1);
    let wrap_width = transcript_wrap_width(transcript_width);
    let content_area = Rect {
        x: inner.x,
        y: inner.y,
        width: wrap_width.min(transcript_width),
        height: inner.height,
    };
    let transcript = app.transcript_messages();
    let viewport_rows = usize::from(inner.height.max(1));
    let fingerprint = transcript_fingerprint(&transcript, state, wrap_width);
    let message_fingerprints = transcript_message_fingerprints(&transcript);
    if state.transcript_cache.fingerprint != fingerprint
        || state.transcript_cache.width != wrap_width
    {
        let cache = &state.transcript_cache;
        let can_incremental_append = !cache.had_streaming
            && state.stream_buffer.is_empty()
            && cache.width == wrap_width
            && cache.total_messages > 0
            && transcript.len() > cache.total_messages
            && cache.show_timestamps == state.show_timestamps
            && cache.view_density == state.view_density
            && cache.message_fingerprints.len() == cache.total_messages
            && message_fingerprints.starts_with(&cache.message_fingerprints);

        if can_incremental_append {
            let start_idx = state.transcript_cache.total_messages;
            let mut lines = std::mem::take(&mut state.transcript_cache.lines);
            let mut rendered_messages = state.transcript_cache.rendered_messages;
            let divider = transcript_divider(wrap_width);
            for (msg_idx, msg) in transcript.iter().enumerate().skip(start_idx) {
                append_transcript_message_lines(
                    &mut lines,
                    msg,
                    msg_idx,
                    &mut rendered_messages,
                    state,
                    styles,
                    colors,
                    &divider,
                );
            }
            state.transcript_cache = TranscriptCache {
                fingerprint,
                width: wrap_width,
                visual_rows: approximate_visual_rows(&lines, wrap_width),
                total_messages: transcript.len(),
                rendered_messages,
                message_fingerprints,
                show_timestamps: state.show_timestamps,
                view_density: state.view_density,
                had_streaming: false,
                lines,
            };
        } else {
            let prev_width = state.transcript_cache.width;
            let prev_len = state.transcript_cache.lines.len();
            let prev_anchor_line = if prev_width != 0
                && prev_width != wrap_width
                && state.scroll_offset > 0
                && prev_len > 0
            {
                let old_view_rows = viewport_rows.min(prev_len.max(1));
                let max_hidden = prev_len.saturating_sub(old_view_rows);
                let hidden = state.scroll_offset.min(max_hidden);
                let old_end = prev_len.saturating_sub(hidden);
                let old_start = old_end.saturating_sub(old_view_rows);
                state
                    .transcript_cache
                    .lines
                    .get(old_start)
                    .map(Line::to_string)
                    .map(|text| (text, old_start, prev_len))
            } else {
                None
            };

            let new_lines = build_transcript_lines(&transcript, state, styles, colors, wrap_width);
            let new_visual_rows = approximate_visual_rows(&new_lines, wrap_width);
            if let Some((anchor_text, old_start, old_len)) = prev_anchor_line {
                let new_len = new_lines.len();
                let expected_idx = if old_len > 0 {
                    old_start.saturating_mul(new_len) / old_len
                } else {
                    0
                };
                if let Some(new_idx) =
                    find_anchor_line_index(&new_lines, &anchor_text, expected_idx)
                {
                    let new_len = new_lines.len();
                    let visible = viewport_rows.min(new_len.max(1));
                    let new_hidden = new_len.saturating_sub((new_idx + visible).min(new_len));
                    state.scroll_offset = new_hidden;
                }
            }
            state.transcript_cache = TranscriptCache {
                fingerprint,
                width: wrap_width,
                visual_rows: new_visual_rows,
                total_messages: transcript.len(),
                rendered_messages: count_renderable_messages(&transcript),
                message_fingerprints,
                show_timestamps: state.show_timestamps,
                view_density: state.view_density,
                had_streaming: !state.stream_buffer.is_empty(),
                lines: new_lines,
            };
        }
    }
    let lines = &state.transcript_cache.lines;
    if state.auto_follow_transcript {
        state.scroll_offset = 0;
    }
    let total_visual_rows = state.transcript_cache.visual_rows.max(1);
    let max_hidden_from_bottom = total_visual_rows.saturating_sub(viewport_rows);
    let hidden_from_bottom = state.scroll_offset.min(max_hidden_from_bottom);
    if state.scroll_offset != hidden_from_bottom {
        state.scroll_offset = hidden_from_bottom;
    }
    let top_visual_row = total_visual_rows.saturating_sub(viewport_rows + hidden_from_bottom);

    let (render_lines, scroll_rows_in_window) =
        project_transcript_window(lines, wrap_width, top_visual_row, viewport_rows);
    let text = Text::from(render_lines);
    let top_visual_row_u16 = scroll_rows_in_window.min(u16::MAX as usize) as u16;

    let title = if hidden_from_bottom > 0 {
        format!(" Conversation (+{}) ", hidden_from_bottom)
    } else {
        " Conversation ".to_string()
    };
    let block = base_block.title(title);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(text)
            .wrap(Wrap { trim: false })
            .scroll((top_visual_row_u16, 0)),
        content_area,
    );

    if total_visual_rows > viewport_rows {
        let scrollbar_area = Rect {
            x: content_area.x + content_area.width,
            y: content_area.y,
            width: 1,
            height: content_area.height,
        };
        let mut scrollbar_state = ScrollbarState::new(total_visual_rows)
            .position(top_visual_row)
            .viewport_content_length(viewport_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.background),
            )
            .thumb_style(
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.background),
            );
        frame.render_stateful_widget(scrollbar, scrollbar_area, &mut scrollbar_state);
    }
}

fn approximate_visual_rows(lines: &[Line<'static>], wrap_width: u16) -> usize {
    let width = usize::from(wrap_width.max(1));
    lines
        .iter()
        .map(|line| line_visual_rows(line, width))
        .sum::<usize>()
        .max(1)
}

fn line_visual_rows(line: &Line<'static>, width: usize) -> usize {
    let display_width = UnicodeWidthStr::width(line.to_string().as_str()).max(1);
    ((display_width - 1) / width.max(1)) + 1
}

fn project_transcript_window(
    lines: &[Line<'static>],
    wrap_width: u16,
    top_visual_row: usize,
    viewport_rows: usize,
) -> (Vec<Line<'static>>, usize) {
    if lines.is_empty() {
        return (Vec::new(), 0);
    }

    let width = usize::from(wrap_width.max(1));
    let mut cumulative = 0usize;
    let mut start_idx = 0usize;
    let mut intra_line_offset = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        let line_rows = line_visual_rows(line, width);
        if cumulative + line_rows > top_visual_row {
            start_idx = idx;
            intra_line_offset = top_visual_row.saturating_sub(cumulative);
            break;
        }
        cumulative = cumulative.saturating_add(line_rows);
        start_idx = idx.saturating_add(1);
    }

    if start_idx >= lines.len() {
        start_idx = lines.len().saturating_sub(1);
        intra_line_offset = 0;
    }

    // Keep paragraph scroll offset representable (u16) by rebasing start line forward when needed.
    while intra_line_offset > u16::MAX as usize && start_idx + 1 < lines.len() {
        let consume = line_visual_rows(&lines[start_idx], width);
        if consume == 0 {
            break;
        }
        intra_line_offset = intra_line_offset.saturating_sub(consume);
        start_idx += 1;
    }

    let needed_rows = intra_line_offset.saturating_add(viewport_rows.max(1));
    let mut collected_rows = 0usize;
    let mut window: Vec<Line<'static>> = Vec::new();
    for line in lines.iter().skip(start_idx) {
        collected_rows = collected_rows.saturating_add(line_visual_rows(line, width));
        window.push(line.clone());
        if collected_rows >= needed_rows {
            break;
        }
    }
    if window.is_empty() {
        window.push(lines[lines.len() - 1].clone());
    }

    (window, intra_line_offset)
}

/// Render slash-command completions as a popup over the conversation panel.
fn render_completions_popup(
    frame: &mut Frame,
    completions: &[String],
    selected: Option<usize>,
    messages_area: Rect,
    input_area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    if completions.is_empty() {
        return;
    }
    let max_inner_rows = 10usize;
    let visible_rows = completions.len().min(max_inner_rows).max(1);
    let mut start = 0usize;
    if let Some(sel) = selected {
        if sel >= visible_rows {
            start = sel + 1 - visible_rows;
        }
    }
    let end = (start + visible_rows).min(completions.len());
    let max_item_width = completions[start..end]
        .iter()
        .map(|c| {
            let desc = crate::commands::help_for(c).unwrap_or("");
            if desc.is_empty() {
                c.chars().count()
            } else {
                format!("{c} — {desc}").chars().count()
            }
        })
        .max()
        .unwrap_or(0);
    let popup_max_width = messages_area.width.saturating_sub(2).max(1);
    let popup_min_width = 36u16.min(popup_max_width);
    let popup_width = (max_item_width as u16 + 8).clamp(popup_min_width, popup_max_width);
    let popup_height = (end.saturating_sub(start) as u16 + 2).max(3);
    if popup_width == 0 || popup_height == 0 {
        return;
    }
    let right_bound = messages_area.x + messages_area.width.saturating_sub(1);
    let mut x = input_area.x.saturating_add(1);
    if x + popup_width > right_bound {
        x = right_bound.saturating_sub(popup_width);
    }
    let min_y = messages_area.y.saturating_add(1);
    let y = input_area.y.saturating_sub(popup_height).max(min_y);
    let popup = Rect {
        x,
        y,
        width: popup_width,
        height: popup_height,
    };

    let inner_width = popup_width.saturating_sub(4) as usize;
    let items: Vec<Line<'static>> = completions
        .iter()
        .enumerate()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(|(i, cmd)| {
            let style = if selected == Some(i) {
                Style::default()
                    .fg(Color::Black)
                    .bg(colors.status_bar_strong)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(colors.status_bar_text)
                    .bg(colors.status_bar_bg)
            };
            let desc = crate::commands::help_for(cmd).unwrap_or("");
            let text = if desc.is_empty() {
                cmd.to_string()
            } else {
                format!("{:<18} {}", cmd, desc)
            };
            Line::from(Span::styled(truncate_chars(&text, inner_width), style))
        })
        .collect();

    let title = if completions.len() > visible_rows {
        format!(
            " Slash Commands ({}/{}) ↑↓ scroll Tab accept ",
            end,
            completions.len()
        )
    } else {
        " Slash Commands ".to_string()
    };

    let paragraph = Paragraph::new(Text::from(items))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().bg(colors.status_bar_bg))
                .border_style(Style::default().fg(colors.status_bar_strong))
                .title(title),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(Clear, popup);
    frame.render_widget(paragraph, popup);

    if completions.len() > visible_rows {
        let inner = Rect {
            x: popup.x.saturating_add(1),
            y: popup.y.saturating_add(1),
            width: popup.width.saturating_sub(2),
            height: popup.height.saturating_sub(2),
        };
        let mut scrollbar_state = ScrollbarState::new(completions.len())
            .position(start)
            .viewport_content_length(visible_rows);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█")
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(
                Style::default()
                    .fg(colors.status_bar_dim)
                    .bg(colors.status_bar_bg),
            )
            .thumb_style(
                Style::default()
                    .fg(colors.status_bar_strong)
                    .bg(colors.status_bar_bg),
            );
        frame.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
    }
}

fn render_picker_modal(
    frame: &mut Frame,
    modal: &PickerModal,
    colors: &crate::theme::RatatuiColors,
) {
    let area = frame.area();
    if area.width < 20 || area.height < 8 {
        return;
    }
    let width = (area.width.saturating_sub(6)).min(110).max(48);
    let height = (area.height.saturating_sub(4)).min(22).max(10);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", modal.title))
        .style(Style::default().bg(colors.status_bar_bg))
        .border_style(Style::default().fg(colors.status_bar_strong));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let footer_height = 2u16;
    let query_height = 1u16;
    let rows_height = inner.height.saturating_sub(footer_height + query_height);
    let rows_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: rows_height,
    };
    let query_area = Rect {
        x: inner.x,
        y: inner.y + rows_height,
        width: inner.width,
        height: query_height,
    };
    let footer_area = Rect {
        x: inner.x,
        y: inner.y + rows_height + query_height,
        width: inner.width,
        height: footer_height,
    };

    let (start, end) = modal.visible_window();
    let items: Vec<Line<'static>> = if modal.filtered_indices.is_empty() {
        vec![Line::from(vec![Span::styled(
            "No matches for current search query.",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::ITALIC),
        )])]
    } else {
        modal
            .filtered_indices
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|(filtered_idx, item_idx)| {
                let item = &modal.items[*item_idx];
                let selected = filtered_idx == modal.selected_filtered;
                let selected_marker = if selected { "▶" } else { " " };
                let absolute_number = filtered_idx + 1;
                let multi_marker = if modal.allow_multi {
                    if modal.selected_values.contains(&item.value) {
                        "■ "
                    } else {
                        "□ "
                    }
                } else {
                    ""
                };
                let row_style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(colors.status_bar_strong)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                        .fg(colors.status_bar_text)
                        .bg(colors.status_bar_bg)
                };
                let detail_style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(colors.status_bar_strong)
                } else {
                    Style::default()
                        .fg(colors.status_bar_dim)
                        .bg(colors.status_bar_bg)
                };
                let text = format!(
                    "{selected_marker} {:>3}. {multi_marker}{}",
                    absolute_number, item.label
                );
                let available = rows_area.width.saturating_sub(2) as usize;
                let primary = truncate_chars(&text, available);
                if item.detail.is_empty() {
                    Line::from(vec![Span::styled(primary, row_style)])
                } else {
                    let detail = truncate_chars(
                        &format!("  {}", item.detail),
                        rows_area.width.saturating_sub(2) as usize,
                    );
                    Line::from(vec![
                        Span::styled(primary, row_style),
                        Span::styled("  ", row_style),
                        Span::styled(detail, detail_style),
                    ])
                }
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(Text::from(items))
            .style(Style::default().bg(colors.status_bar_bg))
            .wrap(Wrap { trim: true }),
        rows_area,
    );

    let query_line = match &modal.kind {
        PickerKind::InteractiveQuestion { prompt } => {
            format!("Question: {}", truncate_chars(prompt, 200))
        }
        _ => format!(
            "Search: {}",
            if modal.query.is_empty() {
                "(type to filter)"
            } else {
                modal.query.as_str()
            }
        ),
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            truncate_chars(&query_line, query_area.width as usize),
            Style::default()
                .fg(colors.accent)
                .bg(colors.status_bar_bg)
                .add_modifier(Modifier::BOLD),
        )])),
        query_area,
    );

    let footer = if matches!(modal.kind, PickerKind::InteractiveQuestion { .. }) {
        "↑↓ choose • Enter insert answer • Esc close"
    } else if modal.allow_multi {
        "↑↓ move • PgUp/PgDn page • Space toggle • Enter confirm • Esc close"
    } else if matches!(modal.kind, PickerKind::ModelProvider) {
        "↑↓ move • 1-9/0 quick-pick • d disconnect • Enter select • Esc close"
    } else {
        "↑↓ move • PgUp/PgDn page • 1-9/0 quick-pick • Enter select • Esc close"
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            truncate_chars(footer, footer_area.width as usize),
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.status_bar_bg),
        )])),
        footer_area,
    );
}

/// Render the input area (supports multi-line display with wrapping).
fn render_input(
    frame: &mut Frame,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) -> Option<Position> {
    let mode_label = match state.mode {
        InputMode::Normal => "NORMAL",
        InputMode::Insert => "INSERT",
        InputMode::Command => "COMMAND",
    };
    let mode_color = match state.mode {
        InputMode::Normal => colors.status_bar_dim,
        InputMode::Insert => colors.status_bar_good,
        InputMode::Command => colors.accent,
    };
    let line_count = state.input.matches('\n').count() + 1;

    let mut block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(vec![
            Span::styled(" Message  •  ", Style::default().fg(colors.status_bar_dim)),
            Span::styled(
                mode_label.to_string(),
                Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  •  L{}  •  Ctrl+←/→ word-jump ", line_count),
                Style::default().fg(colors.status_bar_dim),
            ),
        ]))
        .style(Style::default().bg(colors.background))
        .border_style(Style::default().fg(colors.status_bar_strong));
    if state.history_search_active {
        block = block.title_bottom(Line::from(Span::styled(
            format!(
                " reverse-i-search: `{}` (Ctrl+R to exit) ",
                state.history_search_query
            ),
            Style::default()
                .fg(colors.status_bar_warn)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    let input_text = if state.input.is_empty()
        && state.mode == InputMode::Insert
        && !state.history_search_active
    {
        Text::from(Line::from(Span::styled(
            "Type a message (Enter sends, Shift+Enter/Ctrl+J inserts newline)",
            Style::default()
                .fg(colors.status_bar_dim)
                .bg(colors.background)
                .add_modifier(Modifier::ITALIC),
        )))
    } else {
        Text::from(state.input_line_text())
    };
    let input = Paragraph::new(input_text)
        .block(block.clone())
        .style(Style::default().fg(colors.foreground).bg(colors.background))
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(input, area);

    if state.mode == InputMode::Normal {
        return None;
    }

    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        return None;
    }
    let (row, col) = TuiState::cursor_row_col(&state.input, state.cursor_position);
    Some(Position {
        x: inner.x + (col as u16).min(inner.width.saturating_sub(1)),
        y: inner.y + (row as u16).min(inner.height.saturating_sub(1)),
    })
}

/// Render the status bar at the bottom of the screen.
fn status_message_style(message: &str, colors: &crate::theme::RatatuiColors) -> Style {
    let lower = message.to_ascii_lowercase();
    if lower.contains("error") {
        Style::default()
            .fg(colors.status_bar_critical)
            .bg(colors.status_bar_bg)
    } else if lower.contains("warn") {
        Style::default()
            .fg(colors.status_bar_warn)
            .bg(colors.status_bar_bg)
    } else {
        Style::default()
            .fg(colors.status_bar_text)
            .bg(colors.status_bar_bg)
    }
}

/// Render the status bar at the bottom of the screen.
fn render_status(
    frame: &mut Frame,
    app: &App,
    state: &TuiState,
    area: Rect,
    colors: &crate::theme::RatatuiColors,
) {
    let processing_indicator = if state.processing {
        format!("⟳{}", state.spinner_char())
    } else {
        "✓".to_string()
    };
    let model = &app.current_model;
    let session = &app.session_id[..8.min(app.session_id.len())];
    let msg_count = state
        .transcript_cache
        .total_messages
        .max(app.messages.len());
    let scroll_hint = if state.scroll_offset > 0 {
        format!(" (history +{})", state.scroll_offset)
    } else {
        String::new()
    };

    let base = Style::default()
        .fg(colors.status_bar_text)
        .bg(colors.status_bar_bg);

    let mut status_text = if state.processing {
        let elapsed = state.processing_elapsed().as_secs_f64();
        format!(
            "{} PROCESSING {:.1}s [{}] {} | {} | {} msgs | {}",
            processing_indicator,
            elapsed,
            animated_processing_bar(state.spinner_frame, 12),
            state.processing_stage_label(),
            state.mode,
            msg_count,
            session
        )
    } else {
        format!(
            "{} {} | {} | {} msgs | {}",
            processing_indicator, state.mode, model, msg_count, session
        )
    };
    status_text.push_str(match state.view_density {
        ViewDensity::Compact => " | compact",
        ViewDensity::Detailed => " | detailed",
    });
    if state.show_timestamps {
        status_text.push_str(" | ts:on");
    }
    if state.activity_lane_open {
        status_text.push_str(" | lane:on");
    } else {
        status_text.push_str(" | lane:off");
    }
    status_text.push_str(match state.activity_lane_mode {
        ActivityLaneMode::Live => " (live)",
        ActivityLaneMode::Cockpit => " (cockpit)",
    });
    if state.background_jobs_running > 0 {
        status_text.push_str(&format!(" | bg:{}", state.background_jobs_running));
    }
    if state.active_subagents_running > 0 {
        status_text.push_str(&format!(" | ⛓:{}", state.active_subagents_running));
    }
    if let Some(credits_notice) = hermes_core::credits::last_nous_credits_notice_line() {
        status_text.push_str(" | ");
        status_text.push_str(&credits_notice);
    }
    status_text.push_str(if app.mouse_enabled() {
        " | mouse:on"
    } else {
        " | mouse:off"
    });
    if !state.sticky_prompt.is_empty() {
        status_text.push_str(&format!(
            " | ↳ {}",
            truncate_chars(&state.sticky_prompt, 40)
        ));
    }
    if !state.status_message.is_empty() || !scroll_hint.is_empty() {
        status_text.push_str(" | ");
        status_text.push_str(&state.status_message);
        status_text.push_str(&scroll_hint);
    }
    if let Some(frame_token) =
        pet_frame_token(app.pet_settings(), state.pet_frame, state.processing)
    {
        if matches!(app.pet_settings().dock, crate::app::PetDock::Left) {
            status_text = format!("{frame_token} | {status_text}");
        } else {
            status_text.push_str(&format!(" | {frame_token}"));
        }
    }
    let clipped = fit_status_line(&status_text, area.width.saturating_sub(1) as usize);
    let line_style = if state.status_message.is_empty() {
        base
    } else {
        status_message_style(&state.status_message, colors).bg(colors.status_bar_bg)
    };
    let status_bar = Paragraph::new(Line::from(Span::styled(clipped, line_style)))
        .block(Block::default().style(Style::default().bg(colors.status_bar_bg)));
    frame.render_widget(status_bar, area);
}

fn animated_processing_bar(frame: usize, width: usize) -> String {
    let width = width.max(6);
    let head = frame % width;
    let trail = 3usize;
    let mut out = String::with_capacity(width);
    for i in 0..width {
        let lit = if head >= trail {
            i >= head - trail && i <= head
        } else {
            i <= head || i + width >= head + width - trail
        };
        out.push(if lit { '█' } else { '·' });
    }
    out
}

fn transcript_divider(content_width: u16) -> String {
    let width = usize::from(content_width.max(12));
    let rule = "─".repeat(width.saturating_sub(3).max(8));
    format!(" ╰{}", rule)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let take = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(take).collect();
    out.push('…');
    out
}

fn fit_status_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if cw == 0 {
            continue;
        }
        if used + cw > width {
            break;
        }
        out.push(ch);
        used += cw;
    }
    while used < width {
        out.push(' ');
        used += 1;
    }
    out
}

fn hard_wrap_segments(text: &str, max_chars: usize) -> Vec<String> {
    let width = max_chars.max(1);
    if text.is_empty() {
        return vec![String::new()];
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return vec![String::new()];
    }
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for token in trimmed.split_whitespace() {
        let token_len = token.chars().count();
        if token_len > width {
            if !current.is_empty() {
                segments.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let mut chunk = String::new();
            let mut chunk_len = 0usize;
            for ch in token.chars() {
                chunk.push(ch);
                chunk_len += 1;
                if chunk_len >= width {
                    segments.push(std::mem::take(&mut chunk));
                    chunk_len = 0;
                }
            }
            if !chunk.is_empty() {
                current = chunk;
                current_len = chunk_len;
            }
            continue;
        }

        let needed = if current.is_empty() {
            token_len
        } else {
            current_len + 1 + token_len
        };
        if needed <= width {
            if !current.is_empty() {
                current.push(' ');
                current_len += 1;
            }
            current.push_str(token);
            current_len += token_len;
        } else {
            segments.push(std::mem::take(&mut current));
            current.push_str(token);
            current_len = token_len;
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    if segments.is_empty() {
        segments.push(String::new());
    }
    segments
}

fn fuzzy_subsequence_score(needle: &str, haystack: &str) -> i32 {
    if needle.is_empty() || haystack.is_empty() {
        return 0;
    }
    let mut score = 0i32;
    let mut idx = 0usize;
    let chars: Vec<char> = haystack.chars().collect();
    for ch in needle.chars() {
        let mut found = false;
        while idx < chars.len() {
            if chars[idx] == ch {
                score += 2;
                if idx > 0 && chars[idx - 1] == '-' {
                    score += 1;
                }
                idx += 1;
                found = true;
                break;
            }
            idx += 1;
        }
        if !found {
            return 0;
        }
    }
    score
}

fn pet_frame_token(
    settings: &crate::app::PetSettings,
    frame: usize,
    processing: bool,
) -> Option<String> {
    if !settings.enabled {
        return None;
    }
    let effective_mood = if processing && settings.mood != "sleepy" {
        "working"
    } else {
        settings.mood.as_str()
    };
    let frames: [&str; 2] = match (settings.species.as_str(), effective_mood) {
        ("boba", "sleepy") => ["(-_- )z", "(-_- )Z"],
        ("boba", "working") => ["(>_< )", "(<_< )"],
        ("boba", "hyped") => ["(o_o)!", "(!o_o)"],
        ("boba", "chill") => ["(u_u )", "(u_U )"],
        ("bytecat", "sleepy") => ["= -.-=z", "= -.-=Z"],
        ("bytecat", "working") => ["=^x^=", "=^_^="],
        ("bytecat", "hyped") => ["=^o^=!", "=^O^=!"],
        ("bytecat", "chill") => ["=^.^=~", "=^.-=~"],
        ("otter", "sleepy") => ["(>< )z", "(>< )Z"],
        ("otter", "working") => ["(>> )~", "(<< )~"],
        ("otter", "hyped") => ["(OO )~", "(oo )~"],
        ("otter", "chill") => ["(~~ )~", "(~_ )~"],
        ("fox", "sleepy") => ["{-- }z", "{-- }Z"],
        ("fox", "working") => ["{^x }", "{x^ }"],
        ("fox", "hyped") => ["{^^ }!", "{oo }!"],
        ("fox", "chill") => ["{.. }", "{._ }"],
        ("owl", "sleepy") => ["(v_v)z", "(v_v)Z"],
        ("owl", "working") => ["(O_O)", "(0_0)"],
        ("owl", "hyped") => ["(O0O)!", "(0O0)!"],
        ("owl", "chill") => ["(o_o)", "(o_O)"],
        ("capy", "sleepy") => ["(._.)z", "(._.)Z"],
        ("capy", "working") => ["(>_.)", "(._<)"],
        ("capy", "hyped") => ["(o_.)!", "(._o)!"],
        ("capy", "chill") => ["(._.)~", "(.._)~"],
        ("boba", _) => ["(o_o )", "(O_O )"],
        ("bytecat", _) => ["=^.^=", "=^o^="],
        ("otter", _) => ["(>< )~", "(~>< )"],
        ("fox", _) => ["{^.^}", "{^o^}"],
        ("owl", _) => ["(OvO)", "(oVo)"],
        ("capy", _) => ["(._.)", "(o_.)"],
        _ => ["(o_o )", "(O_O )"],
    };
    Some(frames[frame % frames.len()].to_string())
}

fn is_ctrl_c(key: &KeyEvent) -> bool {
    matches!(key.code, crossterm::event::KeyCode::Char('\u{3}'))
        || (key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
            && matches!(key.code, crossterm::event::KeyCode::Char('c' | 'C')))
}

fn is_submit_shortcut(key: &KeyEvent, _input: &str) -> bool {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mods = key.modifiers;

    // Some PTY harnesses deliver Enter as a raw CR/LF character rather than
    // `KeyCode::Enter`; accept both so interactive smoke tests and terminals
    // with reduced key translation still submit slash commands.
    if matches!(key.code, KeyCode::Char('\r' | '\n')) {
        return true;
    }

    if key.code == KeyCode::Enter {
        if mods.contains(KeyModifiers::SHIFT) {
            return false;
        }
        if mods.is_empty()
            || mods.contains(KeyModifiers::CONTROL)
            || mods.contains(KeyModifiers::ALT)
        {
            return true;
        }
    }

    // Fallback for terminals that encode Ctrl+Enter as Ctrl+M.
    key.code == KeyCode::Char('m') && mods.contains(KeyModifiers::CONTROL)
}

fn parse_slash_parts(input: &str) -> Option<(String, Vec<String>)> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return None;
    }
    let mut iter = trimmed.split_whitespace();
    let cmd = iter.next()?.to_string();
    let args = iter.map(ToString::to_string).collect::<Vec<_>>();
    Some((cmd, args))
}

#[derive(Debug, Clone)]
struct InteractiveQuestionRequest {
    prompt: String,
    options: Vec<PickerItem>,
}

fn strip_question_option_marker(line: &str) -> String {
    let trimmed = line.trim();
    if let Some(body) = trimmed.strip_prefix("- ") {
        return body.trim().to_string();
    }
    if let Some(body) = trimmed.strip_prefix("* ") {
        return body.trim().to_string();
    }
    if let Some(body) = trimmed.strip_prefix("+ ") {
        return body.trim().to_string();
    }
    if let Some((_marker, body)) = parse_markdown_numbered_marker(trimmed) {
        return body.trim().to_string();
    }
    trimmed.to_string()
}

fn parse_question_option(value: &str) -> PickerItem {
    let raw = value.trim();
    if let Some((label, detail)) = raw.split_once("::") {
        return PickerItem {
            label: label.trim().to_string(),
            detail: detail.trim().to_string(),
            value: label.trim().to_string(),
        };
    }
    PickerItem {
        label: raw.to_string(),
        detail: String::new(),
        value: raw.to_string(),
    }
}

fn parse_interactive_question_request(input: &str) -> Result<InteractiveQuestionRequest, String> {
    let trimmed = input.trim();
    if !(trimmed.starts_with("/ask") || trimmed.starts_with("/question")) {
        return Err("not an interactive question command".to_string());
    }
    let cmd = trimmed
        .split_whitespace()
        .next()
        .ok_or_else(|| "missing command".to_string())?;
    let rest = trimmed.strip_prefix(cmd).unwrap_or("").trim();
    if rest.is_empty() || rest.eq_ignore_ascii_case("help") {
        return Err("Usage: `/ask <question> | <option 1> | <option 2> [| <option 3> ...]`\nAlternative multiline format:\n`/ask\\n<question>\\n- <option 1>\\n- <option 2>`".to_string());
    }

    if rest.eq_ignore_ascii_case("demo") {
        return Ok(InteractiveQuestionRequest {
            prompt: "How should we proceed?".to_string(),
            options: vec![
                PickerItem {
                    label: "Continue implementation (Recommended)".to_string(),
                    detail: "Keep shipping patches now.".to_string(),
                    value: "Continue implementation".to_string(),
                },
                PickerItem {
                    label: "Pause for diagnosis".to_string(),
                    detail: "Inspect logs and root-cause first.".to_string(),
                    value: "Pause for diagnosis".to_string(),
                },
                PickerItem {
                    label: "Switch model/provider".to_string(),
                    detail: "Try a different runtime profile.".to_string(),
                    value: "Switch model/provider".to_string(),
                },
            ],
        });
    }

    let mut prompt = String::new();
    let mut raw_options: Vec<String> = Vec::new();
    if rest.contains('|') {
        let pieces: Vec<String> = rest
            .split('|')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(ToString::to_string)
            .collect();
        if let Some(first) = pieces.first() {
            prompt = first.clone();
        }
        for piece in pieces.iter().skip(1) {
            raw_options.push(strip_question_option_marker(piece));
        }
    } else {
        let lines: Vec<String> = rest
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(ToString::to_string)
            .collect();
        if let Some(first) = lines.first() {
            prompt = first.clone();
        }
        for line in lines.iter().skip(1) {
            raw_options.push(strip_question_option_marker(line));
        }
    }

    raw_options.retain(|o| !o.trim().is_empty());
    if prompt.trim().is_empty() {
        return Err("Question prompt is empty. Provide a question before the options.".to_string());
    }
    if raw_options.len() < 2 {
        return Err(
            "Provide at least 2 options. Example: `/ask Pick mode | safe | fast`".to_string(),
        );
    }
    if raw_options.len() > 12 {
        raw_options.truncate(12);
    }
    let options = raw_options
        .iter()
        .map(|value| parse_question_option(value))
        .collect();

    Ok(InteractiveQuestionRequest { prompt, options })
}

fn provider_env_key_hints(provider: &str) -> &'static [&'static str] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openai-codex" | "codex" => &["HERMES_OPENAI_CODEX_API_KEY"],
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "nous" => &["NOUS_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "gemini" | "google" => &["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "google-gemini-cli" => &["HERMES_GEMINI_OAUTH_API_KEY"],
        "qwen" => &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        "qwen-oauth" => &["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "kimi-coding" => &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"],
        "kimi" | "moonshot" => &["KIMI_API_KEY", "KIMI_CODING_API_KEY", "MOONSHOT_API_KEY"],
        "kimi-coding-cn" => &["KIMI_CN_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"],
        "ollama-local" => &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"],
        "llama-cpp" => &["LLAMA_CPP_API_KEY"],
        "vllm" => &["VLLM_API_KEY"],
        "mlx" => &["MLX_API_KEY"],
        "apple-ane" => &["APPLE_ANE_API_KEY"],
        "sglang" => &["SGLANG_API_KEY"],
        "tgi" => &["TGI_API_KEY"],
        "lmstudio" | "lm-studio" => &["LMSTUDIO_API_KEY"],
        "lmdeploy" | "lm-deploy" => &["LMDEPLOY_API_KEY"],
        "localai" | "local-ai" => &["LOCALAI_API_KEY"],
        "koboldcpp" | "kobold-cpp" => &["KOBOLDCPP_API_KEY"],
        "text-generation-webui" | "oobabooga" => &["TEXT_GENERATION_WEBUI_API_KEY"],
        "tabbyapi" | "tabby-api" | "exllama" | "exllamav2" => &["TABBYAPI_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "minimax" | "minimax-cn" => &["MINIMAX_API_KEY"],
        "stepfun" => &["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"],
        _ => &[],
    }
}

async fn load_token_store_providers() -> HashSet<String> {
    let path = hermes_config::paths::hermes_home()
        .join("auth")
        .join("tokens.json");
    let Ok(store) = FileTokenStore::new(path).await else {
        return HashSet::new();
    };
    store
        .list_providers()
        .await
        .into_iter()
        .map(|provider| provider.to_ascii_lowercase())
        .collect()
}

fn provider_auth_detail(provider: &str, token_store_providers: &HashSet<String>) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    let mut sources: Vec<String> = Vec::new();
    for key in provider_env_key_hints(&normalized) {
        if std::env::var(key)
            .ok()
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        {
            sources.push(format!("env:{key}"));
            break;
        }
    }
    if token_store_providers.contains(&normalized) {
        sources.push("vault".to_string());
    }
    if crate::auth::read_provider_auth_state(&normalized)
        .ok()
        .flatten()
        .is_some()
    {
        sources.push("oauth".to_string());
    }

    if sources.is_empty() {
        let setup_hint = provider_env_key_hints(&normalized)
            .first()
            .copied()
            .unwrap_or("API_KEY");
        format!("auth:missing (setup: /auth {normalized} or {setup_hint})")
    } else {
        format!("auth:{}", sources.join("+"))
    }
}

async fn disconnect_provider_credentials(provider: &str) -> Result<(bool, bool), AgentError> {
    let normalized = provider.trim().to_ascii_lowercase();
    let path = hermes_config::paths::hermes_home()
        .join("auth")
        .join("tokens.json");
    let mut removed_vault = false;
    if let Ok(store) = FileTokenStore::new(path).await {
        removed_vault = store
            .list_providers()
            .await
            .into_iter()
            .any(|p| p.eq_ignore_ascii_case(&normalized));
        let _ = store.remove(&normalized).await;
    }
    let removed_oauth = crate::auth::clear_provider_auth_state(&normalized).unwrap_or(false);
    Ok((removed_vault, removed_oauth))
}

async fn open_model_provider_modal(state: &mut TuiState, app: &App) {
    let providers = crate::model_switch::curated_provider_slugs();
    let entries = crate::model_switch::provider_catalog_entries(&providers).await;
    let token_store_providers = load_token_store_providers().await;
    let mut items: Vec<PickerItem> = Vec::new();
    for provider in providers {
        let entry = entries
            .iter()
            .find(|entry| entry.provider.eq_ignore_ascii_case(provider));
        let auth_detail = provider_auth_detail(provider, &token_store_providers);
        let description = crate::model_switch::provider_picker_description(provider);
        let detail = if let Some(entry) = entry {
            if entry.models.is_empty() {
                format!(
                    "{} • {} models • {}",
                    description, entry.total_models, auth_detail
                )
            } else {
                format!(
                    "{} • {} models • {} • {}",
                    description,
                    entry.total_models,
                    entry.models.join(", "),
                    auth_detail
                )
            }
        } else {
            format!("{} • catalog unavailable • {}", description, auth_detail)
        };
        items.push(PickerItem {
            label: provider.to_string(),
            detail,
            value: provider.to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::ModelProvider, "Select Provider", items);
    let (current_provider, _) = app
        .current_model
        .split_once(':')
        .unwrap_or(("openai", app.current_model.as_str()));
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(current_provider)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

async fn open_provider_model_modal(state: &mut TuiState, app: &App, provider: &str) {
    let models = crate::model_switch::provider_model_ids(provider).await;
    if models.is_empty() {
        state.status_message = format!("No models available for provider `{provider}`");
        return;
    }
    let mut items = Vec::with_capacity(models.len());
    for model in models {
        items.push(PickerItem {
            label: model.clone(),
            detail: format!("{provider}:{model}"),
            value: model,
        });
    }
    let mut modal = PickerModal::new(
        PickerKind::ModelForProvider {
            provider: provider.to_string(),
        },
        format!("Select {provider} model"),
        items,
    );
    let (_, current_model_id) = app
        .current_model
        .split_once(':')
        .unwrap_or(("openai", app.current_model.as_str()));
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(current_model_id)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

fn open_personality_modal(state: &mut TuiState, app: &App) {
    let descriptions = hermes_agent::builtin_personality_descriptions();
    let mut items = Vec::with_capacity(descriptions.len());
    for (name, detail) in descriptions {
        items.push(PickerItem {
            label: (*name).to_string(),
            detail: (*detail).to_string(),
            value: (*name).to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::Personality, "Select Personality", items);
    if let Some(current) = app.current_personality.as_deref() {
        if let Some(idx) = modal
            .filtered_indices
            .iter()
            .position(|item_idx| modal.items[*item_idx].value.eq_ignore_ascii_case(current))
        {
            modal.selected_filtered = idx;
        }
    }
    state.open_modal(modal);
}

fn open_skin_modal(state: &mut TuiState) {
    let mut items = Vec::with_capacity(crate::skin_engine::BUILTIN_SKINS.len());
    for (name, detail) in crate::skin_engine::BUILTIN_SKINS {
        items.push(PickerItem {
            label: (*name).to_string(),
            detail: (*detail).to_string(),
            value: (*name).to_string(),
        });
    }
    let mut modal = PickerModal::new(PickerKind::Skin, "Select Skin", items);
    let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-sunburst".to_string());
    let active_canonical =
        crate::skin_engine::canonical_skin_name(&active).unwrap_or("ultra-sunburst");
    if let Some(idx) = modal.filtered_indices.iter().position(|item_idx| {
        modal.items[*item_idx]
            .value
            .eq_ignore_ascii_case(active_canonical)
    }) {
        modal.selected_filtered = idx;
    }
    state.open_modal(modal);
}

fn open_interactive_question_modal(state: &mut TuiState, request: InteractiveQuestionRequest) {
    let mut modal = PickerModal::new(
        PickerKind::InteractiveQuestion {
            prompt: request.prompt,
        },
        "Interactive Question",
        request.options,
    );
    modal.page_size = 8;
    modal.refresh_filter();
    state.open_modal(modal);
}

async fn process_modal_disconnect(state: &mut TuiState, app: &mut App) -> Result<(), AgentError> {
    let Some(modal) = state.modal.clone() else {
        return Ok(());
    };
    let Some(item) = modal.selected_item().cloned() else {
        state.status_message = "No provider selected".to_string();
        return Ok(());
    };
    if !matches!(modal.kind, PickerKind::ModelProvider) {
        state.status_message = "Disconnect is only supported in provider picker".to_string();
        return Ok(());
    }
    let provider = item.value.trim().to_ascii_lowercase();
    match disconnect_provider_credentials(&provider).await {
        Ok((removed_vault, removed_oauth)) => {
            if removed_vault || removed_oauth {
                state.status_message = format!(
                    "Disconnected `{provider}` (vault={}, oauth={})",
                    removed_vault, removed_oauth
                );
                app.push_ui_assistant(format!(
                    "Disconnected provider `{}` (vault={}, oauth={}).",
                    provider, removed_vault, removed_oauth
                ));
            } else {
                state.status_message =
                    format!("No stored credential found for `{provider}` to disconnect");
            }
            open_model_provider_modal(state, app).await;
        }
        Err(err) => {
            state.status_message = format!("Disconnect failed for `{provider}`: {err}");
        }
    }
    Ok(())
}

async fn process_modal_confirm(state: &mut TuiState, app: &mut App) -> Result<(), AgentError> {
    let Some(modal) = state.modal.clone() else {
        return Ok(());
    };
    let Some(item) = modal.selected_item().cloned() else {
        state.status_message = "Nothing selected".to_string();
        return Ok(());
    };
    match modal.kind {
        PickerKind::ModelProvider => {
            open_provider_model_modal(state, app, &item.value).await;
            state.status_message = format!("Provider selected: {}", item.value);
        }
        PickerKind::ModelForProvider { provider } => {
            let provider_model = format!("{provider}:{}", item.value.trim());
            let warning = app.model_switch_preflight_warning(&provider_model);
            if let Err(err) = app.try_switch_model(&provider_model) {
                let previous = app.current_model.clone();
                let notice = format!(
                    "Model switch to {} failed ({}); staying on {}.",
                    provider_model, err, previous
                );
                app.push_ui_assistant(notice.clone());
                state.close_modal();
                state.status_message = notice;
                return Ok(());
            }
            let mut notice = format!("Model switched to: {}", provider_model);
            if let Some(warning) = warning.as_deref() {
                notice.push('\n');
                notice.push_str(warning);
            }
            app.push_ui_assistant(notice);
            state.close_modal();
            state.status_message =
                warning.unwrap_or_else(|| format!("Switched model to {}", provider_model));
        }
        PickerKind::Personality => {
            app.switch_personality(item.value.as_str());
            app.push_ui_assistant(format!("Switched personality to `{}`.", item.value));
            state.close_modal();
            state.status_message = format!("Personality: {}", item.value);
        }
        PickerKind::Skin => {
            let skin = crate::skin_engine::canonical_skin_name(item.value.as_str())
                .unwrap_or("ultra-sunburst")
                .to_string();
            std::env::set_var("HERMES_THEME", &skin);
            app.request_theme_change(&skin);
            app.push_ui_assistant(format!("Switched skin to `{}`.", skin));
            state.close_modal();
            state.status_message = format!("Skin: {}", skin);
        }
        PickerKind::InteractiveQuestion { prompt } => {
            let chosen = item.value.trim().to_string();
            state.input = format!("{prompt}\nAnswer: {chosen}");
            state.cursor_position = state.input.len();
            state.close_modal();
            state.status_message = "Interactive answer inserted. Press Enter to send.".to_string();
            state.refresh_completions_for_app(Some(app));
        }
    }
    Ok(())
}

fn handle_agent_run_complete(
    app: &mut App,
    state: &mut TuiState,
    result: Result<AgentResult, String>,
    elapsed_secs: f64,
) {
    match result {
        Ok(agent_result) => {
            let total_turns = agent_result.total_turns;
            let interrupted = agent_result.interrupted;
            let finished_naturally = agent_result.finished_naturally;
            if let Err(err) = app.apply_agent_result_and_persist(agent_result) {
                tracing::warn!("session autosave skipped: {}", err);
                state.push_activity(format!("⚠ autosave skipped: {}", err));
            }
            state.finish_processing_cycle("✔ completed in");
            state.status_message.clear();
            state.push_activity(format!(
                "run finished in {:.2}s (total_turns={})",
                elapsed_secs, total_turns
            ));
            if interrupted {
                app.push_ui_assistant("[Agent execution interrupted]");
            } else if !finished_naturally {
                state.push_activity("run stopped before natural finish".to_string());
            }
        }
        Err(err) => {
            state.finish_processing_cycle("✖ failed after");
            state.status_message = format!("Error: {}", err);
            state.push_activity(format!("✖ {}", err));
            app.push_ui_assistant(format!("Error: {}", err));
        }
    }
    state.stream_buffer.clear();
    state.stream_muted = false;
    state.stream_needs_break = false;
    state.active_tools.clear();
    state.awaiting_run_complete = false;
}

fn handle_managed_app_run_complete(
    app: &mut App,
    state: &mut TuiState,
    result: Result<Box<App>, String>,
    elapsed_secs: f64,
) {
    match result {
        Ok(completed_app) => {
            *app = *completed_app;
            state.finish_processing_cycle("✔ completed in");
            state.status_message.clear();
            state.push_activity(format!("managed run finished in {:.2}s", elapsed_secs));
        }
        Err(err) => {
            state.finish_processing_cycle("✖ failed after");
            state.status_message = format!("Error: {}", err);
            state.push_activity(format!("✖ {}", err));
            app.push_ui_assistant(format!("Error: {}", err));
        }
    }
    state.stream_buffer.clear();
    state.stream_muted = false;
    state.stream_needs_break = false;
    state.active_tools.clear();
    state.awaiting_run_complete = false;
}

fn finalize_interrupted_tui_session(app: &mut App, state: &mut TuiState, reason: &str) {
    let partial_assistant = if state.stream_buffer.trim().is_empty() {
        None
    } else {
        Some(state.stream_buffer.clone())
    };
    if let Err(err) = app.finalize_interrupted_tui_session(partial_assistant.as_deref(), reason) {
        tracing::warn!(reason, error = %err, "interrupted TUI session autosave skipped");
        state.push_activity(format!("⚠ interrupted autosave skipped: {}", err));
    } else if !app.messages.is_empty() {
        state.push_activity("interrupted session snapshot flushed".to_string());
    }
    state.awaiting_run_complete = false;
}

fn extract_file_like_hints(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        if out.len() >= limit {
            break;
        }
        let cleaned = token
            .trim_matches(|c: char| {
                c == '"' || c == '\'' || c == ',' || c == ';' || c == ')' || c == '('
            })
            .to_string();
        if cleaned.len() < 4 {
            continue;
        }
        let looks_like_path = cleaned.contains('/')
            || cleaned.ends_with(".rs")
            || cleaned.ends_with(".py")
            || cleaned.ends_with(".toml")
            || cleaned.ends_with(".md")
            || cleaned.ends_with(".json")
            || cleaned.ends_with(".yaml")
            || cleaned.ends_with(".yml");
        if !looks_like_path {
            continue;
        }
        if !out.iter().any(|v| v == &cleaned) {
            out.push(cleaned);
        }
    }
    out
}

fn stream_chunk_has_progress(chunk: &StreamChunk) -> bool {
    if let Some(delta) = chunk.delta.as_ref() {
        let has_content = delta
            .content
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty());
        let has_tool_calls = delta
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty());
        let has_extra_event = delta.extra.as_ref().is_some_and(|extra| match extra {
            serde_json::Value::Null => false,
            serde_json::Value::Object(map) => !map.is_empty(),
            _ => true,
        });
        if has_content || has_tool_calls || has_extra_event {
            return true;
        }
    }
    chunk
        .finish_reason
        .as_ref()
        .is_some_and(|reason| !reason.trim().is_empty())
        || chunk.usage.is_some()
}

fn process_stream_lane_event(app: &mut App, state: &mut TuiState, event: Event) -> bool {
    match event {
        Event::StreamDelta(delta) => {
            if !delta.is_empty() {
                state.stream_chunk_count = state.stream_chunk_count.saturating_add(1);
                state.stream_char_count = state
                    .stream_char_count
                    .saturating_add(delta.chars().count());
                if !state.saw_first_token {
                    state.saw_first_token = true;
                    let first_token_ms = state
                        .processing_started_at
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or_default();
                    state.push_activity(format!("↧ first token in {}ms", first_token_ms));
                }
            }
            state.stream_buffer.push_str(&delta);
            true
        }
        Event::StreamChunk(chunk) => {
            if stream_chunk_has_progress(&chunk) {
                state.stream_chunk_count = state.stream_chunk_count.saturating_add(1);
            }
            if let Some(delta) = chunk.delta {
                if let Some(content) = delta.content.as_ref().filter(|text| !text.is_empty()) {
                    state.stream_char_count = state
                        .stream_char_count
                        .saturating_add(content.chars().count());
                }
                if let Some(extra) = delta.extra.as_ref() {
                    if let Some(control) = extra.get("control").and_then(|v| v.as_str()) {
                        if control == "mute_post_response" {
                            state.stream_muted = extra
                                .get("enabled")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false);
                        } else if control == "stream_break" {
                            state.stream_needs_break = true;
                        }
                    }
                    if let Some(ui_event) = extra.get("ui_event").and_then(|v| v.as_str()) {
                        match ui_event {
                            "tool_start" => {
                                let tool = extra
                                    .get("tool")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool")
                                    .trim()
                                    .to_string();
                                if !tool.is_empty()
                                    && !state.active_tools.iter().any(|t| t == &tool)
                                {
                                    state.active_tools.push(tool.clone());
                                }
                                let args_preview = extra
                                    .get("args_preview")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim();
                                if args_preview.is_empty() {
                                    state.push_activity(format!("▶ {}", tool));
                                } else {
                                    state.push_activity(format!("▶ {} {}", tool, args_preview));
                                }
                                state.push_activity(format!(
                                    "Δtools active={}",
                                    state.active_tools.len()
                                ));
                            }
                            "tool_complete" => {
                                let tool = extra
                                    .get("tool")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("tool")
                                    .trim()
                                    .to_string();
                                if let Some(idx) =
                                    state.active_tools.iter().position(|t| t == &tool)
                                {
                                    state.active_tools.remove(idx);
                                }
                                let result_preview = extra
                                    .get("result_preview")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim();
                                if result_preview.is_empty() {
                                    state.push_activity(format!("✓ {}", tool));
                                } else {
                                    state.push_activity(format!("✓ {} {}", tool, result_preview));
                                    let file_hints = extract_file_like_hints(result_preview, 3);
                                    if !file_hints.is_empty() {
                                        state.push_activity(format!(
                                            "Δfiles {}",
                                            file_hints.join(", ")
                                        ));
                                    }
                                }
                            }
                            "status" => {
                                let event_type = extra
                                    .get("event_type")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("status")
                                    .trim();
                                let message = extra
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim();
                                if !message.is_empty() {
                                    state.push_activity(format!("[{}] {}", event_type, message));
                                }
                            }
                            "phase" => {
                                let phase = extra
                                    .get("phase")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("phase");
                                let label =
                                    extra.get("label").and_then(|v| v.as_str()).unwrap_or("");
                                let progress_pct = extra
                                    .get("progress_pct")
                                    .and_then(|v| v.as_u64())
                                    .and_then(|v| u8::try_from(v).ok());
                                state.update_processing_phase(phase, label, progress_pct);
                            }
                            "lifecycle" => {
                                let message = extra
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .trim();
                                if !message.is_empty() {
                                    state.push_activity(format!("⟡ {}", message));
                                    let lower = message.to_ascii_lowercase();
                                    if lower.contains("mismatch")
                                        || lower.contains("remediation")
                                        || lower.contains("auto-refresh")
                                        || lower.contains("retrying")
                                        || lower.contains("fallback")
                                    {
                                        state.processing_degraded = true;
                                        state.degraded_notes.push(truncate_chars(message, 120));
                                        if state.degraded_notes.len() > 4 {
                                            let drop_count = state.degraded_notes.len() - 4;
                                            state.degraded_notes.drain(0..drop_count);
                                        }
                                    }
                                }
                            }
                            "thinking" => {
                                if let Some(text) = extra.get("text").and_then(|v| v.as_str()) {
                                    state.append_live_thinking(text);
                                }
                            }
                            _ => {}
                        }
                    }
                    if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                        state.append_live_thinking(thinking);
                    }
                }
                if let Some(content) = delta.content {
                    if !state.stream_muted {
                        if state.stream_needs_break {
                            state.stream_buffer.push_str("\n\n");
                            state.stream_needs_break = false;
                        }
                        state.stream_buffer.push_str(&content);
                        state.stream_char_count = state
                            .stream_char_count
                            .saturating_add(content.chars().count());
                        if !state.saw_first_token {
                            state.saw_first_token = true;
                            let first_token_ms = state
                                .processing_started_at
                                .map(|t| t.elapsed().as_millis())
                                .unwrap_or_default();
                            state.push_activity(format!("↧ first token in {}ms", first_token_ms));
                        }
                        if state.auto_follow_transcript {
                            state.scroll_offset = 0;
                        }
                    }
                }
            }
            if let Some(usage) = chunk.usage {
                state.last_usage = Some((
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens,
                ));
                let previous = state.last_usage_total_emitted.unwrap_or(0);
                if usage.total_tokens >= previous.saturating_add(64)
                    || state.last_usage_total_emitted.is_none()
                {
                    let delta_total = usage.total_tokens.saturating_sub(previous);
                    state.push_activity(format!(
                        "Δtokens p={} c={} t={} (+{})",
                        usage.prompt_tokens,
                        usage.completion_tokens,
                        usage.total_tokens,
                        delta_total
                    ));
                    state.last_usage_total_emitted = Some(usage.total_tokens);
                }
            }
            true
        }
        Event::AgentDone => {
            if state.awaiting_run_complete {
                state.push_activity("finalizing transcript writeback…".to_string());
                state.status_message = "Finalizing response…".to_string();
            } else {
                state.finish_processing_cycle("✔ completed in");
                state.stream_buffer.clear();
                state.stream_muted = false;
                state.stream_needs_break = false;
                state.active_tools.clear();
                state.status_message.clear();
            }
            true
        }
        Event::AgentRunComplete {
            result,
            elapsed_secs,
        } => {
            handle_agent_run_complete(app, state, result, elapsed_secs);
            true
        }
        Event::ManagedAppRunComplete {
            result,
            elapsed_secs,
        } => {
            handle_managed_app_run_complete(app, state, result, elapsed_secs);
            true
        }
        _ => false,
    }
}

