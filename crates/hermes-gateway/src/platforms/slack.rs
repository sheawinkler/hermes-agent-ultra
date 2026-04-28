//! Slack Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Slack using the Web API
//! for message operations (`chat.postMessage`, `chat.update`, `files.upload`)
//! and Socket Mode via WebSocket for receiving events.
//! Supports Block Kit formatting and thread replies via `thread_ts`.
//!
//! Additional capabilities: Socket Mode session management, Block Kit builder,
//! App Home tab publishing, interactive component handling, modals, user info,
//! reactions, topic setting, and permalinks.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};

/// Slack Web API base URL.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Maximum message length for Slack (4000 characters for text blocks).
const MAX_MESSAGE_LENGTH: usize = 4000;

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// SlackConfig
// ---------------------------------------------------------------------------

/// Configuration for the Slack adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Slack bot token (xoxb-...).
    pub token: String,

    /// Slack app-level token for socket mode (xapp-...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_token: Option<String>,

    /// Whether to use Socket Mode for receiving events.
    #[serde(default)]
    pub socket_mode: bool,

    /// Whether reaction lifecycle updates are enabled.
    #[serde(default = "default_true")]
    pub reactions: bool,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// Slack API types
// ---------------------------------------------------------------------------

/// Generic Slack API response.
#[derive(Debug, Deserialize)]
pub struct SlackResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Response for `users.info`.
#[derive(Debug, Deserialize)]
pub struct UserInfoResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub user: Option<SlackUser>,
}

/// Slack user profile data.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackUser {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub real_name: Option<String>,
    #[serde(default)]
    pub is_bot: bool,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub tz: Option<String>,
    #[serde(default)]
    pub profile: Option<SlackUserProfile>,
}

/// Subset of `users.info` profile fields.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SlackUserProfile {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub image_72: Option<String>,
}

/// Response for `chat.getPermalink`.
#[derive(Debug, Deserialize)]
pub struct PermalinkResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub permalink: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Slack Socket Mode hello event.
#[derive(Debug, Deserialize)]
pub struct SocketModeHello {
    #[serde(rename = "type")]
    pub event_type: String,
}

/// Slack Socket Mode envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct SocketModeEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    #[serde(default)]
    pub envelope_id: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

/// Slack event payload (from Events API / Socket Mode).
#[derive(Debug, Clone, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

/// Incoming message parsed from a Slack event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingSlackMessage {
    pub channel: String,
    pub user_id: Option<String>,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub is_bot: bool,
}

// ---------------------------------------------------------------------------
// Socket Mode session management
// ---------------------------------------------------------------------------

/// Connection state for a Socket Mode WebSocket session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketModeConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Closing,
}

/// Describes what the caller should do after `handle_envelope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SocketModeAction {
    Ack,
    MessageEvent(IncomingSlackMessage),
    InteractiveEvent(InteractivePayload),
    SlashCommand(SlashCommandPayload),
    Ignore,
}

/// Manages a single Socket Mode WebSocket session, tracking connection
/// lifecycle and providing envelope acknowledgment helpers.
#[derive(Debug)]
pub struct SocketModeSession {
    state: SocketModeConnectionState,
    envelopes_acked: u64,
}

impl SocketModeSession {
    pub fn new() -> Self {
        Self {
            state: SocketModeConnectionState::Disconnected,
            envelopes_acked: 0,
        }
    }

    pub fn state(&self) -> SocketModeConnectionState {
        self.state
    }
    pub fn envelopes_acked(&self) -> u64 {
        self.envelopes_acked
    }

    pub fn mark_connecting(&mut self) {
        self.state = SocketModeConnectionState::Connecting;
    }

    pub fn mark_connected(&mut self) {
        self.state = SocketModeConnectionState::Connected;
        debug!("Socket Mode session connected");
    }

    pub fn mark_closing(&mut self) {
        self.state = SocketModeConnectionState::Closing;
    }

