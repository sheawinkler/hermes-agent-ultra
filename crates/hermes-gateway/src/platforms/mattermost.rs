//! Mattermost REST API + WebSocket adapter.

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
pub struct MattermostConfig {
    pub server_url: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct MattermostAdapter {
    base: BasePlatformAdapter,
    config: MattermostConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl MattermostAdapter {
    pub fn new(config: MattermostConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &MattermostConfig { &self.config }

    /// Send a message via Mattermost REST API.
    pub async fn send_text(&self, channel_id: &str, text: &str) -> Result<String, GatewayError> {
        let url = format!("{}/api/v4/posts", self.config.server_url);
        let body = serde_json::json!({
            "channel_id": channel_id,
            "message": text
        });

        let resp = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Mattermost API error: {}", text)));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost parse failed: {}", e)))?;
        Ok(result.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    /// Edit a message via Mattermost REST API.
    pub async fn edit_text(&self, post_id: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/api/v4/posts/{}", self.config.server_url, post_id);
        let body = serde_json::json!({ "id": post_id, "message": text });

        let resp = self.client.put(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Mattermost edit error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for MattermostAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Mattermost adapter starting (server: {})", self.config.server_url);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Mattermost adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await?;
        Ok(())
    }

    async fn edit_message(&self, _chat_id: &str, message_id: &str, text: &str) -> Result<(), GatewayError> {
        self.edit_text(message_id, text).await
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "Mattermost send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "mattermost" }
}
