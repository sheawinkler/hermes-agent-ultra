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
use crate::format::to_telegram_markdown_v2;

/// Maximum message length for Telegram (4096 characters).
const MAX_MESSAGE_LENGTH: usize = 4096;

/// Default long-polling timeout in seconds.
const DEFAULT_POLL_TIMEOUT: u64 = 30;

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
}

fn default_true() -> bool {
    true
}

fn default_poll_timeout() -> u64 {
    DEFAULT_POLL_TIMEOUT
}

fn default_reply_to_mode() -> String {
    "first".to_string()
}

fn default_text_batch_delay_ms() -> u64 {
    DEFAULT_TEXT_BATCH_DELAY_MS
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
    pub fn from_str(s: &str) -> Self {
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
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &TelegramConfig {
        &self.config
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
        let chat_kind = ChatKind::from_str(&msg.chat.chat_type);
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

    /// Edit an existing message's text.
    pub async fn edit_text(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
        parse_mode: Option<&str>,
    ) -> Result<(), GatewayError> {
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "message_id": message_id.parse::<i64>().unwrap_or(0),
            "text": self.outgoing_text_for_parse_mode(
                &text[..text.len().min(MAX_MESSAGE_LENGTH)],
                parse_mode,
            ),
        });

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
                let description = resp
                    .description
                    .unwrap_or_else(|| format!("{method} failed"));
                if Self::thread_or_reply_missing(&description)
                    && Self::strip_thread_fields_for_fallback(&mut body)
                {
                    let retry = self.post_json(&url, &body).await?;
                    if retry.ok {
                        return Ok(retry);
                    }
                    return Err(GatewayError::SendFailed(
                        retry
                            .description
                            .unwrap_or_else(|| format!("{method} fallback failed")),
                    ));
                }
                Err(GatewayError::SendFailed(description))
            }
            Err(err) if Self::gateway_error_thread_or_reply_missing(&err) => {
                if Self::strip_thread_fields_for_fallback(&mut body) {
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
        removed_thread || removed_reply
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
            return Err(GatewayError::SendFailed(format!(
                "Rate limited on {}: {}",
                request.method, body_text
            )));
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
            Err(GatewayError::SendFailed(
                result
                    .description
                    .unwrap_or_else(|| format!("{} failed", request.method)),
            ))
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

        let resp: TelegramResponse<Vec<Update>> = self.post_json(&url, &body).await?;

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
        let text = msg.text.clone().or_else(|| msg.caption.clone());
        let user_id = msg.from.as_ref().map(|u| u.id);
        let username = msg.from.as_ref().and_then(|u| u.username.clone());

        let is_voice = msg.voice.is_some();
        let voice_file_id = msg.voice.as_ref().map(|v| v.file_id.clone());

        let is_photo = msg.photo.is_some();
        let photo_file_id = msg
            .photo
            .as_ref()
            .and_then(|photos| photos.last().map(|p| p.file_id.clone()));

        let is_sticker = msg.sticker.is_some();
        let sticker_file_id = msg.sticker.as_ref().map(|s| s.file_id.clone());

        let is_document = msg.document.is_some();
        let document_file_id = msg.document.as_ref().map(|d| d.file_id.clone());
        let document_file_name = msg.document.as_ref().and_then(|d| d.file_name.clone());
        let document_mime_type = msg.document.as_ref().and_then(|d| d.mime_type.clone());
        let document_file_size = msg.document.as_ref().and_then(|d| d.file_size);

        let reply_to_message_id = msg.reply_to_message.as_ref().map(|r| r.message_id);

        let chat_type = ChatKind::from_str(&msg.chat.chat_type);
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
            .map(|m| ChatKind::from_str(&m.chat.chat_type))
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
    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let mut retries = 0u32;

        loop {
            let resp = self.client.post(url).json(body).send().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Telegram API request failed: {}", e))
            })?;

            let status = resp.status();

            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let text = resp.text().await.unwrap_or_default();

                let retry_after = serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| v.get("parameters")?.get("retry_after")?.as_u64())
                    .unwrap_or(5);

                retries += 1;
                if retries > RATE_LIMIT_MAX_RETRIES {
                    return Err(GatewayError::SendFailed(format!(
                        "Rate limited after {} retries (retry_after={}s): {}",
                        retries, retry_after, text
                    )));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

    struct JsonFieldAbsent(&'static str);

    impl Match for JsonFieldAbsent {
        fn matches(&self, request: &Request) -> bool {
            request
                .body_json::<Value>()
                .ok()
                .and_then(|v| v.as_object().cloned())
                .map(|obj| !obj.contains_key(self.0))
                .unwrap_or(false)
        }
    }

    fn test_config() -> TelegramConfig {
        TelegramConfig {
            token: "fake_token_12345".into(),
            webhook_url: None,
            webhook_secret: None,
            polling: true,
            proxy: AdapterProxyConfig::default(),
            parse_markdown: false,
            parse_html: false,
            poll_timeout: 30,
            reply_to_mode: "first".into(),
            reactions: false,
            fallback_ips: Vec::new(),
            require_mention: false,
            guest_mode: false,
            free_response_chats: Vec::new(),
            allowed_chats: Vec::new(),
            group_allowed_chats: Vec::new(),
            ignored_threads: Vec::new(),
            allowed_topics: Vec::new(),
            mention_patterns: Vec::new(),
            exclusive_bot_mentions: false,
            observe_unmentioned_group_messages: false,
            text_batch_delay_ms: DEFAULT_TEXT_BATCH_DELAY_MS,
            bot_username: None,
        }
    }

    fn test_adapter(config: TelegramConfig) -> TelegramAdapter {
        TelegramAdapter::new(config).unwrap()
    }

    // -----------------------------------------------------------------------
    // split_message tests (original)
    // -----------------------------------------------------------------------

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 4096);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_exact_boundary() {
        let text = "a".repeat(4096);
        let chunks = split_message(&text, 4096);
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(5000);
        let chunks = split_message(&text, 4096);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4096);
        assert_eq!(chunks[1].len(), 904);
    }

    #[test]
    fn split_message_prefers_newline() {
        let mut text = "a".repeat(4000);
        text.push('\n');
        text.push_str(&"b".repeat(200));
        let chunks = split_message(&text, 4096);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].ends_with('\n'));
    }

    #[test]
    fn telegram_reply_to_mode_parse_and_chunk_policy() {
        assert_eq!(TelegramReplyToMode::parse(None), TelegramReplyToMode::First);
        assert_eq!(
            TelegramReplyToMode::parse(Some("off")),
            TelegramReplyToMode::Off
        );
        assert_eq!(
            TelegramReplyToMode::parse(Some("ALL")),
            TelegramReplyToMode::All
        );
        assert_eq!(
            TelegramReplyToMode::parse(Some("invalid")),
            TelegramReplyToMode::First
        );

        assert!(!TelegramReplyToMode::Off.references_chunk(0));
        assert!(TelegramReplyToMode::First.references_chunk(0));
        assert!(!TelegramReplyToMode::First.references_chunk(1));
        assert!(TelegramReplyToMode::All.references_chunk(10));
    }

    #[test]
    fn telegram_should_thread_reply_respects_reply_mode() {
        let mut cfg = test_config();
        cfg.reply_to_mode = "off".into();
        let adapter = test_adapter(cfg);
        assert!(!adapter.should_thread_reply(Some(99), 0));

        let mut cfg = test_config();
        cfg.reply_to_mode = "first".into();
        let adapter = test_adapter(cfg);
        assert!(adapter.should_thread_reply(Some(99), 0));
        assert!(!adapter.should_thread_reply(Some(99), 1));
        assert!(!adapter.should_thread_reply(None, 0));

        let mut cfg = test_config();
        cfg.reply_to_mode = "all".into();
        let adapter = test_adapter(cfg);
        assert!(adapter.should_thread_reply(Some(99), 0));
        assert!(adapter.should_thread_reply(Some(99), 2));
    }

    #[test]
    fn telegram_split_gateway_chat_thread_preserves_topic_suffix() {
        assert_eq!(
            TelegramAdapter::split_gateway_chat_thread("-1001:17585"),
            ("-1001", Some(17585))
        );
        assert_eq!(
            TelegramAdapter::split_gateway_chat_thread("-1001:0"),
            ("-1001:0", None)
        );
        assert_eq!(
            TelegramAdapter::split_gateway_chat_thread("room:server"),
            ("room:server", None)
        );
    }

    #[test]
    fn telegram_merge_caption_uses_exact_dedupe() {
        assert_eq!(TelegramAdapter::merge_caption(None, "Hello"), "Hello");
        assert_eq!(
            TelegramAdapter::merge_caption(Some("Revenue"), "Revenue  "),
            "Revenue"
        );
        assert_eq!(
            TelegramAdapter::merge_caption(Some("Meeting agenda"), "Meeting"),
            "Meeting agenda\n\nMeeting"
        );
        assert_eq!(
            TelegramAdapter::merge_caption(Some("Revenue"), "Revenue and Profit"),
            "Revenue\n\nRevenue and Profit"
        );
        let merged = TelegramAdapter::merge_caption(Some("A\n\nB"), "A");
        assert_eq!(merged, "A\n\nB");
    }

    #[test]
    fn telegram_webhook_secret_required_only_for_webhook_mode() {
        let polling = test_config();
        assert!(TelegramAdapter::new(polling).is_ok());

        let mut webhook = test_config();
        webhook.webhook_url = Some("https://hooks.example.com/tg".into());
        let err = match TelegramAdapter::new(webhook) {
            Ok(_) => panic!("webhook mode without secret must fail"),
            Err(err) => err.to_string(),
        };
        assert!(err.contains("TELEGRAM_WEBHOOK_SECRET"));
        assert!(err.contains("GHSA-3vpc-7q5r-276h"));
        assert!(err.contains("openssl rand"));

        let mut webhook = test_config();
        webhook.webhook_url = Some("https://hooks.example.com/tg".into());
        webhook.webhook_secret = Some("secret-token".into());
        assert!(TelegramAdapter::new(webhook).is_ok());
    }

    #[tokio::test]
    async fn telegram_reactions_call_set_message_reaction_when_enabled() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botfake_token_12345/setMessageReaction"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": true
            })))
            .mount(&server)
            .await;

        let mut cfg = test_config();
        cfg.reactions = true;
        let mut adapter = test_adapter(cfg);
        adapter.api_base = format!("{}/botfake_token_12345", server.uri());

        adapter.add_reaction("123", "456", "👀").await.unwrap();
        adapter.remove_reaction("123", "456", "👀").await.unwrap();

        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
        let add_json: Value = serde_json::from_slice(&requests[0].body).expect("add json");
        assert_eq!(
            add_json.pointer("/chat_id").and_then(|v| v.as_str()),
            Some("123")
        );
        assert_eq!(
            add_json.pointer("/message_id").and_then(|v| v.as_i64()),
            Some(456)
        );
        assert_eq!(
            add_json
                .pointer("/reaction/0/emoji")
                .and_then(|v| v.as_str()),
            Some("👀")
        );
        let remove_json: Value = serde_json::from_slice(&requests[1].body).expect("remove json");
        assert_eq!(
            remove_json
                .pointer("/reaction")
                .and_then(|v| v.as_array())
                .map(Vec::len),
            Some(0)
        );
    }

    #[tokio::test]
    async fn telegram_reactions_are_noop_when_disabled() {
        let server = MockServer::start().await;
        let mut adapter = test_adapter(test_config());
        adapter.api_base = format!("{}/botfake_token_12345", server.uri());

        adapter.add_reaction("123", "456", "👀").await.unwrap();

        let requests = server.received_requests().await.expect("requests");
        assert!(requests.is_empty());
    }

    #[test]
    fn telegram_network_fallback_ips_filter_and_deduplicate() {
        let raw = vec![
            "149.154.167.220".to_string(),
            "not-valid".to_string(),
            "149.154.167.220,149.154.167.221".to_string(),
            "::1".to_string(),
        ];
        let addrs = TelegramAdapter::fallback_socket_addrs(&raw);
        let rendered = addrs
            .iter()
            .map(|addr| addr.ip().to_string())
            .collect::<Vec<_>>();
        assert_eq!(rendered, vec!["149.154.167.220", "149.154.167.221", "::1"]);

        let mut cfg = test_config();
        cfg.fallback_ips = raw;
        assert!(TelegramAdapter::new(cfg).is_ok());
    }

    #[test]
    fn telegram_group_gating_covers_mentions_guests_threads_and_topics() {
        let mut cfg = test_config();
        cfg.bot_username = Some("hermes_bot".into());
        cfg.require_mention = true;
        cfg.allowed_chats = vec!["-100".into()];
        cfg.guest_mode = true;
        cfg.mention_patterns = vec![r"^\s*chompy\b".into(), "(".into()];
        cfg.ignored_threads = vec!["31".into()];
        cfg.allowed_topics = vec!["8".into(), "0".into()];
        let adapter = test_adapter(cfg);

        let mut msg = make_text_message(
            1,
            make_chat(-100, "supergroup"),
            make_user(11, Some("u")),
            "hello",
        );
        msg.message_thread_id = Some(8);
        assert!(!adapter.should_process_message(&msg, false));

        msg.text = Some("hi @hermes_bot".into());
        msg.entities = vec![MessageEntity {
            entity_type: "mention".into(),
            offset: 3,
            length: 11,
        }];
        assert!(adapter.should_process_message(&msg, false));

        msg.text = Some("chompy status".into());
        msg.entities.clear();
        assert!(adapter.should_process_message(&msg, false));

        msg.message_thread_id = Some(31);
        assert!(!adapter.should_process_message(&msg, false));

        msg.message_thread_id = Some(9);
        msg.text = Some("hi @hermes_bot".into());
        msg.entities = vec![MessageEntity {
            entity_type: "mention".into(),
            offset: 3,
            length: 11,
        }];
        assert!(!adapter.should_process_message(&msg, false));

        msg.chat.id = -200;
        msg.message_thread_id = Some(8);
        assert!(adapter.should_process_message(&msg, false));

        msg.text = Some("chompy status".into());
        msg.entities.clear();
        assert!(!adapter.should_process_message(&msg, false));
    }

    #[test]
    fn telegram_text_batcher_aggregates_by_chat_user_and_thread() {
        let now = Instant::now();
        let mut batcher = TelegramTextBatcher::new(Duration::from_millis(50));
        let mut first = IncomingMessage {
            chat_id: 1,
            user_id: Some(2),
            username: None,
            text: Some("part one".into()),
            message_id: 10,
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
            message_thread_id: Some(8),
            chat_type: ChatKind::Private,
            is_group: false,
            callback_query_id: None,
            callback_data: None,
        };
        batcher.enqueue_at(first.clone(), now);
        first.text = Some("part two".into());
        first.message_id = 11;
        batcher.enqueue_at(first, now + Duration::from_millis(10));
        assert!(batcher
            .drain_ready_at(now + Duration::from_millis(40))
            .is_empty());
        let ready = batcher.drain_ready_at(now + Duration::from_millis(70));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].text.as_deref(), Some("part one\npart two"));
        assert_eq!(batcher.pending_len(), 0);
    }

    #[test]
    fn telegram_topic_binding_store_recovers_only_lobby_replies() {
        let mut store = TelegramTopicBindingStore::default();
        store.enable("208214988", "user1");
        store.bind("208214988", "111", "session-a", "user1", None, false);
        store.bind("208214988", "222", "session-b", "user1", None, false);

        assert_eq!(
            store.recover_thread_id("208214988", "user1", None),
            Some("222".into())
        );
        assert_eq!(
            store.recover_thread_id("208214988", "user1", Some("0")),
            Some("222".into())
        );
        assert_eq!(
            store.recover_thread_id("208214988", "user1", Some("9999")),
            None
        );
        assert_eq!(store.get_by_session("session-b").unwrap().thread_id, "222");
        assert_eq!(
            store
                .list_for_chat("208214988")
                .into_iter()
                .map(|binding| binding.thread_id)
                .collect::<Vec<_>>(),
            vec!["222", "111"]
        );
        store.remove_session("session-b");
        assert!(store.get("208214988", "222").is_none());
        store.disable("208214988", "user1");
        assert!(!store.is_enabled("208214988", "user1"));
    }

    #[tokio::test]
    async fn telegram_thread_fallback_retries_without_thread_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botfake_token_12345/sendMessage"))
            .and(body_partial_json(serde_json::json!({
                "message_thread_id": 999
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "description": "Bad Request: message thread not found"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/botfake_token_12345/sendMessage"))
            .and(JsonFieldAbsent("message_thread_id"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 77 }
            })))
            .mount(&server)
            .await;

        let mut adapter = test_adapter(test_config());
        adapter.api_base = format!("{}/botfake_token_12345", server.uri());

        let ids = adapter
            .send_text_with_keyboard(
                "123",
                "hello [world]_1",
                InlineKeyboardMarkup {
                    inline_keyboard: vec![vec![InlineKeyboardButton {
                        text: "Go".into(),
                        callback_data: Some("go".into()),
                        url: None,
                    }]],
                },
                Some("MarkdownV2"),
                Some(55),
                Some(999),
            )
            .await
            .unwrap();
        assert_eq!(ids, vec![77]);
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 2);
        let first: Value = requests[0].body_json().expect("json");
        assert_eq!(
            first.pointer("/message_thread_id").and_then(|v| v.as_i64()),
            Some(999)
        );
        let second: Value = requests[1].body_json().expect("json");
        assert!(second.get("message_thread_id").is_none());
        assert!(second.get("reply_to_message_id").is_none());
        assert_eq!(
            second
                .pointer("/reply_markup/inline_keyboard/0/0/callback_data")
                .and_then(|v| v.as_str()),
            Some("go")
        );
        assert_eq!(
            second.pointer("/parse_mode").and_then(|v| v.as_str()),
            Some("MarkdownV2")
        );
        assert!(second
            .pointer("/text")
            .and_then(|v| v.as_str())
            .unwrap()
            .contains("\\[world\\]\\_1"));
    }

    #[tokio::test]
    async fn telegram_encoded_gateway_chat_id_sends_to_topic_thread() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botfake_token_12345/sendMessage"))
            .and(body_partial_json(serde_json::json!({
                "chat_id": "-1001",
                "message_thread_id": 17585,
                "text": "topic hello"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": { "message_id": 88 }
            })))
            .mount(&server)
            .await;

        let mut adapter = test_adapter(test_config());
        adapter.api_base = format!("{}/botfake_token_12345", server.uri());

        let ids = adapter
            .send_text("-1001:17585", "topic hello", None, None)
            .await
            .unwrap();
        assert_eq!(ids, vec![88]);
        let requests = server.received_requests().await.expect("requests");
        assert_eq!(requests.len(), 1);
        let body: Value = requests[0].body_json().expect("json body");
        assert_eq!(
            body.pointer("/chat_id").and_then(|v| v.as_str()),
            Some("-1001")
        );
        assert_eq!(
            body.pointer("/message_thread_id").and_then(|v| v.as_i64()),
            Some(17585)
        );
    }

    #[test]
    fn telegram_media_method_matches_document_contracts() {
        assert_eq!(
            TelegramAdapter::media_method_for_extension("mp3"),
            ("sendAudio", "audio")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("wav"),
            ("sendDocument", "document")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("flac"),
            ("sendDocument", "document")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("mp4"),
            ("sendVideo", "video")
        );
    }

    #[test]
    fn telegram_approval_callback_parses_and_tracks_state() {
        assert_eq!(
            parse_approval_callback("approval:once:42"),
            Some((ApprovalChoice::Once, 42))
        );
        assert_eq!(
            parse_approval_callback("approval:session:42"),
            Some((ApprovalChoice::Session, 42))
        );
        assert_eq!(
            parse_approval_callback("approval:deny:42"),
            Some((ApprovalChoice::Deny, 42))
        );
        assert_eq!(parse_approval_callback("model:pick:gpt"), None);
        assert_eq!(truncate_chars("abcdef", 5), "ab...");
    }

    // -----------------------------------------------------------------------
    // parse_update tests (original, updated for new fields)
    // -----------------------------------------------------------------------

    fn make_chat(id: i64, chat_type: &str) -> Chat {
        Chat {
            id,
            chat_type: chat_type.into(),
            title: None,
            username: None,
        }
    }

    fn make_user(id: i64, username: Option<&str>) -> User {
        User {
            id,
            first_name: Some("Test".into()),
            username: username.map(|s| s.to_string()),
            is_bot: Some(false),
        }
    }

    fn make_text_message(msg_id: i64, chat: Chat, user: User, text: &str) -> TelegramMessage {
        TelegramMessage {
            message_id: msg_id,
            chat,
            from: Some(user),
            text: Some(text.into()),
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: None,
            message_thread_id: None,
            is_topic_message: None,
        }
    }

    #[test]
    fn parse_update_text_message() {
        let update = Update {
            update_id: 1,
            message: Some(make_text_message(
                42,
                make_chat(100, "private"),
                make_user(200, Some("testuser")),
                "hello bot",
            )),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(incoming.chat_id, 100);
        assert_eq!(incoming.user_id, Some(200));
        assert_eq!(incoming.text, Some("hello bot".into()));
        assert!(!incoming.is_voice);
        assert!(!incoming.is_photo);
        assert!(!incoming.is_sticker);
        assert!(!incoming.is_document);
        assert!(!incoming.is_group);
        assert_eq!(incoming.chat_type, ChatKind::Private);
        assert!(incoming.callback_query_id.is_none());
    }

    #[test]
    fn parse_update_voice_message() {
        let update = Update {
            update_id: 2,
            message: Some(TelegramMessage {
                message_id: 43,
                chat: make_chat(100, "private"),
                from: Some(make_user(200, None)),
                text: None,
                voice: Some(Voice {
                    file_id: "voice123".into(),
                    file_unique_id: "unique123".into(),
                    duration: 5,
                }),
                photo: None,
                caption: None,
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: None,
                document: None,
                reply_to_message: None,
                message_thread_id: None,
                is_topic_message: None,
            }),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_voice);
        assert_eq!(incoming.voice_file_id, Some("voice123".into()));
    }

    #[test]
    fn parse_update_photo_message() {
        let update = Update {
            update_id: 3,
            message: Some(TelegramMessage {
                message_id: 44,
                chat: make_chat(100, "group"),
                from: Some(make_user(200, None)),
                text: None,
                voice: None,
                photo: Some(vec![
                    PhotoSize {
                        file_id: "small".into(),
                        file_unique_id: "s1".into(),
                        width: 90,
                        height: 90,
                    },
                    PhotoSize {
                        file_id: "large".into(),
                        file_unique_id: "s2".into(),
                        width: 800,
                        height: 600,
                    },
                ]),
                caption: Some("my photo".into()),
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: None,
                document: None,
                reply_to_message: None,
                message_thread_id: None,
                is_topic_message: None,
            }),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_photo);
        assert_eq!(incoming.photo_file_id, Some("large".into()));
        assert_eq!(incoming.text, Some("my photo".into()));
        assert!(incoming.is_group);
        assert_eq!(incoming.chat_type, ChatKind::Group);
    }

    #[test]
    fn parse_update_no_message() {
        let update = Update {
            update_id: 4,
            message: None,
            callback_query: None,
        };
        assert!(TelegramAdapter::parse_update(&update).is_none());
    }

    // -----------------------------------------------------------------------
    // Sticker tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_update_sticker_message() {
        let update = Update {
            update_id: 10,
            message: Some(TelegramMessage {
                message_id: 100,
                chat: make_chat(300, "private"),
                from: Some(make_user(400, Some("stickeruser"))),
                text: None,
                voice: None,
                photo: None,
                caption: None,
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: Some(Sticker {
                    file_id: "sticker_abc".into(),
                    file_unique_id: "su_abc".into(),
                    width: Some(512),
                    height: Some(512),
                    is_animated: Some(false),
                    is_video: Some(false),
                    emoji: Some("😀".into()),
                    set_name: Some("TestPack".into()),
                }),
                document: None,
                reply_to_message: None,
                message_thread_id: None,
                is_topic_message: None,
            }),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_sticker);
        assert_eq!(incoming.sticker_file_id, Some("sticker_abc".into()));
        assert!(!incoming.is_voice);
        assert!(!incoming.is_photo);
        assert!(!incoming.is_document);
    }

    #[test]
    fn parse_update_document_message() {
        let update = Update {
            update_id: 11,
            message: Some(TelegramMessage {
                message_id: 101,
                chat: make_chat(301, "private"),
                from: Some(make_user(401, Some("docuser"))),
                text: None,
                voice: None,
                photo: None,
                caption: Some("document caption".into()),
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: None,
                document: Some(Document {
                    file_id: "doc_abc".into(),
                    file_unique_id: Some("du_abc".into()),
                    file_name: Some("notes.md".into()),
                    mime_type: Some("text/markdown".into()),
                    file_size: Some(2048),
                }),
                reply_to_message: None,
                message_thread_id: None,
                is_topic_message: None,
            }),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_document);
        assert_eq!(incoming.document_file_id, Some("doc_abc".into()));
        assert_eq!(incoming.document_file_name, Some("notes.md".into()));
        assert_eq!(incoming.document_mime_type, Some("text/markdown".into()));
        assert_eq!(incoming.document_file_size, Some(2048));
        assert_eq!(incoming.text, Some("document caption".into()));
    }

    // -----------------------------------------------------------------------
    // Callback query tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_update_callback_query() {
        let update = Update {
            update_id: 20,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_123".into(),
                from: make_user(500, Some("cbuser")),
                message: Some(TelegramMessage {
                    message_id: 200,
                    chat: make_chat(600, "private"),
                    from: None,
                    text: Some("Original message".into()),
                    voice: None,
                    photo: None,
                    caption: None,
                    entities: Vec::new(),
                    caption_entities: Vec::new(),
                    sticker: None,
                    document: None,
                    reply_to_message: None,
                    message_thread_id: None,
                    is_topic_message: None,
                }),
                data: Some("btn_action_1".into()),
                chat_instance: Some("inst".into()),
            }),
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(incoming.callback_query_id, Some("cq_123".into()));
        assert_eq!(incoming.callback_data, Some("btn_action_1".into()));
        assert_eq!(incoming.user_id, Some(500));
        assert_eq!(incoming.chat_id, 600);
        assert_eq!(incoming.message_id, 200);
        assert_eq!(incoming.text, Some("btn_action_1".into()));
    }

    #[test]
    fn parse_update_callback_query_no_message() {
        let update = Update {
            update_id: 21,
            message: None,
            callback_query: Some(CallbackQuery {
                id: "cq_456".into(),
                from: make_user(500, None),
                message: None,
                data: Some("data".into()),
                chat_instance: None,
            }),
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(incoming.callback_query_id, Some("cq_456".into()));
        assert_eq!(incoming.chat_id, 0);
        assert_eq!(incoming.message_id, 0);
    }

    // -----------------------------------------------------------------------
    // Reply-to and thread_id tests
    // -----------------------------------------------------------------------

    #[test]
    fn parse_update_with_reply_and_thread() {
        let reply_msg = TelegramMessage {
            message_id: 10,
            chat: make_chat(100, "supergroup"),
            from: Some(make_user(50, None)),
            text: Some("original".into()),
            voice: None,
            photo: None,
            caption: None,
            entities: Vec::new(),
            caption_entities: Vec::new(),
            sticker: None,
            document: None,
            reply_to_message: None,
            message_thread_id: None,
            is_topic_message: None,
        };

        let update = Update {
            update_id: 30,
            message: Some(TelegramMessage {
                message_id: 55,
                chat: make_chat(100, "supergroup"),
                from: Some(make_user(200, None)),
                text: Some("replying".into()),
                voice: None,
                photo: None,
                caption: None,
                entities: Vec::new(),
                caption_entities: Vec::new(),
                sticker: None,
                document: None,
                reply_to_message: Some(Box::new(reply_msg)),
                message_thread_id: Some(999),
                is_topic_message: None,
            }),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(incoming.reply_to_message_id, Some(10));
        assert_eq!(incoming.message_thread_id, Some(999));
        assert!(incoming.is_group);
        assert_eq!(incoming.chat_type, ChatKind::Supergroup);
    }

    // -----------------------------------------------------------------------
    // Group chat / ChatKind tests
    // -----------------------------------------------------------------------

    #[test]
    fn chat_kind_from_str_variants() {
        assert_eq!(ChatKind::from_str("private"), ChatKind::Private);
        assert_eq!(ChatKind::from_str("group"), ChatKind::Group);
        assert_eq!(ChatKind::from_str("supergroup"), ChatKind::Supergroup);
        assert_eq!(ChatKind::from_str("channel"), ChatKind::Channel);
        assert_eq!(
            ChatKind::from_str("something"),
            ChatKind::Unknown("something".into())
        );
    }

    #[test]
    fn chat_kind_is_group_like() {
        assert!(!ChatKind::Private.is_group_like());
        assert!(ChatKind::Group.is_group_like());
        assert!(ChatKind::Supergroup.is_group_like());
        assert!(!ChatKind::Channel.is_group_like());
        assert!(!ChatKind::Unknown("x".into()).is_group_like());
    }

    #[test]
    fn parse_group_message_is_group_flag() {
        let update = Update {
            update_id: 40,
            message: Some(make_text_message(
                60,
                make_chat(700, "supergroup"),
                make_user(800, Some("groupuser")),
                "hello group",
            )),
            callback_query: None,
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_group);
        assert_eq!(incoming.chat_type, ChatKind::Supergroup);
    }

    // -----------------------------------------------------------------------
    // Inline keyboard serialization tests
    // -----------------------------------------------------------------------

    #[test]
    fn inline_keyboard_serialization() {
        let kb = InlineKeyboardMarkup {
            inline_keyboard: vec![
                vec![
                    InlineKeyboardButton {
                        text: "Option A".into(),
                        callback_data: Some("a".into()),
                        url: None,
                    },
                    InlineKeyboardButton {
                        text: "Option B".into(),
                        callback_data: Some("b".into()),
                        url: None,
                    },
                ],
                vec![InlineKeyboardButton {
                    text: "Visit".into(),
                    callback_data: None,
                    url: Some("https://example.com".into()),
                }],
            ],
        };

        let json = serde_json::to_value(&kb).unwrap();
        let rows = json["inline_keyboard"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].as_array().unwrap().len(), 2);
        assert_eq!(rows[0][0]["text"], "Option A");
        assert_eq!(rows[0][0]["callback_data"], "a");
        assert!(rows[0][0].get("url").is_none());
        assert_eq!(rows[1][0]["url"], "https://example.com");
        assert!(rows[1][0].get("callback_data").is_none());
    }

    #[test]
    fn inline_keyboard_deserialization() {
        let json = r#"{
            "inline_keyboard": [
                [{"text": "Go", "callback_data": "go"}],
                [{"text": "Link", "url": "https://x.com"}]
            ]
        }"#;
        let kb: InlineKeyboardMarkup = serde_json::from_str(json).unwrap();
        assert_eq!(kb.inline_keyboard.len(), 2);
        assert_eq!(kb.inline_keyboard[0][0].text, "Go");
        assert_eq!(kb.inline_keyboard[0][0].callback_data, Some("go".into()));
        assert_eq!(kb.inline_keyboard[1][0].url, Some("https://x.com".into()));
    }

    // -----------------------------------------------------------------------
    // Media method routing tests
    // -----------------------------------------------------------------------

    #[test]
    fn media_method_for_known_extensions() {
        assert_eq!(
            TelegramAdapter::media_method_for_extension("jpg"),
            ("sendPhoto", "photo")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("png"),
            ("sendPhoto", "photo")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("gif"),
            ("sendAnimation", "animation")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("mp4"),
            ("sendVideo", "video")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("mp3"),
            ("sendAudio", "audio")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("ogg"),
            ("sendVoice", "voice")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("pdf"),
            ("sendDocument", "document")
        );
        assert_eq!(
            TelegramAdapter::media_method_for_extension("zip"),
            ("sendDocument", "document")
        );
    }

    // -----------------------------------------------------------------------
    // Sticker type serde tests
    // -----------------------------------------------------------------------

    #[test]
    fn sticker_deserialization() {
        let json = r#"{
            "file_id": "stk_1",
            "file_unique_id": "stk_u1",
            "width": 512,
            "height": 512,
            "is_animated": true,
            "emoji": "🔥",
            "set_name": "HotPack"
        }"#;
        let sticker: Sticker = serde_json::from_str(json).unwrap();
        assert_eq!(sticker.file_id, "stk_1");
        assert_eq!(sticker.emoji, Some("🔥".into()));
        assert_eq!(sticker.is_animated, Some(true));
        assert_eq!(sticker.is_video, None);
    }

    #[test]
    fn sticker_deserialization_minimal() {
        let json = r#"{"file_id": "s1", "file_unique_id": "su1"}"#;
        let sticker: Sticker = serde_json::from_str(json).unwrap();
        assert_eq!(sticker.file_id, "s1");
        assert!(sticker.width.is_none());
        assert!(sticker.emoji.is_none());
    }

    // -----------------------------------------------------------------------
    // CallbackQuery serde tests
    // -----------------------------------------------------------------------

    #[test]
    fn callback_query_deserialization() {
        let json = r#"{
            "id": "cq_999",
            "from": {"id": 123, "first_name": "Alice", "is_bot": false},
            "data": "pressed_ok",
            "chat_instance": "ci"
        }"#;
        let cq: CallbackQuery = serde_json::from_str(json).unwrap();
        assert_eq!(cq.id, "cq_999");
        assert_eq!(cq.from.id, 123);
        assert_eq!(cq.data, Some("pressed_ok".into()));
        assert!(cq.message.is_none());
    }

    // -----------------------------------------------------------------------
    // Update deserialization with callback_query
    // -----------------------------------------------------------------------

    #[test]
    fn update_with_callback_query_deser() {
        let json = r#"{
            "update_id": 50,
            "callback_query": {
                "id": "cq_1",
                "from": {"id": 1, "first_name": "Bob"},
                "message": {
                    "message_id": 77,
                    "chat": {"id": 88, "type": "private"},
                    "text": "Pick one"
                },
                "data": "choice_a"
            }
        }"#;
        let update: Update = serde_json::from_str(json).unwrap();
        assert!(update.message.is_none());
        let cq = update.callback_query.as_ref().unwrap();
        assert_eq!(cq.id, "cq_1");
        assert_eq!(cq.data, Some("choice_a".into()));
        assert_eq!(cq.message.as_ref().unwrap().message_id, 77);
    }

    // -----------------------------------------------------------------------
    // TelegramResponse with parameters (rate limiting)
    // -----------------------------------------------------------------------

    #[test]
    fn telegram_response_rate_limit_params() {
        let json = r#"{
            "ok": false,
            "description": "Too Many Requests: retry after 5",
            "parameters": {"retry_after": 5}
        }"#;
        let resp: TelegramResponse<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert!(!resp.ok);
        assert_eq!(resp.parameters.as_ref().unwrap().retry_after, Some(5));
    }

    #[test]
    fn telegram_response_no_params() {
        let json = r#"{"ok": true, "result": 42}"#;
        let resp: TelegramResponse<i32> = serde_json::from_str(json).unwrap();
        assert!(resp.ok);
        assert_eq!(resp.result, Some(42));
        assert!(resp.parameters.is_none());
    }

    // -----------------------------------------------------------------------
    // Chat deserialization with optional fields
    // -----------------------------------------------------------------------

    #[test]
    fn chat_with_title() {
        let json = r#"{"id": 1, "type": "supergroup", "title": "My Group", "username": "mygrp"}"#;
        let chat: Chat = serde_json::from_str(json).unwrap();
        assert_eq!(chat.id, 1);
        assert_eq!(chat.chat_type, "supergroup");
        assert_eq!(chat.title, Some("My Group".into()));
        assert_eq!(chat.username, Some("mygrp".into()));
    }

    // -----------------------------------------------------------------------
    // TelegramMessage with reply_to and sticker
    // -----------------------------------------------------------------------

    #[test]
    fn telegram_message_full_deser() {
        let json = r#"{
            "message_id": 10,
            "chat": {"id": 1, "type": "private"},
            "from": {"id": 2, "first_name": "X", "is_bot": false},
            "text": "hi",
            "message_thread_id": 42,
            "reply_to_message": {
                "message_id": 5,
                "chat": {"id": 1, "type": "private"},
                "text": "earlier"
            }
        }"#;
        let msg: TelegramMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.message_id, 10);
        assert_eq!(msg.message_thread_id, Some(42));
        let reply = msg.reply_to_message.as_ref().unwrap();
        assert_eq!(reply.message_id, 5);
        assert_eq!(reply.text, Some("earlier".into()));
    }

    // -----------------------------------------------------------------------
    // Backoff state tests
    // -----------------------------------------------------------------------

    #[test]
    fn backoff_doubling_capped() {
        let vals: Vec<u64> = {
            let mut v = Vec::new();
            let mut current = 0u64;
            for _ in 0..10 {
                current = if current == 0 {
                    INITIAL_BACKOFF_MS
                } else {
                    (current * 2).min(MAX_BACKOFF_MS)
                };
                v.push(current);
            }
            v
        };

        assert_eq!(vals[0], 1_000);
        assert_eq!(vals[1], 2_000);
        assert_eq!(vals[2], 4_000);
        assert_eq!(vals[3], 8_000);
        assert_eq!(vals[4], 16_000);
        assert_eq!(vals[5], 32_000);
        assert_eq!(vals[6], 60_000);
        assert_eq!(vals[7], 60_000);
    }

    #[test]
    fn supported_document_type_from_extension_or_mime() {
        let doc_from_name = Document {
            file_id: "d1".into(),
            file_unique_id: None,
            file_name: Some("report.PDF".into()),
            mime_type: None,
            file_size: Some(1024),
        };
        assert!(TelegramAdapter::is_supported_document(&doc_from_name));

        let doc_from_mime = Document {
            file_id: "d2".into(),
            file_unique_id: None,
            file_name: None,
            mime_type: Some("text/plain".into()),
            file_size: Some(256),
        };
        assert!(TelegramAdapter::is_supported_document(&doc_from_mime));

        let doc_unsupported = Document {
            file_id: "d3".into(),
            file_unique_id: None,
            file_name: Some("archive.rar".into()),
            mime_type: Some("application/x-rar-compressed".into()),
            file_size: Some(1024),
        };
        assert!(!TelegramAdapter::is_supported_document(&doc_unsupported));

        let zip_doc = Document {
            file_id: "d4".into(),
            file_unique_id: None,
            file_name: Some("archive.zip".into()),
            mime_type: Some("application/zip".into()),
            file_size: Some(1024),
        };
        assert!(TelegramAdapter::is_supported_document(&zip_doc));

        let png_doc_from_mime = Document {
            file_id: "d5".into(),
            file_unique_id: None,
            file_name: None,
            mime_type: Some("image/png".into()),
            file_size: Some(1024),
        };
        assert!(TelegramAdapter::is_supported_document(&png_doc_from_mime));
    }

    #[test]
    fn document_size_limit_check() {
        let small = Document {
            file_id: "d1".into(),
            file_unique_id: None,
            file_name: Some("ok.txt".into()),
            mime_type: Some("text/plain".into()),
            file_size: Some(1_024),
        };
        assert!(!TelegramAdapter::document_exceeds_size_limit(&small));

        let large = Document {
            file_id: "d2".into(),
            file_unique_id: None,
            file_name: Some("large.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size: Some(TELEGRAM_MAX_DOCUMENT_SIZE_BYTES + 1),
        };
        assert!(TelegramAdapter::document_exceeds_size_limit(&large));

        let unknown_size = Document {
            file_id: "d3".into(),
            file_unique_id: None,
            file_name: Some("unknown.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size: None,
        };
        assert!(TelegramAdapter::document_exceeds_size_limit(&unknown_size));
    }

    // -----------------------------------------------------------------------
    // Bot mention tests
    // -----------------------------------------------------------------------

    fn make_adapter_with_bot_username(username: Option<&str>) -> TelegramAdapter {
        let mut config = test_config();
        config.bot_username = username.map(|s| s.to_string());
        TelegramAdapter::new(config).unwrap()
    }

    #[test]
    fn is_mentioned_with_username() {
        let adapter = make_adapter_with_bot_username(Some("mybot"));
        assert!(adapter.is_mentioned_in("Hello @mybot how are you?"));
        assert!(!adapter.is_mentioned_in("Hello @otherbot"));
        assert!(!adapter.is_mentioned_in("Hello world"));
    }

    #[test]
    fn is_mentioned_without_username_passthrough() {
        let adapter = make_adapter_with_bot_username(None);
        assert!(adapter.is_mentioned_in("anything"));
        assert!(adapter.is_mentioned_in(""));
    }

    #[test]
    fn strip_mention_removes_at_mention() {
        let adapter = make_adapter_with_bot_username(Some("mybot"));
        assert_eq!(adapter.strip_mention("@mybot do something"), "do something");
        assert_eq!(
            adapter.strip_mention("hey @mybot please help"),
            "hey  please help"
        );
    }

    #[test]
    fn strip_mention_no_username_passthrough() {
        let adapter = make_adapter_with_bot_username(None);
        assert_eq!(adapter.strip_mention("hello world"), "hello world");
    }

    // -----------------------------------------------------------------------
    // User with is_bot field
    // -----------------------------------------------------------------------

    #[test]
    fn user_is_bot_field() {
        let json = r#"{"id": 1, "first_name": "BotX", "is_bot": true}"#;
        let user: User = serde_json::from_str(json).unwrap();
        assert_eq!(user.is_bot, Some(true));
    }

    // -----------------------------------------------------------------------
    // ChatMember deserialization
    // -----------------------------------------------------------------------

    #[test]
    fn chat_member_deser() {
        let json = r#"{
            "status": "administrator",
            "user": {"id": 1, "first_name": "Admin"}
        }"#;
        let member: ChatMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.status, "administrator");
        assert_eq!(member.user.id, 1);
    }

    // -----------------------------------------------------------------------
    // Config serde
    // -----------------------------------------------------------------------

    #[test]
    fn config_defaults() {
        let json = r#"{"token": "abc"}"#;
        let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.token, "abc");
        assert!(cfg.polling);
        assert!(!cfg.parse_markdown);
        assert!(!cfg.parse_html);
        assert_eq!(cfg.poll_timeout, DEFAULT_POLL_TIMEOUT);
        assert_eq!(cfg.reply_to_mode, "first");
        assert!(cfg.webhook_secret.is_none());
        assert!(!cfg.reactions);
        assert!(cfg.bot_username.is_none());
    }

    #[test]
    fn config_with_bot_username() {
        let json = r#"{"token": "abc", "bot_username": "mybot"}"#;
        let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.bot_username, Some("mybot".into()));
    }

    #[test]
    fn config_with_reply_mode_webhook_secret_and_reactions() {
        let json = r#"{
            "token": "abc",
            "webhook_secret": "secret",
            "reply_to_mode": "all",
            "reactions": true
        }"#;
        let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.webhook_secret.as_deref(), Some("secret"));
        assert_eq!(cfg.reply_to_mode, "all");
        assert!(cfg.reactions);
    }
}
