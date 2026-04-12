//! Telegram Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Telegram using the Bot API.
//! Supports sending/editing messages, file operations, long polling for
//! receiving updates, and voice/photo message handling with media caching.

use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

/// Maximum message length for Telegram (4096 characters).
const MAX_MESSAGE_LENGTH: usize = 4096;

/// Default long-polling timeout in seconds.
const DEFAULT_POLL_TIMEOUT: u64 = 30;

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
}

/// Telegram Update object.
#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub update_id: i64,
    #[serde(default)]
    pub message: Option<TelegramMessage>,
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
}

/// Telegram Chat object.
#[derive(Debug, Clone, Deserialize)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
}

/// Telegram User object.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
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
    pub voice_file_id: Option<String>,
    pub photo_file_id: Option<String>,
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
}

impl TelegramAdapter {
    /// Create a new Telegram adapter with the given configuration.
    pub fn new(config: TelegramConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token)
            .with_proxy(config.proxy.clone());

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

            // Only reply to the original message for the first chunk.
            if i == 0 {
                if let Some(reply_id) = reply_to_message_id {
                    body["reply_to_message_id"] = serde_json::Value::Number(reply_id.into());
                }
            }

            let url = format!("{}/sendMessage", self.api_base);
            let resp: TelegramResponse<SentMessage> = self
                .post_json(&url, &body)
                .await?;

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
        let url = format!("{}/sendDocument", self.api_base);
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e)))?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self.client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("sendDocument failed: {}", e)))?;

        let result: TelegramResponse<SentMessage> = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to parse sendDocument response: {}", e)))?;

        result.result
            .map(|m| m.message_id)
            .ok_or_else(|| GatewayError::SendFailed(
                result.description.unwrap_or_else(|| "sendDocument failed".into())
            ))
    }

    /// Send a photo file.
    pub async fn send_photo(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<i64, GatewayError> {
        let url = format!("{}/sendPhoto", self.api_base);
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e)))?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("photo.jpg")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("photo", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self.client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("sendPhoto failed: {}", e)))?;

        let result: TelegramResponse<SentMessage> = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to parse sendPhoto response: {}", e)))?;

        result.result
            .map(|m| m.message_id)
            .ok_or_else(|| GatewayError::SendFailed(
                result.description.unwrap_or_else(|| "sendPhoto failed".into())
            ))
    }

    // -----------------------------------------------------------------------
    // Receiving messages (long polling)
    // -----------------------------------------------------------------------

    /// Fetch updates from Telegram using long polling.
    pub async fn get_updates(&self) -> Result<Vec<Update>, GatewayError> {
        let offset = self.poll_offset.load(std::sync::atomic::Ordering::SeqCst);
        let url = format!("{}/getUpdates", self.api_base);

        let body = serde_json::json!({
            "offset": offset,
            "timeout": self.config.poll_timeout,
            "allowed_updates": ["message"],
        });

        let resp: TelegramResponse<Vec<Update>> = self.post_json(&url, &body).await?;

        if let Some(updates) = resp.result {
            // Advance offset past the last update
            if let Some(last) = updates.last() {
                self.poll_offset.store(
                    last.update_id + 1,
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
            Ok(updates)
        } else {
            Ok(Vec::new())
        }
    }

    /// Parse a Telegram Update into an IncomingMessage.
    pub fn parse_update(update: &Update) -> Option<IncomingMessage> {
        let msg = update.message.as_ref()?;

        let text = msg.text.clone().or_else(|| msg.caption.clone());
        let user_id = msg.from.as_ref().map(|u| u.id);
        let username = msg.from.as_ref().and_then(|u| u.username.clone());

        let is_voice = msg.voice.is_some();
        let voice_file_id = msg.voice.as_ref().map(|v| v.file_id.clone());

        let is_photo = msg.photo.is_some();
        let photo_file_id = msg.photo.as_ref().and_then(|photos| {
            // Pick the largest photo (last in the array)
            photos.last().map(|p| p.file_id.clone())
        });

        Some(IncomingMessage {
            chat_id: msg.chat.id,
            user_id,
            username,
            text,
            message_id: msg.message_id,
            is_voice,
            is_photo,
            voice_file_id,
            photo_file_id,
        })
    }

    /// Download a file from Telegram by file_id.
    /// Returns the URL from which the file can be downloaded.
    pub async fn get_file_url(&self, file_id: &str) -> Result<String, GatewayError> {
        let url = format!("{}/getFile", self.api_base);
        let body = serde_json::json!({ "file_id": file_id });

        let resp: TelegramResponse<TelegramFile> = self.post_json(&url, &body).await?;

        let file = resp.result.ok_or_else(|| {
            GatewayError::ConnectionFailed(
                resp.description.unwrap_or_else(|| "getFile failed".into())
            )
        })?;

        let file_path = file.file_path.ok_or_else(|| {
            GatewayError::ConnectionFailed("File path not available".into())
        })?;

        Ok(format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.config.token, file_path
        ))
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// POST JSON to a Telegram API endpoint and deserialize the response.
    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<TelegramResponse<T>, GatewayError> {
        let resp = self.client
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Telegram API request failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Telegram API returned HTTP {}: {}", status, text
            )));
        }

        resp.json::<TelegramResponse<T>>()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!(
                "Failed to parse Telegram API response: {}", e
            )))
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
}

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Telegram adapter starting (token: {}...)",
            &self.config.token[..8.min(self.config.token.len())]
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
        // Detect image extensions to use sendPhoto, otherwise sendDocument.
        let ext = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" => {
                self.send_photo(chat_id, file_path, caption).await?;
            }
            _ => {
                self.send_document(chat_id, file_path, caption).await?;
            }
        }
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
    fn parse_update_text_message() {
        let update = Update {
            update_id: 1,
            message: Some(TelegramMessage {
                message_id: 42,
                chat: Chat { id: 100, chat_type: "private".into() },
                from: Some(User { id: 200, first_name: Some("Test".into()), username: Some("testuser".into()) }),
                text: Some("hello bot".into()),
                voice: None,
                photo: None,
                caption: None,
            }),
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert_eq!(incoming.chat_id, 100);
        assert_eq!(incoming.user_id, Some(200));
        assert_eq!(incoming.text, Some("hello bot".into()));
        assert!(!incoming.is_voice);
        assert!(!incoming.is_photo);
    }

    #[test]
    fn parse_update_voice_message() {
        let update = Update {
            update_id: 2,
            message: Some(TelegramMessage {
                message_id: 43,
                chat: Chat { id: 100, chat_type: "private".into() },
                from: Some(User { id: 200, first_name: None, username: None }),
                text: None,
                voice: Some(Voice {
                    file_id: "voice123".into(),
                    file_unique_id: "unique123".into(),
                    duration: 5,
                }),
                photo: None,
                caption: None,
            }),
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
                chat: Chat { id: 100, chat_type: "group".into() },
                from: Some(User { id: 200, first_name: None, username: None }),
                text: None,
                voice: None,
                photo: Some(vec![
                    PhotoSize { file_id: "small".into(), file_unique_id: "s1".into(), width: 90, height: 90 },
                    PhotoSize { file_id: "large".into(), file_unique_id: "s2".into(), width: 800, height: 600 },
                ]),
                caption: Some("my photo".into()),
            }),
        };

        let incoming = TelegramAdapter::parse_update(&update).unwrap();
        assert!(incoming.is_photo);
        // Should pick the largest (last) photo
        assert_eq!(incoming.photo_file_id, Some("large".into()));
        assert_eq!(incoming.text, Some("my photo".into()));
    }

    #[test]
    fn parse_update_no_message() {
        let update = Update {
            update_id: 4,
            message: None,
        };
        assert!(TelegramAdapter::parse_update(&update).is_none());
    }
}
