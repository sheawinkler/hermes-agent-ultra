// ---------------------------------------------------------------------------
// SlackAdapter
// ---------------------------------------------------------------------------

/// Slack Bot API platform adapter.
pub struct SlackAdapter {
    base: BasePlatformAdapter,
    config: SlackConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    group_dm_scope_warned: Mutex<BTreeSet<String>>,
}

impl SlackAdapter {
    /// Create a new Slack adapter with the given configuration.
    pub fn new(config: SlackConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());

        base.validate_token()?;
        let client = base.build_client()?;

        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
            group_dm_scope_warned: Mutex::new(BTreeSet::new()),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &SlackConfig {
        &self.config
    }

    fn reactions_enabled(&self) -> bool {
        reactions_toggle_enabled(
            std::env::var("SLACK_REACTIONS").ok().as_deref(),
            self.config.reactions,
        )
    }

    // -----------------------------------------------------------------------
    // Web API: Sending messages
    // -----------------------------------------------------------------------

    /// Post a message to a Slack channel using `chat.postMessage`.
    /// Supports thread replies via `thread_ts` and Block Kit formatting.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, GatewayError> {
        let chunks = split_message(text, MAX_MESSAGE_LENGTH);
        let mut last_ts = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({
                "channel": channel,
                "text": chunk,
            });

            // Thread the first chunk to the specified thread, subsequent
            // chunks reply to the first chunk's ts.
            if i == 0 {
                if let Some(ts) = thread_ts {
                    body["thread_ts"] = serde_json::Value::String(ts.to_string());
                }
            } else if !last_ts.is_empty() {
                body["thread_ts"] = serde_json::Value::String(last_ts.clone());
            }

