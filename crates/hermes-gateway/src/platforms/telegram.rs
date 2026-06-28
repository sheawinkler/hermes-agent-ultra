//! Telegram Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Telegram using the Bot API.
//! Supports sending/editing messages, file operations, long polling for
//! receiving updates, voice/photo/video/sticker message handling with media
//! caching, inline keyboards, callback queries, rate limiting, exponential
//! backoff reconnection, and group chat support.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_tools::approval::{self, ApprovalChoice, GatewayApprovalRequest};

use crate::adapter::{
    describe_secret, platform_http_client_builder, AdapterProxyConfig, BasePlatformAdapter,
};
use crate::commands::all_commands;
use crate::format::to_telegram_markdown_v2;

/// Maximum message length for Telegram (4096 characters).
const MAX_MESSAGE_LENGTH: usize = 4096;

/// Bot API 10.1 rich-message raw markdown character cap.
const RICH_MESSAGE_MAX_CHARS: usize = 32_768;

/// Default long-polling timeout in seconds.
const DEFAULT_POLL_TIMEOUT: u64 = 30;

/// Extra time allowed beyond Telegram's long-poll window before treating a
/// getUpdates request as wedged and handing it to the reconnect ladder.
const DEFAULT_POLL_STALL_GRACE_SECONDS: u64 = 15;

/// Initial backoff delay for reconnection (in milliseconds).
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Maximum backoff delay for reconnection (in milliseconds).
const MAX_BACKOFF_MS: u64 = 60_000;

/// Maximum number of retries for rate-limited requests.
const RATE_LIMIT_MAX_RETRIES: u32 = 3;

/// Maximum supported Telegram document size for processing (20 MB).
const TELEGRAM_MAX_DOCUMENT_SIZE_BYTES: u64 = 20 * 1024 * 1024;

/// Supported document extensions for Telegram document processing.
const SUPPORTED_DOCUMENT_EXTENSIONS: &[&str] = &[
    "pdf", "md", "txt", "docx", "xlsx", "pptx", "zip", "png", "jpg", "jpeg",
];

/// Telegram API host used for DNS fallback overrides.
const TELEGRAM_API_HOST: &str = "api.telegram.org";

/// Maximum Telegram caption length.
const MAX_CAPTION_LENGTH: usize = 1024;

/// Default aggregation delay for rapid Telegram text chunks.
const DEFAULT_TEXT_BATCH_DELAY_MS: u64 = 750;
const TELEGRAM_BOT_COMMAND_API_MAX: usize = 100;
const DEFAULT_TELEGRAM_COMMAND_MENU_MAX: usize = 60;
const TELEGRAM_COMMAND_MENU_PRIORITY: &[&str] = &[
    "start",
    "new",
    "help",
    "stop",
    "status",
    "resume",
    "sessions",
    "model",
    "update",
    "verbose",
    "commands",
    "approve",
    "deny",
    "queue",
    "steer",
    "background",
    "reasoning",
    "usage",
    "platform",
    "profile",
];

// ---------------------------------------------------------------------------
// TelegramConfig
// ---------------------------------------------------------------------------

