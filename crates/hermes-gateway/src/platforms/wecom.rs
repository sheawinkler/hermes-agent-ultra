//! WeCom (Enterprise WeChat) API adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComConfig {
    pub corp_id: String,
    pub agent_id: String,
    pub secret: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct WeComAdapter {
    base: BasePlatformAdapter,
    config: WeComConfig,
    client: Client,
    access_token: RwLock<Option<String>>,
    stop_signal: Arc<Notify>,
}

impl WeComAdapter {
    pub fn new(config: WeComConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.corp_id)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, access_token: RwLock::new(None), stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &WeComConfig { &self.config }

    /// Get or refresh the access token.
    pub async fn get_access_token(&self) -> Result<String, GatewayError> {
        if let Some(ref token) = *self.access_token.read().await {
            return Ok(token.clone());
        }

        let url = format!(
            "{}/gettoken?corpid={}&corpsecret={}",
            WECOM_API_BASE, self.config.corp_id, self.config.secret
        );

        let resp = self.client.get(&url).send().await
            .map_err(|e| GatewayError::Auth(format!("WeCom auth failed: {}", e)))?;

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::Auth(format!("WeCom auth parse failed: {}", e)))?;

        let token = result.get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::Auth("No access_token in WeCom response".into()))?
            .to_string();

        *self.access_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Send a text message via WeCom API.
    pub async fn send_text(&self, user_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let url = format!("{}/message/send?access_token={}", WECOM_API_BASE, token);

        let body = serde_json::json!({
            "touser": user_id,
            "msgtype": "text",
            "agentid": self.config.agent_id.parse::<i64>().unwrap_or(0),
            "text": { "content": text }
        });

        let resp = self.client.post(&url).json(&body).send().await
            .map_err(|e| GatewayError::SendFailed(format!("WeCom send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("WeCom API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for WeComAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("WeCom adapter starting (corp_id: {})", self.config.corp_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("WeCom adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("WeCom does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "WeCom send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "wecom" }
}
