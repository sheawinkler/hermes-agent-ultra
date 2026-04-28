//! Discord Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Discord using the REST API
//! for message operations and the Gateway WebSocket for receiving events.
//! Supports message splitting at 2000 characters, file uploads via
//! multipart form data, embeds, threads, reactions, slash commands, and
//! Gateway event handling (IDENTIFY, HEARTBEAT, RESUME, READY,
//! MESSAGE_CREATE, MESSAGE_UPDATE, INTERACTION_CREATE, VOICE_STATE_UPDATE,
//! MESSAGE_REACTION_ADD, MESSAGE_REACTION_REMOVE).

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};

/// Maximum message length for Discord (2000 characters).
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

// ---------------------------------------------------------------------------
// DiscordConfig
// ---------------------------------------------------------------------------

/// Configuration for the Discord adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Discord bot token.
    pub token: String,

    /// Application ID for interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,

    /// Whether the bot must be @mentioned in group channels.
    #[serde(default)]
    pub require_mention: bool,

    /// Gateway intents bitmask (default: GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT).
    #[serde(default = "default_intents")]
    pub intents: u64,
}

fn default_intents() -> u64 {
    // GUILDS (1<<0) | GUILD_MESSAGES (1<<9) | MESSAGE_CONTENT (1<<15)
    (1 << 0) | (1 << 9) | (1 << 15)
}

// ---------------------------------------------------------------------------
// Discord Gateway opcodes & payload
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// DiscordAdapter
// ---------------------------------------------------------------------------

/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    client: Client,
    stop_signal: Arc<Notify>,
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
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &DiscordConfig {
        &self.config
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
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();

        for chunk in &chunks {
            let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);
            let body = serde_json::json!({ "content": chunk });

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

        Ok(message_ids)
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

        let body = serde_json::json!({
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
        });

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
        let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);

        let mut body = serde_json::json!({ "embeds": embeds });
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
        let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);

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

        if let Some(cap) = caption {
            let payload = serde_json::json!({ "content": cap });
            form = form.text("payload_json", payload.to_string());
        }

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

        Ok(msg.id)
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

        Some(IncomingDiscordMessage {
            channel_id,
            message_id,
            user_id,
            username,
            content,
            is_bot,
        })
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

// ---------------------------------------------------------------------------
// Typed dispatch events
// ---------------------------------------------------------------------------

/// A strongly-typed dispatch event produced by [`DiscordAdapter::parse_dispatch`].
#[derive(Debug, Clone)]
pub enum DispatchEvent {
    MessageCreate(IncomingDiscordMessage),
    MessageUpdate(MessageUpdateEvent),
    InteractionCreate(InteractionData),
    ReactionAdd(ReactionEvent),
    ReactionRemove(ReactionEvent),
    VoiceStateUpdate(VoiceState),
}

// ---------------------------------------------------------------------------
// PlatformAdapter trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Discord adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Discord adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption).await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let mut embed = DiscordEmbed::new();
        embed.image = Some(EmbedMedia {
            url: image_url.to_string(),
        });
        self.send_embed(chat_id, caption, &[embed]).await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "discord"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Split a message into chunks that fit within the given max length.
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

        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

/// URL-encode a unicode emoji for use in reaction endpoints.
pub fn encode_emoji(emoji: &str) -> String {
    percent_encode_emoji(emoji)
}

