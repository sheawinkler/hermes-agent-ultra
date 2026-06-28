/// Discord Gateway payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

/// Discord Gateway opcodes.
pub mod opcodes {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const PRESENCE_UPDATE: u8 = 3;
    pub const VOICE_STATE: u8 = 4;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const REQUEST_GUILD_MEMBERS: u8 = 8;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Discord IDENTIFY payload data.
#[derive(Debug, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: u64,
    pub properties: IdentifyProperties,
}

/// Discord IDENTIFY connection properties.
#[derive(Debug, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

/// Discord RESUME payload data.
#[derive(Debug, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Gateway state machine
// ---------------------------------------------------------------------------

/// Actions that the external WebSocket driver should take after processing
/// a gateway event through [`GatewaySession::handle_gateway_event`].
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayAction {
    /// Send an IDENTIFY payload to the gateway.
    SendIdentify,
    /// Send a HEARTBEAT payload with the current sequence number.
    SendHeartbeat,
    /// Send a RESUME payload to continue a disconnected session.
    SendResume,
    /// The gateway requested a reconnect – close and reconnect.
    Reconnect,
    /// The session has been invalidated; if `bool` is true the session
    /// is resumable, otherwise a fresh IDENTIFY is required.
    InvalidSession(bool),
    /// A dispatch event arrived. Contains the event name and its data.
    Dispatch(String, serde_json::Value),
}

/// Manages the client-side state for a single Discord Gateway connection.
///
/// This is a pure state machine: feed it [`GatewayPayload`]s received from
/// the WebSocket and it will return a list of [`GatewayAction`]s that the
/// driver should execute. The struct never performs I/O itself, making it
/// easy to test and compose with any WebSocket library.
#[derive(Debug)]
pub struct GatewaySession {
    /// Last received sequence number.
    pub sequence: Option<u64>,
    /// Session ID from the READY event.
    pub session_id: Option<String>,
    /// Resume gateway URL from the READY event.
    pub resume_gateway_url: Option<String>,
    /// Heartbeat interval in milliseconds, extracted from HELLO.
    pub heartbeat_interval_ms: Option<u64>,
    /// Whether the last heartbeat was acknowledged.
    pub heartbeat_acknowledged: bool,
    /// Tracks whether we have successfully identified.
    pub identified: bool,
}

impl GatewaySession {
    pub fn new() -> Self {
        Self {
            sequence: None,
            session_id: None,
            resume_gateway_url: None,
            heartbeat_interval_ms: None,
            heartbeat_acknowledged: true,
            identified: false,
        }
    }

    /// Returns `true` if the session holds enough data to attempt a RESUME.
    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.sequence.is_some()
    }

    /// Process an incoming gateway payload and return the actions the driver
    /// should perform.
    pub fn handle_gateway_event(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        if let Some(seq) = payload.s {
            self.sequence = Some(seq);
        }

        match payload.op {
            opcodes::HELLO => self.handle_hello(payload),
            opcodes::HEARTBEAT_ACK => self.handle_heartbeat_ack(),
            opcodes::HEARTBEAT => self.handle_heartbeat_request(),
            opcodes::RECONNECT => vec![GatewayAction::Reconnect],
            opcodes::INVALID_SESSION => self.handle_invalid_session(payload),
            opcodes::DISPATCH => self.handle_dispatch(payload),
            _ => {
                debug!("unhandled gateway opcode {}", payload.op);
                vec![]
            }
        }
    }

    fn handle_hello(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let mut actions = Vec::new();

        if let Some(d) = &payload.d {
            if let Some(interval) = d.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                self.heartbeat_interval_ms = Some(interval);
                debug!("gateway HELLO: heartbeat_interval={}ms", interval);
            }
        }

        actions.push(GatewayAction::SendHeartbeat);

        if self.can_resume() {
            actions.push(GatewayAction::SendResume);
        } else {
            actions.push(GatewayAction::SendIdentify);
        }

        actions
    }

    fn handle_heartbeat_ack(&mut self) -> Vec<GatewayAction> {
        self.heartbeat_acknowledged = true;
        debug!("heartbeat ACK received");
        vec![]
    }

    fn handle_heartbeat_request(&self) -> Vec<GatewayAction> {
        vec![GatewayAction::SendHeartbeat]
    }

    fn handle_invalid_session(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let resumable = payload
            .d
            .as_ref()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !resumable {
            self.session_id = None;
            self.sequence = None;
            self.identified = false;
        }

        warn!("INVALID_SESSION received (resumable={})", resumable);
        vec![GatewayAction::InvalidSession(resumable)]
    }

    fn handle_dispatch(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let event_name = match &payload.t {
            Some(name) => name.clone(),
            None => return vec![],
        };

        let data = payload.d.clone().unwrap_or(serde_json::Value::Null);

        if event_name == "READY" {
            self.handle_ready(&data);
        }

        vec![GatewayAction::Dispatch(event_name, data)]
    }

    fn handle_ready(&mut self, data: &serde_json::Value) {
        self.identified = true;

        if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
            self.session_id = Some(sid.to_string());
        }
        if let Some(url) = data.get("resume_gateway_url").and_then(|v| v.as_str()) {
            self.resume_gateway_url = Some(url.to_string());
        }

        info!(
            "READY: session_id={:?}, resume_url={:?}",
            self.session_id, self.resume_gateway_url
        );
    }

    /// Mark a heartbeat as sent (used by the driver before sending).
    pub fn heartbeat_sent(&mut self) {
        self.heartbeat_acknowledged = false;
    }

    /// Returns `true` if the last heartbeat was not acknowledged, indicating
    /// the connection is likely zombied and should be reconnected.
    pub fn is_zombie(&self) -> bool {
        !self.heartbeat_acknowledged
    }

    /// Reset the session state for a fresh connection.
    pub fn reset(&mut self) {
        self.sequence = None;
        self.session_id = None;
        self.resume_gateway_url = None;
        self.heartbeat_interval_ms = None;
        self.heartbeat_acknowledged = true;
        self.identified = false;
    }
}

