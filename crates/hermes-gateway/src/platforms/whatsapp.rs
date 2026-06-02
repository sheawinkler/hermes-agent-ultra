//! WhatsApp Business Cloud API adapter.
//!
//! Implements the `PlatformAdapter` trait for WhatsApp using the Cloud API.
//! Sends messages via `POST /v1/messages` and receives via webhook.

use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const WHATSAPP_API_BASE: &str = "https://graph.facebook.com/v18.0";
const MAX_WHATSAPP_MESSAGE_LENGTH: usize = 4096;

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// Parsed incoming WhatsApp message from a webhook notification.
#[derive(Debug, Clone)]
pub struct IncomingWhatsAppMessage {
    pub from: String,
    pub message_id: String,
    pub text: String,
    pub message_type: String,
    pub timestamp: String,
}

// ---------------------------------------------------------------------------
// WhatsAppConfig
// ---------------------------------------------------------------------------

/// Configuration for the WhatsApp adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppConfig {
    /// WhatsApp Business API access token.
    pub token: String,

    /// Phone number ID for sending messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone_number_id: Option<String>,

    /// WhatsApp Business Account ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business_account_id: Option<String>,

    /// Webhook verify token for incoming events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_token: Option<String>,

    /// Optional prefix prepended to outbound text replies. Empty string disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_prefix: Option<String>,

    /// Whether group chats must directly mention or command the bot.
    #[serde(default)]
    pub require_mention: bool,

    /// Additional regex wake patterns accepted in group chats.
    #[serde(default)]
    pub mention_patterns: Vec<String>,

    /// Group chats where free-response text is accepted without a mention.
    #[serde(default)]
    pub free_response_chats: Vec<String>,

    /// DM policy: open, disabled, or allowlist.
    #[serde(default = "default_whatsapp_open_policy")]
    pub dm_policy: String,

    /// Sender allowlist for DM allowlist mode.
    #[serde(default)]
    pub allow_from: Vec<String>,

    /// Group policy: open, disabled, or allowlist.
    #[serde(default = "default_whatsapp_open_policy")]
    pub group_policy: String,

    /// Group chat allowlist for group allowlist mode.
    #[serde(default)]
    pub group_allow_from: Vec<String>,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_whatsapp_open_policy() -> String {
    "open".to_string()
}

// ---------------------------------------------------------------------------
// WhatsAppAdapter
// ---------------------------------------------------------------------------

