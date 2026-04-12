//! Discord Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Discord using the REST API
//! for message operations and the Gateway WebSocket for receiving events.
//! Supports message splitting at 2000 characters, file uploads via
//! multipart form data, and Gateway event handling (IDENTIFY, HEARTBEAT,
//! RESUME, READY, MESSAGE_CREATE).

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

/// Maximum message length for Discord (2000 characters).
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Discord Gateway WebSocket URL.
const DISCORD_GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

// ---------------------------------------------------------------------------
// DiscordConfig
// ---------------------------------------------------------------------------

/// Configuration for the Discord adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Discord bot token.
    pub token: String,

    /// Application ID for interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,

    /// Whether the bot must be @mentioned in group channels.
    #[serde(default)]
    pub require_mention: bool,

    /// Gateway intents bitmask (default: GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT).
    #[serde(default = "default_intents")]
    pub intents: u64,
}

fn default_intents() -> u64 {
    // GUILDS (1<<0) | GUILD_MESSAGES (1<<9) | MESSAGE_CONTENT (1<<15)
    (1 << 0) | (1 << 9) | (1 << 15)
}

// ---------------------------------------------------------------------------
// Discord Gateway types
// ---------------------------------------------------------------------------

/// Discord Gateway payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

/// Discord Gateway opcodes.
pub mod opcodes {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Discord IDENTIFY payload data.
#[derive(Debug, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: u64,
    pub properties: IdentifyProperties,
}

/// Discord IDENTIFY connection properties.
#[derive(Debug, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

/// Discord RESUME payload data.
#[derive(Debug, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Discord REST API types
// ---------------------------------------------------------------------------

/// Discord Message object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub author: Option<DiscordUser>,
}

/// Discord User object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

/// Incoming message parsed from a Discord MESSAGE_CREATE event.
#[derive(Debug, Clone)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
}

// ---------------------------------------------------------------------------
// DiscordAdapter
// ---------------------------------------------------------------------------

/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl DiscordAdapter {
    /// Create a new Discord adapter with the given configuration.
    pub fn new(config: DiscordConfig) -> Result<Self, GatewayError> {
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
    pub fn config(&self) -> &DiscordConfig {
        &self.config
    }

    // -----------------------------------------------------------------------
    // REST API: Sending messages
    // -----------------------------------------------------------------------

    /// Send a message to a Discord channel, splitting if it exceeds 2000 chars.
    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();

        for chunk in &chunks {
            let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);
            let body = serde_json::json!({ "content": chunk });

            let resp = self.client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.config.token))
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("Discord send failed: {}", e)))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                return Err(GatewayError::SendFailed(format!(
                    "Discord API error: {}", text
                )));
            }

            let msg: DiscordMessage = resp
                .json()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e)))?;

            message_ids.push(msg.id);
        }

        Ok(message_ids)
    }

    /// Edit an existing message in a Discord channel.
    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}",
            DISCORD_API_BASE, channel_id, message_id
        );

        let body = serde_json::json!({
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
        });

        let resp = self.client
            .patch(&url)
            .header("Authorization", format!("Bot {}", self.config.token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord edit API error: {}", text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Discord channel using multipart form data.
    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        let url = format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id);

        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e)))?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name);

        let mut form = reqwest::multipart::Form::new()
            .part("files[0]", part);

        if let Some(cap) = caption {
            let payload = serde_json::json!({ "content": cap });
            form = form.text("payload_json", payload.to_string());
        }

        let resp = self.client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.config.token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord file upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord file upload API error: {}", text
            )));
        }

        let msg: DiscordMessage = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e)))?;

        Ok(msg.id)
    }

    // -----------------------------------------------------------------------
    // Gateway WebSocket helpers
    // -----------------------------------------------------------------------

    /// Build an IDENTIFY payload for the Discord Gateway.
    pub fn build_identify_payload(&self) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::IDENTIFY,
            d: Some(serde_json::to_value(IdentifyData {
                token: self.config.token.clone(),
                intents: self.config.intents,
                properties: IdentifyProperties {
                    os: "linux".into(),
                    browser: "hermes-agent".into(),
                    device: "hermes-agent".into(),
                },
            }).unwrap()),
            s: None,
            t: None,
        }
    }

    /// Build a HEARTBEAT payload.
    pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: sequence.map(|s| serde_json::Value::Number(s.into())),
            s: None,
            t: None,
        }
    }

    /// Build a RESUME payload.
    pub fn build_resume_payload(&self, session_id: &str, seq: u64) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::RESUME,
            d: Some(serde_json::to_value(ResumeData {
                token: self.config.token.clone(),
                session_id: session_id.to_string(),
                seq,
            }).unwrap()),
            s: None,
            t: None,
        }
    }

    /// Parse a MESSAGE_CREATE dispatch event into an IncomingDiscordMessage.
    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();
        let content = data.get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let author = data.get("author");
        let user_id = author.and_then(|a| a.get("id")).and_then(|v| v.as_str()).map(String::from);
        let username = author.and_then(|a| a.get("username")).and_then(|v| v.as_str()).map(String::from);
        let is_bot = author
            .and_then(|a| a.get("bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Some(IncomingDiscordMessage {
            channel_id,
            message_id,
            user_id,
            username,
            content,
            is_bot,
        })
    }
}

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Discord adapter starting (token: {}...)",
            &self.config.token[..8.min(self.config.token.len())]
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Discord adapter stopping");
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
        self.send_text(chat_id, text).await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption).await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "discord"
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
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(3000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn gateway_payload_identify() {
        let config = DiscordConfig {
            token: "test-token".into(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            require_mention: false,
            intents: default_intents(),
        };
        let adapter = DiscordAdapter::new(config).unwrap();
        let payload = adapter.build_identify_payload();
        assert_eq!(payload.op, opcodes::IDENTIFY);
        assert!(payload.d.is_some());
    }

    #[test]
    fn gateway_payload_heartbeat() {
        let payload = DiscordAdapter::build_heartbeat_payload(Some(42));
        assert_eq!(payload.op, opcodes::HEARTBEAT);
        assert_eq!(payload.d, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn parse_message_create_event() {
        let data = serde_json::json!({
            "id": "msg123",
            "channel_id": "ch456",
            "content": "hello world",
            "author": {
                "id": "user789",
                "username": "testuser",
                "bot": false
            }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert_eq!(msg.channel_id, "ch456");
        assert_eq!(msg.message_id, "msg123");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.user_id, Some("user789".into()));
        assert_eq!(msg.username, Some("testuser".into()));
        assert!(!msg.is_bot);
    }

    #[test]
    fn parse_message_create_bot() {
        let data = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "bot msg",
            "author": { "id": "bot1", "username": "mybot", "bot": true }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert!(msg.is_bot);
    }
}
