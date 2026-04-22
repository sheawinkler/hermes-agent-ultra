//! Signal messaging adapter via signal-cli REST API.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// Parsed incoming Signal message from the signal-cli receive endpoint.
#[derive(Debug, Clone)]
pub struct IncomingSignalMessage {
    pub source: String,
    pub timestamp: u64,
    pub text: String,
    pub group_id: Option<String>,
    pub attachments: Vec<String>,
}

// ---------------------------------------------------------------------------
// SignalConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalConfig {
    /// Signal number (e.g., "+1234567890").
    pub phone_number: String,
    /// Signal CLI REST API URL.
    #[serde(default = "default_signal_api_url")]
    pub api_url: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_signal_api_url() -> String {
    "http://localhost:8080".to_string()
}

// ---------------------------------------------------------------------------
// SignalAdapter
// ---------------------------------------------------------------------------

pub struct SignalAdapter {
    base: BasePlatformAdapter,
    config: SignalConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl SignalAdapter {
    pub fn new(config: SignalConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.phone_number).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &SignalConfig {
        &self.config
    }

    /// Send a message via signal-cli REST API.
    pub async fn send_text(&self, recipient: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/v2/send", self.config.api_url);
        let body = serde_json::json!({
            "message": text,
            "number": self.config.phone_number,
            "recipients": [recipient]
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Signal send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Signal API error: {}",
                text
            )));
        }
        Ok(())
    }

    /// Parse a single message from signal-cli's receive endpoint into a typed struct.
    ///
    /// Expects the signal-cli JSON envelope format with `envelope.dataMessage`.
    pub fn parse_received_message(msg: &serde_json::Value) -> Option<IncomingSignalMessage> {
        let envelope = msg.get("envelope")?;
        let source = envelope.get("source").and_then(|v| v.as_str())?.to_string();
        let timestamp = envelope
            .get("timestamp")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        let data_message = envelope.get("dataMessage")?;
        let text = data_message
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let group_id = data_message
            .get("groupInfo")
            .and_then(|g| g.get("groupId"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let attachments = data_message
            .get("attachments")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Some(IncomingSignalMessage {
            source,
            timestamp,
            text,
            group_id,
            attachments,
        })
    }

    /// Receive messages via signal-cli REST API polling.
    pub async fn receive_messages(&self) -> Result<Vec<serde_json::Value>, GatewayError> {
        let url = format!(
            "{}/v1/receive/{}",
            self.config.api_url, self.config.phone_number
        );
        let resp =
            self.client.get(&url).send().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!("Signal receive failed: {}", e))
            })?;

        let messages: Vec<serde_json::Value> = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Signal parse failed: {}", e)))?;
        Ok(messages)
    }
}

fn normalized_image_content_type(content_type: Option<&str>) -> Option<String> {
    let normalized = content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_ascii_lowercase();
    if normalized.starts_with("image/") {
        Some(normalized)
    } else {
        None
    }
}

fn image_extension_from_content_type(content_type: Option<&str>) -> Option<&'static str> {
    let normalized = normalized_image_content_type(content_type)?;
    match normalized.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn remote_image_file_name(image_url: &str, content_type: Option<&str>) -> String {
    let stripped = image_url
        .split('#')
        .next()
        .unwrap_or(image_url)
        .split('?')
        .next()
        .unwrap_or(image_url)
        .trim_end_matches('/');
    let base = stripped.rsplit('/').next().unwrap_or("").trim();
    let mut file_name = if base.is_empty() {
        "image".to_string()
    } else {
        base.to_string()
    };

    let has_extension = std::path::Path::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some();
    if !has_extension {
        let ext = image_extension_from_content_type(content_type).unwrap_or("png");
        file_name.push('.');
        file_name.push_str(ext);
    }
    file_name
}

fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    match caption.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("{c}\n{image_url}"),
        None => image_url.to_string(),
    }
}

#[async_trait]
impl PlatformAdapter for SignalAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Signal adapter starting (number: {})",
            self.config.phone_number
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Signal adapter stopping");
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
        debug!("Signal does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let b64 = base64_encode(&file_bytes);

        let url = format!("{}/v2/send", self.config.api_url);
        let body = serde_json::json!({
            "message": caption.unwrap_or(""),
            "number": self.config.phone_number,
            "recipients": [chat_id],
            "base64_attachments": [b64]
        });

        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Signal attachment send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Signal attachment error: {text}"
            )));
        }
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        if let Some(path) = image_url.strip_prefix("file://") {
            let decoded_path = urlencoding::decode(path)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| path.to_string());
            return self.send_file(chat_id, &decoded_path, caption).await;
        }

        let downloaded = async {
            let resp = self
                .client
                .get(image_url)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status()));
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
                .to_vec();
            if bytes.is_empty() {
                return Err("empty body".to_string());
            }
            Ok((bytes, content_type))
        }
        .await;

        let (bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Signal image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let suffix = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| ".png".to_string());
        let temp_path = std::env::temp_dir().join(format!(
            "hermes_signal_img_{}{}",
            uuid::Uuid::new_v4(),
            suffix
        ));
        tokio::fs::write(&temp_path, &bytes).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to write temp image file: {e}"))
        })?;

        let temp_path_str = temp_path.to_string_lossy().to_string();
        let send_result = self.send_file(chat_id, &temp_path_str, caption).await;
        if let Err(err) = tokio::fs::remove_file(&temp_path).await {
            warn!(
                path = %temp_path.display(),
                error = %err,
                "Failed to remove temporary Signal image file"
            );
        }

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Signal image upload failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                self.send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await
            }
        }
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "signal"
    }
}

/// Simple base64 encoding using the `base64` crate convention (standard alphabet).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        result.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_image_file_name_keeps_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.png?token=abc",
            Some("image/png"),
        );
        assert_eq!(file_name, "diagram.png");
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/jpeg"));
        assert_eq!(file_name, "diagram.jpg");
    }

    #[test]
    fn image_fallback_text_with_caption() {
        let text = image_fallback_text("https://cdn.example.com/path/diagram", Some("Figure 1"));
        assert_eq!(text, "Figure 1\nhttps://cdn.example.com/path/diagram");
    }
}
