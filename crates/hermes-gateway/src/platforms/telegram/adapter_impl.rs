impl TelegramAdapter {
    /// Create a new Telegram adapter with the given configuration.
    pub fn new(config: TelegramConfig) -> Result<Self, GatewayError> {
        Self::validate_webhook_secret(&config)?;

        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());

        base.validate_token()?;

        let client = Self::build_client(&base, &config.fallback_ips)?;
        let api_base = format!("https://api.telegram.org/bot{}", config.token);

        Ok(Self {
            base,
            config,
            client,
            api_base,
            poll_offset: AtomicI64::new(0),
            stop_signal: Arc::new(Notify::new()),
            backoff_ms: AtomicU64::new(0),
            consecutive_errors: AtomicU64::new(0),
            status_message_ids: Mutex::new(HashMap::new()),
            approval_state: Mutex::new(HashMap::new()),
            approval_counter: AtomicU64::new(1),
            rich_send_disabled: Mutex::new(false),
            topic_bindings: Mutex::new(TelegramTopicBindingStore::default()),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &TelegramConfig {
        &self.config
    }

    pub fn bind_dm_topic(
        &self,
        chat_id: impl Into<String>,
        thread_id: impl Into<String>,
        session_id: impl Into<String>,
        user_id: impl Into<String>,
        title: Option<String>,
        operator_declared: bool,
    ) {
        if let Ok(mut store) = self.topic_bindings.lock() {
            store.bind(
                chat_id,
                thread_id,
                session_id,
                user_id,
                title,
                operator_declared,
            );
        }
    }

    pub fn dm_topic_binding(&self, chat_id: &str, thread_id: &str) -> Option<TelegramTopicBinding> {
        self.topic_bindings
            .lock()
            .ok()
            .and_then(|store| store.get(chat_id, thread_id).cloned())
    }

    pub fn prune_stale_dm_topic_binding(&self, chat_id: &str, thread_id: &str) -> bool {
        self.topic_bindings
            .lock()
            .map(|mut store| store.remove(chat_id, thread_id))
            .unwrap_or(false)
    }

    fn split_gateway_chat_thread(chat_id: &str) -> (&str, Option<i64>) {
        let Some((base_chat_id, thread_id)) = chat_id.rsplit_once(':') else {
            return (chat_id, None);
        };
        if base_chat_id.trim().is_empty() || thread_id.trim().is_empty() {
            return (chat_id, None);
        }
        if base_chat_id.parse::<i64>().is_err() {
            return (chat_id, None);
        }
        match thread_id.parse::<i64>() {
            Ok(0) | Err(_) => (chat_id, None),
            Ok(thread_id) => (base_chat_id, Some(thread_id)),
        }
    }

    fn build_client(
        base: &BasePlatformAdapter,
        fallback_ips: &[String],
    ) -> Result<Client, GatewayError> {
        let valid_fallbacks = Self::fallback_socket_addrs(fallback_ips);
        if valid_fallbacks.is_empty() {
            return base.build_client();
        }

        let mut builder =
            platform_http_client_builder().resolve_to_addrs(TELEGRAM_API_HOST, &valid_fallbacks);

        if let Some(ref http_proxy) = base.proxy.http_proxy {
            let proxy = reqwest::Proxy::all(http_proxy).map_err(|e| {
                GatewayError::ConnectionFailed(format!("Invalid HTTP proxy: {}", e))
            })?;
            builder = builder.proxy(proxy);
        }

        if let Some(ref socks_proxy) = base.proxy.socks_proxy {
            let proxy = reqwest::Proxy::all(socks_proxy).map_err(|e| {
                GatewayError::ConnectionFailed(format!("Invalid SOCKS proxy: {}", e))
            })?;
            builder = builder.proxy(proxy);
        }

        builder.build().map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to build HTTP client: {}", e))
        })
    }

    pub fn fallback_socket_addrs(raw_ips: &[String]) -> Vec<SocketAddr> {
        let mut seen = HashSet::new();
        raw_ips
            .iter()
            .flat_map(|entry| entry.split(','))
            .filter_map(|entry| {
                let trimmed = entry.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let ip = trimmed.parse::<IpAddr>().ok()?;
                if !seen.insert(ip) {
                    return None;
                }
                Some(SocketAddr::new(ip, 0))
            })
            .collect()
    }

    fn validate_webhook_secret(config: &TelegramConfig) -> Result<(), GatewayError> {
        let webhook_url = config.webhook_url.as_deref().map(str::trim);
        let webhook_enabled = webhook_url.filter(|s| !s.is_empty()).is_some() || !config.polling;
        if !webhook_enabled {
            return Ok(());
        }

        let has_secret = config
            .webhook_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
        if has_secret {
            return Ok(());
        }

        Err(GatewayError::Auth(
            "Telegram webhook mode requires TELEGRAM_WEBHOOK_SECRET / webhook_secret; \
             generate one with `openssl rand -hex 32` and set it before enabling webhooks \
             (GHSA-3vpc-7q5r-276h)"
                .to_string(),
        ))
    }

    pub fn reply_to_mode(&self) -> TelegramReplyToMode {
        TelegramReplyToMode::parse(Some(&self.config.reply_to_mode))
    }

    pub fn should_thread_reply(
        &self,
        reply_to_message_id: Option<i64>,
        chunk_index: usize,
    ) -> bool {
        reply_to_message_id.is_some() && self.reply_to_mode().references_chunk(chunk_index)
    }

    fn sanitize_bot_command_name(raw: &str) -> Option<String> {
        let token = raw
            .trim()
            .trim_start_matches('/')
            .split_whitespace()
            .next()?
            .split('@')
            .next()
            .unwrap_or_default()
            .trim();
        if token.is_empty() {
            return None;
        }

        let mut out = String::new();
        let mut last_underscore = false;
        for ch in token.to_ascii_lowercase().replace('-', "_").chars() {
            if ch.is_ascii_alphanumeric() {
                out.push(ch);
                last_underscore = false;
            } else if ch == '_' && !last_underscore {
                out.push('_');
                last_underscore = true;
            }
            if out.len() >= 32 {
                break;
            }
        }

        let out = out.trim_matches('_').to_string();
        (!out.is_empty()).then_some(out)
    }

    fn command_menu_limit(&self) -> usize {
        self.config
            .command_menu_max_commands
            .clamp(1, TELEGRAM_BOT_COMMAND_API_MAX)
    }

    fn command_menu_priority(&self) -> Vec<String> {
        let configured = self
            .config
            .command_menu_priority
            .iter()
            .filter_map(|name| Self::sanitize_bot_command_name(name))
            .collect::<Vec<_>>();
        let defaults = TELEGRAM_COMMAND_MENU_PRIORITY
            .iter()
            .filter_map(|name| Self::sanitize_bot_command_name(name))
            .collect::<Vec<_>>();

        let mut raw = match self
            .config
            .command_menu_priority_mode
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "replace" => configured,
            "append" => defaults.into_iter().chain(configured).collect(),
            _ => configured.into_iter().chain(defaults).collect(),
        };

        let mut seen = HashSet::new();
        raw.retain(|name| seen.insert(name.clone()));
        raw
    }

    fn command_menu_commands(&self) -> Vec<TelegramBotCommand> {
        if !self.config.command_menu_enabled {
            return Vec::new();
        }

        let priority = self
            .command_menu_priority()
            .into_iter()
            .enumerate()
            .map(|(index, name)| (name, index))
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        let mut commands = all_commands()
            .into_iter()
            .filter_map(|info| {
                let command = Self::sanitize_bot_command_name(info.name)?;
                if !seen.insert(command.clone()) {
                    return None;
                }
                Some(TelegramBotCommand {
                    command,
                    description: truncate_chars(info.description.trim(), 256),
                })
            })
            .enumerate()
            .collect::<Vec<_>>();

        commands.sort_by_key(|(original_index, command)| {
            (
                priority
                    .get(&command.command)
                    .copied()
                    .unwrap_or(usize::MAX),
                *original_index,
            )
        });

        commands
            .into_iter()
            .map(|(_, command)| command)
            .take(self.command_menu_limit())
            .collect()
    }

    async fn register_command_menu(&self) -> Result<usize, GatewayError> {
        let commands = self.command_menu_commands();
        if commands.is_empty() {
            return Ok(0);
        }

        let url = format!("{}/setMyCommands", self.api_base);
        for scope_type in ["default", "all_private_chats", "all_group_chats"] {
            let body = serde_json::json!({
                "commands": commands.clone(),
                "scope": { "type": scope_type },
            });
            let resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
            if !resp.ok {
                return Err(GatewayError::SendFailed(resp.description.unwrap_or_else(
                    || format!("setMyCommands failed for scope {scope_type}"),
                )));
            }
        }

        Ok(commands.len())
    }

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

    /// Send a text message, splitting into multiple messages if it exceeds
    /// the 4096 character limit.
    pub async fn send_text(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        self.send_text_inner(chat_id, text, parse_mode, reply_to_message_id, None, None)
            .await
    }

    /// Send a text message with an inline keyboard attached.
    pub async fn send_text_with_keyboard(
        &self,
        chat_id: &str,
        text: &str,
        keyboard: InlineKeyboardMarkup,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        self.send_text_inner(
            chat_id,
            text,
            parse_mode,
            reply_to_message_id,
            Some(keyboard),
            message_thread_id,
        )
        .await
    }

    async fn send_text_inner(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<&str>,
        reply_to_message_id: Option<i64>,
        keyboard: Option<InlineKeyboardMarkup>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_thread_id = message_thread_id.or(inferred_thread_id);
        if self.should_attempt_rich_text(text, keyboard.as_ref()) {
            if let Some(message_id) = self
                .try_send_rich_text(chat_id, text, reply_to_message_id, message_thread_id)
                .await?
            {
                return Ok(vec![message_id]);
            }
        }

        let chunks = split_message(text, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let rendered_chunk = self.outgoing_text_for_parse_mode(chunk, parse_mode);
            let mut body = serde_json::json!({
                "chat_id": chat_id,
                "text": rendered_chunk,
            });

            if let Some(pm) = parse_mode {
                body["parse_mode"] = serde_json::Value::String(pm.to_string());
            }

            if self.config.disable_link_previews {
                body["disable_web_page_preview"] = serde_json::Value::Bool(true);
            }

            if let Some(thread_id) = message_thread_id {
                body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
            }

            if self.should_thread_reply(reply_to_message_id, i) {
                if let Some(reply_id) = reply_to_message_id {
                    body["reply_to_message_id"] = serde_json::Value::Number(reply_id.into());
                }
            }

            // Attach keyboard only to the last chunk.
            if i == chunks.len() - 1 {
                if let Some(ref kb) = keyboard {
                    body["reply_markup"] =
                        serde_json::to_value(kb).unwrap_or(serde_json::Value::Null);
                }
            }

            let resp: TelegramResponse<SentMessage> = self
                .send_json_with_thread_fallback("sendMessage", body)
                .await?;

            if let Some(msg) = resp.result {
                message_ids.push(msg.message_id);
            } else {
                return Err(GatewayError::SendFailed(
                    resp.description
                        .unwrap_or_else(|| "sendMessage returned no message".to_string()),
                ));
            }
        }

        Ok(message_ids)
    }

    async fn try_send_rich_text(
        &self,
        chat_id: &str,
        text: &str,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Option<i64>, GatewayError> {
        let body = self.rich_message_body(chat_id, text, reply_to_message_id, message_thread_id);
        let resp: TelegramResponse<SentMessage> = match self
            .send_json_with_thread_fallback("sendRichMessage", body)
            .await
        {
            Ok(resp) => resp,
            Err(err) if Self::rich_fallback_error(&err) => {
                if Self::rich_capability_error(&err) {
                    self.latch_rich_send_disabled();
                }
                return Ok(None);
            }
            Err(err) => return Err(err),
        };
        resp.result
            .map(|msg| msg.message_id)
            .ok_or_else(|| GatewayError::SendFailed("sendRichMessage returned no message".into()))
            .map(Some)
    }

    async fn try_edit_rich_text(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        message_thread_id: Option<i64>,
    ) -> Result<Option<()>, GatewayError> {
        let body = self.rich_edit_body(chat_id, message_id, text, message_thread_id);
        match self
            .send_json_with_thread_fallback::<serde_json::Value>("editMessageText", body)
            .await
        {
            Ok(_) => Ok(Some(())),
            Err(err) if Self::rich_not_modified_error(&err) => Ok(Some(())),
            Err(err) if Self::rich_fallback_error(&err) => {
                if Self::rich_capability_error(&err) {
                    self.latch_rich_send_disabled();
                }
                Ok(None)
            }
            Err(err) => Err(err),
        }
    }

    /// Edit an existing message's text.
    pub async fn edit_text(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (chat_id, message_thread_id) = Self::split_gateway_chat_thread(chat_id);
        if self.rich_eligible_text(text) {
            if let Some(()) = self
                .try_edit_rich_text(chat_id, message_id, text, message_thread_id)
                .await?
            {
                return Ok(());
            }
        }

        let rendered_text = self.outgoing_text_for_parse_mode(text, parse_mode);
        let rendered_text = truncate_chars(&rendered_text, MAX_MESSAGE_LENGTH);

        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "text": rendered_text,
        });

        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }

        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm.to_string());
        }

        let _resp: TelegramResponse<serde_json::Value> = self
            .send_json_with_thread_fallback("editMessageText", body)
            .await?;
        Ok(())
    }

    /// Answer a callback query (acknowledges the button press to the user).
    pub async fn answer_callback_query(
        &self,
        callback_query_id: &str,
        text: Option<&str>,
        show_alert: bool,
    ) -> Result<(), GatewayError> {
        let mut body = serde_json::json!({
            "callback_query_id": callback_query_id,
            "show_alert": show_alert,
        });

        if let Some(t) = text {
            body["text"] = serde_json::Value::String(t.to_string());
        }

        let url = format!("{}/answerCallbackQuery", self.api_base);
        let _resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        Ok(())
    }

    async fn send_json_with_thread_fallback<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        mut body: serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let url = format!("{}/{}", self.api_base, method);
        match self.post_json(&url, &body).await {
            Ok(resp) if resp.ok => Ok(resp),
            Ok(resp) => {
                let stale_topic_target = Self::thread_fallback_target_from_body(&body);
                let description = resp
                    .description
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{method} failed"));
                if Self::thread_or_reply_missing(&description)
                    && Self::strip_thread_fields_for_fallback(&mut body)
                {
                    self.prune_stale_dm_topic_binding_target(stale_topic_target);
                    let retry = self.post_json(&url, &body).await?;
                    if retry.ok {
                        return Ok(retry);
                    }
                    return Err(Self::telegram_response_error(
                        &format!("{method} fallback"),
                        &retry,
                    ));
                }
                Err(Self::telegram_response_error(method, &resp))
            }
            Err(err) if Self::gateway_error_thread_or_reply_missing(&err) => {
                let stale_topic_target = Self::thread_fallback_target_from_body(&body);
                if Self::strip_thread_fields_for_fallback(&mut body) {
                    self.prune_stale_dm_topic_binding_target(stale_topic_target);
                    self.post_json(&url, &body).await
                } else {
                    Err(err)
                }
            }
            Err(err) => Err(err),
        }
    }

    fn strip_thread_fields_for_fallback(body: &mut serde_json::Value) -> bool {
        let Some(obj) = body.as_object_mut() else {
            return false;
        };
        let removed_thread = obj.remove("message_thread_id").is_some();
        let removed_reply = obj.remove("reply_to_message_id").is_some();
        let removed_reply_parameters = obj.remove("reply_parameters").is_some();
        removed_thread || removed_reply || removed_reply_parameters
    }

    fn thread_fallback_target_from_body(body: &serde_json::Value) -> Option<(String, String)> {
        let obj = body.as_object()?;
        let chat_id = Self::telegram_id_value_to_string(obj.get("chat_id")?)?;
        let thread_id = Self::telegram_id_value_to_string(obj.get("message_thread_id")?)?;
        Some((chat_id, thread_id))
    }

    fn telegram_id_value_to_string(value: &serde_json::Value) -> Option<String> {
        value
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| value.as_i64().map(|id| id.to_string()))
    }

    fn prune_stale_dm_topic_binding_target(&self, target: Option<(String, String)>) {
        let Some((chat_id, thread_id)) = target else {
            return;
        };
        if self.prune_stale_dm_topic_binding(&chat_id, &thread_id) {
            info!(
                chat_id = %chat_id,
                thread_id = %thread_id,
                "Pruned stale Telegram DM topic binding after Bot API reported the thread missing"
            );
        }
    }

    fn gateway_error_thread_or_reply_missing(err: &GatewayError) -> bool {
        match err {
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::ConnectionFailed(message) => Self::thread_or_reply_missing(message),
            _ => false,
        }
    }

    fn thread_or_reply_missing(message: &str) -> bool {
        let lower = message.to_ascii_lowercase();
        lower.contains("message thread not found")
            || lower.contains("thread not found")
            || lower.contains("message to be replied not found")
            || lower.contains("reply message not found")
    }

    // -----------------------------------------------------------------------
    // File operations
    // -----------------------------------------------------------------------

    /// Send a document file.
    pub async fn send_document(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendDocument",
            field_name: "document",
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    pub async fn send_document_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendDocument",
            field_name: "document",
            reply_to_message_id,
            message_thread_id,
        })
        .await
    }

    /// Send a photo file.
    pub async fn send_photo(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendPhoto",
            field_name: "photo",
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    pub async fn send_photo_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method: "sendPhoto",
            field_name: "photo",
            reply_to_message_id,
            message_thread_id,
        })
        .await
    }

    /// Send an audio file.
    pub async fn send_audio(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendAudio", "audio")
            .await
    }

    /// Send a video file.
    pub async fn send_video(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendVideo", "video")
            .await
    }

    /// Send a voice message (OGG Opus).
    pub async fn send_voice(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendVoice", "voice")
            .await
    }

    /// Send an animation (GIF / MPEG4).
    pub async fn send_animation(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendAnimation", "animation")
            .await
    }

    /// Send a sticker by file path.
    pub async fn send_sticker(&self, chat_id: &str, file_path: &str) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, None, "sendSticker", "sticker")
            .await
    }

    /// Send a sticker by its `file_id` (already on Telegram servers).
    pub async fn send_sticker_by_id(
        &self,
        chat_id: &str,
        sticker_file_id: &str,
    ) -> Result<i64, GatewayError> {
        let body = serde_json::json!({
            "chat_id": chat_id,
            "sticker": sticker_file_id,
        });

        let url = format!("{}/sendSticker", self.api_base);
        let resp: TelegramResponse<SentMessage> = self.post_json(&url, &body).await?;

        resp.result.map(|m| m.message_id).ok_or_else(|| {
            GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "sendSticker failed".into()),
            )
        })
    }

    /// Shared multipart upload for all media-sending endpoints.
    async fn send_multipart(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        method: &str,
        field_name: &str,
    ) -> Result<i64, GatewayError> {
        self.send_multipart_with_options(TelegramMultipartRequest {
            chat_id,
            file_path,
            caption,
            method,
            field_name,
            reply_to_message_id: None,
            message_thread_id: None,
        })
        .await
    }

    async fn send_multipart_with_options(
        &self,
        mut request: TelegramMultipartRequest<'_>,
    ) -> Result<i64, GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(request.chat_id);
        request.chat_id = chat_id;
        request.message_thread_id = request.message_thread_id.or(inferred_thread_id);
        match self.send_multipart_once(request).await {
            Ok(id) => Ok(id),
            Err(err)
                if Self::gateway_error_thread_or_reply_missing(&err)
                    && request.has_thread_context() =>
            {
                if let Some(thread_id) = request.message_thread_id {
                    self.prune_stale_dm_topic_binding_target(Some((
                        request.chat_id.to_string(),
                        thread_id.to_string(),
                    )));
                }
                self.send_multipart_once(request.without_thread_context())
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn send_multipart_once(
        &self,
        request: TelegramMultipartRequest<'_>,
    ) -> Result<i64, GatewayError> {
        let url = format!("{}/{}", self.api_base, request.method);

        let file_bytes = tokio::fs::read(request.file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", request.file_path, e))
        })?;

        let file_name = std::path::Path::new(request.file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", request.chat_id.to_string())
            .part(request.field_name.to_string(), part);

        if let Some(cap) = request.caption.map(str::trim).filter(|s| !s.is_empty()) {
            let truncated: String = cap.chars().take(MAX_CAPTION_LENGTH).collect();
            form = form.text("caption", truncated);
        }

        if let Some(reply_id) = request.reply_to_message_id {
            form = form.text("reply_to_message_id", reply_id.to_string());
        }

        if let Some(thread_id) = request.message_thread_id {
            form = form.text("message_thread_id", thread_id.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("{} failed: {}", request.method, e)))?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::RateLimited {
                retry_after_secs: Self::retry_after_from_telegram_body(&body_text),
            });
        }

        let result: TelegramResponse<SentMessage> = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!(
                "Failed to parse {} response: {}",
                request.method, e
            ))
        })?;

        if result.ok {
            result.result.map(|m| m.message_id).ok_or_else(|| {
                GatewayError::SendFailed(
                    result
                        .description
                        .unwrap_or_else(|| format!("{} returned no message", request.method)),
                )
            })
        } else {
            Err(Self::telegram_response_error(request.method, &result))
        }
    }

    // -----------------------------------------------------------------------
    // Receiving messages (long polling)
    // -----------------------------------------------------------------------

    /// Fetch updates from Telegram using long polling.
    pub async fn get_updates(&self) -> Result<Vec<Update>, GatewayError> {
        let offset = self.poll_offset.load(Ordering::SeqCst);
        let url = format!("{}/getUpdates", self.api_base);

        let body = serde_json::json!({
            "offset": offset,
            "timeout": self.config.poll_timeout,
            "allowed_updates": ["message", "callback_query"],
        });

        let resp: TelegramResponse<Vec<Update>> = self
            .post_json_with_request_timeout(&url, &body, Some(self.poll_request_timeout()))
            .await?;

        if let Some(updates) = resp.result {
            if let Some(last) = updates.last() {
                self.poll_offset.store(last.update_id + 1, Ordering::SeqCst);
            }
            Ok(updates)
        } else {
            Ok(Vec::new())
        }
    }

    pub async fn delete_webhook(&self, drop_pending_updates: bool) -> Result<(), GatewayError> {
        let url = format!("{}/deleteWebhook", self.api_base);
        let body = serde_json::json!({ "drop_pending_updates": drop_pending_updates });
        let resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        if resp.ok {
            Ok(())
        } else {
            Err(GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "deleteWebhook failed".to_string()),
            ))
        }
    }

    /// Fetch updates with exponential backoff on failures.
    ///
    /// On success the backoff resets to zero. On failure the delay doubles
    /// each time (1 s → 2 s → 4 s … capped at 60 s). The caller can inspect
    /// `PollResult::Backoff` and decide whether to sleep or abort.
    pub async fn poll_with_backoff(&self) -> PollResult {
        match self.get_updates().await {
            Ok(updates) => {
                self.backoff_ms.store(0, Ordering::SeqCst);
                self.consecutive_errors.store(0, Ordering::SeqCst);
                PollResult::Updates(updates)
            }
            Err(e) => {
                let prev = self.backoff_ms.load(Ordering::SeqCst);
                let next = if prev == 0 {
                    INITIAL_BACKOFF_MS
                } else {
                    (prev * 2).min(MAX_BACKOFF_MS)
                };
                self.backoff_ms.store(next, Ordering::SeqCst);

                let err_count = self.consecutive_errors.fetch_add(1, Ordering::SeqCst) + 1;
                let conflict = Self::is_polling_conflict_error(&e);
                warn!(
                    consecutive_errors = err_count,
                    backoff_ms = next,
                    polling_conflict = conflict,
                    "Telegram poll failed: {}",
                    e
                );

                PollResult::Backoff {
                    error: e,
                    delay_ms: next,
                }
            }
        }
    }

    /// Convenience: sleep for the backoff delay. Should be called after
    /// receiving `PollResult::Backoff`.
    pub async fn sleep_backoff(&self) {
        let ms = self.backoff_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
    }

    /// Return the current consecutive error count.
    pub fn consecutive_error_count(&self) -> u64 {
        self.consecutive_errors.load(Ordering::SeqCst)
    }

    pub fn polling_reconnect_threshold_reached(&self, threshold: u64) -> bool {
        threshold > 0 && self.consecutive_error_count() >= threshold
    }

    pub fn mark_polling_unhealthy(&self) {
        self.base.mark_stopped();
    }

    pub fn poll_request_timeout(&self) -> Duration {
        Duration::from_secs(
            self.config
                .poll_timeout
                .saturating_add(poll_stall_grace_seconds())
                .max(1),
        )
    }

    pub fn is_polling_conflict_error(err: &GatewayError) -> bool {
        let message = err.to_string().to_ascii_lowercase();
        message.contains("409")
            || message.contains("conflict")
            || message.contains("terminated by other getupdates request")
    }

    // -----------------------------------------------------------------------
    // Update parsing
    // -----------------------------------------------------------------------

    /// Parse a Telegram Update into an IncomingMessage.
    ///
    /// Handles both regular messages and callback queries.
    pub fn parse_update(update: &Update) -> Option<IncomingMessage> {
        if let Some(ref cq) = update.callback_query {
            return Self::parse_callback_query(cq);
        }

        let msg = update.message.as_ref()?;
        Self::parse_telegram_message(msg, None)
    }

    /// Parse a regular `TelegramMessage` into `IncomingMessage`.
    fn parse_telegram_message(
        msg: &TelegramMessage,
        callback: Option<(&str, &str)>,
    ) -> Option<IncomingMessage> {
        let text = msg
            .text
            .clone()
            .or_else(|| msg.caption.clone())
            .or_else(|| Self::extract_rich_reply_text(msg));
        let reply = msg.reply_to_message.as_deref();
        let own_media = msg.voice.is_some()
            || msg.photo.is_some()
            || msg.sticker.is_some()
            || msg.document.is_some();
        let replied_media = if own_media { None } else { reply };
        let user_id = msg.from.as_ref().map(|u| u.id);
        let username = msg.from.as_ref().and_then(|u| u.username.clone());

        let voice = msg
            .voice
            .as_ref()
            .or_else(|| replied_media.and_then(|r| r.voice.as_ref()));
        let is_voice = voice.is_some();
        let voice_file_id = voice.map(|v| v.file_id.clone());

        let photo = msg
            .photo
            .as_ref()
            .or_else(|| replied_media.and_then(|r| r.photo.as_ref()));
        let is_photo = photo.is_some();
        let photo_file_id = photo.and_then(|photos| photos.last().map(|p| p.file_id.clone()));

        let is_sticker = msg.sticker.is_some();
        let sticker_file_id = msg.sticker.as_ref().map(|s| s.file_id.clone());

        let replied_document = replied_media
            .and_then(|r| r.document.as_ref())
            .filter(|doc| !Self::document_exceeds_size_limit(doc));
        let document = msg.document.as_ref().or(replied_document);
        let is_document = document.is_some();
        let document_file_id = document.map(|d| d.file_id.clone());
        let document_file_name = document.and_then(|d| d.file_name.clone());
        let document_mime_type = document.and_then(|d| d.mime_type.clone());
        let document_file_size = document.and_then(|d| d.file_size);

        let reply_to_message_id = msg.reply_to_message.as_ref().map(|r| r.message_id);

        let chat_type = ChatKind::from_telegram_type(&msg.chat.chat_type);
        let is_group = chat_type.is_group_like();

        let (cb_id, cb_data) = match callback {
            Some((id, data)) => (Some(id.to_string()), Some(data.to_string())),
            None => (None, None),
        };

        Some(IncomingMessage {
            chat_id: msg.chat.id,
            user_id,
            username,
            text,
            message_id: msg.message_id,
            is_voice,
            is_photo,
            is_sticker,
            is_document,
            voice_file_id,
            photo_file_id,
            sticker_file_id,
            document_file_id,
            document_file_name,
            document_mime_type,
            document_file_size,
            reply_to_message_id,
            message_thread_id: msg.message_thread_id,
            chat_type,
            is_group,
            callback_query_id: cb_id,
            callback_data: cb_data,
        })
    }

    /// Parse a `CallbackQuery` into an `IncomingMessage`.
    fn parse_callback_query(cq: &CallbackQuery) -> Option<IncomingMessage> {
        let msg = cq.message.as_ref();
        let chat_id = msg.map(|m| m.chat.id).unwrap_or(0);
        let message_id = msg.map(|m| m.message_id).unwrap_or(0);

        let chat_type = msg
            .map(|m| ChatKind::from_telegram_type(&m.chat.chat_type))
            .unwrap_or(ChatKind::Private);
        let is_group = chat_type.is_group_like();

        Some(IncomingMessage {
            chat_id,
            user_id: Some(cq.from.id),
            username: cq.from.username.clone(),
            text: cq.data.clone(),
            message_id,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            reply_to_message_id: None,
            message_thread_id: msg.and_then(|m| m.message_thread_id),
            chat_type,
            is_group,
            callback_query_id: Some(cq.id.clone()),
            callback_data: cq.data.clone(),
        })
    }

    // -----------------------------------------------------------------------
    // File downloads
    // -----------------------------------------------------------------------

    /// Download a file from Telegram by file_id.
    /// Returns the URL from which the file can be downloaded.
    pub async fn get_file_url(&self, file_id: &str) -> Result<String, GatewayError> {
        let url = format!("{}/getFile", self.api_base);
        let body = serde_json::json!({ "file_id": file_id });

        let resp: TelegramResponse<TelegramFile> = self.post_json(&url, &body).await?;

        let file = resp.result.ok_or_else(|| {
            GatewayError::ConnectionFailed(
                resp.description.unwrap_or_else(|| "getFile failed".into()),
            )
        })?;

        let file_path = file
            .file_path
            .ok_or_else(|| GatewayError::ConnectionFailed("File path not available".into()))?;

        Ok(format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.config.token, file_path
        ))
    }

    // -----------------------------------------------------------------------
    // Group chat helpers
    // -----------------------------------------------------------------------

    /// Get information about a chat member (useful for admin checks).
    pub async fn get_chat_member(
        &self,
        chat_id: &str,
        user_id: i64,
    ) -> Result<ChatMember, GatewayError> {
        let url = format!("{}/getChatMember", self.api_base);
        let body = serde_json::json!({
            "chat_id": chat_id,
            "user_id": user_id,
        });

        let resp: TelegramResponse<ChatMember> = self.post_json(&url, &body).await?;

        resp.result.ok_or_else(|| {
            GatewayError::Platform(
                resp.description
                    .unwrap_or_else(|| "getChatMember failed".into()),
            )
        })
    }

    /// Check if a user is an admin or creator in a chat.
    pub async fn is_admin(&self, chat_id: &str, user_id: i64) -> Result<bool, GatewayError> {
        let member = self.get_chat_member(chat_id, user_id).await?;
        Ok(matches!(
            member.status.as_str(),
            "administrator" | "creator"
        ))
    }

    /// Check whether a text message mentions this bot (for group filtering).
    ///
    /// Returns `true` if the message contains `@bot_username` or if the
    /// bot_username is not configured (pass-through).
    pub fn is_mentioned_in(&self, text: &str) -> bool {
        match self.config.bot_username {
            Some(ref bot_user) => {
                let mention = format!("@{}", bot_user);
                text.contains(&mention)
            }
            None => true,
        }
    }

    /// Strip the bot mention from text, returning the cleaned message.
    pub fn strip_mention(&self, text: &str) -> String {
        match self.config.bot_username {
            Some(ref bot_user) => {
                let mention = format!("@{}", bot_user);
                text.replace(&mention, "").trim().to_string()
            }
            None => text.to_string(),
        }
    }

    /// Return true if this Telegram document can be processed by parity flows.
    pub fn is_supported_document(doc: &Document) -> bool {
        let ext = doc
            .file_name
            .as_deref()
            .and_then(Self::extract_extension)
            .or_else(|| doc.mime_type.as_deref().and_then(Self::extension_from_mime));
        ext.map(|e| SUPPORTED_DOCUMENT_EXTENSIONS.contains(&e.as_str()))
            .unwrap_or(false)
    }

    /// Return true if this Telegram document exceeds processing size limits.
    pub fn document_exceeds_size_limit(doc: &Document) -> bool {
        doc.file_size
            .map(|sz| sz > TELEGRAM_MAX_DOCUMENT_SIZE_BYTES)
            .unwrap_or(true)
    }

    fn extract_extension(name: &str) -> Option<String> {
        std::path::Path::new(name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .filter(|e| !e.is_empty())
    }

    fn extension_from_mime(mime: &str) -> Option<String> {
        match mime {
            "application/pdf" => Some("pdf".to_string()),
            "text/markdown" => Some("md".to_string()),
            "text/plain" => Some("txt".to_string()),
            "application/zip" => Some("zip".to_string()),
            "image/png" => Some("png".to_string()),
            "image/jpeg" => Some("jpg".to_string()),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
                Some("docx".to_string())
            }
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => {
                Some("xlsx".to_string())
            }
            "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
                Some("pptx".to_string())
            }
            _ => None,
        }
    }

    pub async fn send_image_url_with_options(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<(), GatewayError> {
        let (chat_id, inferred_thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_thread_id = message_thread_id.or(inferred_thread_id);
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "photo": image_url,
        });
        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            let truncated: String = cap.chars().take(MAX_CAPTION_LENGTH).collect();
            body["caption"] = serde_json::Value::String(truncated);
        }
        if let Some(reply_id) = reply_to_message_id {
            body["reply_to_message_id"] = serde_json::Value::Number(reply_id.into());
        }
        if let Some(thread_id) = message_thread_id {
            body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
        }
        let _resp: TelegramResponse<SentMessage> = self
            .send_json_with_thread_fallback("sendPhoto", body)
            .await?;
        Ok(())
    }

    async fn set_message_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        reaction: Option<&str>,
    ) -> Result<(), GatewayError> {
        if !self.config.reactions {
            return Ok(());
        }

        let message_id = message_id.parse::<i64>().map_err(|_| {
            GatewayError::SendFailed(format!(
                "Invalid Telegram message_id for reaction: {message_id}"
            ))
        })?;
        let reaction_value = match reaction {
            Some(emoji) => serde_json::json!([{ "type": "emoji", "emoji": emoji }]),
            None => serde_json::json!([]),
        };
        let body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "reaction": reaction_value,
        });
        let url = format!("{}/setMessageReaction", self.api_base);
        let _resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        Ok(())
    }

    pub async fn send_approval_request(
        &self,
        chat_id: &str,
        request: &GatewayApprovalRequest,
        reply_to_message_id: Option<i64>,
        message_thread_id: Option<i64>,
    ) -> Result<Vec<i64>, GatewayError> {
        let approval_id = self.approval_counter.fetch_add(1, Ordering::SeqCst);
        self.approval_state
            .lock()
            .map_err(|_| GatewayError::Platform("telegram approval state poisoned".to_string()))?
            .insert(approval_id, request.session_key.clone());

        let command = truncate_chars(&request.command, 1800);
        let description = truncate_chars(&request.description, 1200);
        let text = format!(
            "*Command approval required*\n\nCommand:\n`{command}`\n\nReason: {description}"
        );
        let keyboard = InlineKeyboardMarkup {
            inline_keyboard: vec![
                vec![
                    InlineKeyboardButton {
                        text: "Approve once".to_string(),
                        callback_data: Some(format!("approval:once:{approval_id}")),
                        url: None,
                    },
                    InlineKeyboardButton {
                        text: "Approve session".to_string(),
                        callback_data: Some(format!("approval:session:{approval_id}")),
                        url: None,
                    },
                ],
                vec![InlineKeyboardButton {
                    text: "Deny".to_string(),
                    callback_data: Some(format!("approval:deny:{approval_id}")),
                    url: None,
                }],
            ],
        };
        self.send_text_with_keyboard(
            chat_id,
            &text,
            keyboard,
            Some("MarkdownV2"),
            reply_to_message_id,
            message_thread_id,
        )
        .await
    }

    pub async fn handle_approval_callback(
        &self,
        callback_query_id: &str,
        callback_data: &str,
    ) -> Result<bool, GatewayError> {
        let Some((choice, approval_id)) = parse_approval_callback(callback_data) else {
            return Ok(false);
        };
        let session_key = self
            .approval_state
            .lock()
            .map_err(|_| GatewayError::Platform("telegram approval state poisoned".to_string()))?
            .remove(&approval_id);
        let Some(session_key) = session_key else {
            self.answer_callback_query(callback_query_id, Some("Approval already resolved"), true)
                .await?;
            return Ok(true);
        };
        let resolved = approval::resolve_gateway_approval(
            &session_key,
            choice,
            matches!(choice, ApprovalChoice::Session),
        );
        let answer = if resolved == 0 {
            "No pending approval for this session"
        } else if choice == ApprovalChoice::Deny {
            "Denied"
        } else {
            "Approved"
        };
        self.answer_callback_query(callback_query_id, Some(answer), false)
            .await?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// POST JSON to a Telegram API endpoint and deserialize the response.
    ///
    /// Detects HTTP 429 (rate limited) responses, extracts `retry_after`
    /// from the response body, sleeps, then retries up to
    /// `RATE_LIMIT_MAX_RETRIES` times.
    fn telegram_response_error<T>(method: &str, response: &TelegramResponse<T>) -> GatewayError {
        if let Some(retry_after_secs) = response
            .parameters
            .as_ref()
            .and_then(|parameters| parameters.retry_after)
        {
            return GatewayError::RateLimited {
                retry_after_secs: Some(retry_after_secs),
            };
        }

        GatewayError::SendFailed(
            response
                .description
                .clone()
                .unwrap_or_else(|| format!("{method} failed")),
        )
    }

    fn retry_after_from_telegram_body(text: &str) -> Option<u64> {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get("parameters")?.get("retry_after")?.as_u64())
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        self.post_json_with_request_timeout(url, body, None).await
    }

    async fn post_json_with_request_timeout<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
        request_timeout: Option<Duration>,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let mut retries = 0u32;

        loop {
            let mut request = self.client.post(url).json(body);
            if let Some(timeout) = request_timeout {
                request = request.timeout(timeout);
            }
            let resp = request.send().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Telegram API request failed: {}", e))
            })?;

            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let text = resp.text().await.unwrap_or_default();

                let retry_after = Self::retry_after_from_telegram_body(&text).unwrap_or(5);

                retries += 1;
                if retries > RATE_LIMIT_MAX_RETRIES {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: Some(retry_after),
                    });
                }

                warn!(
                    retry_after_secs = retry_after,
                    attempt = retries,
                    "Telegram API rate limited, backing off"
                );
                tokio::time::sleep(tokio::time::Duration::from_secs(retry_after)).await;
                continue;
            }

            if !status.is_success() {
                let text = resp.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    return Err(GatewayError::RateLimited {
                        retry_after_secs: Self::retry_after_from_telegram_body(&text),
                    });
                }
                return Err(GatewayError::SendFailed(format!(
                    "Telegram API returned HTTP {}: {}",
                    status, text
                )));
            }

            return resp.json::<TelegramResponse<T>>().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to parse Telegram API response: {}",
                    e
                ))
            });
        }
    }

    /// Resolve a `ParseMode` to the Telegram API string.
    fn resolve_parse_mode(&self, parse_mode: Option<ParseMode>) -> Option<&'static str> {
        match parse_mode {
            Some(ParseMode::Markdown) => Some("MarkdownV2"),
            Some(ParseMode::Html) => Some("HTML"),
            Some(ParseMode::Plain) | None => {
                if self.config.parse_markdown {
                    Some("MarkdownV2")
                } else if self.config.parse_html {
                    Some("HTML")
                } else {
                    None
                }
            }
        }
    }

    /// Determine the appropriate send method for a file based on extension.
    fn media_method_for_extension(ext: &str) -> (&'static str, &'static str) {
        match ext {
            "jpg" | "jpeg" | "png" | "webp" => ("sendPhoto", "photo"),
            "gif" => ("sendAnimation", "animation"),
            "mp4" | "mov" | "avi" | "mkv" | "webm" => ("sendVideo", "video"),
            "mp3" | "aac" | "m4a" => ("sendAudio", "audio"),
            "ogg" | "oga" => ("sendVoice", "voice"),
            "webm_sticker" | "tgs" => ("sendSticker", "sticker"),
            _ => ("sendDocument", "document"),
        }
    }
}

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Telegram adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        match self.register_command_menu().await {
            Ok(count) if count > 0 => {
                info!(count, "Telegram command menu registered");
            }
            Ok(_) => {}
            Err(err) => {
                warn!(error = %err, "Telegram command menu registration failed");
            }
        }
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Telegram adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let pm = self.resolve_parse_mode(parse_mode);
        self.send_text(chat_id, text, pm, None).await?;
        Ok(())
    }

    async fn send_or_update_status(
        &self,
        chat_id: &str,
        status_key: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let key = (chat_id.to_string(), status_key.to_string());
        let existing_id = self
            .status_message_ids
            .lock()
            .ok()
            .and_then(|ids| ids.get(&key).cloned());
        let pm = self.resolve_parse_mode(parse_mode);

        if let Some(message_id) = existing_id {
            match self.edit_text(chat_id, &message_id, text, pm).await {
                Ok(()) => return Ok(()),
                Err(err) => {
                    warn!(
                        chat_id,
                        status_key,
                        message_id,
                        error = %err,
                        "Telegram status edit failed; sending replacement status message"
                    );
                }
            }
        }

        let sent_ids = self.send_text(chat_id, text, pm, None).await?;
        if let Some(message_id) = sent_ids.first() {
            if let Ok(mut ids) = self.status_message_ids.lock() {
                ids.insert(key, message_id.to_string());
            }
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let pm = self.resolve_parse_mode(None);
        self.edit_text(chat_id, message_id, text, pm).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let (method, field) = Self::media_method_for_extension(&ext);
        self.send_multipart(chat_id, file_path, caption, method, field)
            .await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_image_url_with_options(chat_id, image_url, caption, None, None)
            .await
    }

    async fn delete_message(&self, chat_id: &str, message_id: &str) -> Result<bool, GatewayError> {
        let (base_chat_id, thread_id) = Self::split_gateway_chat_thread(chat_id);
        let message_id = if thread_id.is_some() {
            message_id
                .split_once(':')
                .map(|(_, id)| id)
                .unwrap_or(message_id)
        } else {
            message_id
        };
        let url = format!("{}/deleteMessage", self.api_base);
        let message_id = message_id.parse::<i64>().map_err(|err| {
            GatewayError::SendFailed(format!("invalid Telegram message_id '{message_id}': {err}"))
        })?;
        let body = serde_json::json!({
            "chat_id": base_chat_id,
            "message_id": message_id,
        });
        let resp: TelegramResponse<bool> = self.post_json(&url, &body).await?;
        if resp.ok {
            Ok(resp.result.unwrap_or(true))
        } else {
            Err(GatewayError::SendFailed(
                resp.description
                    .unwrap_or_else(|| "deleteMessage failed".to_string()),
            ))
        }
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        self.set_message_reaction(chat_id, message_id, Some(emoji))
            .await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        _emoji: &str,
    ) -> Result<(), GatewayError> {
        self.set_message_reaction(chat_id, message_id, None).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn splits_long_messages(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "telegram"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Split a message into chunks that fit within the given max length,
/// preferring to break at newline boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = (start + max_len).min(text.len());

        if end >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }

        // Try to break at a newline near the boundary.
        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

fn parse_approval_callback(data: &str) -> Option<(ApprovalChoice, u64)> {
    let mut parts = data.split(':');
    if parts.next()? != "approval" {
        return None;
    }
    let choice = match parts.next()? {
        "once" => ApprovalChoice::Once,
        "session" => ApprovalChoice::Session,
        "deny" => ApprovalChoice::Deny,
        _ => return None,
    };
    let approval_id = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((choice, approval_id))
}
