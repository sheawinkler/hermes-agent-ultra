//! BlueBubbles REST API adapter (iMessage).

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
pub struct BlueBubblesConfig {
    pub server_url: String,
    pub password: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct BlueBubblesAdapter {
    base: BasePlatformAdapter,
    config: BlueBubblesConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl BlueBubblesAdapter {
    pub fn new(config: BlueBubblesConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.password)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &BlueBubblesConfig { &self.config }

    /// Send a text message via BlueBubbles REST API.
    pub async fn send_text(&self, chat_guid: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/api/v1/message/text", self.config.server_url);
        let body = serde_json::json!({
            "chatGuid": chat_guid,
            "message": text,
            "method": "private-api"
        });

        let resp = self.client.post(&url)
            .query(&[("password", &self.config.password)])
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("BlueBubbles send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("BlueBubbles API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for BlueBubblesAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("BlueBubbles adapter starting (server: {})", self.config.server_url);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("BlueBubbles adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("BlueBubbles does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "BlueBubbles send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "bluebubbles" }
}