    /// Build the JSON ack payload for a Socket Mode envelope.
    pub fn build_ack_payload(envelope_id: &str) -> String {
        format!(r#"{{"envelope_id":"{}"}}"#, envelope_id)
    }

    /// Inspect an envelope and return a typed action the caller should take.
    pub fn handle_envelope(&mut self, envelope: &SocketModeEnvelope) -> SocketModeAction {
        match envelope.envelope_type.as_str() {
            "hello" => {
                self.mark_connected();
                SocketModeAction::Ignore
            }
            "disconnect" => {
                info!("Socket Mode disconnect requested by server");
                self.mark_closing();
                SocketModeAction::Ignore
            }
            "events_api" => {
                self.envelopes_acked += 1;
                match SlackAdapter::parse_event(envelope) {
                    Some(msg) => SocketModeAction::MessageEvent(msg),
                    None => SocketModeAction::Ack,
                }
            }
            "interactive" => {
                self.envelopes_acked += 1;
                match InteractivePayload::from_envelope(envelope) {
                    Some(payload) => SocketModeAction::InteractiveEvent(payload),
                    None => SocketModeAction::Ack,
                }
            }
            "slash_commands" => {
                self.envelopes_acked += 1;
                match SlashCommandPayload::from_envelope(envelope) {
                    Some(cmd) => SocketModeAction::SlashCommand(cmd),
                    None => SocketModeAction::Ack,
                }
            }
            other => {
                debug!(envelope_type = other, "Unhandled Socket Mode envelope type");
                SocketModeAction::Ignore
            }
        }
    }
}

impl Default for SocketModeSession {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Interactive components & slash commands
// ---------------------------------------------------------------------------

/// Parsed interactive payload from `block_actions`, `view_submission`, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractivePayload {
    #[serde(rename = "type")]
    pub payload_type: String,
    #[serde(default)]
    pub trigger_id: Option<String>,
    #[serde(default)]
    pub actions: Vec<InteractiveAction>,
    #[serde(default)]
    pub user: Option<InteractiveUser>,
    #[serde(default)]
    pub channel: Option<InteractiveChannel>,
    #[serde(default)]
    pub message: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveAction {
    #[serde(default)]
    pub action_id: Option<String>,
    #[serde(default)]
    pub block_id: Option<String>,
    #[serde(rename = "type", default)]
    pub action_type: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub selected_option: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveChannel {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

impl InteractivePayload {
    pub fn from_envelope(envelope: &SocketModeEnvelope) -> Option<Self> {
        serde_json::from_value(envelope.payload.as_ref()?.clone()).ok()
    }
}

/// Parsed slash command payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommandPayload {
    pub command: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub trigger_id: Option<String>,
    #[serde(default)]
    pub response_url: Option<String>,
}

impl SlashCommandPayload {
    pub fn from_envelope(envelope: &SocketModeEnvelope) -> Option<Self> {
        serde_json::from_value(envelope.payload.as_ref()?.clone()).ok()
    }
}

// ---------------------------------------------------------------------------
// Block Kit message builder
// ---------------------------------------------------------------------------

/// A text object used throughout Block Kit (`plain_text` or `mrkdwn`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextObject {
    #[serde(rename = "type")]
    pub text_type: String,
    pub text: String,
}

impl TextObject {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text_type: "plain_text".into(),
            text: text.into(),
        }
    }
    pub fn mrkdwn(text: impl Into<String>) -> Self {
        Self {
            text_type: "mrkdwn".into(),
            text: text.into(),
        }
    }
}

/// An interactive element within an actions or section block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BlockElement {
    Button {
        text: TextObject,
        action_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        value: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        style: Option<String>,
    },
    Image {
        image_url: String,
        alt_text: String,
    },
    StaticSelect {
        placeholder: TextObject,
        action_id: String,
        options: Vec<SelectOption>,
    },
    Overflow {
        action_id: String,
        options: Vec<SelectOption>,
    },
}

/// An option inside a select menu or overflow element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    pub text: TextObject,
    pub value: String,
}

/// A section block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectionBlock {
    pub text: TextObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accessory: Option<BlockElement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<TextObject>>,
}

/// An actions block containing interactive elements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionsBlock {
    pub elements: Vec<BlockElement>,
}

/// A header block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeaderBlock {
    pub text: TextObject,
}

