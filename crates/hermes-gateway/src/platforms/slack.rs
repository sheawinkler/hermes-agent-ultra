//! Slack Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Slack using the Web API
//! for message operations (`chat.postMessage`, `chat.update`, `files.upload`)
//! and Socket Mode via WebSocket for receiving events.
//! Supports Block Kit formatting and thread replies via `thread_ts`.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

/// Slack Web API base URL.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Maximum message length for Slack (4000 characters for text blocks).
const MAX_MESSAGE_LENGTH: usize = 4000;

// ---------------------------------------------------------------------------
// SlackConfig
// ---------------------------------------------------------------------------

/// Configuration for the Slack adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackConfig {
    /// Slack bot token (xoxb-...).
    pub token: String,

    /// Slack app-level token for socket mode (xapp-...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_token: Option<String>,

    /// Whether to use Socket Mode for receiving events.
    #[serde(default)]
    pub socket_mode: bool,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// Slack API types
// ---------------------------------------------------------------------------

/// Generic Slack API response.
#[derive(Debug, Deserialize)]
pub struct SlackResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
}

/// Slack Socket Mode hello event.
#[derive(Debug, Deserialize)]
pub struct SocketModeHello {
    #[serde(rename = "type")]
    pub event_type: String,
}

/// Slack Socket Mode envelope.
#[derive(Debug, Clone, Deserialize)]
pub struct SocketModeEnvelope {
    #[serde(rename = "type")]
    pub envelope_type: String,
    #[serde(default)]
    pub envelope_id: Option<String>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

/// Slack event payload (from Events API / Socket Mode).
#[derive(Debug, Clone, Deserialize)]
pub struct SlackEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub thread_ts: Option<String>,
    #[serde(default)]
    pub bot_id: Option<String>,
}

/// Incoming message parsed from a Slack event.
#[derive(Debug, Clone)]
pub struct IncomingSlackMessage {
    pub channel: String,
    pub user_id: Option<String>,
    pub text: String,
    pub ts: String,
    pub thread_ts: Option<String>,
    pub is_bot: bool,
}

// ---------------------------------------------------------------------------
// SlackAdapter
// ---------------------------------------------------------------------------

/// Slack Bot API platform adapter.
pub struct SlackAdapter {
    base: BasePlatformAdapter,
    config: SlackConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl SlackAdapter {
    /// Create a new Slack adapter with the given configuration.
    pub fn new(config: SlackConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token)
            .with_proxy(config.proxy.clone());

        base.validate_token()?;

        let client = base.build_client()?;

        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &SlackConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // Web API: Sending messages
    // -----------------------------------------------------------------------

    /// Post a message to a Slack channel using `chat.postMessage`.
    /// Supports thread replies via `thread_ts` and Block Kit formatting.
    pub async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, GatewayError> {
        let chunks = split_message(text, MAX_MESSAGE_LENGTH);
        let mut last_ts = String::new();

        for (i, chunk) in chunks.iter().enumerate() {
            let mut body = serde_json::json!({
                "channel": channel,
                "text": chunk,
            });

            // Thread the first chunk to the specified thread, subsequent
            // chunks reply to the first chunk's ts.
            if i == 0 {
                if let Some(ts) = thread_ts {
                    body["thread_ts"] = serde_json::Value::String(ts.to_string());
                }
            } else if !last_ts.is_empty() {
                body["thread_ts"] = serde_json::Value::String(last_ts.clone());
            }

            let resp = self.slack_post("chat.postMessage", &body).await?;
            if let Some(ts) = resp.ts {
                last_ts = ts;
            }
        }

        Ok(last_ts)
    }

    /// Post a message with Block Kit blocks.
    pub async fn post_blocks(
        &self,
        channel: &str,
        blocks: &serde_json::Value,
        fallback_text: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, GatewayError> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": fallback_text,
            "blocks": blocks,
        });

        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }

        let resp = self.slack_post("chat.postMessage", &body).await?;
        resp.ts.ok_or_else(|| GatewayError::SendFailed("No ts in response".into()))
    }

    /// Update an existing message using `chat.update`.
    pub async fn update_message(
        &self,
        channel: &str,
        ts: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "channel": channel,
            "ts": ts,
            "text": &text[..text.len().min(MAX_MESSAGE_LENGTH)],
        });

        self.slack_post("chat.update", &body).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Web API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Slack channel using `files.uploadV2` flow.
    pub async fn upload_file(
        &self,
        channel: &str,
        file_path: &str,
        title: Option<&str>,
        thread_ts: Option<&str>,
    ) -> Result<(), GatewayError> {
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e)))?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.clone());

        let mut form = reqwest::multipart::Form::new()
            .text("channels", channel.to_string())
            .text("filename", file_name.clone())
            .part("file", part);

        if let Some(t) = title {
            form = form.text("title", t.to_string());
        }

        if let Some(ts) = thread_ts {
            form = form.text("thread_ts", ts.to_string());
        }

        let url = format!("{}/files.upload", SLACK_API_BASE);
        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Slack file upload failed: {}", e)))?;

        let result: SlackResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to parse Slack response: {}", e)))?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack files.upload error: {}",
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Socket Mode: Receiving events
    // -----------------------------------------------------------------------

    /// Get a WebSocket URL for Socket Mode connection.
    pub async fn get_socket_mode_url(&self) -> Result<String, GatewayError> {
        let app_token = self.config.app_token.as_ref().ok_or_else(|| {
            GatewayError::Auth("Socket Mode requires an app-level token (xapp-...)".into())
        })?;

        let resp = self.client
            .post(&format!("{}/apps.connections.open", SLACK_API_BASE))
            .header("Authorization", format!("Bearer {}", app_token))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!(
                "Failed to open Socket Mode connection: {}", e
            )))?;

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!(
                "Failed to parse Socket Mode response: {}", e
            )))?;

        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("unknown");
            return Err(GatewayError::ConnectionFailed(format!(
                "Socket Mode connection failed: {}", err
            )));
        }

        body.get("url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| GatewayError::ConnectionFailed("No URL in Socket Mode response".into()))
    }

    /// Parse a Socket Mode envelope into an IncomingSlackMessage.
    pub fn parse_event(envelope: &SocketModeEnvelope) -> Option<IncomingSlackMessage> {
        let payload = envelope.payload.as_ref()?;
        let event = payload.get("event")?;

        let event_type = event.get("type")?.as_str()?;
        if event_type != "message" {
            return None;
        }

        // Skip bot messages
        if event.get("bot_id").is_some() {
            return None;
        }

        let channel = event.get("channel")?.as_str()?.to_string();
        let text = event.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let user_id = event.get("user").and_then(|v| v.as_str()).map(String::from);
        let ts = event.get("ts")?.as_str()?.to_string();
        let thread_ts = event.get("thread_ts").and_then(|v| v.as_str()).map(String::from);

        Some(IncomingSlackMessage {
            channel,
            user_id,
            text,
            ts,
            thread_ts,
            is_bot: false,
        })
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// POST to a Slack Web API method with JSON body.
    async fn slack_post(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<SlackResponse, GatewayError> {
        let url = format!("{}/{}", SLACK_API_BASE, method);

        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Slack {} failed: {}", method, e)))?;

        let result: SlackResponse = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!(
                "Failed to parse Slack {} response: {}", method, e
            )))?;

        if !result.ok {
            return Err(GatewayError::SendFailed(format!(
                "Slack {} error: {}",
                method,
                result.error.unwrap_or_else(|| "unknown".into())
            )));
        }

        Ok(result)
    }
}