/// WhatsApp Business API platform adapter.
pub struct WhatsAppAdapter {
    base: BasePlatformAdapter,
    config: WhatsAppConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl WhatsAppAdapter {
    /// Create a new WhatsApp adapter with the given configuration.
    pub fn new(config: WhatsAppConfig) -> Result<Self, GatewayError> {
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

    pub fn config(&self) -> &WhatsAppConfig {
        &self.config
    }

    /// Convert common Markdown shapes to WhatsApp's lightweight formatting.
    pub fn format_message(content: &str) -> String {
        if content.is_empty() {
            return String::new();
        }

        let mut out = String::with_capacity(content.len());
        let mut in_code_block = false;
        for (idx, part) in content.split("```").enumerate() {
            if idx > 0 {
                out.push_str("```");
                in_code_block = !in_code_block;
            }
            if in_code_block {
                out.push_str(part);
            } else {
                out.push_str(&format_non_code_markdown(part));
            }
        }
        out
    }

    pub fn formatted_outbound_text(&self, content: &str) -> String {
        let prefixed = match self.config.reply_prefix.as_deref() {
            Some("") | None => content.to_string(),
            Some(prefix) if prefix.ends_with('\n') => format!("{prefix}{content}"),
            Some(prefix) => format!("{prefix}\n{content}"),
        };
        Self::format_message(&prefixed)
    }

    pub fn split_message_chunks(content: &str, max_len: usize) -> Vec<String> {
        if content.is_empty() {
            return Vec::new();
        }
        let max_len = max_len.max(1);
        let mut chunks = Vec::new();
        let mut current = String::new();
        for paragraph in content.split_inclusive('\n') {
            if current.chars().count() + paragraph.chars().count() <= max_len {
                current.push_str(paragraph);
                continue;
            }
            if !current.is_empty() {
                chunks.push(current.trim_end().to_string());
                current = String::new();
            }
            let mut segment = String::new();
            for ch in paragraph.chars() {
                if segment.chars().count() >= max_len {
                    chunks.push(segment);
                    segment = String::new();
                }
                segment.push(ch);
            }
            current.push_str(&segment);
        }
        if !current.trim().is_empty() {
            chunks.push(current.trim_end().to_string());
        }
        chunks
    }

    pub fn should_process_message(&self, data: &serde_json::Value) -> bool {
        let body = data.get("body").and_then(|v| v.as_str()).unwrap_or("");
        let chat_id = data
            .get("chatId")
            .or_else(|| data.get("chat_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sender_id = data
            .get("senderId")
            .or_else(|| data.get("from"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let is_group = data
            .get("isGroup")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| chat_id.ends_with("@g.us"));

        if !is_group {
            return self.dm_allows(sender_id);
        }
        if !self.group_allows(chat_id) {
            return false;
        }
        if !self.config.require_mention || self.free_response_allows(chat_id) {
            return true;
        }
        if body.trim_start().starts_with('/') {
            return true;
        }
        if self.custom_mention_pattern_matches(body) {
            return true;
        }
        self.bot_was_mentioned_or_quoted(data)
    }

    pub fn clean_bot_mention_text(&self, text: &str, data: &serde_json::Value) -> String {
        let mut cleaned = text.to_string();
        for bot_id in string_array(data.get("botIds").or_else(|| data.get("bot_ids"))) {
            let phone = bot_id
                .split('@')
                .next()
                .unwrap_or(bot_id.as_str())
                .trim_start_matches('+');
            cleaned = cleaned.replace(&format!("@{phone}"), "");
            cleaned = cleaned.replace(&bot_id, "");
        }
        cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Send a text message via WhatsApp Cloud API.
    pub async fn send_text(&self, to: &str, text: &str) -> Result<(), GatewayError> {
        let phone_id = self
            .config
            .phone_number_id
            .as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "text",
            "text": { "body": text }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp API error: {}",
                text
            )));
        }
        Ok(())
    }

    fn dm_allows(&self, sender_id: &str) -> bool {
        match self.config.dm_policy.trim().to_ascii_lowercase().as_str() {
            "disabled" => false,
            "allowlist" => contains_normalized_whatsapp_id(&self.config.allow_from, sender_id),
            _ => true,
        }
    }

    fn group_allows(&self, chat_id: &str) -> bool {
        match self
            .config
            .group_policy
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "disabled" => false,
            "allowlist" => contains_normalized_whatsapp_id(&self.config.group_allow_from, chat_id),
            _ => true,
        }
    }

    fn free_response_allows(&self, chat_id: &str) -> bool {
        contains_normalized_whatsapp_id(&self.config.free_response_chats, chat_id)
    }

    fn custom_mention_pattern_matches(&self, body: &str) -> bool {
        self.config.mention_patterns.iter().any(|pattern| {
            Regex::new(pattern)
                .map(|regex| regex.is_match(body))
                .unwrap_or(false)
        })
    }

    fn bot_was_mentioned_or_quoted(&self, data: &serde_json::Value) -> bool {
        let mentioned = string_array(
            data.get("mentionedIds")
                .or_else(|| data.get("mentioned_ids")),
        );
        let bots = string_array(data.get("botIds").or_else(|| data.get("bot_ids")));
        let quoted = data
            .get("quotedParticipant")
            .or_else(|| data.get("quoted_participant"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        bots.iter().any(|bot| {
            mentioned.iter().any(|id| id == bot)
                || (!quoted.trim().is_empty() && quoted.trim() == bot.trim())
        })
    }

    /// Verify a WhatsApp webhook subscription challenge.
    ///
    /// Returns `Some(challenge)` if `mode` is `"subscribe"` and `token` matches
    /// the configured `verify_token`; otherwise returns `None`.
    pub fn verify_webhook(
        mode: &str,
        token: &str,
        challenge: &str,
        verify_token: &str,
    ) -> Option<String> {
        if mode == "subscribe" && token == verify_token {
            Some(challenge.to_string())
        } else {
            None
        }
    }

    /// Parse incoming messages from a WhatsApp webhook notification body.
    ///
    /// Walks through `entry[].changes[].value.messages[]` and extracts text
    /// messages (other types are recorded with an empty `text` field).
    pub fn parse_webhook_event(body: &serde_json::Value) -> Vec<IncomingWhatsAppMessage> {
        let mut messages = Vec::new();

        let entries = match body.get("entry").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => return messages,
        };

        for entry in entries {
            let changes = match entry.get("changes").and_then(|v| v.as_array()) {
                Some(arr) => arr,
                None => continue,
            };
            for change in changes {
                let value = match change.get("value") {
                    Some(v) => v,
                    None => continue,
                };
                let msgs = match value.get("messages").and_then(|v| v.as_array()) {
                    Some(arr) => arr,
                    None => continue,
                };
                for msg in msgs {
                    let from = msg
                        .get("from")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let message_id = msg
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let message_type = msg
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("text")
                        .to_string();
                    let timestamp = msg
                        .get("timestamp")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    let text = msg
                        .get("text")
                        .and_then(|t| t.get("body"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();

                    messages.push(IncomingWhatsAppMessage {
                        from,
                        message_id,
                        text,
                        message_type,
                        timestamp,
                    });
                }
            }
        }

        messages
    }

    /// Mark a message as read via the WhatsApp Cloud API.
    pub async fn mark_as_read(&self, message_id: &str) -> Result<(), GatewayError> {
        let phone_id = self
            .config
            .phone_number_id
            .as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "status": "read",
            "message_id": message_id
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WhatsApp mark_as_read failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp mark_as_read error: {}",
                text
            )));
        }
        Ok(())
    }