/// Configuration for the Telegram adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Bot token from @BotFather.
    pub token: String,

    /// Optional webhook URL for receiving updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,

    /// Secret token required for webhook delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,

    /// Whether to use polling (true) or webhooks (false) for updates.
    #[serde(default = "default_true")]
    pub polling: bool,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,

    /// Whether to parse Markdown in messages.
    #[serde(default)]
    pub parse_markdown: bool,

    /// Whether to parse HTML in messages.
    #[serde(default)]
    pub parse_html: bool,

    /// Disable link previews on outbound Telegram text/rich messages.
    #[serde(default)]
    pub disable_link_previews: bool,

    /// Use Bot API 10.1 rich messages for eligible final text sends/edits.
    ///
    /// Opt-in because some Telegram desktop clients accept rich payloads but
    /// still render specific scripts/layouts worse than the legacy path.
    #[serde(default)]
    pub rich_messages: bool,

    /// Long-polling timeout in seconds.
    #[serde(default = "default_poll_timeout")]
    pub poll_timeout: u64,

    /// Reply threading behavior for split replies: off, first, or all.
    #[serde(default = "default_reply_to_mode")]
    pub reply_to_mode: String,

    /// Whether Telegram message reactions are enabled.
    #[serde(default)]
    pub reactions: bool,

    /// Optional Telegram API fallback IPs. Valid IPv4/IPv6 literals are used
    /// as reqwest DNS overrides for `api.telegram.org`.
    #[serde(default)]
    pub fallback_ips: Vec<String>,

    /// Whether group messages must directly address the bot.
    #[serde(default)]
    pub require_mention: bool,

    /// Allow direct mentions outside `allowed_chats` without opening the
    /// broader custom wake-word/reply gates.
    #[serde(default)]
    pub guest_mode: bool,

    /// Chats where group messages can bypass mention requirements.
    #[serde(default)]
    pub free_response_chats: Vec<String>,

    /// Group chats allowed to interact with the bot.
    #[serde(default)]
    pub allowed_chats: Vec<String>,

    /// Alias for Python `group_allowed_chats`.
    #[serde(default)]
    pub group_allowed_chats: Vec<String>,

    /// Forum topic/thread IDs to ignore.
    #[serde(default)]
    pub ignored_threads: Vec<String>,

    /// Forum topic/thread IDs allowed for processing. Threadless messages are
    /// treated as Telegram's general topic (`0`).
    #[serde(default)]
    pub allowed_topics: Vec<String>,

    /// Extra wake-word regexes accepted when `require_mention` is enabled.
    #[serde(default)]
    pub mention_patterns: Vec<String>,

    /// Require bot-command entities to address this bot explicitly.
    #[serde(default)]
    pub exclusive_bot_mentions: bool,

    /// Preserve unmentioned group media as observations instead of treating
    /// them as priority interrupts.
    #[serde(default)]
    pub observe_unmentioned_group_messages: bool,

    /// Delay for rapid text-chunk aggregation.
    #[serde(default = "default_text_batch_delay_ms")]
    pub text_batch_delay_ms: u64,

    /// Bot username (without @), used for mention filtering in groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot_username: Option<String>,

    /// Register Telegram BotCommand menu entries on startup.
    #[serde(default = "default_true")]
    pub command_menu_enabled: bool,

    /// Maximum BotCommand entries to register. Telegram caps this at 100.
    #[serde(default = "default_command_menu_max_commands")]
    pub command_menu_max_commands: usize,

    /// Priority command names that should survive a capped Telegram menu.
    #[serde(default)]
    pub command_menu_priority: Vec<String>,

    /// How configured priority interacts with defaults: prepend, append, replace.
    #[serde(default = "default_command_menu_priority_mode")]
    pub command_menu_priority_mode: String,
}

fn default_true() -> bool {
    true
}

fn default_poll_timeout() -> u64 {
    DEFAULT_POLL_TIMEOUT
}

fn poll_stall_grace_seconds() -> u64 {
    std::env::var("TELEGRAM_POLL_STALL_GRACE_SECONDS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_POLL_STALL_GRACE_SECONDS)
}

fn default_reply_to_mode() -> String {
    "first".to_string()
}

fn default_text_batch_delay_ms() -> u64 {
    DEFAULT_TEXT_BATCH_DELAY_MS
}

fn default_command_menu_max_commands() -> usize {
    DEFAULT_TELEGRAM_COMMAND_MENU_MAX
}

fn default_command_menu_priority_mode() -> String {
    "prepend".to_string()
}

/// Effective behavior for Telegram reply anchors across split chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelegramReplyToMode {
    Off,
    First,
    All,
}

impl TelegramReplyToMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(mode) if mode.eq_ignore_ascii_case("off") => Self::Off,
            Some(mode) if mode.eq_ignore_ascii_case("all") => Self::All,
            _ => Self::First,
        }
    }

    pub fn references_chunk(self, chunk_index: usize) -> bool {
        match self {
            Self::Off => false,
            Self::First => chunk_index == 0,
            Self::All => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Telegram API types
// ---------------------------------------------------------------------------

/// Telegram API response wrapper.
#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<ResponseParameters>,
}

/// Optional parameters returned on API errors (e.g. rate limiting).
#[derive(Debug, Clone, Deserialize)]
pub struct ResponseParameters {
    #[serde(default)]
    pub retry_after: Option<u64>,
    #[serde(default)]
    pub migrate_to_chat_id: Option<i64>,
}

/// Telegram Update object.
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
    #[serde(default)]
    pub callback_query: Option<CallbackQuery>,
}

