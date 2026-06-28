impl TelegramAdapter {
    fn rich_send_is_disabled(&self) -> bool {
        self.rich_send_disabled
            .lock()
            .map(|guard| *guard)
            .unwrap_or(true)
    }

    fn latch_rich_send_disabled(&self) {
        if let Ok(mut disabled) = self.rich_send_disabled.lock() {
            *disabled = true;
        }
    }

    fn content_fits_rich_limits(content: &str) -> bool {
        content.chars().count() <= RICH_MESSAGE_MAX_CHARS
    }

    fn has_telegram_desktop_details_math_crash_shape(content: &str) -> bool {
        if content.trim().is_empty() || !content.to_ascii_lowercase().contains("<details") {
            return false;
        }
        let details = regex::RegexBuilder::new(r"<details\b[^>]*>.*?</details>")
            .case_insensitive(true)
            .dot_matches_new_line(true)
            .build();
        let math = regex::RegexBuilder::new(
            r"(\$\$.*?\$\$|\\\[.*?\\\]|\\\(.*?\\\)|\\(?:sum|frac|alpha|beta|gamma|delta|theta|lambda|mu|pi|sigma|int|prod|sqrt|lim|infty|begin\{(?:equation|align|matrix|cases)\}))",
        )
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .build();
        let (Ok(details), Ok(math)) = (details, math) else {
            return false;
        };
        let has_crash_shape = details
            .find_iter(content)
            .any(|block| math.is_match(block.as_str()));
        has_crash_shape
    }

    fn has_telegram_desktop_cjk_rich_garble_shape(content: &str) -> bool {
        content.chars().any(|ch| {
            matches!(
                ch,
                '\u{3040}'..='\u{30ff}'
                    | '\u{3400}'..='\u{4dbf}'
                    | '\u{4e00}'..='\u{9fff}'
                    | '\u{ac00}'..='\u{d7af}'
                    | '\u{f900}'..='\u{faff}'
                    | '\u{20000}'..='\u{323af}'
            )
        })
    }

    fn needs_rich_rendering(content: &str) -> bool {
        if content.trim().is_empty() {
            return false;
        }
        if content
            .lines()
            .any(Self::looks_like_markdown_table_separator)
        {
            return true;
        }
        if regex::Regex::new(r"(?m)^\s*[-*]\s+\[[ xX]\]\s+")
            .map(|re| re.is_match(content))
            .unwrap_or(false)
        {
            return true;
        }
        if regex::RegexBuilder::new(r"(?m)^</?details\b|^</?summary\b")
            .case_insensitive(true)
            .build()
            .map(|re| re.is_match(content))
            .unwrap_or(false)
        {
            return true;
        }
        content.contains("$$")
    }

    fn content_is_pipe_table_primary(content: &str) -> bool {
        if content.trim().is_empty()
            || !content
                .lines()
                .any(Self::looks_like_markdown_table_separator)
        {
            return false;
        }
        if regex::Regex::new(r"(?m)^\s*[-*]\s+\[[ xX]\]\s+")
            .map(|re| re.is_match(content))
            .unwrap_or(false)
        {
            return false;
        }
        if regex::RegexBuilder::new(r"(?m)^</?details\b|^</?summary\b")
            .case_insensitive(true)
            .build()
            .map(|re| re.is_match(content))
            .unwrap_or(false)
        {
            return false;
        }
        !content.contains("$$")
    }

    fn looks_like_markdown_table_separator(line: &str) -> bool {
        let trimmed = line.trim();
        if !trimmed.contains('|') || !trimmed.contains('-') {
            return false;
        }
        let cells = trimmed.trim_matches('|').split('|').collect::<Vec<_>>();
        cells.len() >= 2
            && cells.iter().all(|cell| {
                let cell = cell.trim();
                cell.len() >= 3
                    && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
                    && cell.contains('-')
            })
    }

    fn rich_eligible_text(&self, text: &str) -> bool {
        (self.config.rich_messages || Self::content_is_pipe_table_primary(text))
            && !self.rich_send_is_disabled()
            && !text.trim().is_empty()
            && Self::needs_rich_rendering(text)
            && Self::content_fits_rich_limits(text)
            && !Self::has_telegram_desktop_details_math_crash_shape(text)
            && !Self::has_telegram_desktop_cjk_rich_garble_shape(text)
    }

    fn should_attempt_rich_text(
        &self,
        text: &str,
        keyboard: Option<&InlineKeyboardMarkup>,
    ) -> bool {
        keyboard.is_none() && self.rich_eligible_text(text)
    }

    fn rich_message_body(
        &self,
        chat_id: &str,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> serde_json::Value {
        let markdown = Self::rich_normalize_linebreaks(text);
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "rich_message": {
                "markdown": markdown,
            },
        });
        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }
        if let Some(reply_id) = reply_to_message_id {
            body["reply_parameters"] = serde_json::json!({ "message_id": reply_id });
        }
        if self.config.disable_link_previews {
            body["link_preview_options"] = serde_json::json!({ "is_disabled": true });
        }
        body
    }

    fn rich_edit_body(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        message_thread_id: Option<i64>,
    ) -> serde_json::Value {
        let markdown = Self::rich_normalize_linebreaks(text);
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "rich_message": {
                "markdown": markdown,
            },
        });
        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }
        if self.config.disable_link_previews {
            body["link_preview_options"] = serde_json::json!({ "is_disabled": true });
        }
        body
    }

    fn line_without_newline(line: &str) -> &str {
        line.trim_end_matches('\n').trim_end_matches('\r')
    }

    fn rich_line_protection_mask(lines: &[&str]) -> Vec<bool> {
        let mut protected = vec![false; lines.len()];

        let mut in_fence = false;
        for (idx, line) in lines.iter().enumerate() {
            let trimmed = Self::line_without_newline(line).trim_start();
            if in_fence || trimmed.starts_with("```") {
                protected[idx] = true;
            }
            if trimmed.starts_with("```") {
                in_fence = !in_fence;
            }
        }

        let mut idx = 0;
        while idx + 1 < lines.len() {
            if protected[idx] || protected[idx + 1] {
                idx += 1;
                continue;
            }

            let header = Self::line_without_newline(lines[idx]);
            let delimiter = Self::line_without_newline(lines[idx + 1]);
            if header.contains('|') && Self::looks_like_markdown_table_separator(delimiter) {
                protected[idx] = true;
                protected[idx + 1] = true;

                let mut body_idx = idx + 2;
                while body_idx < lines.len() {
                    let body = Self::line_without_newline(lines[body_idx]);
                    if protected[body_idx] || body.trim().is_empty() || !body.contains('|') {
                        break;
                    }
                    protected[body_idx] = true;
                    body_idx += 1;
                }

                idx = body_idx;
            } else {
                idx += 1;
            }
        }

        protected
    }

    fn rich_normalize_linebreaks(text: &str) -> String {
        if text.is_empty() || !text.contains('\n') {
            return text.to_string();
        }

        let lines = text.split_inclusive('\n').collect::<Vec<_>>();
        let protected = Self::rich_line_protection_mask(&lines);
        let mut out = String::with_capacity(text.len() + lines.len().saturating_mul(2));

        for (idx, line) in lines.iter().enumerate() {
            let Some(without_newline) = line.strip_suffix('\n') else {
                out.push_str(line);
                continue;
            };

            out.push_str(without_newline);
            let current_blank = Self::line_without_newline(line).trim().is_empty();
            let next_blank = lines
                .get(idx + 1)
                .map(|next| Self::line_without_newline(next).trim().is_empty())
                .unwrap_or(false);

            if idx + 1 < lines.len()
                && !protected[idx]
                && !protected[idx + 1]
                && !current_blank
                && !next_blank
            {
                out.push_str("  ");
            }
            out.push('\n');
        }

        out
    }

    fn flatten_rich_inline_text(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(text) => text.clone(),
            serde_json::Value::Array(items) => items
                .iter()
                .map(Self::flatten_rich_inline_text)
                .collect::<String>(),
            serde_json::Value::Object(map) => map
                .get("text")
                .map(Self::flatten_rich_inline_text)
                .or_else(|| map.get("children").map(Self::flatten_rich_inline_text))
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn rich_label_text(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(text) => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
            _ => None,
        }
    }

    fn flatten_rich_blocks(blocks: &serde_json::Value) -> String {
        let Some(blocks) = blocks.as_array() else {
            return String::new();
        };

        let mut lines = Vec::new();
        for block in blocks {
            let Some(block) = block.as_object() else {
                continue;
            };

            if let Some(items) = block.get("items").and_then(|value| value.as_array()) {
                for item in items {
                    let Some(item) = item.as_object() else {
                        continue;
                    };
                    let item_text = item
                        .get("blocks")
                        .map(Self::flatten_rich_blocks)
                        .unwrap_or_default();
                    if item_text.trim().is_empty() {
                        continue;
                    }

                    let mut item_lines = item_text.lines();
                    let Some(first_line) = item_lines.next() else {
                        continue;
                    };
                    if let Some(label) = item.get("label").and_then(Self::rich_label_text) {
                        lines.push(format!("{label} {first_line}"));
                    } else {
                        lines.push(first_line.to_string());
                    }
                    lines.extend(item_lines.map(ToOwned::to_owned));
                }
                continue;
            }

            if let Some(text) = block.get("text").map(Self::flatten_rich_inline_text) {
                lines.extend(text.lines().map(ToOwned::to_owned));
            }
        }

        lines
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn extract_rich_reply_text(reply_to_message: &TelegramMessage) -> Option<String> {
        let text = reply_to_message
            .rich_message
            .as_ref()
            .and_then(|rich| rich.get("blocks"))
            .map(Self::flatten_rich_blocks)
            .unwrap_or_default();
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_string())
    }

    fn rich_capability_error(err: &GatewayError) -> bool {
        let message = match err {
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::ConnectionFailed(message) => message.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("no such method")
            || message.contains("not implemented")
            || ((message.contains("method") || message.contains("endpoint"))
                && (message.contains("not found") || message.contains("does not exist")))
    }

    fn rich_fallback_error(err: &GatewayError) -> bool {
        let message = match err {
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::ConnectionFailed(message) => message.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("bad request")
            || message.contains("unsupported")
            || message.contains("not implemented")
            || message.contains("no such method")
            || ((message.contains("method") || message.contains("endpoint"))
                && (message.contains("not found") || message.contains("does not exist")))
    }

    fn rich_not_modified_error(err: &GatewayError) -> bool {
        match err {
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::ConnectionFailed(message) => {
                message.to_ascii_lowercase().contains("not modified")
            }
            _ => false,
        }
    }

    /// Merge media captions without using substring checks that drop distinct captions.
    pub fn merge_caption(existing: Option<&str>, caption: &str) -> String {
        let caption = caption.trim();
        let existing = existing.unwrap_or("").trim();

        if existing.is_empty() {
            return caption.to_string();
        }
        if caption.is_empty() {
            return existing.to_string();
        }

        let seen = existing
            .split("\n\n")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .any(|part| part == caption);
        if seen {
            existing.to_string()
        } else {
            format!("{existing}\n\n{caption}")
        }
    }

}
