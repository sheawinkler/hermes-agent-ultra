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

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// Raw webhook callback event from DingTalk robot.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkWebhookEvent {
    #[serde(default)]
    pub msg_type: Option<String>,
    #[serde(default)]
    pub text: Option<serde_json::Value>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub conversation_type: Option<String>,
    #[serde(default)]
    pub sender_id: Option<String>,
    #[serde(default)]
    pub sender_nick: Option<String>,
    #[serde(default)]
    pub at_users: Option<Vec<serde_json::Value>>,
}

/// Parsed incoming DingTalk message.
#[derive(Debug, Clone)]
pub struct IncomingDingTalkMessage {
    pub conversation_id: String,
    pub sender_id: String,
    pub text: String,
    pub is_group: bool,
    pub at_users: Vec<String>,
}

// ---------------------------------------------------------------------------
// DingTalkConfig
// ---------------------------------------------------------------------------

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

    /// Parse a DingTalk webhook callback body into an incoming message.
    pub fn parse_webhook_event(body: &serde_json::Value) -> Option<IncomingDingTalkMessage> {
        let conversation_id = body.get("conversationId")
            .and_then(|v| v.as_str())?
            .to_string();
        let sender_id = body.get("senderId")
            .and_then(|v| v.as_str())?
            .to_string();

        let text = body.get("text")
            .and_then(|t| t.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let is_group = body.get("conversationType")
            .and_then(|v| v.as_str())
            .map(|v| v == "2")
            .unwrap_or(false);

        let at_users = body.get("atUsers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|u| u.get("dingtalkId").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Some(IncomingDingTalkMessage {
            conversation_id,
            sender_id,
            text,
            is_group,
            at_users,
        })
    }

    /// Send an ActionCard message via DingTalk robot webhook.
    pub async fn send_action_card(
        &self,
        title: &str,
        text: &str,
        btn_title: &str,
        btn_url: &str,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "msgtype": "actionCard",
            "actionCard": {
                "title": title,
                "text": text,
                "singleTitle": btn_title,
                "singleURL": btn_url
            }
        });

        let resp = self.client.post(&self.config.webhook_url)
            .json(&body)
            .send().await
            .map_err(|e| GatewayError::SendFailed(format!("DingTalk actionCard send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!("DingTalk actionCard error: {}", text)));
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

    async fn send_file(&self, _chat_id: &str, file_path: &str, caption: Option<&str>) -> Result<(), GatewayError> {
        use crate::platforms::helpers::media_category;

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let category = media_category(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");

        // DingTalk robot webhooks only support text/markdown/link/actionCard.
        // For files, send a link-type message pointing to the file if it's a URL,
        // otherwise send a markdown message with the file info.
        if category == "image" && (file_path.starts_with("http://") || file_path.starts_with("https://")) {
            let body = serde_json::json!({
                "msgtype": "markdown",
                "markdown": {
                    "title": caption.unwrap_or("Image"),
                    "text": format!("![{}]({})\n\n{}", file_name, file_path, caption.unwrap_or(""))
                }
            });
            let resp = self.client.post(&self.config.webhook_url)
                .json(&body)
                .send().await
                .map_err(|e| GatewayError::SendFailed(format!("DingTalk file send failed: {e}")))?;
            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(GatewayError::SendFailed(format!("DingTalk file send error: {text}")));
            }
        } else if let Some(ref client_id) = self.config.client_id {
            // If client_id is available, use the internal API for file upload
            let file_bytes = tokio::fs::read(file_path).await
                .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;
            let mime = crate::platforms::helpers::mime_from_extension(ext);
            let upload_url = format!("https://oapi.dingtalk.com/media/upload?access_token={}&type=file", client_id);

            let part = reqwest::multipart::Part::bytes(file_bytes)
                .file_name(file_name.to_string())
                .mime_str(mime)
                .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
            let form = reqwest::multipart::Form::new().part("media", part);

            let resp = self.client.post(&upload_url)
                .multipart(form)
                .send().await
                .map_err(|e| GatewayError::SendFailed(format!("DingTalk upload failed: {e}")))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(GatewayError::SendFailed(format!("DingTalk upload error: {text}")));
            }
        } else {
            let msg = format!("[File: {}]{}", file_name, caption.map(|c| format!(" - {c}")).unwrap_or_default());
            self.send_text(&msg).await?;
        }
        Ok(())
    }

    fn is_running(&self) -> bool { self.base.is_running() }
    fn platform_name(&self) -> &str { "dingtalk" }
}