impl Default for GatewaySession {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Discord REST API types
// ---------------------------------------------------------------------------

/// Discord Message object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub author: Option<DiscordUser>,
}

/// Discord User object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

/// Incoming message parsed from a Discord MESSAGE_CREATE event.
#[derive(Debug, Clone)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
    pub message_type: u8,
    pub mention_user_ids: Vec<String>,
    pub reply_to_message_id: Option<String>,
    pub reply_to_text: Option<String>,
    pub attachments: Vec<DiscordIncomingAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordIncomingAttachment {
    pub filename: String,
    pub url: String,
    pub content_type: Option<String>,
    pub size: Option<u64>,
}

impl IncomingDiscordMessage {
    pub fn mentions_user(&self, user_id: &str) -> bool {
        let needle = user_id.trim();
        !needle.is_empty()
            && self
                .mention_user_ids
                .iter()
                .any(|mentioned| mentioned.trim() == needle)
    }
}

fn parse_discord_incoming_attachments(
    value: Option<&serde_json::Value>,
) -> Vec<DiscordIncomingAttachment> {
    value
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let url = item.get("url")?.as_str()?.to_string();
                    let filename = item
                        .get("filename")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    Some(DiscordIncomingAttachment {
                        filename,
                        url,
                        content_type: item
                            .get("content_type")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        size: item.get("size").and_then(|v| v.as_u64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Event types: MESSAGE_UPDATE
// ---------------------------------------------------------------------------

/// Parsed data from a `MESSAGE_UPDATE` dispatch event.
///
/// Discord may send partial updates — only `id` and `channel_id` are
/// guaranteed; other fields are optional.
#[derive(Debug, Clone)]
pub struct MessageUpdateEvent {
    pub channel_id: String,
    pub message_id: String,
    pub content: Option<String>,
    pub author_id: Option<String>,
    pub guild_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: INTERACTION_CREATE (slash commands)
// ---------------------------------------------------------------------------

/// Parsed interaction from `INTERACTION_CREATE`.
#[derive(Debug, Clone)]
pub struct InteractionData {
    pub id: String,
    pub application_id: String,
    /// Interaction type (2 = APPLICATION_COMMAND, 3 = MESSAGE_COMPONENT, …).
    pub interaction_type: u8,
    pub token: String,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub user_id: Option<String>,
    pub command_name: Option<String>,
    pub command_options: Vec<InteractionOption>,
}

/// A single option supplied to a slash command invocation.
#[derive(Debug, Clone)]
pub struct InteractionOption {
    pub name: String,
    pub value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Event types: Reactions
// ---------------------------------------------------------------------------

/// Parsed data from `MESSAGE_REACTION_ADD` / `MESSAGE_REACTION_REMOVE`.
#[derive(Debug, Clone)]
pub struct ReactionEvent {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub guild_id: Option<String>,
    pub emoji_name: Option<String>,
    pub emoji_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: Voice state
// ---------------------------------------------------------------------------

/// Parsed `VOICE_STATE_UPDATE` event.
#[derive(Debug, Clone)]
pub struct VoiceState {
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: String,
    pub session_id: String,
    pub deaf: bool,
    pub mute: bool,
    pub self_deaf: bool,
    pub self_mute: bool,
    pub suppress: bool,
}

// ---------------------------------------------------------------------------
// Slash command registration types
// ---------------------------------------------------------------------------

/// Definition of a slash command to register with Discord.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SlashCommandOption>>,
    /// Discord permission bitset string. "0" hides the command by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_member_permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dm_permission: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nsfw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexts: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_types: Option<Vec<u8>>,
    /// Command type (1 = CHAT_INPUT, 2 = USER, 3 = MESSAGE). Default 1.
    #[serde(rename = "type", default = "default_command_type")]
    pub command_type: u8,
}

/// A single option for a slash command.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandOption {
    pub name: String,
    pub description: String,
    /// Option type (3 = STRING, 4 = INTEGER, 5 = BOOLEAN, 6 = USER, …).
    #[serde(rename = "type")]
    pub option_type: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<SlashCommandChoice>>,
}

/// A predefined choice for a slash command option.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandChoice {
    pub name: String,
    pub value: serde_json::Value,
}

pub fn apply_owner_only_slash_visibility(commands: &mut [SlashCommand]) {
    for command in commands {
        command.default_member_permissions = Some("0".to_string());
    }
}

// ---------------------------------------------------------------------------
// Embed types
// ---------------------------------------------------------------------------

/// A Discord rich embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordEmbed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<EmbedAuthor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EmbedField>,
}

impl DiscordEmbed {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            url: None,
            color: None,
            timestamp: None,
            footer: None,
            image: None,
            thumbnail: None,
            author: None,
            fields: Vec::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn with_color(mut self, color: u32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(EmbedFooter {
            text: text.into(),
            icon_url: None,
        });
        self
    }

    pub fn with_timestamp(mut self, ts: impl Into<String>) -> Self {
        self.timestamp = Some(ts.into());
        self
    }

    pub fn add_field(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> Self {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline: Some(inline),
        });
        self
    }
}

impl Default for DiscordEmbed {
    fn default() -> Self {
        Self::new()
    }
}

/// Embed footer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedFooter {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// Embed media (image / thumbnail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedMedia {
    pub url: String,
}

/// Embed author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedAuthor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// A single field in an embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

// ---------------------------------------------------------------------------
// Thread creation result
// ---------------------------------------------------------------------------

/// Response from creating a thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordThread {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub thread_type: Option<u8>,
    pub guild_id: Option<String>,
    pub parent_id: Option<String>,
}