    /// Send a reaction emoji to a specific message.
    pub async fn send_reaction(
        &self,
        to: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let phone_id = self
            .config
            .phone_number_id
            .as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "reaction",
            "reaction": {
                "message_id": message_id,
                "emoji": emoji
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WhatsApp reaction send failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp reaction error: {}",
                text
            )));
        }
        Ok(())
    }

    /// Send a media message (image/document) via WhatsApp Cloud API.
    pub async fn send_media(
        &self,
        to: &str,
        media_type: &str,
        media_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let phone_id = self
            .config
            .phone_number_id
            .as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let body = build_link_media_body(to, media_type, media_url, caption);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp media send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp API error: {}",
                text
            )));
        }
        Ok(())
    }
}

fn build_link_media_body(
    to: &str,
    media_type: &str,
    media_url: &str,
    caption: Option<&str>,
) -> serde_json::Value {
    let mut media_obj = serde_json::json!({ "link": media_url });
    if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
        media_obj["caption"] = serde_json::Value::String(cap.to_string());
    }

    serde_json::json!({
        "messaging_product": "whatsapp",
        "to": to,
        "type": media_type,
        media_type: media_obj
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> WhatsAppConfig {
        WhatsAppConfig {
            token: "token".to_string(),
            phone_number_id: Some("phone-1".to_string()),
            business_account_id: None,
            verify_token: None,
            reply_prefix: None,
            require_mention: false,
            mention_patterns: Vec::new(),
            free_response_chats: Vec::new(),
            dm_policy: "open".to_string(),
            allow_from: Vec::new(),
            group_policy: "open".to_string(),
            group_allow_from: Vec::new(),
            proxy: AdapterProxyConfig::default(),
        }
    }

    #[test]
    fn build_link_media_body_with_caption() {
        let body = build_link_media_body(
            "15551234567",
            "image",
            "https://example.com/preview.png",
            Some("Status update"),
        );

        assert_eq!(body["messaging_product"], "whatsapp");
        assert_eq!(body["to"], "15551234567");
        assert_eq!(body["type"], "image");
        assert_eq!(body["image"]["link"], "https://example.com/preview.png");
        assert_eq!(body["image"]["caption"], "Status update");
    }

    #[test]
    fn build_link_media_body_omits_blank_caption() {
        let body = build_link_media_body(
            "15551234567",
            "image",
            "https://example.com/preview.png",
            Some("   "),
        );

        assert_eq!(body["type"], "image");
        assert_eq!(body["image"]["link"], "https://example.com/preview.png");
        assert!(body["image"]["caption"].is_null());
    }

    #[test]
    fn format_message_converts_markdown_without_touching_code() {
        let text =
            "## Title\nbefore **bold** and ~~gone~~ [docs](https://example.com) `**raw**`\n```python\n**raw**\n```";
        let formatted = WhatsAppAdapter::format_message(text);

        assert!(formatted.starts_with("*Title*"));
        assert!(formatted.contains("*bold*"));
        assert!(formatted.contains("~gone~"));
        assert!(formatted.contains("docs (https://example.com)"));
        assert!(formatted.contains("`**raw**`"));
        assert!(formatted.contains("```python\n**raw**\n```"));
    }

    #[test]
    fn formatted_outbound_text_applies_reply_prefix() {
        let cfg = WhatsAppConfig {
            reply_prefix: Some("Hermes Bot".to_string()),
            ..config()
        };
        let adapter = WhatsAppAdapter::new(cfg).expect("adapter");

        assert_eq!(
            adapter.formatted_outbound_text("**hello**"),
            "Hermes Bot\n*hello*"
        );
    }

    #[test]
    fn split_message_chunks_respects_char_boundary_limit() {
        let chunks = WhatsAppAdapter::split_message_chunks("ååååå", 2);
        assert_eq!(chunks, vec!["åå", "åå", "å"]);
    }

    #[test]
    fn should_process_message_applies_dm_and_group_policies() {
        let adapter = WhatsAppAdapter::new(WhatsAppConfig {
            require_mention: true,
            dm_policy: "allowlist".to_string(),
            allow_from: vec!["6281234567890".to_string()],
            group_policy: "allowlist".to_string(),
            group_allow_from: vec!["120363001234567890@g.us".to_string()],
            mention_patterns: vec![r"^\s*chompy\b".to_string()],
            ..config()
        })
        .expect("adapter");

        assert!(adapter.should_process_message(&serde_json::json!({
            "isGroup": false,
            "senderId": "6281234567890@s.whatsapp.net",
            "body": "hello"
        })));
        assert!(!adapter.should_process_message(&serde_json::json!({
            "isGroup": false,
            "senderId": "6289999999999@s.whatsapp.net",
            "body": "hello"
        })));
        assert!(adapter.should_process_message(&serde_json::json!({
            "isGroup": true,
            "chatId": "120363001234567890@g.us",
            "body": "chompy status",
            "mentionedIds": [],
            "botIds": ["15551230000@s.whatsapp.net"]
        })));
        assert!(!adapter.should_process_message(&serde_json::json!({
            "isGroup": true,
            "chatId": "999999@g.us",
            "body": "chompy status",
            "mentionedIds": [],
            "botIds": ["15551230000@s.whatsapp.net"]
        })));
    }

    #[test]
    fn clean_bot_mention_text_removes_configured_bot_phone() {
        let adapter = WhatsAppAdapter::new(config()).expect("adapter");
        let cleaned = adapter.clean_bot_mention_text(
            "@15551230000 what is the weather?",
            &serde_json::json!({"botIds": ["15551230000@s.whatsapp.net"]}),
        );
        assert_eq!(cleaned, "what is the weather?");
    }
}

#[async_trait]
impl PlatformAdapter for WhatsAppAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("WhatsApp adapter starting");
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("WhatsApp adapter stopping");
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
        let formatted = self.formatted_outbound_text(text);
        for chunk in Self::split_message_chunks(&formatted, MAX_WHATSAPP_MESSAGE_LENGTH) {
            self.send_text(chat_id, &chunk).await?;
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        // WhatsApp does not natively support message editing
        debug!("WhatsApp does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::{media_category, mime_from_extension};

        let phone_id = self
            .config
            .phone_number_id
            .as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");

        // Step 1: Upload media to WhatsApp Cloud API
        let upload_url = format!("{}/{}/media", WHATSAPP_API_BASE, phone_id);
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .text("messaging_product", "whatsapp")
            .part("file", part);

        let resp = self
            .client
            .post(&upload_url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp media upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp media upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp upload parse failed: {e}")))?;
        let media_id = result.get("id").and_then(|v| v.as_str()).unwrap_or("");

        // Step 2: Send message with uploaded media ID
        let media_type = match media_category(ext) {
            "image" => "image",
            "video" => "video",
            "audio" => "audio",
            _ => "document",
        };

        let send_url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let mut media_obj = serde_json::json!({ "id": media_id });
        if let Some(cap) = caption {
            media_obj["caption"] = serde_json::Value::String(cap.to_string());
        }
        if media_type == "document" {
            media_obj["filename"] = serde_json::Value::String(file_name.to_string());
        }

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": chat_id,
            "type": media_type,
            media_type: media_obj
        });

        let resp = self
            .client
            .post(&send_url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp media send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WhatsApp media send error: {text}"
            )));
        }
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_media(chat_id, "image", image_url, caption).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "whatsapp"
    }
}

