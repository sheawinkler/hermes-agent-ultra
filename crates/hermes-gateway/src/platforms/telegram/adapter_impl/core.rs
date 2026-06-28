impl TelegramAdapter {
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

}