fn percent_encode_emoji(s: &str) -> String {
    let mut out = String::new();
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_' || *byte == b':' {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- existing tests (preserved) -----------------------------------------

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(3000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn gateway_payload_identify() {
        let config = DiscordConfig {
            token: "test-token".into(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            require_mention: false,
            intents: default_intents(),
        };
        let adapter = DiscordAdapter::new(config).unwrap();
        let payload = adapter.build_identify_payload();
        assert_eq!(payload.op, opcodes::IDENTIFY);
        assert!(payload.d.is_some());
    }

    #[test]
    fn gateway_payload_heartbeat() {
        let payload = DiscordAdapter::build_heartbeat_payload(Some(42));
        assert_eq!(payload.op, opcodes::HEARTBEAT);
        assert_eq!(payload.d, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn parse_message_create_event() {
        let data = serde_json::json!({
            "id": "msg123",
            "channel_id": "ch456",
            "content": "hello world",
            "author": {
                "id": "user789",
                "username": "testuser",
                "bot": false
            }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert_eq!(msg.channel_id, "ch456");
        assert_eq!(msg.message_id, "msg123");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.user_id, Some("user789".into()));
        assert_eq!(msg.username, Some("testuser".into()));
        assert!(!msg.is_bot);
    }

    #[test]
    fn parse_message_create_bot() {
        let data = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "bot msg",
            "author": { "id": "bot1", "username": "mybot", "bot": true }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert!(msg.is_bot);
    }

    // -- GatewaySession tests -----------------------------------------------

    #[test]
    fn session_handles_hello() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 41250 })),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(session.heartbeat_interval_ms, Some(41250));
        assert!(actions.contains(&GatewayAction::SendHeartbeat));
        assert!(actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn session_handles_hello_with_resume() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess123".into());
        session.sequence = Some(42);

        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 30000 })),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert!(actions.contains(&GatewayAction::SendResume));
        assert!(!actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn session_handles_heartbeat_ack() {
        let mut session = GatewaySession::new();
        session.heartbeat_acknowledged = false;

        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT_ACK,
            d: None,
            s: None,
            t: None,
        };

        session.handle_gateway_event(&payload);
        assert!(session.heartbeat_acknowledged);
    }

    #[test]
    fn session_handles_reconnect() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::RECONNECT,
            d: None,
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::Reconnect]);
    }

    #[test]
    fn session_handles_invalid_session_resumable() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);

        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(true)),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::InvalidSession(true)]);
        assert!(session.session_id.is_some());
    }

    #[test]
    fn session_handles_invalid_session_not_resumable() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);

        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(false)),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::InvalidSession(false)]);
        assert!(session.session_id.is_none());
        assert!(session.sequence.is_none());
    }

    #[test]
    fn session_handles_ready_dispatch() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "session_id": "abc123",
                "resume_gateway_url": "wss://resume.discord.gg",
                "user": { "id": "12345", "username": "testbot" }
            })),
            s: Some(1),
            t: Some("READY".into()),
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(session.session_id, Some("abc123".into()));
        assert_eq!(
            session.resume_gateway_url,
            Some("wss://resume.discord.gg".into())
        );
        assert_eq!(session.sequence, Some(1));
        assert!(session.identified);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GatewayAction::Dispatch(name, _) => assert_eq!(name, "READY"),
            other => panic!("expected Dispatch, got {:?}", other),
        }
    }

    #[test]
    fn session_tracks_sequence() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({})),
            s: Some(42),
            t: Some("GUILD_CREATE".into()),
        };

        session.handle_gateway_event(&payload);
        assert_eq!(session.sequence, Some(42));
    }

    #[test]
    fn session_zombie_detection() {
        let mut session = GatewaySession::new();
        assert!(!session.is_zombie());

        session.heartbeat_sent();
        assert!(session.is_zombie());

        session.heartbeat_acknowledged = true;
        assert!(!session.is_zombie());
    }

    #[test]
    fn session_reset() {
        let mut session = GatewaySession::new();
        session.session_id = Some("s".into());
        session.sequence = Some(99);
        session.heartbeat_interval_ms = Some(5000);
        session.identified = true;

        session.reset();
        assert!(session.session_id.is_none());
        assert!(session.sequence.is_none());
        assert!(session.heartbeat_interval_ms.is_none());
        assert!(!session.identified);
    }

    #[test]
    fn session_heartbeat_request() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: None,
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::SendHeartbeat]);
    }

    // -- Event parsing tests ------------------------------------------------

    #[test]
    fn parse_message_update_full() {
        let data = serde_json::json!({
            "id": "msg100",
            "channel_id": "ch200",
            "content": "edited content",
            "author": { "id": "user300" },
            "guild_id": "guild400"
        });

        let evt = DiscordAdapter::parse_message_update(&data).unwrap();
        assert_eq!(evt.message_id, "msg100");
        assert_eq!(evt.channel_id, "ch200");
        assert_eq!(evt.content, Some("edited content".into()));
        assert_eq!(evt.author_id, Some("user300".into()));
        assert_eq!(evt.guild_id, Some("guild400".into()));
    }

    #[test]
    fn parse_message_update_partial() {
        let data = serde_json::json!({
            "id": "msg100",
            "channel_id": "ch200"
        });

        let evt = DiscordAdapter::parse_message_update(&data).unwrap();
        assert!(evt.content.is_none());
        assert!(evt.author_id.is_none());
    }

    #[test]
    fn parse_interaction_create_slash_command() {
        let data = serde_json::json!({
            "id": "int1",
            "application_id": "app1",
            "type": 2,
            "token": "tok1",
            "channel_id": "ch1",
            "guild_id": "g1",
            "member": {
                "user": { "id": "u1" }
            },
            "data": {
                "name": "hello",
                "options": [
                    { "name": "target", "value": "world" },
                    { "name": "count", "value": 3 }
                ]
            }
        });

        let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
        assert_eq!(interaction.id, "int1");
        assert_eq!(interaction.interaction_type, 2);
        assert_eq!(interaction.command_name, Some("hello".into()));
        assert_eq!(interaction.user_id, Some("u1".into()));
        assert_eq!(interaction.command_options.len(), 2);
        assert_eq!(interaction.command_options[0].name, "target");
        assert_eq!(
            interaction.command_options[0].value,
            serde_json::json!("world")
        );
        assert_eq!(interaction.command_options[1].name, "count");
        assert_eq!(interaction.command_options[1].value, serde_json::json!(3));
    }

    #[test]
    fn parse_interaction_create_dm() {
        let data = serde_json::json!({
            "id": "int2",
            "application_id": "app2",
            "type": 2,
            "token": "tok2",
            "user": { "id": "dm_user" },
            "data": { "name": "ping" }
        });

        let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
        assert_eq!(interaction.user_id, Some("dm_user".into()));
        assert!(interaction.guild_id.is_none());
        assert!(interaction.command_options.is_empty());
    }

    #[test]
    fn parse_reaction_add_event() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "ch1",
            "message_id": "msg1",
            "guild_id": "g1",
            "emoji": {
                "name": "\u{1f44d}",
                "id": null
            }
        });

        let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
        assert_eq!(evt.user_id, "u1");
        assert_eq!(evt.channel_id, "ch1");
        assert_eq!(evt.message_id, "msg1");
        assert_eq!(evt.guild_id, Some("g1".into()));
        assert_eq!(evt.emoji_name, Some("\u{1f44d}".into()));
        assert!(evt.emoji_id.is_none());
    }

    #[test]
    fn parse_reaction_custom_emoji() {
        let data = serde_json::json!({
            "user_id": "u2",
            "channel_id": "ch2",
            "message_id": "msg2",
            "emoji": {
                "name": "custom_emote",
                "id": "12345678"
            }
        });

        let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
        assert_eq!(evt.emoji_name, Some("custom_emote".into()));
        assert_eq!(evt.emoji_id, Some("12345678".into()));
    }

    #[test]
    fn parse_voice_state_update_event() {
        let data = serde_json::json!({
            "guild_id": "g1",
            "channel_id": "vc1",
            "user_id": "u1",
            "session_id": "sess1",
            "deaf": false,
            "mute": false,
            "self_deaf": true,
            "self_mute": true,
            "suppress": false
        });

        let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
        assert_eq!(vs.guild_id, Some("g1".into()));
        assert_eq!(vs.channel_id, Some("vc1".into()));
        assert_eq!(vs.user_id, "u1");
        assert!(!vs.deaf);
        assert!(!vs.mute);
        assert!(vs.self_deaf);
        assert!(vs.self_mute);
        assert!(!vs.suppress);
    }

    #[test]
    fn parse_voice_state_leave() {
        let data = serde_json::json!({
            "guild_id": "g1",
            "channel_id": null,
            "user_id": "u1",
            "session_id": "sess2",
            "deaf": false,
            "mute": false,
            "self_deaf": false,
            "self_mute": false,
            "suppress": false
        });

        let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
        assert!(vs.channel_id.is_none());
    }

    // -- Dispatch routing tests ---------------------------------------------

    #[test]
    fn dispatch_routes_message_create() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "content": "hi",
            "author": { "id": "u1", "username": "a", "bot": false }
        });

        let evt = DiscordAdapter::parse_dispatch("MESSAGE_CREATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::MessageCreate(_))));
    }

    #[test]
    fn dispatch_routes_message_update() {
        let data = serde_json::json!({ "id": "m1", "channel_id": "c1" });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_UPDATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::MessageUpdate(_))));
    }

    #[test]
    fn dispatch_routes_interaction_create() {
        let data = serde_json::json!({
            "id": "i1",
            "application_id": "a1",
            "type": 2,
            "token": "t1",
            "data": { "name": "test" }
        });
        let evt = DiscordAdapter::parse_dispatch("INTERACTION_CREATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::InteractionCreate(_))));
    }

    #[test]
    fn dispatch_routes_reaction_add() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "c1",
            "message_id": "m1",
            "emoji": { "name": "x" }
        });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_ADD", &data);
        assert!(matches!(evt, Some(DispatchEvent::ReactionAdd(_))));
    }

    #[test]
    fn dispatch_routes_reaction_remove() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "c1",
            "message_id": "m1",
            "emoji": { "name": "x" }
        });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_REMOVE", &data);
        assert!(matches!(evt, Some(DispatchEvent::ReactionRemove(_))));
    }

    #[test]
    fn dispatch_routes_voice_state() {
        let data = serde_json::json!({
            "user_id": "u1",
            "session_id": "s1",
            "deaf": false,
            "mute": false,
            "self_deaf": false,
            "self_mute": false,
            "suppress": false
        });
        let evt = DiscordAdapter::parse_dispatch("VOICE_STATE_UPDATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::VoiceStateUpdate(_))));
    }

    #[test]
    fn dispatch_unknown_event_returns_none() {
        let data = serde_json::json!({});
        let evt = DiscordAdapter::parse_dispatch("UNKNOWN_EVENT", &data);
        assert!(evt.is_none());
    }

    // -- Embed builder tests ------------------------------------------------

    #[test]
    fn embed_builder() {
        let embed = DiscordEmbed::new()
            .with_title("Test Embed")
            .with_description("A description")
            .with_color(0xFF5733)
            .with_footer("footer text")
            .with_timestamp("2026-01-01T00:00:00Z")
            .add_field("Field 1", "Value 1", true)
            .add_field("Field 2", "Value 2", false);

        assert_eq!(embed.title, Some("Test Embed".into()));
        assert_eq!(embed.description, Some("A description".into()));
        assert_eq!(embed.color, Some(0xFF5733));
        assert_eq!(embed.footer.as_ref().unwrap().text, "footer text");
        assert_eq!(embed.timestamp, Some("2026-01-01T00:00:00Z".into()));
        assert_eq!(embed.fields.len(), 2);
        assert_eq!(embed.fields[0].name, "Field 1");
        assert_eq!(embed.fields[0].inline, Some(true));
        assert_eq!(embed.fields[1].inline, Some(false));
    }

    #[test]
    fn embed_serialization() {
        let embed = DiscordEmbed::new().with_title("Hello").with_color(0x00FF00);

        let json = serde_json::to_value(&embed).unwrap();
        assert_eq!(json["title"], "Hello");
        assert_eq!(json["color"], 0x00FF00);
        assert!(json.get("description").is_none());
        assert!(json.get("footer").is_none());
    }

    // -- Slash command serialization tests ----------------------------------

    #[test]
    fn slash_command_serialization() {
        let cmd = SlashCommand {
            name: "greet".into(),
            description: "Say hello".into(),
            command_type: 1,
            options: Some(vec![
                SlashCommandOption {
                    name: "name".into(),
                    description: "Who to greet".into(),
                    option_type: 3, // STRING
                    required: Some(true),
                    choices: None,
                },
                SlashCommandOption {
                    name: "style".into(),
                    description: "Greeting style".into(),
                    option_type: 3,
                    required: Some(false),
                    choices: Some(vec![
                        SlashCommandChoice {
                            name: "Formal".into(),
                            value: serde_json::json!("formal"),
                        },
                        SlashCommandChoice {
                            name: "Casual".into(),
                            value: serde_json::json!("casual"),
                        },
                    ]),
                },
            ]),
        };

        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["name"], "greet");
        assert_eq!(json["type"], 1);
        let options = json["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["required"], true);
        let choices = options[1]["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["name"], "Formal");
    }

    // -- Emoji encoding tests -----------------------------------------------

    #[test]
    fn encode_emoji_unicode() {
        let encoded = encode_emoji("\u{1f44d}");
        assert_eq!(encoded, "%F0%9F%91%8D");
    }

    #[test]
    fn encode_emoji_custom() {
        let encoded = encode_emoji("custom_emote:12345");
        assert_eq!(encoded, "custom_emote:12345");
    }

    // -- Default trait impls ------------------------------------------------

    #[test]
    fn gateway_session_default() {
        let session = GatewaySession::default();
        assert!(session.sequence.is_none());
        assert!(session.session_id.is_none());
        assert!(!session.identified);
        assert!(session.heartbeat_acknowledged);
    }

    #[test]
    fn embed_default() {
        let embed = DiscordEmbed::default();
        assert!(embed.title.is_none());
        assert!(embed.fields.is_empty());
    }
}