#[async_trait]
impl PlatformAdapter for SlackAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Slack adapter starting (token: {}...)",
            &self.config.token[..8.min(self.config.token.len())]
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Slack adapter stopping");
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
        self.post_message(chat_id, text, None).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        // In Slack, message_id is the `ts` timestamp.
        self.update_message(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption, None).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "slack"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Split a message into chunks that fit within the given max length.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = (start + max_len).min(text.len());

        if end >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }

        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 4000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(5000);
        let chunks = split_message(&text, 4000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn parse_event_message() {
        let envelope = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": {
                    "type": "message",
                    "text": "hello bot",
                    "channel": "C123",
                    "user": "U456",
                    "ts": "1234567890.123456"
                }
            })),
        };

        let msg = SlackAdapter::parse_event(&envelope).unwrap();
        assert_eq!(msg.channel, "C123");
        assert_eq!(msg.user_id, Some("U456".into()));
        assert_eq!(msg.text, "hello bot");
        assert!(!msg.is_bot);
    }

    #[test]
    fn parse_event_bot_message_skipped() {
        let envelope = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": {
                    "type": "message",
                    "text": "bot msg",
                    "channel": "C123",
                    "bot_id": "B789",
                    "ts": "1234567890.123456"
                }
            })),
        };

        assert!(SlackAdapter::parse_event(&envelope).is_none());
    }

    #[test]
    fn parse_event_thread_reply() {
        let envelope = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": {
                    "type": "message",
                    "text": "thread reply",
                    "channel": "C123",
                    "user": "U456",
                    "ts": "1234567891.000000",
                    "thread_ts": "1234567890.123456"
                }
            })),
        };

        let msg = SlackAdapter::parse_event(&envelope).unwrap();
        assert_eq!(msg.thread_ts, Some("1234567890.123456".into()));
    }

    #[test]
    fn parse_event_non_message_skipped() {
        let envelope = SocketModeEnvelope {
            envelope_type: "events_api".into(),
            envelope_id: Some("env123".into()),
            payload: Some(serde_json::json!({
                "event": {
                    "type": "reaction_added",
                    "reaction": "thumbsup",
                    "user": "U456"
                }
            })),
        };

        assert!(SlackAdapter::parse_event(&envelope).is_none());
    }
}