/// Response from creating a Discord forum post thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordForumThread {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub message: Option<DiscordMessage>,
}

/// Result of a forum post send where follow-up chunk failures are non-fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordForumSendOutcome {
    pub thread_id: String,
    pub message_id: String,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// DiscordAdapter
// ---------------------------------------------------------------------------

/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    thread_participation: Mutex<DiscordThreadParticipationTracker>,
    non_conversational_messages: Mutex<DiscordNonConversationalMessageTracker>,
}

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
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
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

        let body = with_default_allowed_mentions(serde_json::json!({
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
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

    // -----------------------------------------------------------------------
    // Event parsing
    // -----------------------------------------------------------------------

    /// Parse a MESSAGE_CREATE dispatch event into an IncomingDiscordMessage.
    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();
        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let author = data.get("author");
        let user_id = author
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let username = author
            .and_then(|a| a.get("username"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let is_bot = author
            .and_then(|a| a.get("bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let message_type = data.get("type").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        let mention_user_ids = data
            .get("mentions")
            .and_then(|v| v.as_array())
            .map(|mentions| {
                mentions
                    .iter()
                    .filter_map(|mention| mention.get("id").and_then(|id| id.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let reply_to_message_id = data
            .get("message_reference")
            .and_then(|reference| reference.get("message_id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let reply_to_text = data
            .get("referenced_message")
            .and_then(|message| message.get("content"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(String::from);
        let mut attachments = parse_discord_incoming_attachments(data.get("attachments"));
        attachments.extend(parse_discord_incoming_attachments(
            data.get("referenced_message")
                .and_then(|message| message.get("attachments")),
        ));

        Some(IncomingDiscordMessage {
            channel_id,
            message_id,
            user_id,
            username,
            content,
            is_bot,
            message_type,
            mention_user_ids,
            reply_to_message_id,
            reply_to_text,
            attachments,
        })
    }

    /// Apply Discord inbound self/system/bot filtering to a parsed message.
    pub fn should_accept_message(
        message: &IncomingDiscordMessage,
        client_user_id: Option<&str>,
        bot_policy: DiscordBotMessagePolicy,
    ) -> bool {
        if let (Some(author_id), Some(client_id)) = (message.user_id.as_deref(), client_user_id) {
            if author_id.trim() == client_id.trim() {
                return false;
            }
        }

        if !discord_message_type_is_user_visible(message.message_type) {
            return false;
        }

        if !message.is_bot {
            return true;
        }

        match bot_policy {
            DiscordBotMessagePolicy::None => false,
            DiscordBotMessagePolicy::All => true,
            DiscordBotMessagePolicy::Mentions => client_user_id
                .map(|id| message.mentions_user(id))
                .unwrap_or(false),
        }
    }

    /// Parse a MESSAGE_UPDATE dispatch event.
    pub fn parse_message_update(data: &serde_json::Value) -> Option<MessageUpdateEvent> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();

        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);
        let author_id = data
            .get("author")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(MessageUpdateEvent {
            channel_id,
            message_id,
            content,
            author_id,
            guild_id,
        })
    }

    /// Parse an INTERACTION_CREATE dispatch event.
    pub fn parse_interaction_create(data: &serde_json::Value) -> Option<InteractionData> {
        let id = data.get("id")?.as_str()?.to_string();
        let application_id = data.get("application_id")?.as_str()?.to_string();
        let interaction_type = data.get("type")?.as_u64()? as u8;
        let token = data.get("token")?.as_str()?.to_string();

        let channel_id = data
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // User ID can be in `member.user.id` (guild) or `user.id` (DM).
        let user_id = data
            .get("member")
            .and_then(|m| m.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(String::from);

        let cmd_data = data.get("data");
        let command_name = cmd_data
            .and_then(|d| d.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let command_options = cmd_data
            .and_then(|d| d.get("options"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|opt| {
                        let name = opt.get("name")?.as_str()?.to_string();
                        let value = opt.get("value").cloned().unwrap_or(serde_json::Value::Null);
                        Some(InteractionOption { name, value })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(InteractionData {
            id,
            application_id,
            interaction_type,
            token,
            channel_id,
            guild_id,
            user_id,
            command_name,
            command_options,
        })
    }

    /// Parse a MESSAGE_REACTION_ADD or MESSAGE_REACTION_REMOVE event.
    pub fn parse_reaction_event(data: &serde_json::Value) -> Option<ReactionEvent> {
        let user_id = data.get("user_id")?.as_str()?.to_string();
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("message_id")?.as_str()?.to_string();

        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let emoji = data.get("emoji");
        let emoji_name = emoji
            .and_then(|e| e.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let emoji_id = emoji
            .and_then(|e| e.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(ReactionEvent {
            user_id,
            channel_id,
            message_id,
            guild_id,
            emoji_name,
            emoji_id,
        })
    }

    /// Parse a VOICE_STATE_UPDATE event.
    pub fn parse_voice_state_update(data: &serde_json::Value) -> Option<VoiceState> {
        let user_id = data.get("user_id")?.as_str()?.to_string();
        let session_id = data.get("session_id")?.as_str()?.to_string();

        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let channel_id = data
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let deaf = data.get("deaf").and_then(|v| v.as_bool()).unwrap_or(false);
        let mute = data.get("mute").and_then(|v| v.as_bool()).unwrap_or(false);
        let self_deaf = data
            .get("self_deaf")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let self_mute = data
            .get("self_mute")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let suppress = data
            .get("suppress")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Some(VoiceState {
            guild_id,
            channel_id,
            user_id,
            session_id,
            deaf,
            mute,
            self_deaf,
            self_mute,
            suppress,
        })
    }

    /// Route a dispatch event by name to the appropriate parser.
    ///
    /// Returns a [`DispatchEvent`] for known event types, or `None`.
    pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
        match event_name {
            "MESSAGE_CREATE" => Self::parse_message_create(data).map(DispatchEvent::MessageCreate),
            "MESSAGE_UPDATE" => Self::parse_message_update(data).map(DispatchEvent::MessageUpdate),
            "INTERACTION_CREATE" => {
                Self::parse_interaction_create(data).map(DispatchEvent::InteractionCreate)
            }
            "MESSAGE_REACTION_ADD" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionAdd)
            }
            "MESSAGE_REACTION_REMOVE" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionRemove)
            }
            "VOICE_STATE_UPDATE" => {
                Self::parse_voice_state_update(data).map(DispatchEvent::VoiceStateUpdate)
            }
            _ => None,
        }
    }
}
