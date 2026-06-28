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

