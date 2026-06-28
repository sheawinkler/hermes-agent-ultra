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

