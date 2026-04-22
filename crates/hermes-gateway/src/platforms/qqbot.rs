//! QQ Bot (official QQ v2 API) adapter.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, RwLock};
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

const QQ_TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const QQ_API_BASE: &str = "https://api.sgroup.qq.com";
const QQ_FILE_TYPE_IMAGE: u8 = 1;
const QQ_MSG_TYPE_MEDIA: u8 = 7;

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
        let endpoint = Self::message_endpoint(chat_id);
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

    fn message_endpoint(chat_id: &str) -> String {
        if Self::looks_like_group_chat(chat_id) {
            format!("{QQ_API_BASE}/v2/groups/{chat_id}/messages")
        } else {
            format!("{QQ_API_BASE}/v2/users/{chat_id}/messages")
        }
    }

    fn file_endpoint(chat_id: &str) -> String {
        if Self::looks_like_group_chat(chat_id) {
            format!("{QQ_API_BASE}/v2/groups/{chat_id}/files")
        } else {
            format!("{QQ_API_BASE}/v2/users/{chat_id}/files")
        }
    }

    fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
        match caption.map(str::trim).filter(|s| !s.is_empty()) {
            Some(c) => format!("{c}\n{image_url}"),
            None => image_url.to_string(),
        }
    }

    fn json_file_info_value(upload: &serde_json::Value) -> Result<serde_json::Value, GatewayError> {
        let file_info = upload.get("file_info").ok_or_else(|| {
            GatewayError::SendFailed("QQBot media upload response missing file_info".into())
        })?;
        Ok(file_info.clone())
    }

    async fn upload_remote_image(
        &self,
        chat_id: &str,
        image_url: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let token = self.get_access_token().await?;
        let endpoint = Self::file_endpoint(chat_id);
        let body = serde_json::json!({
            "file_type": QQ_FILE_TYPE_IMAGE,
            "srv_send_msg": false,
            "url": image_url
        });

        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("QQBot {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("QQBot image upload failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "QQBot image upload API error: {text}"
            )));
        }
        let payload: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("QQBot image upload parse failed: {e}"))
        })?;
        Self::json_file_info_value(&payload)
    }

    async fn upload_local_image(
        &self,
        chat_id: &str,
        file_path: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let token = self.get_access_token().await?;
        let endpoint = Self::file_endpoint(chat_id);
        let data = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("QQBot local image read failed ({file_path}): {e}"))
        })?;
        if data.is_empty() {
            return Err(GatewayError::SendFailed(format!(
                "QQBot local image file is empty: {file_path}"
            )));
        }
        let body = serde_json::json!({
            "file_type": QQ_FILE_TYPE_IMAGE,
            "srv_send_msg": false,
            "file_data": base64::engine::general_purpose::STANDARD.encode(data)
        });
        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("QQBot {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("QQBot local image upload failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "QQBot local image upload API error: {text}"
            )));
        }
        let payload: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("QQBot local image upload parse failed: {e}"))
        })?;
        Self::json_file_info_value(&payload)
    }

    async fn send_image_message(
        &self,
        chat_id: &str,
        file_info: serde_json::Value,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let token = self.get_access_token().await?;
        let endpoint = Self::message_endpoint(chat_id);
        let mut body = serde_json::json!({
            "msg_type": QQ_MSG_TYPE_MEDIA,
            "media": { "file_info": file_info },
            "msg_seq": (chrono::Utc::now().timestamp_millis() % 65535)
        });
        if let Some(c) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            body["content"] = serde_json::Value::String(c.to_string());
        }
        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("QQBot {token}"))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("QQBot image send request failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "QQBot image send API error: {text}"
            )));
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

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let send_result = if let Some(path) = image_url.strip_prefix("file://") {
            let decoded_path = urlencoding::decode(path)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| path.to_string());
            let file_info = self.upload_local_image(chat_id, &decoded_path).await?;
            self.send_image_message(chat_id, file_info, caption).await
        } else if image_url.starts_with("http://") || image_url.starts_with("https://") {
            let file_info = self.upload_remote_image(chat_id, image_url).await?;
            self.send_image_message(chat_id, file_info, caption).await
        } else {
            Err(GatewayError::SendFailed(format!(
                "QQBot image source must be file:// or http(s): {image_url}"
            )))
        };

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "QQBot image send failed; falling back to text"
                );
                let fallback = Self::image_fallback_text(image_url, caption);
                self.send_text(chat_id, &fallback).await
            }
        }
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "qqbot"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qqbot_image_fallback_text_formats_caption() {
        assert_eq!(
            QqBotAdapter::image_fallback_text("https://example.com/plot.png", Some("Daily chart")),
            "Daily chart\nhttps://example.com/plot.png"
        );
        assert_eq!(
            QqBotAdapter::image_fallback_text("https://example.com/plot.png", Some("   ")),
            "https://example.com/plot.png"
        );
    }

    #[test]
    fn qqbot_json_file_info_value_requires_field() {
        let payload = serde_json::json!({"file_info":"opaque-token"});
        assert_eq!(
            QqBotAdapter::json_file_info_value(&payload).unwrap(),
            serde_json::json!("opaque-token")
        );

        let missing = serde_json::json!({"ok": true});
        let err = QqBotAdapter::json_file_info_value(&missing).unwrap_err();
        assert!(format!("{err}").contains("file_info"));
    }
}
