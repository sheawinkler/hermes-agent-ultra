//! WeChat Official Account API adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const WEIXIN_API_BASE: &str = "https://api.weixin.qq.com/cgi-bin";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeixinConfig {
    pub app_id: String,
    pub app_secret: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding_aes_key: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct WeChatAdapter {
    base: BasePlatformAdapter,
    config: WeixinConfig,
    client: Client,
    access_token: RwLock<Option<String>>,
    stop_signal: Arc<Notify>,
}

impl WeChatAdapter {
    pub fn new(config: WeixinConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.app_id)
            .with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self { base, config, client, access_token: RwLock::new(None), stop_signal: Arc::new(Notify::new()) })
    }

    pub fn config(&self) -> &WeixinConfig { &self.config }

    /// Get or refresh the access token.
    pub async fn get_access_token(&self) -> Result<String, GatewayError> {
        if let Some(ref token) = *self.access_token.read().await {
            return Ok(token.clone());
        }

        let url = format!(
            "{}/token?grant_type=client_credential&appid={}&secret={}",
            WEIXIN_API_BASE, self.config.app_id, self.config.app_secret
        );

        let resp = self.client.get(&url).send().await
            .map_err(|e| GatewayError::Auth(format!("Weixin auth failed: {}", e)))?;

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::Auth(format!("Weixin auth parse failed: {}", e)))?;

        let token = result.get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::Auth("No access_token in Weixin response".into()))?
            .to_string();

        *self.access_token.write().await = Some(token.clone());
        Ok(token)
    }

    /// Send a customer service text message.
    pub async fn send_text(&self, openid: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let url = format!("{}/message/custom/send?access_token={}", WEIXIN_API_BASE, token);

        let body = serde_json::json!({
            "touser": openid,
            "msgtype": "text",
            "text": { "content": text }
        });

        let resp = self.client.post(&url).json(&body).send().await
            .map_err(|e| GatewayError::SendFailed(format!("Weixin send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("Weixin API error: {}", text)));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for WeChatAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Weixin adapter starting (app_id: {})", self.config.app_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Weixin adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(&self, chat_id: &str, text: &str, _parse_mode: Option<ParseMode>) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await
    }

    async fn edit_message(&self, _chat_id: &str, _message_id: &str, _text: &str) -> Result<(), GatewayError> {
        debug!("Weixin does not support message editing");
        Ok(())
    }

    async fn send_file(&self, chat_id: &str, file_path: &str, _caption: Option<&str>) -> Result<(), GatewayError> {
        debug!(chat_id = chat_id, file_path = file_path, "Weixin send_file");
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "weixin" }
}
