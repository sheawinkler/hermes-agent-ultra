//! DingTalk Robot webhook adapter.

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
pub struct DingTalkConfig {
    pub webhook_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct DingTalkAdapter {
    base: BasePlatformAdapter,
    config: DingTalkConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl DingTalkAdapter {
    pub fn new(config: DingTalkConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.webhook_url)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &DingTalkConfig { &self.config }

    /// Send a text message via DingTalk robot webhook.
    pub async fn send_text(&self, text: &str) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "msgtype": "text",
            "text": { "content": text }
        });

        let resp = self.client.post(&self.config.webhook_url)
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("DingTalk send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("DingTalk API error: {}", text)));
        }
        Ok(())
    }

    /// Send a markdown message via DingTalk robot webhook.
    pub async fn send_markdown(&self, title: &str, text: &str) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": { "title": title, "text": text }
        });

        let resp = self.client.post(&self.config.webhook_url)
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("DingTalk markdown send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("DingTalk API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for DingTalkAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("DingTalk adapter starting");
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("DingTalk adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, _chat_id: &str, text: &str, parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        match parse_mode {
            Some(ParseMode::Markdown) => self.send_markdown("Message", text).await,
            _ => self.send_text(text).await,
        }
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("DingTalk webhook does not support message editing");
        Ok(())
    }

    async fn send_file(&self, _chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(file_path = file_path, "DingTalk send_file not supported via webhook");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "dingtalk" }
}
