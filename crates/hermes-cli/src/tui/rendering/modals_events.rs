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
