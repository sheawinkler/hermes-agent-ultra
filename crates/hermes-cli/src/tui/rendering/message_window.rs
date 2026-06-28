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
