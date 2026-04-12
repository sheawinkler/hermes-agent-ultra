//! Home Assistant REST API + WebSocket adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeAssistantConfig {
    pub base_url: String,
    pub long_lived_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct HomeAssistantAdapter {
    base: BasePlatformAdapter,
    config: HomeAssistantConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl HomeAssistantAdapter {
    pub fn new(config: HomeAssistantConfig) -> Result<Self, GatewayError> {
        let base =
            BasePlatformAdapter::new(&config.long_lived_token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &HomeAssistantConfig {
        &self.config
    }

    /// Send a notification via Home Assistant REST API.
    pub async fn send_notification(
        &self,
        service: &str,
        message: &str,
    ) -> Result<(), GatewayError> {
        let url = format!("{}/api/services/notify/{}", self.config.base_url, service);
        let body = serde_json::json!({ "message": message });

        let resp = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.long_lived_token),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("HA notify failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("HA API error: {}", text)));
        }
        Ok(())
    }

    /// Call a Home Assistant service.
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        data: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/api/services/{}/{}",
            self.config.base_url, domain, service
        );

        let resp = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.long_lived_token),
            )
            .json(data)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("HA service call failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "HA service error: {}",
                text
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for HomeAssistantAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "HomeAssistant adapter starting (url: {})",
            self.config.base_url
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("HomeAssistant adapter stopping");
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
        // chat_id is used as the notification service name
        self.send_notification(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("HomeAssistant does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        // Home Assistant notify service supports `data` field with file attachments.
        // Different notification integrations handle this differently; the most common
        // pattern is to pass the file path in `data.attachment.url` or `data.file`.
        let url = format!("{}/api/services/notify/{}", self.config.base_url, chat_id);

        let message = caption.unwrap_or("File attachment");
        let body = serde_json::json!({
            "message": message,
            "data": {
                "attachment": {
                    "url": file_path
                },
                "file": file_path
            }
        });

        let resp = self
            .client
            .post(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.long_lived_token),
            )
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("HA file notify failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "HA file notify error: {text}"
            )));
        }
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "homeassistant"
    }
}