            let resp = self.slack_post("chat.postMessage", &body).await?;
            if let Some(ts) = resp.ts {
                last_ts = ts;
            }
        }

        Ok(last_ts)
    }

    /// Post a message with Block Kit blocks.
    pub async fn post_blocks(
        &self,
        channel: &str,
        blocks: &serde_json::Value,
        fallback_text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, GatewayError> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": fallback_text,
            "blocks": blocks,
        });

        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp = self.slack_post("chat.postMessage", &body).await?;
        resp.ts
            .ok_or_else(|| GatewayError::SendFailed("No ts in response".into()))
    }

    /// Post a `BlockKitMessage` (type-safe builder variant).
    pub async fn post_block_kit(
        &self,
        channel: &str,
        message: &BlockKitMessage,
        fallback_text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, GatewayError> {
        self.post_blocks(channel, &message.to_json(), fallback_text, thread_ts)
            .await
    }

    /// Update an existing message using `chat.update`.
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": &text[..text.len().min(MAX_MESSAGE_LENGTH)],
        });

        self.slack_post("chat.update", &body).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Web API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Slack channel using `files.uploadV2` flow.
    pub async fn upload_file(
        &self,
        channel: &str,
        file_path: &str,
        title: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<(), GatewayError> {
        let file_bytes = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e))
        })?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name.clone());

        let mut form = reqwest::multipart::Form::new()
            .text("channels", channel.to_string())
            .text("filename", file_name.clone())
            .part("file", part);

        if let Some(t) = title {
            form = form.text("title", t.to_string());
        }
        if let Some(ts) = thread_ts {
            form = form.text("thread_ts", ts.to_string());
        }

        let url = format!("{}/files.upload", SLACK_API_BASE);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Slack file upload failed: {}", e)))?;

        let result: SlackResponse = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Slack response: {}", e))
        })?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack files.upload error: {}",
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Socket Mode: Receiving events
    // -----------------------------------------------------------------------

    /// Get a WebSocket URL for Socket Mode connection.
    pub async fn get_socket_mode_url(&self) -> Result<String, GatewayError> {
        let app_token = self.config.app_token.as_ref().ok_or_else(|| {
            GatewayError::Auth("Socket Mode requires an app-level token (xapp-...)".into())
        })?;

        let resp = self
            .client
            .post(&format!("{}/apps.connections.open", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", app_token))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to open Socket Mode connection: {}",
                    e
                ))
            })?;

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to parse Socket Mode response: {}", e))
        })?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(GatewayError::ConnectionFailed(format!(
                "Socket Mode connection failed: {}",
                err
            )));
        }

        body.get("url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| GatewayError::ConnectionFailed("No URL in Socket Mode response".into()))
    }

    pub(crate) async fn warn_if_missing_group_dm_scopes_from_auth_test(
        &self,
        base_url: &str,
    ) -> Result<bool, GatewayError> {
        let resp = self
            .client
            .post(&format!("{}/auth.test", base_url.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Slack auth.test failed: {e}")))?;
        let scopes = resp
            .headers()
            .get("x-oauth-scopes")
            .or_else(|| resp.headers().get("X-OAuth-Scopes"))
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body: SlackAuthTestResponse = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to parse Slack auth.test response: {e}"))
        })?;
        if !body.ok {
            return Err(GatewayError::Auth(format!(
                "Slack auth.test error: {}",
                body.error.unwrap_or_else(|| "unknown".into())
            )));
        }
        Ok(self.warn_if_missing_group_dm_scopes(
            &scopes,
            body.team.as_deref().or(body.team_id.as_deref()),
        ))
    }

    pub(crate) fn warn_if_missing_group_dm_scopes(
        &self,
        scopes_header: &str,
        team_name: Option<&str>,
    ) -> bool {
        let granted = parse_slack_scope_header(scopes_header);
        if !granted.contains("im:history") || granted.contains("mpim:history") {
            return false;
        }
        let team_key = team_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("this workspace")
            .to_string();
        let Ok(mut warned) = self.group_dm_scope_warned.lock() else {
            return false;
        };
        if !warned.insert(team_key.clone()) {
            return false;
        }
        warn!(
            workspace = %team_key,
            "Slack Group DMs will not work: the app is missing `mpim:history` scope and `message.mpim` event subscription. Add `mpim:history` and `mpim:read` to bot scopes, add `message.mpim` to event subscriptions, then reinstall the app to the workspace."
        );
        true
    }

    /// Parse a Socket Mode envelope into an IncomingSlackMessage.
    pub fn parse_event(envelope: &SocketModeEnvelope) -> Option<IncomingSlackMessage> {
        Self::parse_event_unfiltered(envelope)
    }

    /// Parse a Socket Mode envelope and apply Slack mention/wake-word policy.
    pub fn parse_event_with_config(
        envelope: &SocketModeEnvelope,
        config: &SlackConfig,
    ) -> Option<IncomingSlackMessage> {
        Self::parse_event_with_mention_policy(envelope, &SlackMentionPolicy::from_config(config))
    }

    pub fn parse_event_with_mention_policy(
        envelope: &SocketModeEnvelope,
        policy: &SlackMentionPolicy,
    ) -> Option<IncomingSlackMessage> {
        let msg = Self::parse_event_unfiltered(envelope)?;
        if slack_event_is_dm(envelope, &msg.channel) || !policy.require_mention {
            return Some(msg);
        }

        let env_bot_user_id = std::env::var("SLACK_BOT_USER_ID")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let bot_user_id = policy
            .bot_user_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or(env_bot_user_id.as_deref());

        if slack_message_is_addressed(&msg.text, bot_user_id, &policy.mention_patterns) {
            return Some(msg);
        }

        None
    }

    fn parse_event_unfiltered(envelope: &SocketModeEnvelope) -> Option<IncomingSlackMessage> {
        let payload = envelope.payload.as_ref()?;
        let event = payload.get("event")?;

        let event_type = event.get("type")?.as_str()?;
        if event_type != "message" {
            return None;
        }

        // Skip bot messages
        if event.get("bot_id").is_some() {
            return None;
        }

        let channel = event.get("channel")?.as_str()?.to_string();
        let text = event
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let user_id = event.get("user").and_then(|v| v.as_str()).map(String::from);
        let ts = event.get("ts")?.as_str()?.to_string();
        let thread_ts = event
            .get("thread_ts")
            .and_then(|v| v.as_str())
            .map(String::from);
        let media_files = parse_slack_media_files(event);

        Some(IncomingSlackMessage {
            channel,
            user_id,
            text,
            ts,
            thread_ts,
            is_bot: false,
            media_files,
        })
    }

    // -----------------------------------------------------------------------
    // Web API: App Home tab
    // -----------------------------------------------------------------------

    /// Publish a Home tab view for a specific user using `views.publish`.
    pub async fn publish_home_tab(
        &self,
        user_id: &str,
        view: &HomeView,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "user_id": user_id,
            "view": view.to_json(),
        });
        self.slack_post("views.publish", &body).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Web API: Modals
    // -----------------------------------------------------------------------

    /// Open a modal view using `views.open`. Requires a `trigger_id` obtained
    /// from an interactive event or slash command.
    pub async fn open_modal(&self, trigger_id: &str, view: &ModalView) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "trigger_id": trigger_id,
            "view": view.to_json(),
        });
        self.slack_post("views.open", &body).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Web API: Users
    // -----------------------------------------------------------------------

    /// Fetch user profile information using `users.info`.
    pub async fn get_user_info(&self, user_id: &str) -> Result<SlackUser, GatewayError> {
        let url = format!("{}/users.info?user={}", SLACK_API_BASE, user_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Slack users.info failed: {}", e)))?;

        let result: UserInfoResponse = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse users.info response: {}", e))
        })?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack users.info error: {}",
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        result.user.ok_or_else(|| {
            GatewayError::SendFailed("users.info returned ok but no user object".into())
        })
    }

    // -----------------------------------------------------------------------
    // Web API: Reactions
    // -----------------------------------------------------------------------

    /// Add an emoji reaction to a message using `reactions.add`.
    pub async fn add_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": name,
        });
        self.slack_post("reactions.add", &body).await?;
        Ok(())
    }

    /// Remove the bot's own reaction from a message.
    pub async fn remove_reaction(
        &self,
        channel: &str,
        timestamp: &str,
        name: &str,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "channel": channel,
            "timestamp": timestamp,
            "name": name,
        });
        self.slack_post("reactions.remove", &body).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Web API: Conversations
    // -----------------------------------------------------------------------

    /// Set the topic for a channel using `conversations.setTopic`.
    pub async fn set_topic(&self, channel: &str, topic: &str) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "channel": channel,
            "topic": topic,
        });
        self.slack_post("conversations.setTopic", &body).await?;
        Ok(())
    }

    /// List channels visible to the bot user for channel-directory discovery.
    pub async fn list_user_conversations(&self) -> Result<Vec<ChannelEntry>, GatewayError> {
        self.list_user_conversations_from_base(SLACK_API_BASE).await
    }

    async fn list_user_conversations_from_base(
        &self,
        base_url: &str,
    ) -> Result<Vec<ChannelEntry>, GatewayError> {
        let endpoint = format!("{}/users.conversations", base_url.trim_end_matches('/'));
        let mut cursor: Option<String> = None;
        let mut entries = Vec::new();

        loop {
            let mut query = vec![
                ("types", "public_channel,private_channel".to_string()),
                ("limit", "200".to_string()),
            ];
            if let Some(cursor) = cursor.as_deref().filter(|cursor| !cursor.is_empty()) {
                query.push(("cursor", cursor.to_string()));
            }

            let resp = self
                .client
                .get(&endpoint)
                .header("Authorization", format!("Bearer {}", self.config.token))
                .query(&query)
                .send()
                .await
                .map_err(|e| {
                    GatewayError::ConnectionFailed(format!(
                        "Slack users.conversations failed: {}",
                        e
                    ))
                })?;

            let page: SlackConversationsResponse = resp.json().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Failed to parse Slack users.conversations response: {}",
                    e
                ))
            })?;

            if !page.ok {
                return Err(GatewayError::ConnectionFailed(format!(
                    "Slack users.conversations error: {}",
                    page.error.unwrap_or_else(|| "unknown".into())
                )));
            }

            for channel in page.channels {
                let Some(id) = channel.id.filter(|id| !id.is_empty()) else {
                    continue;
                };
                let Some(name) = channel.name.filter(|name| !name.is_empty()) else {
                    continue;
                };
                let kind = if channel.is_private {
                    "private"
                } else {
                    "channel"
                };
                entries.push(ChannelEntry::new("slack", id, name).with_kind(kind));
            }

            cursor = page
                .response_metadata
                .next_cursor
                .filter(|cursor| !cursor.is_empty());
            if cursor.is_none() {
                break;
            }
        }

        Ok(entries)
    }

    // -----------------------------------------------------------------------
    // Web API: Permalinks
    // -----------------------------------------------------------------------

    /// Get a permalink URL for a specific message using `chat.getPermalink`.
    pub async fn get_permalink(
        &self,
        channel: &str,
        message_ts: &str,
    ) -> Result<String, GatewayError> {
        let url = format!(
            "{}/chat.getPermalink?channel={}&message_ts={}",
            SLACK_API_BASE, channel, message_ts
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Slack chat.getPermalink failed: {}", e))
            })?;

        let result: PermalinkResponse = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse getPermalink response: {}", e))
        })?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack chat.getPermalink error: {}",
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        result.permalink.ok_or_else(|| {
            GatewayError::SendFailed("getPermalink returned ok but no permalink".into())
        })
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// POST to a Slack Web API method with JSON body.
    async fn slack_post(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<SlackResponse, GatewayError> {
        let url = format!("{}/{}", SLACK_API_BASE, method);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Slack {} failed: {}", method, e)))?;

        let result: SlackResponse = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Slack {} response: {}", method, e))
        })?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack {} error: {}",
                method,
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        Ok(result)
    }
}
