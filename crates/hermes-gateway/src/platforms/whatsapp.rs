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
        let base = BasePlatformAdapter::new(&config.token)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;

        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &WhatsAppConfig { &self.config }

    /// Send a text message via WhatsApp Cloud API.
    pub async fn send_text(&self, to: &str, text: &str) -> Result<(), GatewayError> {
        let phone_id = self.config.phone_number_id.as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": "text",
            "text": { "body": text }
        });

        let resp = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("WhatsApp API error: {}", text)));
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
        let phone_id = self.config.phone_number_id.as_deref()
            .ok_or_else(|| GatewayError::SendFailed("phone_number_id not configured".into()))?;

        let url = format!("{}/{}/messages", WHATSAPP_API_BASE, phone_id);
        let mut media_obj = serde_json::json!({ "link": media_url });
        if let Some(cap) = caption {
            media_obj["caption"] = serde_json::Value::String(cap.to_string());
        }

        let body = serde_json::json!({
            "messaging_product": "whatsapp",
            "to": to,
            "type": media_type,
            media_type: media_obj
        });

        let resp = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("WhatsApp media send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("WhatsApp API error: {}", text)));
        }
        Ok(())
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

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        // WhatsApp does not natively support message editing
        debug!("WhatsApp does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, caption: Option<&str>) -> Result<(), GatewayError> {
        // For file sending, we'd need to upload to WhatsApp media first.
        // For now, log the intent.
        debug!(chat_id = chat_id, file_path = file_path, "WhatsApp send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "whatsapp" }
}
