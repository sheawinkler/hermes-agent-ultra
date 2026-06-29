impl DiscordAdapter {
    /// Create a new Discord adapter with the given configuration.
    pub fn new(config: DiscordConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());

        base.validate_token()?;

        let client = base.build_client()?;

        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
            thread_participation: Mutex::new(DiscordThreadParticipationTracker::new("discord")),
            non_conversational_messages: Mutex::new(DiscordNonConversationalMessageTracker::new(
                "discord",
            )),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &DiscordConfig {
        &self.config
    }

    pub fn channel_controls(&self) -> &DiscordChannelControls {
        &self.config.channel_controls
    }

    pub fn should_ignore_channel(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.is_ignored(context)
    }

    pub fn should_auto_thread(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.should_auto_thread(context)
    }

    pub fn resolve_channel_skills(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
    ) -> Option<Vec<String>> {
        resolve_channel_skills_from_bindings(
            &self.config.channel_skill_bindings,
            channel_id,
            parent_id,
        )
    }

    pub fn thread_participation_contains(&self, thread_id: &str) -> bool {
        self.thread_participation
            .lock()
            .map(|tracker| tracker.contains(thread_id))
            .unwrap_or(false)
    }

    pub fn mark_thread_participation(&self, thread_id: &str) -> std::io::Result<bool> {
        self.thread_participation
            .lock()
            .map_err(|_| std::io::Error::other("discord thread tracker lock poisoned"))?
            .mark(thread_id)
    }

    pub fn non_conversational_message_contains(&self, message_id: &str) -> bool {
        self.non_conversational_messages
            .lock()
            .map(|tracker| tracker.contains(message_id))
            .unwrap_or(false)
    }

    fn mark_non_conversational_messages(
        &self,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) {
        match self.non_conversational_messages.lock() {
            Ok(mut tracker) => {
                if let Err(err) = tracker.mark_many(message_ids) {
                    debug!(
                        error = %err,
                        "failed to persist Discord non-conversational message IDs"
                    );
                }
            }
            Err(_) => debug!("discord non-conversational tracker lock poisoned"),
        }
    }

    /// Return the authorization header value.
    fn auth_header(&self) -> String {
        format!("Bot {}", self.config.token)
    }

    // -----------------------------------------------------------------------
    // REST API: Sending messages
    // -----------------------------------------------------------------------

    /// Send a message to a Discord channel, splitting if it exceeds 2000 chars.
    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.send_text_with_metadata(channel_id, content, None)
            .await
    }

    /// Send a message, honoring Discord thread routing metadata when present.
    pub async fn send_text_with_metadata(
        &self,
        channel_id: &str,
        content: &str,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<Vec<String>, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let formatted_content = format_discord_outgoing_content(content);
        let chunks = split_message(&formatted_content, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();
        let reply_to_message_id = reply_to_message_id_for_metadata(metadata);
        let reply_to_mode = DiscordReplyToMode::parse(Some(&self.config.reply_to_mode));
        let mut suppress_reply_references = false;

        for (index, chunk) in chunks.iter().enumerate() {
            let url = format!(
                "{}/channels/{}/messages",
                DISCORD_API_BASE, target_channel_id
            );
            let include_reply_reference = !suppress_reply_references
                && reply_to_message_id.is_some()
                && reply_to_mode.references_chunk(index);
            let body = discord_message_body(
                chunk,
                include_reply_reference.then_some(reply_to_message_id.unwrap_or_default()),
                default_discord_allowed_mentions(),
            );

            let resp = self
                .client
                .post(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("Discord send failed: {}", e)))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                if include_reply_reference && discord_reply_reference_error_allows_retry(&text) {
                    suppress_reply_references = true;
                    let retry_body =
                        discord_message_body(chunk, None, default_discord_allowed_mentions());
                    let retry_resp = self
                        .client
                        .post(&url)
                        .header("Authorization", self.auth_header())
                        .header("Content-Type", "application/json")
                        .json(&retry_body)
                        .send()
                        .await
                        .map_err(|e| {
                            GatewayError::SendFailed(format!("Discord send failed: {}", e))
                        })?;

                    if !retry_resp.status().is_success() {
                        let retry_text = retry_resp.text().await.unwrap_or_default();
                        return Err(GatewayError::SendFailed(format!(
                            "Discord API error: {}",
                            retry_text
                        )));
                    }

                    let msg: DiscordMessage = retry_resp.json().await.map_err(|e| {
                        GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
                    })?;

                    message_ids.push(msg.id);
                    continue;
                }

                return Err(GatewayError::SendFailed(format!(
                    "Discord API error: {}",
                    text
                )));
            }

            let msg: DiscordMessage = resp.json().await.map_err(|e| {
                GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
            })?;

            message_ids.push(msg.id);
        }

        if metadata_marks_non_conversational(metadata) {
            self.mark_non_conversational_messages(message_ids.iter().map(String::as_str));
        }

        Ok(message_ids)
    }

    /// Create a Discord forum post thread from message content.
    ///
    /// Follow-up chunks are sent to the created thread. If the starter post is
    /// created but a follow-up chunk fails, the successful starter message is
    /// returned together with warnings, matching the upstream partial-send
    /// behavior.
    pub async fn send_forum_post(
        &self,
        forum_channel_id: &str,
        content: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordForumSendOutcome, GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let Some(first_chunk) = chunks.first() else {
            return Err(GatewayError::SendFailed(
                "Discord forum post requires content".into(),
            ));
        };
        let url = format!("{}/channels/{}/threads", DISCORD_API_BASE, forum_channel_id);
        let body = forum_thread_payload(first_chunk, None, auto_archive_duration);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord forum post failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord forum post API error: {}",
                text
            )));
        }

        let thread: DiscordForumThread = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse forum thread response: {}", e))
        })?;
        let message_id = thread
            .message
            .as_ref()
            .map(|message| message.id.clone())
            .unwrap_or_else(|| thread.id.clone());
        let mut warnings = Vec::new();

        for chunk in chunks.iter().skip(1) {
            let metadata = DiscordSendMetadata::with_thread_id(thread.id.clone());
            if let Err(err) = self
                .send_text_with_metadata(forum_channel_id, chunk, Some(&metadata))
                .await
            {
                warnings.push(err.to_string());
            }
        }

        Ok(DiscordForumSendOutcome {
            thread_id: thread.id,
            message_id,
            warnings,
        })
    }

    /// Edit an existing message in a Discord channel.
    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}",
            DISCORD_API_BASE, channel_id, message_id
        );

        let formatted_content = format_discord_outgoing_content(content);
        let body = with_default_allowed_mentions(serde_json::json!({
            "content": truncate_discord_content(&formatted_content, MAX_MESSAGE_LENGTH),
        }));

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord edit API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Embeds
    // -----------------------------------------------------------------------

    /// Send a message with one or more embeds to a Discord channel.
    pub async fn send_embed(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
    ) -> Result<String, GatewayError> {
        self.send_embed_with_metadata(channel_id, content, embeds, None)
            .await
    }

    /// Send embeds, honoring Discord thread routing metadata when present.
    pub async fn send_embed_with_metadata(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = format!(
            "{}/channels/{}/messages",
            DISCORD_API_BASE, target_channel_id
        );

        let mut body = with_default_allowed_mentions(serde_json::json!({ "embeds": embeds }));
        if let Some(text) = content {
            body["content"] = serde_json::Value::String(text.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord embed send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord embed API error: {}",
                text
            )));
        }

        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
        })?;

        if metadata_marks_non_conversational(metadata) {
            self.mark_non_conversational_messages([msg.id.as_str()]);
        }

        Ok(msg.id)
    }

    // -----------------------------------------------------------------------
    // REST API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Discord channel using multipart form data.
    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, file_path, caption, None)
            .await
    }

    /// Upload a file, honoring Discord thread routing metadata when present.
    pub async fn upload_file_with_metadata(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = format!(
            "{}/channels/{}/messages",
            DISCORD_API_BASE, target_channel_id
        );

        let file_bytes = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e))
        })?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new().part("files[0]", part);

        let payload = with_default_allowed_mentions(match caption {
            Some(cap) => serde_json::json!({ "content": cap }),
            None => serde_json::json!({}),
        });
        form = form.text("payload_json", payload.to_string());

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord file upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord file upload API error: {}",
                text
            )));
        }

        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
        })?;

        if metadata_marks_non_conversational(metadata) {
            self.mark_non_conversational_messages([msg.id.as_str()]);
        }

        Ok(msg.id)
    }

    /// Send a local image file as a Discord attachment.
    pub async fn send_image_file(
        &self,
        channel_id: &str,
        image_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, image_path, caption, metadata)
            .await
    }

    /// Send an image URL as a Discord embed.
    pub async fn send_image(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.send_image_url_with_metadata(channel_id, image_url, caption, metadata)
            .await
    }

    /// Send a voice/audio file as a Discord attachment.
    pub async fn send_voice(
        &self,
        channel_id: &str,
        audio_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, audio_path, caption, metadata)
            .await
    }

    /// Send an image URL as an embed, honoring thread routing metadata.
    pub async fn send_image_url_with_metadata(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let mut embed = DiscordEmbed::new();
        embed.image = Some(EmbedMedia {
            url: image_url.to_string(),
        });
        self.send_embed_with_metadata(channel_id, caption, &[embed], metadata)
            .await
    }

    // -----------------------------------------------------------------------
    // REST API: Reactions
    // -----------------------------------------------------------------------

    /// Add a reaction to a message.
    ///
    /// `emoji` should be a URL-encoded unicode emoji (e.g. `%F0%9F%91%8D`)
    /// or a custom emoji in the form `name:id`.
    pub async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/reactions/{}/@me",
            DISCORD_API_BASE, channel_id, message_id, emoji
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord add_reaction failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord add_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    /// Remove the bot's own reaction from a message.
    pub async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/reactions/{}/@me",
            DISCORD_API_BASE, channel_id, message_id, emoji
        );

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord remove_reaction failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord remove_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Threads
    // -----------------------------------------------------------------------

    /// Create a public thread from an existing message.
    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/threads",
            DISCORD_API_BASE, channel_id, message_id
        );

        let mut body = serde_json::json!({ "name": name });
        if let Some(dur) = auto_archive_duration {
            body["auto_archive_duration"] = serde_json::Value::Number(dur.into());
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord create_thread failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord create_thread API error: {}",
                text
            )));
        }

        let thread: DiscordThread = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse thread response: {}", e))
        })?;

        Ok(thread)
    }

    // -----------------------------------------------------------------------
    // REST API: Slash command registration
    // -----------------------------------------------------------------------

    /// Register (overwrite) global application commands.
    ///
    /// This uses the bulk-overwrite endpoint which replaces all existing
    /// global commands with the ones provided.
    pub async fn register_slash_commands(
        &self,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;

        let url = format!("{}/applications/{}/commands", DISCORD_API_BASE, app_id);

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord register_commands failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_commands API error: {}",
                text
            )));
        }

        info!("registered {} global slash commands", commands.len());
        Ok(())
    }

    /// Register application commands scoped to a specific guild (faster
    /// propagation, useful during development).
    pub async fn register_guild_slash_commands(
        &self,
        guild_id: &str,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;

        let url = format!(
            "{}/applications/{}/guilds/{}/commands",
            DISCORD_API_BASE, app_id, guild_id
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord register_guild_commands failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_guild_commands API error: {}",
                text
            )));
        }

        info!(
            "registered {} guild slash commands for {}",
            commands.len(),
            guild_id
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Interaction responses
    // -----------------------------------------------------------------------

    /// Send an initial response to an interaction (slash command, button, etc.).
    pub async fn respond_to_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{}/{}/callback",
            DISCORD_API_BASE, interaction_id, interaction_token
        );

        let body = serde_json::json!({
            "type": 4, // CHANNEL_MESSAGE_WITH_SOURCE
            "data": { "content": content }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord interaction response failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord interaction response API error: {}",
                text
            )));
        }

        Ok(())
    }

    /// Send a deferred response (shows "thinking..." indicator).
    pub async fn defer_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{}/{}/callback",
            DISCORD_API_BASE, interaction_id, interaction_token
        );

        let body = serde_json::json!({
            "type": 5, // DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord defer interaction failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord defer interaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Gateway WebSocket helpers
    // -----------------------------------------------------------------------

    /// Build an IDENTIFY payload for the Discord Gateway.
    pub fn build_identify_payload(&self) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::IDENTIFY,
            d: Some(
                serde_json::to_value(IdentifyData {
                    token: self.config.token.clone(),
                    intents: self.config.intents,
                    properties: IdentifyProperties {
                        os: "linux".into(),
                        browser: "hermes-agent".into(),
                        device: "hermes-agent".into(),
                    },
                })
                .unwrap(),
            ),
            s: None,
            t: None,
        }
    }

    /// Build a HEARTBEAT payload.
    pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: sequence.map(|s| serde_json::Value::Number(s.into())),
            s: None,
            t: None,
        }
    }

    /// Build a RESUME payload.
    pub fn build_resume_payload(&self, session_id: &str, seq: u64) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::RESUME,
            d: Some(
                serde_json::to_value(ResumeData {
                    token: self.config.token.clone(),
                    session_id: session_id.to_string(),
                    seq,
                })
                .unwrap(),
            ),
            s: None,
            t: None,
        }
    }
}