/// A context block (small text / images below content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBlock {
    pub elements: Vec<ContextElement>,
}

/// An element within a context block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextElement {
    #[serde(rename = "mrkdwn")]
    Mrkdwn {
        text: String,
    },
    #[serde(rename = "plain_text")]
    PlainText {
        text: String,
    },
    Image {
        image_url: String,
        alt_text: String,
    },
}

/// A Block Kit layout block.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Section(SectionBlock),
    Divider {},
    Actions(ActionsBlock),
    Header(HeaderBlock),
    Context(ContextBlock),
}

impl Block {
    pub fn section(text: TextObject) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: None,
            fields: None,
        })
    }

    pub fn section_with_accessory(text: TextObject, accessory: BlockElement) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: Some(accessory),
            fields: None,
        })
    }

    pub fn section_with_fields(text: TextObject, fields: Vec<TextObject>) -> Self {
        Block::Section(SectionBlock {
            text,
            accessory: None,
            fields: Some(fields),
        })
    }

    pub fn divider() -> Self {
        Block::Divider {}
    }

    pub fn actions(elements: Vec<BlockElement>) -> Self {
        Block::Actions(ActionsBlock { elements })
    }

    pub fn header(text: impl Into<String>) -> Self {
        Block::Header(HeaderBlock {
            text: TextObject::plain(text),
        })
    }

    pub fn context(elements: Vec<ContextElement>) -> Self {
        Block::Context(ContextBlock { elements })
    }
}

/// Builder for a complete Block Kit message.
#[derive(Debug, Clone, Default)]
pub struct BlockKitMessage {
    blocks: Vec<Block>,
}

impl BlockKitMessage {
    pub fn new() -> Self {
        Self { blocks: Vec::new() }
    }

    pub fn add_block(mut self, block: Block) -> Self {
        self.blocks.push(block);
        self
    }
    pub fn add_section(self, text: TextObject) -> Self {
        self.add_block(Block::section(text))
    }
    pub fn add_divider(self) -> Self {
        self.add_block(Block::divider())
    }
    pub fn add_header(self, text: impl Into<String>) -> Self {
        self.add_block(Block::header(text))
    }
    pub fn add_actions(self, elems: Vec<BlockElement>) -> Self {
        self.add_block(Block::actions(elems))
    }
    pub fn add_context(self, elems: Vec<ContextElement>) -> Self {
        self.add_block(Block::context(elems))
    }

    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Serialize the blocks array to a `serde_json::Value`.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(&self.blocks).unwrap_or_else(|_| serde_json::json!([]))
    }
}

// ---------------------------------------------------------------------------
// Home tab view
// ---------------------------------------------------------------------------

/// A Slack Home tab view payload for `views.publish`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeView {
    #[serde(rename = "type")]
    view_type: String,
    blocks: Vec<Block>,
}

impl HomeView {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self {
            view_type: "home".into(),
            blocks,
        }
    }

    pub fn from_block_kit(message: &BlockKitMessage) -> Self {
        Self::new(message.blocks().to_vec())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// Modal view (for views.open)
// ---------------------------------------------------------------------------

/// A Slack modal view payload for `views.open`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModalView {
    #[serde(rename = "type")]
    view_type: String,
    title: TextObject,
    #[serde(skip_serializing_if = "Option::is_none")]
    submit: Option<TextObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    close: Option<TextObject>,
    blocks: Vec<Block>,
    #[serde(skip_serializing_if = "Option::is_none")]
    callback_id: Option<String>,
}

impl ModalView {
    pub fn new(title: impl Into<String>, blocks: Vec<Block>) -> Self {
        Self {
            view_type: "modal".into(),
            title: TextObject::plain(title),
            submit: None,
            close: None,
            blocks,
            callback_id: None,
        }
    }

    pub fn with_submit(mut self, label: impl Into<String>) -> Self {
        self.submit = Some(TextObject::plain(label));
        self
    }

    pub fn with_close(mut self, label: impl Into<String>) -> Self {
        self.close = Some(TextObject::plain(label));
        self
    }