fn format_non_code_markdown(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut inline_code = false;
    for (idx, segment) in content.split('`').enumerate() {
        if idx > 0 {
            out.push('`');
            inline_code = !inline_code;
        }
        if inline_code {
            out.push_str(segment);
        } else {
            out.push_str(&format_markdown_segment(segment));
        }
    }
    out
}

fn format_markdown_segment(segment: &str) -> String {
    let header = Regex::new(r"(?m)^(#{1,6})\s+(.+)$").expect("valid header regex");
    let links = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid link regex");
    let bold_star = Regex::new(r"\*\*([^*\n][^*]*?)\*\*").expect("valid bold regex");
    let bold_under = Regex::new(r"__([^_\n][^_]*?)__").expect("valid bold regex");
    let strike = Regex::new(r"~~([^~\n][^~]*?)~~").expect("valid strike regex");

    let segment = header.replace_all(segment, "*$2*");
    let segment = links.replace_all(&segment, "$1 ($2)");
    let segment = bold_star.replace_all(&segment, "*$1*");
    let segment = bold_under.replace_all(&segment, "*$1*");
    strike.replace_all(&segment, "~$1~").into_owned()
}

fn string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn normalize_whatsapp_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('+')
        .trim_end_matches("@s.whatsapp.net")
        .trim_end_matches("@lid")
        .to_ascii_lowercase()
}

fn contains_normalized_whatsapp_id(list: &[String], candidate: &str) -> bool {
    let candidate = normalize_whatsapp_id(candidate);
    list.iter()
        .map(|entry| normalize_whatsapp_id(entry))
        .any(|entry| entry == candidate)
}
