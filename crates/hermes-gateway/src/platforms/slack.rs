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

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};
use crate::channel_directory::{ChannelDirectoryProvider, ChannelEntry};

/// Slack Web API base URL.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Maximum message length for Slack (4000 characters for text blocks).
const MAX_MESSAGE_LENGTH: usize = 4000;

const SLACK_AUDIO_MIME_TO_EXT: &[(&str, &str)] = &[
    ("audio/ogg", ".ogg"),
    ("audio/opus", ".ogg"),
    ("audio/mpeg", ".mp3"),
    ("audio/mp3", ".mp3"),
    ("audio/wav", ".wav"),
    ("audio/x-wav", ".wav"),
    ("audio/webm", ".webm"),
    ("audio/mp4", ".m4a"),
    ("audio/x-m4a", ".m4a"),
    ("audio/m4a", ".m4a"),
    ("audio/aac", ".m4a"),
    ("audio/flac", ".flac"),
    ("audio/x-flac", ".flac"),
];

const SLACK_STT_SUPPORTED_EXTS: &[&str] = &[
    ".mp3", ".mp4", ".mpeg", ".mpga", ".m4a", ".wav", ".webm", ".ogg", ".aac", ".flac",
];

const SLACK_EXT_TO_AUDIO_MIME: &[(&str, &str)] = &[
    (".mp4", "audio/mp4"),
    (".m4a", "audio/mp4"),
    (".mp3", "audio/mpeg"),
    (".mpeg", "audio/mpeg"),
    (".mpga", "audio/mpeg"),
    (".wav", "audio/wav"),
    (".webm", "audio/webm"),
    (".ogg", "audio/ogg"),
    (".aac", "audio/aac"),
    (".flac", "audio/flac"),
];

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

    /// Whether non-DM channel messages must mention or wake-word address the bot.
    #[serde(default)]
    pub require_mention: bool,

    /// Optional Slack bot user id used for literal `<@BOTID>` mention checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_user_id: Option<String>,

    /// Extra regex wake words accepted when `require_mention` is enabled.
    #[serde(default)]
    pub mention_patterns: Vec<String>,

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

#[derive(Debug, Deserialize)]
struct SlackConversationsResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub channels: Vec<SlackConversation>,
    #[serde(default)]
    pub response_metadata: SlackResponseMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SlackResponseMetadata {
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SlackConversation {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub is_private: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackMediaKind {
    Audio,
    Video,
    Image,
    Document,
    Unsupported,
}

/// Slack file attachment metadata preserved by the Socket Mode parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackMediaFile {
    pub id: Option<String>,
    pub name: Option<String>,
    pub mimetype: Option<String>,
    pub subtype: Option<String>,
    pub url_private: Option<String>,
    pub url_private_download: Option<String>,
    pub kind: SlackMediaKind,
    pub cache_extension: Option<String>,
    pub reported_mime_type: Option<String>,
}

impl SlackMediaFile {
    pub fn download_url(&self) -> Option<&str> {
        self.url_private_download
            .as_deref()
            .or(self.url_private.as_deref())
    }
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
    pub media_files: Vec<SlackMediaFile>,
}

/// Token-free mention policy used by Socket Mode routing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackMentionPolicy {
    pub require_mention: bool,
    pub bot_user_id: Option<String>,
    pub mention_patterns: Vec<String>,
}

impl SlackMentionPolicy {
    fn from_config(config: &SlackConfig) -> Self {
        Self {
            require_mention: config.require_mention,
            bot_user_id: config.bot_user_id.clone(),
            mention_patterns: config.mention_patterns.clone(),
        }
    }
}