    pub fn with_callback_id(mut self, id: impl Into<String>) -> Self {
        self.callback_id = Some(id.into());
        self
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

// ---------------------------------------------------------------------------
// SlackAdapter
// ---------------------------------------------------------------------------

/// Slack Bot API platform adapter.
pub struct SlackAdapter {
    base: BasePlatformAdapter,
    config: SlackConfig,
    client: Client,
    stop_signal: Arc<Notify>,
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

    /// Parse a Socket Mode envelope into an IncomingSlackMessage.
    pub fn parse_event(envelope: &SocketModeEnvelope) -> Option<IncomingSlackMessage> {
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

        Some(IncomingSlackMessage {
            channel,
            user_id,
            text,
            ts,
            thread_ts,
            is_bot: false,
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

#[async_trait]
impl PlatformAdapter for SlackAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Slack adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Slack adapter stopping");
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
        self.post_message(chat_id, text, None).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        // In Slack, message_id is the `ts` timestamp.
        self.update_message(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption, None).await
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (blocks, fallback_text) = slack_image_url_blocks(image_url, caption);
        self.post_blocks(chat_id, &blocks, &fallback_text, None)
            .await?;
        Ok(())
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if !self.reactions_enabled() {
            return Ok(());
        }
        SlackAdapter::add_reaction(self, chat_id, message_id, emoji).await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        if !self.reactions_enabled() {
            return Ok(());
        }
        SlackAdapter::remove_reaction(self, chat_id, message_id, emoji).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "slack"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

fn reactions_toggle_enabled(raw: Option<&str>, default_enabled: bool) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => {
            let lowered = value.to_ascii_lowercase();
            !matches!(lowered.as_str(), "false" | "0" | "no")
        }
        None => default_enabled,
    }
}

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

fn slack_image_url_blocks(image_url: &str, caption: Option<&str>) -> (serde_json::Value, String) {
    let caption = caption.map(str::trim).filter(|s| !s.is_empty());
    let mut blocks = Vec::new();

    if let Some(text) = caption {
        blocks.push(serde_json::json!({
            "type": "section",
            "text": { "type": "mrkdwn", "text": text }
        }));
    }

    blocks.push(serde_json::json!({
        "type": "image",
        "image_url": image_url,
        "alt_text": caption.unwrap_or("image")
    }));

    let fallback = caption.unwrap_or(image_url).to_string();
    (serde_json::Value::Array(blocks), fallback)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Original tests (preserved) ---

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(5000);
        let chunks = split_message(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn parse_event_message() {
        let env = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "message", "text": "hello bot", "channel": "C123", "user": "U456", "ts": "1.0" }
            })),
        };
        let msg = SlackAdapter::parse_event(&env).unwrap();
        assert_eq!(msg.channel, "C123");
        assert_eq!(msg.user_id, Some("U456".into()));
        assert_eq!(msg.text, "hello bot");
        assert!(!msg.is_bot);
    }

