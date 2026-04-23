//! Feishu (Lark) Bot API adapter.
//!
//! Supports:
//! - Tenant access token with automatic expiry-based refresh
//! - Text, rich text (post), interactive card, image, audio, and file messages
//! - Message editing, replying, and group chat metadata
//! - Event subscription verification and incoming message parsing
//! - Mention detection and stripping

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";

/// Feishu tokens expire in 2 hours; refresh 5 minutes early.
const TOKEN_EXPIRY_SECS: u64 = 2 * 60 * 60;
const TOKEN_REFRESH_MARGIN_SECS: u64 = 5 * 60;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypt_key: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// Event subscription types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuEvent {
    pub schema: Option<String>,
    pub header: Option<FeishuEventHeader>,
    pub event: Option<serde_json::Value>,
    /// Present in URL verification callbacks.
    pub challenge: Option<String>,
    /// Some older event formats include a top-level token.
    pub token: Option<String>,
    #[serde(rename = "type")]
    pub event_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuEventHeader {
    pub event_id: String,
    pub event_type: String,
    #[serde(default)]
    pub create_time: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub tenant_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct IncomingFeishuMessage {
    pub message_id: String,
    pub chat_id: String,
    pub chat_type: String,
    pub sender_id: Option<String>,
    pub text: String,
    pub message_type: String,
    pub is_mention: bool,
}

// ---------------------------------------------------------------------------
// Rich text (post) types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum PostElement {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        un_escape: Option<bool>,
    },
    #[serde(rename = "a")]
    Link { text: String, href: String },
    #[serde(rename = "at")]
    At {
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_name: Option<String>,
    },
    #[serde(rename = "img")]
    Image {
        image_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
    },
}