impl SlackMediaFile {
    fn from_value(file: &serde_json::Value) -> Option<Self> {
        let id = slack_value_string(file, "id");
        let name = slack_value_string(file, "name");
        let mimetype =
            slack_value_string(file, "mimetype").or_else(|| slack_value_string(file, "mime_type"));
        let subtype = slack_value_string(file, "subtype");
        let url_private = slack_value_string(file, "url_private");
        let url_private_download = slack_value_string(file, "url_private_download");

        if id.is_none()
            && name.is_none()
            && mimetype.is_none()
            && subtype.is_none()
            && url_private.is_none()
            && url_private_download.is_none()
        {
            return None;
        }

        let kind = slack_media_kind(name.as_deref(), mimetype.as_deref(), subtype.as_deref());
        let (cache_extension, reported_mime_type) = if kind == SlackMediaKind::Audio {
            let ext = resolve_slack_audio_ext(name.as_deref(), mimetype.as_deref());
            let reported = slack_audio_mime_for_ext(&ext).to_string();
            (Some(ext), Some(reported))
        } else {
            (
                None,
                mimetype
                    .as_deref()
                    .map(slack_mime_key)
                    .filter(|s| !s.is_empty()),
            )
        };

        Some(Self {
            id,
            name,
            mimetype,
            subtype,
            url_private,
            url_private_download,
            kind,
            cache_extension,
            reported_mime_type,
        })
    }
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
    mention_policy: SlackMentionPolicy,
}

impl SocketModeSession {
    pub fn new() -> Self {
        Self::with_mention_policy(SlackMentionPolicy::default())
    }

    pub fn with_config(config: &SlackConfig) -> Self {
        Self::with_mention_policy(SlackMentionPolicy::from_config(config))
    }

