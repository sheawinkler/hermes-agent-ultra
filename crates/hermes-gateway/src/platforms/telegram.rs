//! Telegram Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Telegram using the Bot API.
//! Supports sending/editing messages, file operations, long polling for
//! receiving updates, voice/photo/video/sticker message handling with media
//! caching, inline keyboards, callback queries, rate limiting, exponential
//! backoff reconnection, and group chat support.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};

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
const SUPPORTED_DOCUMENT_EXTENSIONS: &[&str] = &["pdf", "md", "txt", "docx", "xlsx", "pptx"];

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
    pub sticker: Option<Sticker>,
    #[serde(default)]
    pub document: Option<Document>,
    #[serde(default)]
    pub reply_to_message: Option<Box<TelegramMessage>>,
    #[serde(default)]
    pub message_thread_id: Option<i64>,
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
}

impl TelegramAdapter {
    /// Create a new Telegram adapter with the given configuration.
    pub fn new(config: TelegramConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());

        base.validate_token()?;

        let client = base.build_client()?;
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
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &TelegramConfig {
        &self.config
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
        let chunks = split_message(text, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({
                "chat_id": chat_id,
                "text": chunk,
            });

            if let Some(pm) = parse_mode {
                body["parse_mode"] = serde_json::Value::String(pm.to_string());
            }

            if let Some(thread_id) = message_thread_id {
                body["message_thread_id"] = serde_json::Value::Number(thread_id.into());
            }

            // Only reply to the original message for the first chunk.
            if i == 0 {
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

            let url = format!("{}/sendMessage", self.api_base);
            let resp: TelegramResponse<SentMessage> = self.post_json(&url, &body).await?;

            if let Some(msg) = resp.result {
                message_ids.push(msg.message_id);
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
            "text": &text[..text.len().min(MAX_MESSAGE_LENGTH)],
        });

        if let Some(pm) = parse_mode {
            body["parse_mode"] = serde_json::Value::String(pm.to_string());
        }

        let url = format!("{}/editMessageText", self.api_base);
        let _resp: TelegramResponse<serde_json::Value> = self.post_json(&url, &body).await?;
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
        self.send_multipart(chat_id, file_path, caption, "sendDocument", "document")
            .await
    }

    /// Send a photo file.
    pub async fn send_photo(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        self.send_multipart(chat_id, file_path, caption, "sendPhoto", "photo")
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
        let url = format!("{}/{}", self.api_base, method);

        let file_bytes = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e))
        })?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part(field_name.to_string(), part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("{} failed: {}", method, e)))?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Rate limited on {}: {}",
                method, body_text
            )));
        }

        let result: TelegramResponse<SentMessage> = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse {} response: {}", method, e))
        })?;

        result.result.map(|m| m.message_id).ok_or_else(|| {
            GatewayError::SendFailed(
                result
                    .description
                    .unwrap_or_else(|| format!("{} failed", method)),
            )
        })
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
                warn!(
                    consecutive_errors = err_count,
                    backoff_ms = next,
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
            .unwrap_or(false)
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
            "mp3" | "flac" | "aac" | "m4a" | "wav" => ("sendAudio", "audio"),
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
        let mut body = serde_json::json!({
            "chat_id": chat_id,
            "photo": image_url,
        });
        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            let truncated: String = cap.chars().take(1024).collect();
            body["caption"] = serde_json::Value::String(truncated);
        }
        let url = format!("{}/sendPhoto", self.api_base);
        let _resp: TelegramResponse<SentMessage> = self.post_json(&url, &body).await?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

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
            sticker: None,
            document: None,
            reply_to_message: None,
            message_thread_id: None,
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
                sticker: None,
                document: None,
                reply_to_message: None,
                message_thread_id: None,
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
                sticker: None,
                document: None,
                reply_to_message: None,
                message_thread_id: None,
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
                    sticker: None,
                    document: None,
                    reply_to_message: None,
                    message_thread_id: None,
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
            sticker: None,
            document: None,
            reply_to_message: None,
            message_thread_id: None,
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
                sticker: None,
                document: None,
                reply_to_message: Some(Box::new(reply_msg)),
                message_thread_id: Some(999),
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
    }

    // -----------------------------------------------------------------------
    // Bot mention tests
    // -----------------------------------------------------------------------

    fn make_adapter_with_bot_username(username: Option<&str>) -> TelegramAdapter {
        let config = TelegramConfig {
            token: "fake_token_12345".into(),
            webhook_url: None,
            polling: true,
            proxy: AdapterProxyConfig::default(),
            parse_markdown: false,
            parse_html: false,
            poll_timeout: 30,
            bot_username: username.map(|s| s.to_string()),
        };
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
        assert!(cfg.bot_username.is_none());
    }

    #[test]
    fn config_with_bot_username() {
        let json = r#"{"token": "abc", "bot_username": "mybot"}"#;
        let cfg: TelegramConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.bot_username, Some("mybot".into()));
    }
}
