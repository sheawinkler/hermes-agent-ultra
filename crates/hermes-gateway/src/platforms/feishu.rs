//! Feishu (Lark) Bot API adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const FEISHU_API_BASE: &str = "https://open.feishu.cn/open-apis";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeishuConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_token: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct FeishuAdapter {
    base: BasePlatformAdapter,
    config: FeishuConfig,
    client: Client,
    /// Cached tenant access token.
    tenant_token: RwLock<Option<String>>,
    stop_signal: Arc<Notify>,
}

impl FeishuAdapter {
    pub fn new(config: FeishuConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.app_id)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, tenant_token: RwLock::new(None), stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &FeishuConfig { &self.config }

    /// Get or refresh the tenant access token.
    pub async fn get_tenant_token(&self) -> Result<String, GatewayError> {
        if let Some(ref token) = *self.tenant_token.read().await {
            return Ok(token.clone());
        }

        let url = format!("{}/auth/v3/tenant_access_token/internal", FEISHU_API_BASE);
        let body = serde_json::json!({
            "app_id": self.config.app_id,
            "app_secret": self.config.app_secret
        });

        let resp = self.client.post(&url).json(&body).send().await
            .map_err(|e| GatewayError::Auth(format!("Feishu auth failed: {}", e)))?;

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::Auth(format!("Feishu auth parse failed: {}", e)))?;

        let token = result.get("tenant_access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::Auth("No tenant_access_token in response".into()))?
            .to_string();

        *self.tenant_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Send a text message via Feishu Bot API.
    pub async fn send_text(&self, chat_id: &str, text: &str) -> Result<String, GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages?receive_id_type=chat_id", FEISHU_API_BASE);

        let body = serde_json::json!({
            "receive_id": chat_id,
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string()
        });

        let resp = self.client.post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Feishu API error: {}", text)));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu parse failed: {}", e)))?;

        Ok(result.pointer("/data/message_id").and_then(|v| v.as_str()).unwrap_or("").to_string())
    }

    /// Edit a message via Feishu Bot API.
    pub async fn edit_text(&self, message_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_tenant_token().await?;
        let url = format!("{}/im/v1/messages/{}", FEISHU_API_BASE, message_id);

        let body = serde_json::json!({
            "msg_type": "text",
            "content": serde_json::json!({ "text": text }).to_string()
        });

        let resp = self.client.patch(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("Feishu edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Feishu edit error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for FeishuAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Feishu adapter starting (app_id: {})", self.config.app_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Feishu adapter stopping");
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
        debug!(chat_id = chat_id, file_path = file_path, "Feishu send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "feishu" }
}
