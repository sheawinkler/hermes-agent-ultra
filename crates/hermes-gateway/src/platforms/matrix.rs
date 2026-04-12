//! Matrix Client-Server API adapter.
//!
//! Sends messages via `PUT /_matrix/client/v3/rooms/{roomId}/send/m.room.message/{txnId}`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

// ---------------------------------------------------------------------------
// MatrixConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// MatrixAdapter
// ---------------------------------------------------------------------------

pub struct MatrixAdapter {
    base: BasePlatformAdapter,
    config: MatrixConfig,
    client: Client,
    txn_counter: AtomicU64,
    stop_signal: Arc<Notify>,
}

impl MatrixAdapter {
    pub fn new(config: MatrixConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.access_token)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, txn_counter: AtomicU64::new(0), stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &MatrixConfig { &self.config }

    fn next_txn_id(&self) -> String {
        let n = self.txn_counter.fetch_add(1, Ordering::SeqCst);
        format!("hermes-{}-{}", chrono::Utc::now().timestamp_millis(), n)
    }

    /// Send a message to a Matrix room.
    pub async fn send_text(&self, room_id: &str, text: &str, html: bool) -> Result<String, GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let body = if html {
            serde_json::json!({
                "msgtype": "m.text",
                "body": text,
                "format": "org.matrix.custom.html",
                "formatted_body": text
            })
        } else {
            serde_json::json!({ "msgtype": "m.text", "body": text })
        };

        let resp = self.client.put(&url)
            .header("Authorization", format!("Bearer {}", self.config.access_token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Matrix API error: {}", text)));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix parse failed: {}", e)))?;

        Ok(result.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    /// Edit a message in a Matrix room using m.replace relation.
    pub async fn edit_text(&self, room_id: &str, event_id: &str, new_text: &str) -> Result<(), GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": format!("* {}", new_text),
            "m.new_content": { "msgtype": "m.text", "body": new_text },
            "m.relates_to": { "rel_type": "m.replace", "event_id": event_id }
        });

        let resp = self.client.put(&url)
            .header("Authorization", format!("Bearer {}", self.config.access_token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Matrix edit API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for MatrixAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter starting (user: {})", self.config.user_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        let html = matches!(parse_mode, Some(ParseMode::Html));
        self.send_text(chat_id, text, html).await?;
        Ok(())
    }

    async fn edit_message(&self, chat_id: &str, message_id: &str, text: &str) -> Result<(), GatewayError> {
        self.edit_text(chat_id, message_id, text).await
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "Matrix send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "matrix" }
}
