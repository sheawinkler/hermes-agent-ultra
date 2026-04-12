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

/// Heuristic: WeCom group chat ids often start with `wr` / `ww` / `wm` (length > 8).
fn looks_like_wecom_group_chat_id(id: &str) -> bool {
    let id = id.trim();
    if id.len() < 8 {
        return false;
    }
    id.starts_with("wr") || id.starts_with("ww") || id.starts_with("wm")
}

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
    ///
    /// If `user_id` looks like a group chat id (`wr…` / `ww…` / `wm…`), uses `chatid`
    /// (group broadcast) instead of `touser` (single-user / pipe-separated user list).
    pub async fn send_text(&self, user_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let url = format!("{}/message/send?access_token={}", WECOM_API_BASE, token);

        let agent_id = self.config.agent_id.parse::<i64>().unwrap_or(0);
        let body = if looks_like_wecom_group_chat_id(user_id) {
            serde_json::json!({
                "chatid": user_id,
                "msgtype": "text",
                "agentid": agent_id,
                "text": { "content": text }
            })
        } else {
            serde_json::json!({
                "touser": user_id,
                "msgtype": "text",
                "agentid": agent_id,
                "text": { "content": text }
            })
        };

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

    async fn send_file(&self, chat_id: &str, file_path: &str, caption: Option<&str>) -> Result<(), GatewayError> {
        use crate::platforms::helpers::{media_category, mime_from_extension};

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path).await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let token = self.get_access_token().await?;

        let media_type = match media_category(ext) {
            "image" => "image",
            "video" => "video",
            "audio" => "voice",
            _ => "file",
        };

        // Step 1: Upload media to WeCom
        let upload_url = format!(
            "{}/media/upload?access_token={}&type={}",
            WECOM_API_BASE, token, media_type
        );

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
        let form = reqwest::multipart::Form::new().part("media", part);

        let resp = self.client.post(&upload_url)
            .multipart(form)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("WeCom media upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("WeCom upload error: {text}")));
        }

        let result: serde_json::Value = resp.json().await
            .map_err(|e| GatewayError::SendFailed(format!("WeCom upload parse failed: {e}")))?;
        let media_id = result.get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| GatewayError::SendFailed("No media_id in WeCom response".into()))?;

        // Step 2: Send message with media
        let send_url = format!("{}/message/send?access_token={}", WECOM_API_BASE, token);
        let agent_id = self.config.agent_id.parse::<i64>().unwrap_or(0);
        let group = looks_like_wecom_group_chat_id(chat_id);

        let body = match media_type {
            "image" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "image",
                        "agentid": agent_id,
                        "image": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "image",
                        "agentid": agent_id,
                        "image": { "media_id": media_id }
                    })
                }
            }
            "voice" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "voice",
                        "agentid": agent_id,
                        "voice": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "voice",
                        "agentid": agent_id,
                        "voice": { "media_id": media_id }
                    })
                }
            }
            "video" => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "video",
                        "agentid": agent_id,
                        "video": { "media_id": media_id, "title": caption.unwrap_or(file_name) }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "video",
                        "agentid": agent_id,
                        "video": { "media_id": media_id, "title": caption.unwrap_or(file_name) }
                    })
                }
            }
            _ => {
                if group {
                    serde_json::json!({
                        "chatid": chat_id,
                        "msgtype": "file",
                        "agentid": agent_id,
                        "file": { "media_id": media_id }
                    })
                } else {
                    serde_json::json!({
                        "touser": chat_id,
                        "msgtype": "file",
                        "agentid": agent_id,
                        "file": { "media_id": media_id }
                    })
                }
            }
        };

        let resp = self.client.post(&send_url)
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("WeCom media send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("WeCom media send error: {text}")));
        }
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "wecom" }
}