/// Telegram CallbackQuery from inline keyboard interactions.
#[derive(Debug, Clone, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub from: User,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
    #[serde(default)]
    pub data: Option<String>,
    #[serde(default)]
    pub chat_instance: Option<String>,
}

/// Telegram Message object.
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramMessage {
    pub message_id: i64,
    pub chat: Chat,
    #[serde(default)]
    pub from: Option<User>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub voice: Option<Voice>,
    #[serde(default)]
    pub photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    pub caption: Option<String>,
    #[serde(default)]
    pub entities: Vec<MessageEntity>,
    #[serde(default)]
    pub caption_entities: Vec<MessageEntity>,
    #[serde(default)]
    pub sticker: Option<Sticker>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub reply_to_message: Option<Box<TelegramMessage>>,
    /// Native Bot API rich-message echo for replies to rich bot messages.
    #[serde(default)]
    pub rich_message: Option<serde_json::Value>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
    #[serde(default)]
    pub is_topic_message: Option<bool>,
}

/// Telegram message entity metadata for mention/bot-command gates.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageEntity {
    #[serde(rename = "type")]
    pub entity_type: String,
    pub offset: usize,
    pub length: usize,
}

/// Telegram Chat object.
#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

/// Telegram User object.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub is_bot: Option<bool>,
}

/// Telegram Voice object.
#[derive(Debug, Clone, Deserialize)]
pub struct Voice {
    pub file_id: String,
    pub file_unique_id: String,
    pub duration: u32,
}

/// Telegram PhotoSize object.
#[derive(Debug, Clone, Deserialize)]
pub struct PhotoSize {
    pub file_id: String,
    pub file_unique_id: String,
    pub width: u32,
    pub height: u32,
}

/// Telegram Sticker object.
#[derive(Debug, Clone, Deserialize)]
pub struct Sticker {
    pub file_id: String,
    pub file_unique_id: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub is_animated: Option<bool>,
    #[serde(default)]
    pub is_video: Option<bool>,
    #[serde(default)]
    pub emoji: Option<String>,
    #[serde(default)]
    pub set_name: Option<String>,
}

/// Telegram Document object.
#[derive(Debug, Clone, Deserialize)]
pub struct Document {
    pub file_id: String,
    #[serde(default)]
    pub file_unique_id: Option<String>,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub file_size: Option<u64>,
}

/// Telegram File object (from getFile).
#[derive(Debug, Clone, Deserialize)]
pub struct TelegramFile {
    pub file_id: String,
    #[serde(default)]
    pub file_path: Option<String>,
}

/// Telegram sent message result.
#[derive(Debug, Clone, Deserialize)]
pub struct SentMessage {
    pub message_id: i64,
}

/// Telegram ChatMember result.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatMember {
    pub status: String,
    pub user: User,
}

// ---------------------------------------------------------------------------
// Inline keyboard types
// ---------------------------------------------------------------------------

/// A single inline keyboard button.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardButton {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callback_data: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// An inline keyboard markup containing rows of buttons.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InlineKeyboardMarkup {
    pub inline_keyboard: Vec<Vec<InlineKeyboardButton>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TelegramBotCommand {
    command: String,
    description: String,
}

// ---------------------------------------------------------------------------
// IncomingMessage
// ---------------------------------------------------------------------------

/// Incoming message parsed from a Telegram update.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub chat_id: i64,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub text: Option<String>,
    pub message_id: i64,
    pub is_voice: bool,
    pub is_photo: bool,
    pub is_sticker: bool,
    pub is_document: bool,
    pub voice_file_id: Option<String>,
    pub photo_file_id: Option<String>,
    pub sticker_file_id: Option<String>,
    pub document_file_id: Option<String>,
    pub document_file_name: Option<String>,
    pub document_mime_type: Option<String>,
    pub document_file_size: Option<u64>,
    pub reply_to_message_id: Option<i64>,
    pub message_thread_id: Option<i64>,
    pub chat_type: ChatKind,
    pub is_group: bool,
    /// If this is a callback query, its ID (needed for `answerCallbackQuery`).
    pub callback_query_id: Option<String>,
    /// Data payload from a callback query button press.
    pub callback_data: Option<String>,
}

/// The kind of chat a message originated from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatKind {
    Private,
    Group,
    Supergroup,
    Channel,
    Unknown(String),
}

