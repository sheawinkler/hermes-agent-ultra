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