    #[test]
    fn parse_event_bot_message_skipped() {
        let env = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "message", "text": "bot msg", "channel": "C123", "bot_id": "B789", "ts": "1.0" }
            })),
        };
        assert!(SlackAdapter::parse_event(&env).is_none());
    }

    #[test]
    fn parse_event_thread_reply() {
        let env = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "message", "text": "reply", "channel": "C1", "user": "U4",
                           "ts": "2.0", "thread_ts": "1.0" }
            })),
        };
        assert_eq!(
            SlackAdapter::parse_event(&env).unwrap().thread_ts,
            Some("1.0".into())
        );
    }

    #[test]
    fn parse_event_non_message_skipped() {
        let env = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "reaction_added", "reaction": "thumbsup", "user": "U456" }
            })),
        };
        assert!(SlackAdapter::parse_event(&env).is_none());
    }

    // --- Socket Mode session ---

    #[test]
    fn socket_mode_session_lifecycle() {
        let mut session = SocketModeSession::new();
        assert_eq!(session.state(), SocketModeConnectionState::Disconnected);
        assert_eq!(session.envelopes_acked(), 0);

        session.mark_connecting();
        assert_eq!(session.state(), SocketModeConnectionState::Connecting);
        session.mark_connected();
        assert_eq!(session.state(), SocketModeConnectionState::Connected);
        session.mark_closing();
        assert_eq!(session.state(), SocketModeConnectionState::Closing);

        assert_eq!(
            SocketModeSession::default().state(),
            SocketModeConnectionState::Disconnected
        );
    }

    #[test]
    fn build_ack_payload_format() {
        assert_eq!(
            SocketModeSession::build_ack_payload("abc-123"),
            r#"{"envelope_id":"abc-123"}"#
        );
    }

    #[test]
    fn handle_envelope_hello_and_disconnect() {
        let mut session = SocketModeSession::new();
        let hello = SocketModeEnvelope {
            envelope_type: "hello".into(),
            envelope_id: None,
            payload: None,
        };
        assert_eq!(session.handle_envelope(&hello), SocketModeAction::Ignore);
        assert_eq!(session.state(), SocketModeConnectionState::Connected);

        let disc = SocketModeEnvelope {
            envelope_type: "disconnect".into(),
            envelope_id: None,
            payload: None,
        };
        assert_eq!(session.handle_envelope(&disc), SocketModeAction::Ignore);
        assert_eq!(session.state(), SocketModeConnectionState::Closing);
    }

    #[test]
    fn handle_envelope_events_api() {
        let mut session = SocketModeSession::new();
        session.mark_connected();
        let msg_env = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("e1".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "message", "text": "hi", "channel": "C9", "user": "UA", "ts": "1.2" }
            })),
        };
        match session.handle_envelope(&msg_env) {
            SocketModeAction::MessageEvent(m) => {
                assert_eq!(m.channel, "C9");
                assert_eq!(m.text, "hi");
            }
            other => panic!("Expected MessageEvent, got {:?}", other),
        }
        let non_msg = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("e2".into()),
            payload: Some(serde_json::json!({
                "event": { "type": "app_mention", "channel": "C1", "user": "U1", "ts": "1.0" }
            })),
        };
        assert_eq!(session.handle_envelope(&non_msg), SocketModeAction::Ack);
        assert_eq!(session.envelopes_acked(), 2);
    }

    #[test]
    fn handle_envelope_interactive() {
        let mut session = SocketModeSession::new();
        let envelope = SocketModeEnvelope {
            envelope_type: "interactive".into(),
            envelope_id: Some("e3".into()),
            payload: Some(serde_json::json!({
                "type": "block_actions", "trigger_id": "t1",
                "actions": [{ "action_id": "btn", "type": "button", "value": "ok" }],
                "user": { "id": "U1" }
            })),
        };
        match session.handle_envelope(&envelope) {
            SocketModeAction::InteractiveEvent(p) => {
                assert_eq!(p.payload_type, "block_actions");
                assert_eq!(p.actions[0].action_id.as_deref(), Some("btn"));
            }
            other => panic!("Expected InteractiveEvent, got {:?}", other),
        }
    }

    #[test]
    fn handle_envelope_slash_command() {
        let mut session = SocketModeSession::new();
        let envelope = SocketModeEnvelope {
            envelope_type: "slash_commands".into(),
            envelope_id: Some("e4".into()),
            payload: Some(serde_json::json!({
                "command": "/deploy", "text": "prod", "channel_id": "C5", "user_id": "U7"
            })),
        };
        match session.handle_envelope(&envelope) {
            SocketModeAction::SlashCommand(cmd) => {
                assert_eq!(cmd.command, "/deploy");
                assert_eq!(cmd.text.as_deref(), Some("prod"));
            }
            other => panic!("Expected SlashCommand, got {:?}", other),
        }
    }

    #[test]
    fn handle_envelope_unknown_ignored() {
        let mut s = SocketModeSession::new();
        let e = SocketModeEnvelope {
            envelope_type: "future".into(),
            envelope_id: None,
            payload: None,
        };
        assert_eq!(s.handle_envelope(&e), SocketModeAction::Ignore);
        assert_eq!(s.envelopes_acked(), 0);
    }

    // --- Interactive & slash command parsing ---

    #[test]
    fn interactive_payload_parsing() {
        let env = SocketModeEnvelope {
            envelope_type: "interactive".into(),
            envelope_id: Some("ei".into()),
            payload: Some(serde_json::json!({
                "type": "block_actions", "trigger_id": "t9",
                "actions": [{ "action_id": "a1", "type": "button" }, { "action_id": "a2" }],
                "user": { "id": "U1" }, "channel": { "id": "C1", "name": "general" }
            })),
        };
        let p = InteractivePayload::from_envelope(&env).unwrap();
        assert_eq!(p.actions.len(), 2);
        assert_eq!(p.channel.as_ref().unwrap().id, "C1");
        let empty = SocketModeEnvelope {
            envelope_type: "interactive".into(),
            envelope_id: None,
            payload: None,
        };
        assert!(InteractivePayload::from_envelope(&empty).is_none());
    }

    #[test]
    fn slash_command_parsing() {
        let env = SocketModeEnvelope {
            envelope_type: "slash_commands".into(),
            envelope_id: Some("es".into()),
            payload: Some(serde_json::json!({
                "command": "/status", "text": "all", "channel_id": "C2",
                "user_id": "U2", "response_url": "https://hooks.slack.com/xxx"
            })),
        };
        let cmd = SlashCommandPayload::from_envelope(&env).unwrap();
        assert_eq!(cmd.command, "/status");
        assert_eq!(
            cmd.response_url.as_deref(),
            Some("https://hooks.slack.com/xxx")
        );
    }

    // --- Block Kit builder ---

    #[test]
    fn block_kit_builder() {
        let msg = BlockKitMessage::new();
        assert!(msg.is_empty());
        assert_eq!(msg.to_json(), serde_json::json!([]));

        let msg = BlockKitMessage::new()
            .add_header("Welcome")
            .add_divider()
            .add_section(TextObject::mrkdwn("Info"))
            .add_actions(vec![BlockElement::Button {
                text: TextObject::plain("Click"),
                action_id: "b".into(),
                value: Some("go".into()),
                style: Some("primary".into()),
            }])
            .add_context(vec![ContextElement::Mrkdwn {
                text: "footer".into(),
            }]);

        let arr = msg.to_json();
        let arr = arr.as_array().unwrap();
        assert_eq!(arr.len(), 5);
        assert_eq!(arr[0]["type"], "header");
        assert_eq!(arr[1]["type"], "divider");
        assert_eq!(arr[2]["type"], "section");
        assert_eq!(arr[3]["type"], "actions");
        assert_eq!(arr[4]["type"], "context");
    }

    #[test]
    fn slack_image_url_blocks_with_caption() {
        let (blocks, fallback) =
            slack_image_url_blocks("https://example.com/hero.png", Some("Release snapshot"));
        assert_eq!(fallback, "Release snapshot");
        let arr = blocks.as_array().expect("blocks array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["type"], "section");
        assert_eq!(arr[1]["type"], "image");
        assert_eq!(arr[1]["image_url"], "https://example.com/hero.png");
        assert_eq!(arr[1]["alt_text"], "Release snapshot");
    }

    #[test]
    fn slack_image_url_blocks_without_caption() {
        let (blocks, fallback) =
            slack_image_url_blocks("https://example.com/hero.png", Some("   "));
        assert_eq!(fallback, "https://example.com/hero.png");
        let arr = blocks.as_array().expect("blocks array");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["type"], "image");
        assert_eq!(arr[0]["alt_text"], "image");
    }

    #[test]
    fn reactions_toggle_enabled_defaults_and_env_overrides() {
        assert!(reactions_toggle_enabled(None, true));
        assert!(!reactions_toggle_enabled(None, false));
        assert!(reactions_toggle_enabled(Some("true"), false));
        assert!(!reactions_toggle_enabled(Some("0"), true));
        assert!(!reactions_toggle_enabled(Some("no"), true));
        assert!(reactions_toggle_enabled(Some("1"), false));
    }

    #[test]
    fn block_variants_serialize() {
        let sec = Block::section_with_accessory(
            TextObject::mrkdwn("Pick"),
            BlockElement::Button {
                text: TextObject::plain("Go"),
                action_id: "g".into(),
                value: None,
                style: None,
            },
        );
        assert_eq!(
            serde_json::to_value(&sec).unwrap()["accessory"]["type"],
            "button"
        );

        let fld = Block::section_with_fields(
            TextObject::mrkdwn("S"),
            vec![TextObject::mrkdwn("A"), TextObject::mrkdwn("B")],
        );
        assert_eq!(
            serde_json::to_value(&fld).unwrap()["fields"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let sel = BlockElement::StaticSelect {
            placeholder: TextObject::plain("Choose"),
            action_id: "s".into(),
            options: vec![SelectOption {
                text: TextObject::plain("X"),
                value: "x".into(),
            }],
        };
        assert_eq!(serde_json::to_value(&sel).unwrap()["type"], "static_select");
    }

    #[test]
    fn block_kit_round_trip() {
        let msg = BlockKitMessage::new()
            .add_header("T")
            .add_section(TextObject::plain("b"));
        assert_eq!(
            serde_json::from_value::<Vec<Block>>(msg.to_json())
                .unwrap()
                .len(),
            2
        );
    }

    // --- Home tab & modal views ---

    #[test]
    fn home_view() {
        let view = HomeView::new(vec![Block::header("Home"), Block::divider()]);
        let j = view.to_json();
        assert_eq!(j["type"], "home");
        assert_eq!(j["blocks"].as_array().unwrap().len(), 2);

        let msg = BlockKitMessage::new()
            .add_header("H")
            .add_section(TextObject::plain("S"));
        let view2 = HomeView::from_block_kit(&msg);
        assert_eq!(view2.to_json()["blocks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn modal_view() {
        let m = ModalView::new("Title", vec![Block::section(TextObject::plain("Body"))]);
        let j = m.to_json();
        assert_eq!(j["type"], "modal");
        assert_eq!(j["title"]["text"], "Title");
        assert!(j.get("submit").is_none());

        let m2 = ModalView::new("Confirm", vec![])
            .with_submit("Yes")
            .with_close("No")
            .with_callback_id("cb");
        let j2 = m2.to_json();
        assert_eq!(j2["submit"]["text"], "Yes");
        assert_eq!(j2["close"]["text"], "No");
        assert_eq!(j2["callback_id"], "cb");
    }

    // --- Response types & misc ---

    #[test]
    fn slack_user_deserializes() {
        let u: SlackUser = serde_json::from_value(serde_json::json!({
            "id": "U1", "name": "alice", "real_name": "Alice", "is_bot": false, "is_admin": true,
            "profile": { "email": "a@ex.com" }
        }))
        .unwrap();
        assert!(u.is_admin);
        assert_eq!(u.profile.unwrap().email.as_deref(), Some("a@ex.com"));
    }

    #[test]
    fn user_info_response_variants() {
        let ok: UserInfoResponse = serde_json::from_value(
            serde_json::json!({ "ok": true, "user": { "id": "U1", "is_bot": true } }),
        )
        .unwrap();
        assert!(ok.user.unwrap().is_bot);
        let err: UserInfoResponse =
            serde_json::from_value(serde_json::json!({ "ok": false, "error": "user_not_found" }))
                .unwrap();
        assert_eq!(err.error.as_deref(), Some("user_not_found"));
    }

    #[test]
    fn permalink_response_deserializes() {
        let r: PermalinkResponse = serde_json::from_value(serde_json::json!({
            "ok": true, "permalink": "https://ws.slack.com/archives/C1/p1", "channel": "C1"
        }))
        .unwrap();
        assert!(r.permalink.unwrap().contains("archives"));
    }

    #[test]
    fn context_elements_serialize() {
        let block = Block::context(vec![
            ContextElement::Mrkdwn {
                text: "by *bot*".into(),
            },
            ContextElement::PlainText { text: "now".into() },
            ContextElement::Image {
                image_url: "https://x.com/i.png".into(),
                alt_text: "i".into(),
            },
        ]);
        let elems = serde_json::to_value(&block).unwrap()["elements"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(elems.len(), 3);
        assert_eq!(elems[0]["type"], "mrkdwn");
    }

    #[test]
    fn split_message_at_newline_boundary() {
        let text = format!("{}\n{}", "a".repeat(3999), "b".repeat(100));
        assert_eq!(split_message(&text, 4000).len(), 2);
    }
}