impl ChatKind {
    pub fn from_telegram_type(s: &str) -> Self {
        match s {
            "private" => ChatKind::Private,
            "group" => ChatKind::Group,
            "supergroup" => ChatKind::Supergroup,
            "channel" => ChatKind::Channel,
            other => ChatKind::Unknown(other.to_string()),
        }
    }

    pub fn is_group_like(&self) -> bool {
        matches!(self, ChatKind::Group | ChatKind::Supergroup)
    }
}

// ---------------------------------------------------------------------------
// Telegram text batching and topic-mode helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct PendingTextBatch {
    message: IncomingMessage,
    parts: Vec<String>,
    ready_at: Instant,
}

/// Deterministic aggregator for Telegram clients that split long text into
/// rapid successive updates.
#[derive(Debug)]
pub struct TelegramTextBatcher {
    delay: Duration,
    pending: HashMap<String, PendingTextBatch>,
}

impl TelegramTextBatcher {
    pub fn new(delay: Duration) -> Self {
        Self {
            delay,
            pending: HashMap::new(),
        }
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn enqueue(&mut self, message: IncomingMessage) {
        self.enqueue_at(message, Instant::now());
    }

    pub fn enqueue_at(&mut self, message: IncomingMessage, now: Instant) {
        let key = Self::batch_key(&message);
        let ready_at = now + self.delay;
        let text = message.text.clone().unwrap_or_default();
        self.pending
            .entry(key)
            .and_modify(|batch| {
                if !text.is_empty() {
                    batch.parts.push(text.clone());
                }
                batch.ready_at = ready_at;
                batch.message = message.clone();
            })
            .or_insert_with(|| PendingTextBatch {
                message,
                parts: if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text]
                },
                ready_at,
            });
    }

    pub fn drain_ready(&mut self) -> Vec<IncomingMessage> {
        self.drain_ready_at(Instant::now())
    }

    pub fn drain_ready_at(&mut self, now: Instant) -> Vec<IncomingMessage> {
        let ready_keys = self
            .pending
            .iter()
            .filter(|(_, batch)| batch.ready_at <= now)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();

        ready_keys
            .into_iter()
            .filter_map(|key| self.pending.remove(&key))
            .map(|mut batch| {
                let joined = batch
                    .parts
                    .iter()
                    .map(|s| s.trim())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !joined.is_empty() {
                    batch.message.text = Some(joined);
                }
                batch.message
            })
            .collect()
    }

    fn batch_key(message: &IncomingMessage) -> String {
        format!(
            "{}:{}:{}",
            message.chat_id,
            message.user_id.map(|id| id.to_string()).unwrap_or_default(),
            message
                .message_thread_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "root".to_string())
        )
    }
}

/// In-memory Telegram DM topic binding. The Rust gateway currently uses an
/// in-process session manager, so this mirrors the upstream topic invariants
/// without introducing a parallel SQLite layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramTopicBinding {
    pub chat_id: String,
    pub thread_id: String,
    pub session_id: String,
    pub user_id: String,
    pub title: Option<String>,
    pub operator_declared: bool,
    touched_seq: u64,
}

/// Topic-mode state for Telegram DM topic lanes.
#[derive(Debug, Default)]
pub struct TelegramTopicBindingStore {
    enabled: HashSet<(String, String)>,
    bindings: HashMap<(String, String), TelegramTopicBinding>,
    seq: u64,
}

impl TelegramTopicBindingStore {
    pub fn enable(&mut self, chat_id: impl Into<String>, user_id: impl Into<String>) {
        self.enabled.insert((chat_id.into(), user_id.into()));
    }

    pub fn disable(&mut self, chat_id: &str, user_id: &str) {
        self.enabled
            .remove(&(chat_id.to_string(), user_id.to_string()));
        self.bindings.retain(|(chat, _), binding| {
            !(chat == chat_id && binding.user_id.eq_ignore_ascii_case(user_id))
        });
    }

    pub fn is_enabled(&self, chat_id: &str, user_id: &str) -> bool {
        self.enabled
            .contains(&(chat_id.to_string(), user_id.to_string()))
    }

    pub fn bind(
        &mut self,
        chat_id: impl Into<String>,
        thread_id: impl Into<String>,
        session_id: impl Into<String>,
        user_id: impl Into<String>,
        title: Option<String>,
        operator_declared: bool,
    ) {
        self.seq = self.seq.saturating_add(1);
        let binding = TelegramTopicBinding {
            chat_id: chat_id.into(),
            thread_id: thread_id.into(),
            session_id: session_id.into(),
            user_id: user_id.into(),
            title,
            operator_declared,
            touched_seq: self.seq,
        };
        self.bindings.insert(
            (binding.chat_id.clone(), binding.thread_id.clone()),
            binding,
        );
    }

