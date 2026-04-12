//! Signal messaging adapter via signal-cli REST API.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

// ---------------------------------------------------------------------------
// SignalConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalConfig {
    /// Signal number (e.g., "+1234567890").
    pub phone_number: String,
    /// Signal CLI REST API URL.
    #[serde(default = "default_signal_api_url")]
    pub api_url: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_signal_api_url() -> String { "http://localhost:8080".to_string() }

// ---------------------------------------------------------------------------
// SignalAdapter
// ---------------------------------------------------------------------------

pub struct SignalAdapter {
    base: BasePlatformAdapter,
    config: SignalConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl SignalAdapter {
    pub fn new(config: SignalConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.phone_number)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &SignalConfig { &self.config }

    /// Send a message via signal-cli REST API.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/v2/send", self.config.api_url);
        let body = serde_json::json!({
            "message": text,
            "number": self.config.phone_number,
            "recipients": [recipient]
        });

        let resp = self.client.post(&url).json(&body).send().await
            .map_err(|e| GatewayError::SendFailed(format!("Signal send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Signal API error: {}", text)));
        }
        Ok(())
    }

    /// Receive messages via signal-cli REST API polling.
    pub async fn receive_messages(&self) -> Result<Vec<serde_json::Value>, GatewayError> {
        let url = format!("{}/v1/receive/{}", self.config.api_url, self.config.phone_number);
        let resp = self.client.get(&url).send().await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Signal receive failed: {}", e)))?;

        let messages: Vec<serde_json::Value> = resp.json().await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Signal parse failed: {}", e)))?;
        Ok(messages)
    }
}

#[async_trait]
impl PlatformAdapter for SignalAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Signal adapter starting (number: {})", self.config.phone_number);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Signal adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("Signal does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "Signal send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "signal" }
}