// ---------------------------------------------------------------------------
// Interactive card types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuCard {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<CardHeader>,
    pub elements: Vec<CardElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardHeader {
    pub title: TextContent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContent {
    pub tag: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum CardElement {
    #[serde(rename = "div")]
    Div { text: TextContent },
    #[serde(rename = "action")]
    Action { actions: Vec<CardAction> },
    #[serde(rename = "markdown")]
    Markdown { content: String },
    #[serde(rename = "hr")]
    Hr {},
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "tag")]
pub enum CardAction {
    #[serde(rename = "button")]
    Button {
        text: TextContent,
        #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
        action_type: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Chat info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatInfo {
    pub chat_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub chat_type: Option<String>,
    pub owner_id: Option<String>,
    pub member_count: Option<u32>,
}

// ---------------------------------------------------------------------------
// Token cache with expiry tracking
// ---------------------------------------------------------------------------

struct CachedToken {
    value: String,
    obtained_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        self.obtained_at.elapsed()
            > Duration::from_secs(TOKEN_EXPIRY_SECS - TOKEN_REFRESH_MARGIN_SECS)
    }
}

// ---------------------------------------------------------------------------
// Helper: build Authorization header value
// ---------------------------------------------------------------------------

fn bearer(token: &str) -> String {
    format!("Bearer {}", token)
}

/// Strip `@_user_xxx` mention patterns from text and collapse whitespace.
fn strip_mentions(text: &str) -> String {
    let re = Regex::new(r"@_user_\d+").expect("valid regex");
    let cleaned = re.replace_all(text, " ");
    let multi = Regex::new(r"[ \t]{2,}").expect("valid regex");
    multi.replace_all(cleaned.as_ref(), " ").trim().to_string()
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

pub struct FeishuAdapter {
    base: BasePlatformAdapter,
    config: FeishuConfig,
    client: Client,
    tenant_token: RwLock<Option<CachedToken>>,
    stop_signal: Arc<Notify>,
}

impl FeishuAdapter {
    pub fn new(config: FeishuConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.app_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            tenant_token: RwLock::new(None),
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &FeishuConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Token management (with expiry tracking)
    // -----------------------------------------------------------------------

    /// Get a valid tenant access token, refreshing automatically when expired.
    pub async fn get_tenant_token(&self) -> Result<String, GatewayError> {
        {
            let guard = self.tenant_token.read().await;
            if let Some(ref cached) = *guard {
                if !cached.is_expired() {
                    return Ok(cached.value.clone());
                }
                debug!("Feishu tenant token expired, refreshing");
            }
        }

        let url = format!("{}/auth/v3/tenant_access_token/internal", FEISHU_API_BASE);
        let body = serde_json::json!({
            "app_id": self.config.app_id,
            "app_secret": self.config.app_secret
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Auth(format!("Feishu auth failed: {e}")))?;

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::Auth(format!("Feishu auth parse failed: {e}")))?;

        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
        if code != 0 {
            let msg = result
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            return Err(GatewayError::Auth(format!(
                "Feishu token request failed (code={code}): {msg}"
            )));
        }

        let token = result
            .get("tenant_access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::Auth("No tenant_access_token in response".into()))?
            .to_string();

        info!("Feishu tenant token refreshed successfully");
        *self.tenant_token.write().await = Some(CachedToken {
            value: token.clone(),
            obtained_at: Instant::now(),
        });
        Ok(token)
    }

    /// Force-invalidate the cached token so the next call fetches a new one.
    pub async fn invalidate_token(&self) {
        *self.tenant_token.write().await = None;
    }

    // -----------------------------------------------------------------------
    // Event verification
    // -----------------------------------------------------------------------

    /// Verify an incoming event's token matches the configured verification token.
    ///
    /// For v2 schema events the token lives in `header.token`; for v1 / URL
    /// verification callbacks it may be a top-level `token` field.
    pub fn verify_event(event: &FeishuEvent, verification_token: &str) -> bool {
        let token_from_header = event.header.as_ref().and_then(|h| h.token.as_deref());
        let token_from_top = event.token.as_deref();

        let actual = token_from_header.or(token_from_top);
        match actual {
            Some(t) => t == verification_token,
            None => false,
        }
    }

    // -----------------------------------------------------------------------
    // Message event parsing
    // -----------------------------------------------------------------------

    /// Parse a `im.message.receive_v1` event payload into an `IncomingFeishuMessage`.
    pub fn parse_message_event(event: &serde_json::Value) -> Option<IncomingFeishuMessage> {
        let message = event.get("message")?;
        let message_id = message
            .get("message_id")
            .and_then(|v| v.as_str())?
            .to_string();
        let chat_id = message.get("chat_id").and_then(|v| v.as_str())?.to_string();
        let chat_type = message
            .get("chat_type")
            .and_then(|v| v.as_str())
            .unwrap_or("p2p")
            .to_string();
        let message_type = message
            .get("message_type")
            .and_then(|v| v.as_str())
            .unwrap_or("text")
            .to_string();

        let sender_id = event
            .get("sender")
            .and_then(|s| s.get("sender_id"))
            .and_then(|sid| sid.get("open_id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let mentions = message.get("mentions").and_then(|v| v.as_array());
        let is_mention = mentions.map(|arr| !arr.is_empty()).unwrap_or(false);

        let raw_text = Self::extract_text_content(message, &message_type);
        let text = strip_mentions(&raw_text);

        Some(IncomingFeishuMessage {
            message_id,
            chat_id,
            chat_type,
            sender_id,
            text,
            message_type,
            is_mention,
        })
    }

    /// Extract text from the nested `content` JSON string within a message object.
    fn extract_text_content(message: &serde_json::Value, msg_type: &str) -> String {
        let content_str = match message.get("content").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => return String::new(),
        };

        let content: serde_json::Value = match serde_json::from_str(content_str) {
            Ok(v) => v,
            Err(_) => return content_str.to_string(),
        };

        match msg_type {
            "text" => content
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            "post" => Self::flatten_post_content(&content),
            _ => content_str.to_string(),
        }
    }

    /// Flatten a post-type content structure into plain text.
    fn flatten_post_content(content: &serde_json::Value) -> String {
        let mut parts = Vec::new();

        if let Some(title) = content.get("title").and_then(|v| v.as_str()) {
            if !title.is_empty() {
                parts.push(title.to_string());
            }
        }

        let body = content.get("content").and_then(|v| v.as_array());

        if let Some(lines) = body {
            for line in lines {
                if let Some(elements) = line.as_array() {
                    let line_text: String = elements
                        .iter()
                        .filter_map(|el| {
                            let tag = el.get("tag").and_then(|v| v.as_str())?;
                            match tag {
                                "text" => el.get("text").and_then(|v| v.as_str()).map(String::from),
                                "a" => el.get("text").and_then(|v| v.as_str()).map(String::from),
                                "at" => el
                                    .get("user_name")
                                    .and_then(|v| v.as_str())
                                    .map(|n| format!("@{n}")),
                                _ => None,
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    if !line_text.is_empty() {
                        parts.push(line_text);
                    }
                }
            }
        }

        parts.join("\n")
    }

    /// Returns `true` when the chat type indicates a group conversation.
    pub fn is_group_chat(msg: &IncomingFeishuMessage) -> bool {
        msg.chat_type == "group"
    }

    // -----------------------------------------------------------------------
    // Send text
    // -----------------------------------------------------------------------

    /// Send a text message via Feishu Bot API.
    pub async fn send_text(&self, chat_id: &str, text: &str) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);
        let text = format_message(text);

        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send failed: {e}")))?;

        Self::check_api_response("send_text", resp).await
    }

    // -----------------------------------------------------------------------
    // Edit text
    // -----------------------------------------------------------------------

    /// Edit a message via Feishu Bot API.
    pub async fn edit_text(&self, message_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}", FEISHU_API_BASE, message_id);
        let text = format_message(text);

        let body = serde_json::json!({
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string()
        });

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu edit failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu edit error: {text}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Rich text (post) message
    // -----------------------------------------------------------------------

    /// Send a rich text (post) message with a title and structured content blocks.
    ///
    /// Each inner `Vec<PostElement>` represents one line/paragraph.
    pub async fn send_post(
        &self,
        chat_id: &str,
        title: &str,
        content: Vec<Vec<PostElement>>,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);

        let content_json = serde_json::json!({
            "zh_cn": {
                "title": title,
                "content": content
            }
        });

        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "post",
            "content": content_json.to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send_post failed: {e}")))?;

        Self::check_api_response("send_post", resp).await
    }

    // -----------------------------------------------------------------------
    // Interactive card message
    // -----------------------------------------------------------------------

    /// Send an interactive card message.
    pub async fn send_card(
        &self,
        chat_id: &str,
        card: &FeishuCard,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);

        let card_json = serde_json::json!({
            "config": { "wide_screen_mode": true },
            "header": card.header,
            "elements": card.elements,
        });

        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "interactive",
            "content": card_json.to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send_card failed: {e}")))?;

        Self::check_api_response("send_card", resp).await
    }

    /// Update an existing interactive card message.
    pub async fn update_card(
        &self,
        message_id: &str,
        card: &FeishuCard,
    ) -> Result<(), GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}", FEISHU_API_BASE, message_id);

        let card_json = serde_json::json!({
            "config": { "wide_screen_mode": true },
            "header": card.header,
            "elements": card.elements,
        });

        let body = serde_json::json!({
            "msg_type": "interactive",
            "content": card_json.to_string()
        });

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu update_card failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu update_card error: {text}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Image upload (separate from file upload)
    // -----------------------------------------------------------------------

    /// Upload an image to Feishu and return the `image_key`.
    ///
    /// Uses the `/im/v1/images` endpoint (distinct from file upload).
    pub async fn upload_image(
        &self,
        image_bytes: Vec<u8>,
        file_name: &str,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/images", FEISHU_API_BASE);

        let ext = std::path::Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");
        let mime = crate::platforms::helpers::mime_from_extension(ext);

        let part = reqwest::multipart::Part::bytes(image_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;

        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part("image", part);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu image upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu image upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Feishu image upload parse failed: {e}"))
        })?;

        result
            .pointer("/data/image_key")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| GatewayError::SendFailed("No image_key in upload response".into()))
    }

    /// Upload an image from a file path and send it in a chat.
    pub async fn send_image(
        &self,
        chat_id: &str,
        image_path: &str,
    ) -> Result<String, GatewayError> {
        let file_name = std::path::Path::new(image_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("image.png");

        let image_bytes = tokio::fs::read(image_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read image: {e}")))?;

        let image_key = self.upload_image(image_bytes, file_name).await?;
        self.send_image_by_key(chat_id, &image_key).await
    }

    /// Send an already-uploaded image by its `image_key`.
    pub async fn send_image_by_key(
        &self,
        chat_id: &str,
        image_key: &str,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);

        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "image",
            "content": serde_json::json!({ "image_key": image_key }).to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send_image failed: {e}")))?;

        Self::check_api_response("send_image", resp).await
    }

    // -----------------------------------------------------------------------
    // Audio message
    // -----------------------------------------------------------------------

    /// Upload audio and send it as an audio message.
    ///
    /// Feishu requires opus-encoded audio for the `audio` message type. The file
    /// is uploaded via `/im/v1/files` with `file_type=opus` then sent as `msg_type=audio`.
    pub async fn send_audio(
        &self,
        chat_id: &str,
        audio_path: &str,
        duration_ms: Option<u32>,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;

        let file_name = std::path::Path::new(audio_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("audio.opus");

        let audio_bytes = tokio::fs::read(audio_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read audio file: {e}")))?;

        let upload_url = format!("{}/im/v1/files", FEISHU_API_BASE);
        let part = reqwest::multipart::Part::bytes(audio_bytes)
            .file_name(file_name.to_string())
            .mime_str("audio/ogg")
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;

        let mut form = reqwest::multipart::Form::new()
            .text("file_type", "opus")
            .text("file_name", file_name.to_string())
            .part("file", part);

        if let Some(dur) = duration_ms {
            form = form.text("duration", dur.to_string());
        }

        let resp = self
            .client
            .post(&upload_url)
            .header("Authorization", bearer(&token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu audio upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu audio upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Feishu audio upload parse failed: {e}"))
        })?;

        let file_key = result
            .pointer("/data/file_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GatewayError::SendFailed("No file_key in audio upload response".into())
            })?;

        let msg_url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);
        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "audio",
            "content": serde_json::json!({ "file_key": file_key }).to_string()
        });

        let resp = self
            .client
            .post(&msg_url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send_audio failed: {e}")))?;

        Self::check_api_response("send_audio", resp).await
    }

    // -----------------------------------------------------------------------
    // Group chat info
    // -----------------------------------------------------------------------

    /// Retrieve chat metadata via `/im/v1/chats/{chat_id}`.
    pub async fn get_chat_info(&self, chat_id: &str) -> Result<ChatInfo, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/chats/{}", FEISHU_API_BASE, chat_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", bearer(&token))
            .send()
            .await
            .map_err(|e| GatewayError::Platform(format!("Feishu get_chat_info failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Platform(format!(
                "Feishu get_chat_info error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::Platform(format!("Feishu get_chat_info parse failed: {e}"))
        })?;

        let data = result.get("data").unwrap_or(&serde_json::Value::Null);

        Ok(ChatInfo {
            chat_id: chat_id.to_string(),
            name: data.get("name").and_then(|v| v.as_str()).map(String::from),
            description: data
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from),
            chat_type: data
                .get("chat_mode")
                .and_then(|v| v.as_str())
                .map(String::from),
            owner_id: data
                .get("owner_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            member_count: data
                .get("member_count")
                .and_then(|v| v.as_u64())
                .map(|n| n as u32),
        })
    }

    // -----------------------------------------------------------------------
    // Reply message
    // -----------------------------------------------------------------------

    /// Reply to a specific message. Uses the Feishu reply API endpoint
    /// `POST /im/v1/messages/{message_id}/reply`.
    pub async fn reply_message(
        &self,
        message_id: &str,
        text: &str,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}/reply", FEISHU_API_BASE, message_id);

        let body = serde_json::json!({
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu reply failed: {e}")))?;

        Self::check_api_response("reply_message", resp).await
    }

    /// Reply to a specific message with a rich text (post) body.
    pub async fn reply_post(
        &self,
        message_id: &str,
        title: &str,
        content: Vec<Vec<PostElement>>,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}/reply", FEISHU_API_BASE, message_id);

        let content_json = serde_json::json!({
            "zh_cn": {
                "title": title,
                "content": content
            }
        });

        let body = serde_json::json!({
            "msg_type": "post",
            "content": content_json.to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu reply_post failed: {e}")))?;

        Self::check_api_response("reply_post", resp).await
    }

    /// Reply to a specific message with an interactive card.
    pub async fn reply_card(
        &self,
        message_id: &str,
        card: &FeishuCard,
    ) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}/reply", FEISHU_API_BASE, message_id);

        let card_json = serde_json::json!({
            "config": { "wide_screen_mode": true },
            "header": card.header,
            "elements": card.elements,
        });

        let body = serde_json::json!({
            "msg_type": "interactive",
            "content": card_json.to_string()
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu reply_card failed: {e}")))?;

        Self::check_api_response("reply_card", resp).await
    }

    // -----------------------------------------------------------------------
    // Add reaction (emoji) to a message
    // -----------------------------------------------------------------------

    /// Add a reaction emoji to a message.
    pub async fn add_reaction(
        &self,
        message_id: &str,
        emoji_type: &str,
    ) -> Result<(), GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!(
            "{}/im/v1/messages/{}/reactions",
            FEISHU_API_BASE, message_id
        );

        let body = serde_json::json!({
            "reaction_type": { "emoji_type": emoji_type }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", bearer(&token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu add_reaction failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            warn!("Feishu add_reaction error (non-fatal): {text}");
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Delete message
    // -----------------------------------------------------------------------

    /// Delete a message by its ID.
    pub async fn delete_message(&self, message_id: &str) -> Result<(), GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}", FEISHU_API_BASE, message_id);

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", bearer(&token))
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu delete_message failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu delete_message error: {text}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Get message (read-back)
    // -----------------------------------------------------------------------

    /// Retrieve a message's details by ID.
    pub async fn get_message(&self, message_id: &str) -> Result<serde_json::Value, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}", FEISHU_API_BASE, message_id);

        let resp = self
            .client
            .get(&url)
            .header("Authorization", bearer(&token))
            .send()
            .await
            .map_err(|e| GatewayError::Platform(format!("Feishu get_message failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Platform(format!(
                "Feishu get_message error: {text}"
            )));
        }

        resp.json()
            .await
            .map_err(|e| GatewayError::Platform(format!("Feishu get_message parse failed: {e}")))
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Validate an API response and extract the `message_id` from `/data/message_id`.
    async fn check_api_response(
        method: &str,
        resp: reqwest::Response,
    ) -> Result<String, GatewayError> {
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Feishu {method} API error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu {method} parse failed: {e}")))?;

        let code = result.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        if code != 0 {
            let msg = result
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(GatewayError::SendFailed(format!(
                "Feishu {method} error (code={code}): {msg}"
            )));
        }

        Ok(result
            .pointer("/data/message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}

// ---------------------------------------------------------------------------
// Card builder helpers
// ---------------------------------------------------------------------------

impl FeishuCard {
    pub fn new() -> Self {
        Self {
            header: None,
            elements: Vec::new(),
        }
    }

    pub fn with_header(mut self, title: &str, template: Option<&str>) -> Self {
        self.header = Some(CardHeader {
            title: TextContent {
                tag: "plain_text".to_string(),
                content: title.to_string(),
            },
            template: template.map(String::from),
        });
        self
    }

    pub fn add_markdown(mut self, content: &str) -> Self {
        self.elements.push(CardElement::Markdown {
            content: content.to_string(),
        });
        self
    }

    pub fn add_div(mut self, text: &str) -> Self {
        self.elements.push(CardElement::Div {
            text: TextContent {
                tag: "plain_text".to_string(),
                content: text.to_string(),
            },
        });
        self
    }

    pub fn add_hr(mut self) -> Self {
        self.elements.push(CardElement::Hr {});
        self
    }

    pub fn add_button(
        mut self,
        label: &str,
        value: Option<serde_json::Value>,
        action_type: Option<&str>,
    ) -> Self {
        let action = CardAction::Button {
            text: TextContent {
                tag: "plain_text".to_string(),
                content: label.to_string(),
            },
            action_type: action_type.map(String::from),
            value,
            url: None,
        };
        if let Some(CardElement::Action { ref mut actions }) = self.elements.last_mut() {
            actions.push(action);
        } else {
            self.elements.push(CardElement::Action {
                actions: vec![action],
            });
        }
        self
    }
}

impl Default for FeishuCard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Post element builder helpers
// ---------------------------------------------------------------------------

impl PostElement {
    pub fn text(s: &str) -> Self {
        PostElement::Text {
            text: s.to_string(),
            un_escape: None,
        }
    }

    pub fn link(text: &str, href: &str) -> Self {
        PostElement::Link {
            text: text.to_string(),
            href: href.to_string(),
        }
    }

    pub fn at(user_id: &str) -> Self {
        PostElement::At {
            user_id: user_id.to_string(),
            user_name: None,
        }
    }

    pub fn image(image_key: &str) -> Self {
        PostElement::Image {
            image_key: image_key.to_string(),
            width: None,
            height: None,
        }
    }
}

fn normalized_image_content_type(content_type: Option<&str>) -> Option<String> {
    let normalized = content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_ascii_lowercase();
    if normalized.starts_with("image/") {
        Some(normalized)
    } else {
        None
    }
}

fn image_extension_from_content_type(content_type: Option<&str>) -> Option<&'static str> {
    let normalized = normalized_image_content_type(content_type)?;
    match normalized.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn remote_image_file_name(image_url: &str, content_type: Option<&str>) -> String {
    let stripped = image_url
        .split('#')
        .next()
        .unwrap_or(image_url)
        .split('?')
        .next()
        .unwrap_or(image_url)
        .trim_end_matches('/');
    let base = stripped.rsplit('/').next().unwrap_or("").trim();
    let mut file_name = if base.is_empty() {
        "image".to_string()
    } else {
        base.to_string()
    };

    let has_extension = std::path::Path::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some();
    if !has_extension {
        let ext = image_extension_from_content_type(content_type).unwrap_or("png");
        file_name.push('.');
        file_name.push_str(ext);
    }
    file_name
}

fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    match caption.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("{c}\n{image_url}"),
        None => image_url.to_string(),
    }
}

fn format_message(content: &str) -> String {
    content.trim().to_string()
}

// ---------------------------------------------------------------------------
// PlatformAdapter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for FeishuAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Feishu adapter starting (app_id: {})", self.config.app_id);
        self.get_tenant_token().await?;
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Feishu adapter stopping");
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
        _chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::{media_category, mime_from_extension};

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let token = self.get_tenant_token().await?;
        let category = media_category(ext);

        match category {
            "image" => {
                let image_key = self.upload_image(file_bytes, file_name).await?;
                self.send_image_by_key(chat_id, &image_key).await?;
            }
            "audio" => {
                self.send_audio(chat_id, file_path, None).await?;
            }
            _ => {
                let file_type = "stream";
                let upload_url = format!("{}/im/v1/files", FEISHU_API_BASE);
                let part = reqwest::multipart::Part::bytes(file_bytes)
                    .file_name(file_name.to_string())
                    .mime_str(mime)
                    .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
                let form = reqwest::multipart::Form::new()
                    .text("file_type", file_type.to_string())
                    .text("file_name", file_name.to_string())
                    .part("file", part);

                let resp = self
                    .client
                    .post(&upload_url)
                    .header("Authorization", bearer(&token))
                    .multipart(form)
                    .send()
                    .await
                    .map_err(|e| {
                        GatewayError::SendFailed(format!("Feishu file upload failed: {e}"))
                    })?;

                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(GatewayError::SendFailed(format!(
                        "Feishu upload error: {text}"
                    )));
                }

                let result: serde_json::Value = resp.json().await.map_err(|e| {
                    GatewayError::SendFailed(format!("Feishu upload parse failed: {e}"))
                })?;
                let file_key = result
                    .pointer("/data/file_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let msg_url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);
                let body = serde_json::json!({
                    "receive_id": chat_id,
                    "msg_type": "file",
                    "content": serde_json::json!({ "file_key": file_key }).to_string()
                });

                let resp = self
                    .client
                    .post(&msg_url)
                    .header("Authorization", bearer(&token))
                    .json(&body)
                    .send()
                    .await
                    .map_err(|e| {
                        GatewayError::SendFailed(format!("Feishu file send failed: {e}"))
                    })?;

                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    return Err(GatewayError::SendFailed(format!(
                        "Feishu file send error: {text}"
                    )));
                }
            }
        }

        if let Some(cap) = caption {
            let _ = self.send_text(chat_id, cap).await;
        }

        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let downloaded = async {
            let resp = self
                .client
                .get(image_url)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status()));
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
                .to_vec();
            if bytes.is_empty() {
                return Err("empty body".to_string());
            }
            Ok((bytes, content_type))
        }
        .await;

        let (image_bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Feishu image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let image_key = self.upload_image(image_bytes, &file_name).await?;

        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            self.send_post(
                chat_id,
                "Image",
                vec![
                    vec![PostElement::image(&image_key)],
                    vec![PostElement::text(cap)],
                ],
            )
            .await?;
        } else {
            self.send_image_by_key(chat_id, &image_key).await?;
        }

        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "feishu"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_event_v2() -> FeishuEvent {
        FeishuEvent {
            schema: Some("2.0".to_string()),
            header: Some(FeishuEventHeader {
                event_id: "ev_123".to_string(),
                event_type: "im.message.receive_v1".to_string(),
                create_time: Some("1234567890".to_string()),
                token: Some("test_token_abc".to_string()),
                app_id: Some("cli_xxx".to_string()),
                tenant_key: Some("tk_xxx".to_string()),
            }),
            event: Some(serde_json::json!({
                "sender": {
                    "sender_id": { "open_id": "ou_user1" }
                },
                "message": {
                    "message_id": "om_msg1",
                    "chat_id": "oc_chat1",
                    "chat_type": "group",
                    "message_type": "text",
                    "content": "{\"text\":\"@_user_1 hello world\"}",
                    "mentions": [{ "key": "@_user_1", "id": { "open_id": "ou_bot" } }]
                }
            })),
            challenge: None,
            token: None,
            event_type: None,
        }
    }

    fn sample_url_verification() -> FeishuEvent {
        FeishuEvent {
            schema: None,
            header: None,
            event: None,
            challenge: Some("challenge_abc".to_string()),
            token: Some("test_token_abc".to_string()),
            event_type: Some("url_verification".to_string()),
        }
    }

    #[test]
    fn verify_event_v2_correct_token() {
        let event = sample_event_v2();
        assert!(FeishuAdapter::verify_event(&event, "test_token_abc"));
    }

    #[test]
    fn verify_event_v2_wrong_token() {
        let event = sample_event_v2();
        assert!(!FeishuAdapter::verify_event(&event, "wrong_token"));
    }

    #[test]
    fn verify_url_verification_event() {
        let event = sample_url_verification();
        assert!(FeishuAdapter::verify_event(&event, "test_token_abc"));
    }

    #[test]
    fn parse_message_event_basic() {
        let event = sample_event_v2();
        let msg = FeishuAdapter::parse_message_event(event.event.as_ref().unwrap())
            .expect("should parse");
        assert_eq!(msg.message_id, "om_msg1");
        assert_eq!(msg.chat_id, "oc_chat1");
        assert_eq!(msg.chat_type, "group");
        assert_eq!(msg.sender_id.as_deref(), Some("ou_user1"));
        assert_eq!(msg.text, "hello world");
        assert!(msg.is_mention);
    }

    #[test]
    fn parse_message_event_dm() {
        let event_val = serde_json::json!({
            "sender": { "sender_id": { "open_id": "ou_user2" } },
            "message": {
                "message_id": "om_msg2",
                "chat_id": "oc_chat2",
                "chat_type": "p2p",
                "message_type": "text",
                "content": "{\"text\":\"direct message\"}"
            }
        });
        let msg = FeishuAdapter::parse_message_event(&event_val).expect("should parse");
        assert_eq!(msg.text, "direct message");
        assert!(!msg.is_mention);
        assert!(!FeishuAdapter::is_group_chat(&msg));
    }

    #[test]
    fn is_group_chat_true() {
        let event = sample_event_v2();
        let msg = FeishuAdapter::parse_message_event(event.event.as_ref().unwrap()).unwrap();
        assert!(FeishuAdapter::is_group_chat(&msg));
    }

    #[test]
    fn strip_mentions_basic() {
        assert_eq!(strip_mentions("@_user_123 hello"), "hello");
        assert_eq!(
            strip_mentions("hey @_user_1 and @_user_2 there"),
            "hey and there"
        );
        assert_eq!(strip_mentions("no mentions"), "no mentions");
    }

    #[test]
    fn post_element_builders() {
        let t = PostElement::text("hello");
        match t {
            PostElement::Text { ref text, .. } => assert_eq!(text, "hello"),
            _ => panic!("expected Text"),
        }

        let l = PostElement::link("click", "https://example.com");
        match l {
            PostElement::Link { ref text, ref href } => {
                assert_eq!(text, "click");
                assert_eq!(href, "https://example.com");
            }
            _ => panic!("expected Link"),
        }
    }

    #[test]
    fn card_builder() {
        let card = FeishuCard::new()
            .with_header("Test Card", Some("blue"))
            .add_markdown("**bold** text")
            .add_hr()
            .add_div("plain text")
            .add_button("Click me", None, Some("primary"));

        assert!(card.header.is_some());
        assert_eq!(card.elements.len(), 4);
    }

    #[test]
    fn card_serialization_roundtrip() {
        let card = FeishuCard::new()
            .with_header("Title", None)
            .add_markdown("content");

        let json = serde_json::to_string(&card).expect("serialize");
        let _: FeishuCard = serde_json::from_str(&json).expect("deserialize");
    }

    #[test]
    fn feishu_event_deserialize_url_verification() {
        let json = r#"{
            "challenge": "abc123",
            "token": "verify_tok",
            "type": "url_verification"
        }"#;
        let event: FeishuEvent = serde_json::from_str(json).expect("deserialize");
        assert_eq!(event.challenge.as_deref(), Some("abc123"));
        assert_eq!(event.token.as_deref(), Some("verify_tok"));
        assert_eq!(event.event_type.as_deref(), Some("url_verification"));
    }

    #[test]
    fn feishu_event_deserialize_v2() {
        let json = r#"{
            "schema": "2.0",
            "header": {
                "event_id": "ev_1",
                "event_type": "im.message.receive_v1",
                "token": "tok_abc"
            },
            "event": { "message": {} }
        }"#;
        let event: FeishuEvent = serde_json::from_str(json).expect("deserialize");
        assert_eq!(event.schema.as_deref(), Some("2.0"));
        assert!(event.header.is_some());
        let h = event.header.unwrap();
        assert_eq!(h.event_type, "im.message.receive_v1");
    }

    #[test]
    fn flatten_post_content_basic() {
        let content = serde_json::json!({
            "title": "My Title",
            "content": [
                [{ "tag": "text", "text": "Hello " }, { "tag": "a", "text": "link" }],
                [{ "tag": "text", "text": "Second line" }]
            ]
        });
        let result = FeishuAdapter::flatten_post_content(&content);
        assert!(result.contains("My Title"));
        assert!(result.contains("Hello link"));
        assert!(result.contains("Second line"));
    }

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }

    #[test]
    fn format_message_trims_whitespace() {
        assert_eq!(format_message("\n\nhello world\n"), "hello world");
        assert_eq!(format_message("  hello world  "), "hello world");
    }
}