    pub fn get(&self, chat_id: &str, thread_id: &str) -> Option<&TelegramTopicBinding> {
        self.bindings
            .get(&(chat_id.to_string(), thread_id.to_string()))
    }

    pub fn get_by_session(&self, session_id: &str) -> Option<&TelegramTopicBinding> {
        self.bindings
            .values()
            .find(|binding| binding.session_id == session_id)
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.bindings
            .retain(|_, binding| binding.session_id != session_id);
    }

    pub fn remove(&mut self, chat_id: &str, thread_id: &str) -> bool {
        self.bindings
            .remove(&(chat_id.to_string(), thread_id.to_string()))
            .is_some()
    }

    pub fn list_for_chat(&self, chat_id: &str) -> Vec<TelegramTopicBinding> {
        let mut rows = self
            .bindings
            .values()
            .filter(|binding| binding.chat_id == chat_id)
            .cloned()
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| std::cmp::Reverse(row.touched_seq));
        rows
    }

    /// Recover stripped Telegram DM-topic replies. Known topics and brand-new
    /// non-root topic IDs must be preserved; only root/lobby messages can be
    /// rewritten to the most recently active topic.
    pub fn recover_thread_id(
        &self,
        chat_id: &str,
        user_id: &str,
        incoming_thread_id: Option<&str>,
    ) -> Option<String> {
        if !self.is_enabled(chat_id, user_id) {
            return None;
        }
        if let Some(thread_id) = incoming_thread_id.map(str::trim).filter(|s| !s.is_empty()) {
            if thread_id != "0" {
                return None;
            }
        }
        self.list_for_chat(chat_id)
            .into_iter()
            .find(|binding| binding.user_id == user_id)
            .map(|binding| binding.thread_id)
    }
}

// ---------------------------------------------------------------------------
// PollResult – returned from poll_with_backoff
// ---------------------------------------------------------------------------

/// Outcome of a single `poll_with_backoff` call.
#[derive(Debug)]
pub enum PollResult {
    Updates(Vec<Update>),
    Backoff { error: GatewayError, delay_ms: u64 },
}

// ---------------------------------------------------------------------------
// TelegramAdapter
// ---------------------------------------------------------------------------

/// Telegram Bot API platform adapter.
pub struct TelegramAdapter {
    base: BasePlatformAdapter,
    config: TelegramConfig,
    client: Client,
    /// Base URL for Telegram Bot API calls.
    api_base: String,
    /// Current offset for long polling (tracks last processed update).
    poll_offset: AtomicI64,
    /// Notify handle to signal the polling loop to stop.
    stop_signal: Arc<Notify>,
    /// Current backoff delay in milliseconds for reconnection logic.
    backoff_ms: AtomicU64,
    /// Consecutive poll error count.
    consecutive_errors: AtomicU64,
    /// Status message IDs keyed by `(chat_id, status_key)` so repeated status
    /// events edit one Telegram bubble instead of appending noisy updates.
    status_message_ids: Mutex<HashMap<(String, String), String>>,
    /// Inline approval callback IDs mapped to gateway session keys.
    approval_state: Mutex<HashMap<u64, String>>,
    approval_counter: AtomicU64,
    /// Latched off after the rich endpoint is proven unavailable.
    rich_send_disabled: Mutex<bool>,
    /// DM topic bindings used to recover Telegram clients that strip topic ids.
    topic_bindings: Mutex<TelegramTopicBindingStore>,
}

#[derive(Clone, Copy)]
struct TelegramMultipartRequest<'a> {
    chat_id: &'a str,
    file_path: &'a str,
    caption: Option<&'a str>,
    method: &'a str,
    field_name: &'a str,
    reply_to_message_id: Option<i64>,
    message_thread_id: Option<i64>,
}

impl TelegramMultipartRequest<'_> {
    fn has_thread_context(self) -> bool {
        self.reply_to_message_id.is_some() || self.message_thread_id.is_some()
    }

    fn without_thread_context(self) -> Self {
        Self {
            reply_to_message_id: None,
            message_thread_id: None,
            ..self
        }
    }
}

include!("telegram/adapter_impl.rs");

#[cfg(test)]
mod tests;
