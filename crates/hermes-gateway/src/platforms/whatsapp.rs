//! WhatsApp Business Cloud API adapter.
//!
//! Implements the `PlatformAdapter` trait for WhatsApp using the Cloud API.
//! Sends messages via `POST /v1/messages` and receives via webhook.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const WHATSAPP_API_BASE: &str = "https://graph.facebook.com/v18.0";

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

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
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
    use super::build_link_media_body;

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
        self.send_text(chat_id, text).await
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
