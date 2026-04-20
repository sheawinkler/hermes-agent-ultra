//! QQ Bot (official QQ v2 API) adapter.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const QQ_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const QQ_API_BASE: &str = "https://api.sgroup.qq.com";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QqBotConfig {
    pub app_id: String,
    pub client_secret: String,
    #[serde(default)]
    pub markdown_support: bool,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct QqBotAdapter {
    base: BasePlatformAdapter,
    config: QqBotConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    access_token: RwLock<Option<(String, Instant)>>,
}

impl QqBotAdapter {
    pub fn new(config: QqBotConfig) -> Result<Self, GatewayError> {
        if config.app_id.trim().is_empty() {
            return Err(GatewayError::Platform(
                "QQBot requires app_id (platforms.qqbot.extra.app_id)".into(),
            ));
        }
        if config.client_secret.trim().is_empty() {
            return Err(GatewayError::Platform(
                "QQBot requires client_secret (platforms.qqbot.extra.client_secret)".into(),
            ));
        }

        let base = BasePlatformAdapter::new(&config.app_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
            access_token: RwLock::new(None),
        })
    }

    async fn get_access_token(&self) -> Result<String, GatewayError> {
        if let Some((token, expiry)) = self.access_token.read().await.clone() {
            if Instant::now() < expiry {
                return Ok(token);
            }
        }

        let body = serde_json::json!({
            "appId": self.config.app_id,
            "clientSecret": self.config.client_secret
        });
        let resp = self
            .client
            .post(QQ_TOKEN_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::Auth(format!("QQBot auth request failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Auth(format!(
                "QQBot token endpoint returned non-success: {text}"
            )));
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::Auth(format!("QQBot auth parse failed: {e}")))?;
        let token = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| {
                GatewayError::Auth("QQBot token response missing access_token".into())
            })?;
        let expires_in = value
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(7200);
        let expires_at = Instant::now() + Duration::from_secs(expires_in.saturating_sub(60));
        *self.access_token.write().await = Some((token.clone(), expires_at));
        Ok(token)
    }

    fn looks_like_group_chat(chat_id: &str) -> bool {
        let id = chat_id.trim().to_ascii_lowercase();
        id.starts_with("group_") || id.starts_with("grp_") || id.starts_with("qqgroup_")
    }

    async fn send_text(&self, chat_id: &str, text: &str) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let endpoint = if Self::looks_like_group_chat(chat_id) {
            format!("{QQ_API_BASE}/v2/groups/{chat_id}/messages")
        } else {
            format!("{QQ_API_BASE}/v2/users/{chat_id}/messages")
        };
        let body = if self.config.markdown_support {
            serde_json::json!({
                "msg_type": 2,
                "markdown": { "content": text },
                "msg_seq": (chrono::Utc::now().timestamp_millis() % 65535)
            })
        } else {
            serde_json::json!({
                "msg_type": 0,
                "content": text,
                "msg_seq": (chrono::Utc::now().timestamp_millis() % 65535)
            })
        };
        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("QQBot {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("QQBot send request failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("QQBot API error: {text}")));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for QqBotAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("QQBot adapter starting (app_id={})", self.config.app_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("QQBot adapter stopping");
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
        self.send_text(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("QQBot does not support message editing in this adapter");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        let text = if let Some(c) = caption {
            if c.trim().is_empty() {
                format!("[Attachment: {file_name}]")
            } else {
                format!("[Attachment: {file_name}] {c}")
            }
        } else {
            format!("[Attachment: {file_name}]")
        };
        self.send_text(chat_id, &text).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "qqbot"
    }
}