    pub fn with_mention_policy(mention_policy: SlackMentionPolicy) -> Self {
        Self {
            state: SocketModeConnectionState::Disconnected,
            envelopes_acked: 0,
            mention_policy,
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
                match SlackAdapter::parse_event_with_mention_policy(envelope, &self.mention_policy)
                {
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

#[async_trait]
impl ChannelDirectoryProvider for SlackAdapter {
    fn platform_name(&self) -> &str {
        "slack"
    }

    async fn list_channel_entries(&self) -> Result<Vec<ChannelEntry>, GatewayError> {
        self.list_user_conversations().await
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

    async fn send_message_threaded(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.post_message(chat_id, text, thread_id).await?;
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

    fn splits_long_messages(&self) -> bool {
        true
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

fn slack_event_is_dm(envelope: &SocketModeEnvelope, channel_id: &str) -> bool {
    let channel_type = envelope
        .payload
        .as_ref()
        .and_then(|payload| payload.get("event"))
        .and_then(|event| event.get("channel_type"))
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    matches!(channel_type, "im" | "mpim") || channel_id.starts_with('D')
}

fn slack_value_string(value: &serde_json::Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn slack_mime_key(raw: &str) -> String {
    raw.split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn slack_filename_ext(file_name: &str) -> Option<String> {
    Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_ascii_lowercase()))
}

fn slack_stt_extension_supported(ext: &str) -> bool {
    SLACK_STT_SUPPORTED_EXTS.contains(&ext)
}

fn resolve_slack_audio_ext(file_name: Option<&str>, mimetype: Option<&str>) -> String {
    if let Some(ext) = file_name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(slack_filename_ext)
        .filter(|ext| slack_stt_extension_supported(ext))
    {
        return ext;
    }

    let mime_key = mimetype.map(slack_mime_key).unwrap_or_default();
    if let Some((_, ext)) = SLACK_AUDIO_MIME_TO_EXT
        .iter()
        .find(|(known, _)| *known == mime_key)
    {
        return (*ext).to_string();
    }

    ".m4a".to_string()
}

fn slack_audio_mime_for_ext(ext: &str) -> &'static str {
    SLACK_EXT_TO_AUDIO_MIME
        .iter()
        .find_map(|(known, mime)| (*known == ext).then_some(*mime))
        .unwrap_or("audio/mp4")
}

fn slack_file_is_voice_clip(name: Option<&str>, subtype: Option<&str>) -> bool {
    if subtype
        .map(str::trim)
        .map(|s| s.eq_ignore_ascii_case("slack_audio"))
        .unwrap_or(false)
    {
        return true;
    }

    name.map(str::trim)
        .map(|s| s.to_ascii_lowercase())
        .map(|s| s.starts_with("audio_message"))
        .unwrap_or(false)
}

fn slack_media_kind(
    name: Option<&str>,
    mimetype: Option<&str>,
    subtype: Option<&str>,
) -> SlackMediaKind {
    let mime_key = mimetype.map(slack_mime_key).unwrap_or_default();
    let ext = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .and_then(slack_filename_ext);
    let voice_clip = slack_file_is_voice_clip(name, subtype);

    if mime_key.starts_with("audio/") || voice_clip {
        return SlackMediaKind::Audio;
    }

    if matches!(
        ext.as_deref(),
        Some(".m4a" | ".mp3" | ".mpeg" | ".mpga" | ".wav" | ".ogg" | ".aac" | ".flac")
    ) {
        return SlackMediaKind::Audio;
    }

    if mime_key.starts_with("video/")
        || matches!(ext.as_deref(), Some(".mp4" | ".m4v" | ".mov" | ".webm"))
    {
        return SlackMediaKind::Video;
    }

    if mime_key.starts_with("image/")
        || matches!(
            ext.as_deref(),
            Some(".png" | ".jpg" | ".jpeg" | ".gif" | ".webp")
        )
    {
        return SlackMediaKind::Image;
    }

    if mime_key.starts_with("application/")
        || mime_key.starts_with("text/")
        || matches!(
            ext.as_deref(),
            Some(".pdf" | ".md" | ".txt" | ".csv" | ".json" | ".docx" | ".xlsx" | ".pptx" | ".zip")
        )
    {
        return SlackMediaKind::Document;
    }

    SlackMediaKind::Unsupported
}

fn parse_slack_media_files(event: &serde_json::Value) -> Vec<SlackMediaFile> {
    event
        .get("files")
        .and_then(|files| files.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(SlackMediaFile::from_value)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_slack_mention_pattern_values(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        match value {
            serde_json::Value::Array(values) => {
                return values
                    .into_iter()
                    .filter_map(|value| match value {
                        serde_json::Value::String(s) => Some(s),
                        serde_json::Value::Number(n) => Some(n.to_string()),
                        _ => None,
                    })
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            serde_json::Value::String(s) => {
                return s
                    .trim()
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }

    trimmed
        .replace('\n', ",")
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn slack_mention_pattern_sources(configured: &[String]) -> Vec<String> {
    let mut patterns: Vec<String> = configured
        .iter()
        .flat_map(|pattern| parse_slack_mention_pattern_values(pattern))
        .collect();
    if patterns.is_empty() {
        if let Ok(raw) = std::env::var("SLACK_MENTION_PATTERNS") {
            patterns = parse_slack_mention_pattern_values(&raw);
        }
    }
    patterns
}

fn compile_slack_mention_patterns(configured: &[String]) -> Vec<Regex> {
    slack_mention_pattern_sources(configured)
        .into_iter()
        .filter_map(
            |pattern| match RegexBuilder::new(&pattern).case_insensitive(true).build() {
                Ok(regex) => Some(regex),
                Err(err) => {
                    warn!(pattern = %pattern, error = %err, "Invalid Slack mention pattern");
                    None
                }
            },
        )
        .collect()
}

fn slack_message_matches_mention_patterns(text: &str, configured: &[String]) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    compile_slack_mention_patterns(configured)
        .iter()
        .any(|pattern| pattern.is_match(text))
}

fn slack_message_is_addressed(
    text: &str,
    bot_user_id: Option<&str>,
    mention_patterns: &[String],
) -> bool {
    if let Some(bot_user_id) = bot_user_id.map(str::trim).filter(|s| !s.is_empty()) {
        if text.contains(&format!("<@{bot_user_id}>")) {
            return true;
        }
    }
    slack_message_matches_mention_patterns(text, mention_patterns)
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
mod tests;
