impl TelegramAdapter {
    pub fn should_process_message(&self, msg: &TelegramMessage, is_command: bool) -> bool {
        let chat_kind = ChatKind::from_telegram_type(&msg.chat.chat_type);
        if !chat_kind.is_group_like() {
            return true;
        }

        let chat_id = msg.chat.id.to_string();
        let thread_id = msg
            .message_thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "0".to_string());

        if Self::contains_id(&self.config.ignored_threads, &thread_id) {
            return false;
        }

        if !self.config.allowed_topics.is_empty()
            && !Self::contains_id(&self.config.allowed_topics, &thread_id)
        {
            return false;
        }

        let allowed_chat = self.config.allowed_chats.is_empty()
            && self.config.group_allowed_chats.is_empty()
            || Self::contains_id(&self.config.allowed_chats, &chat_id)
            || Self::contains_id(&self.config.group_allowed_chats, &chat_id);

        let direct_mention = self.has_direct_bot_mention(msg, is_command);
        if !allowed_chat {
            return self.config.guest_mode && direct_mention;
        }

        if Self::contains_id(&self.config.free_response_chats, &chat_id) {
            return true;
        }

        if !self.config.require_mention {
            return true;
        }

        direct_mention || self.is_reply_to_bot(msg) || self.matches_mention_pattern(msg)
    }

    pub fn should_process_update(&self, update: &Update) -> bool {
        match (&update.message, &update.callback_query) {
            (Some(msg), _) => self.should_process_message(
                msg,
                msg.text
                    .as_deref()
                    .map(str::trim_start)
                    .is_some_and(|text| text.starts_with('/')),
            ),
            (None, Some(cq)) => cq
                .message
                .as_ref()
                .map(|msg| self.should_process_message(msg, false))
                .unwrap_or(true),
            (None, None) => true,
        }
    }

    fn contains_id(values: &[String], candidate: &str) -> bool {
        let candidate = candidate.trim();
        values.iter().any(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|part| !part.is_empty() && part == candidate)
        })
    }

    fn has_direct_bot_mention(&self, msg: &TelegramMessage, is_command: bool) -> bool {
        let Some(bot_username) = self.config.bot_username.as_deref() else {
            return !self.config.exclusive_bot_mentions && !self.config.require_mention;
        };
        let bot_username = bot_username.trim().trim_start_matches('@');
        if bot_username.is_empty() {
            return false;
        }
        let mention = format!("@{bot_username}");
        let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");
        let entities = if msg.text.is_some() {
            &msg.entities
        } else {
            &msg.caption_entities
        };

        for entity in entities {
            let Some(token) = Self::entity_text(text, entity) else {
                continue;
            };
            match entity.entity_type.as_str() {
                "mention" | "text_mention" if token.eq_ignore_ascii_case(&mention) => return true,
                "bot_command" if is_command => {
                    if let Some((_, addressed_to)) = token.split_once('@') {
                        return addressed_to.eq_ignore_ascii_case(bot_username);
                    }
                    if !self.config.exclusive_bot_mentions && !self.config.require_mention {
                        return true;
                    }
                }
                _ => {}
            }
        }

        Self::contains_bot_mention_boundary(text, bot_username)
    }

    fn entity_text<'a>(text: &'a str, entity: &MessageEntity) -> Option<&'a str> {
        let start = entity.offset;
        let end = entity.offset.saturating_add(entity.length);
        if start >= end
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            return None;
        }
        Some(&text[start..end])
    }

    fn contains_bot_mention_boundary(text: &str, bot_username: &str) -> bool {
        let target = format!("@{}", bot_username.to_ascii_lowercase());
        let lower = text.to_ascii_lowercase();
        let bytes = lower.as_bytes();
        let target_bytes = target.as_bytes();
        if target_bytes.is_empty() || bytes.len() < target_bytes.len() {
            return false;
        }
        for idx in 0..=bytes.len() - target_bytes.len() {
            if &bytes[idx..idx + target_bytes.len()] != target_bytes {
                continue;
            }
            let before_ok = idx == 0
                || !bytes[idx - 1].is_ascii_alphanumeric()
                    && bytes[idx - 1] != b'_'
                    && bytes[idx - 1] != b'@';
            let after_idx = idx + target_bytes.len();
            let after_ok = after_idx == bytes.len()
                || !bytes[after_idx].is_ascii_alphanumeric() && bytes[after_idx] != b'_';
            if before_ok && after_ok {
                return true;
            }
        }
        false
    }

    fn is_reply_to_bot(&self, msg: &TelegramMessage) -> bool {
        let Some(reply) = msg.reply_to_message.as_ref() else {
            return false;
        };
        let Some(user) = reply.from.as_ref() else {
            return false;
        };
        if user.is_bot == Some(true) {
            return true;
        }
        match (&self.config.bot_username, &user.username) {
            (Some(bot), Some(username)) => bot
                .trim_start_matches('@')
                .eq_ignore_ascii_case(username.trim_start_matches('@')),
            _ => false,
        }
    }

    fn matches_mention_pattern(&self, msg: &TelegramMessage) -> bool {
        let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("");
        self.config.mention_patterns.iter().any(|pattern| {
            regex::Regex::new(pattern)
                .map(|re| re.is_match(text))
                .unwrap_or(false)
        })
    }

    fn outgoing_text_for_parse_mode(&self, text: &str, parse_mode: Option<&str>) -> String {
        match parse_mode {
            Some(mode) if mode.eq_ignore_ascii_case("MarkdownV2") => to_telegram_markdown_v2(text),
            _ => text.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // Sending messages
    // -----------------------------------------------------------------------

}
