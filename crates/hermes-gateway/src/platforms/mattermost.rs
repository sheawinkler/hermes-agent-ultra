//! Mattermost REST API + WebSocket adapter.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// A WebSocket event frame from the Mattermost real-time API.
#[derive(Debug, Clone, Deserialize)]
pub struct MattermostWsEvent {
    pub event: String,
    pub data: Option<serde_json::Value>,
    pub broadcast: Option<serde_json::Value>,
    pub seq: Option<i64>,
}

/// Parsed incoming message extracted from a `posted` WebSocket event.
#[derive(Debug, Clone)]
pub struct IncomingMattermostMessage {
    pub post_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub message: String,
    pub is_bot: bool,
}

// ---------------------------------------------------------------------------
// MattermostConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MattermostConfig {
    pub server_url: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

pub struct MattermostAdapter {
    base: BasePlatformAdapter,
    config: MattermostConfig,
    client: Client,
    stop_signal: Arc<Notify>,
}

impl MattermostAdapter {
    pub fn new(config: MattermostConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
        })
    }

    pub fn config(&self) -> &MattermostConfig {
        &self.config
    }

    /// Send a message via Mattermost REST API.
    pub async fn send_text(&self, channel_id: &str, text: &str) -> Result<String, GatewayError> {
        let url = format!("{}/api/v4/posts", self.config.server_url);
        let body = serde_json::json!({
            "channel_id": channel_id,
            "message": text
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Mattermost API error: {}",
                text
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost parse failed: {}", e)))?;
        Ok(result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Parse a Mattermost WebSocket event into an incoming message.
    ///
    /// Only `posted` events that contain a valid post JSON are returned.
    pub fn parse_ws_event(event: &MattermostWsEvent) -> Option<IncomingMattermostMessage> {
        if event.event != "posted" {
            return None;
        }

        let data = event.data.as_ref()?;
        let post_str = data.get("post").and_then(|v| v.as_str())?;
        let post: serde_json::Value = serde_json::from_str(post_str).ok()?;

        let post_id = post.get("id").and_then(|v| v.as_str())?.to_string();
        let channel_id = post.get("channel_id").and_then(|v| v.as_str())?.to_string();
        let user_id = post.get("user_id").and_then(|v| v.as_str())?.to_string();
        let message = post
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let is_bot = post
            .get("props")
            .and_then(|p| p.get("from_bot"))
            .and_then(|v| v.as_str())
            .map(|v| v == "true")
            .unwrap_or(false);

        Some(IncomingMattermostMessage {
            post_id,
            channel_id,
            user_id,
            message,
            is_bot,
        })
    }

    /// Fetch the authenticated user's profile (`GET /api/v4/users/me`).
    pub async fn get_me(&self) -> Result<serde_json::Value, GatewayError> {
        let url = format!("{}/api/v4/users/me", self.config.server_url);
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Mattermost get_me failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Mattermost get_me error: {}",
                text
            )));
        }

        resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Mattermost get_me parse failed: {}", e))
        })
    }

    /// Edit a message via Mattermost REST API.
    pub async fn edit_text(&self, post_id: &str, text: &str) -> Result<(), GatewayError> {
        let url = format!("{}/api/v4/posts/{}", self.config.server_url, post_id);
        let body = serde_json::json!({ "id": post_id, "message": text });

        let resp = self
            .client
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Mattermost edit error: {}",
                text
            )));
        }
        Ok(())
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
impl PlatformAdapter for MattermostAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Mattermost adapter starting (server: {})",
            self.config.server_url
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Mattermost adapter stopping");
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
        _chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::mime_from_extension;

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        // Step 1: Upload file via Mattermost API
        let upload_url = format!("{}/api/v4/files", self.config.server_url);
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
        let form = reqwest::multipart::Form::new()
            .text("channel_id", chat_id.to_string())
            .part("files", part);

        let resp = self
            .client
            .post(&upload_url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost file upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Mattermost upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Mattermost upload parse failed: {e}"))
        })?;
        let file_ids: Vec<String> = result
            .get("file_infos")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|f| f.get("id").and_then(|id| id.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // Step 2: Create a post with the uploaded file IDs
        let post_url = format!("{}/api/v4/posts", self.config.server_url);
        let body = serde_json::json!({
            "channel_id": chat_id,
            "message": caption.unwrap_or(""),
            "file_ids": file_ids
        });

        let resp = self
            .client
            .post(&post_url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Mattermost file post failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Mattermost file post error: {text}"
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
                    "Mattermost image-url download failed; falling back to text"
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

        let temp_path =
            std::env::temp_dir().join(format!("hermes_mm_img_{}{}", uuid::Uuid::new_v4(), suffix));
        tokio::fs::write(&temp_path, &bytes).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to write temp image file: {e}"))
        })?;

        let temp_path_str = temp_path.to_string_lossy().to_string();
        let send_result = self.send_file(chat_id, &temp_path_str, caption).await;
        if let Err(err) = tokio::fs::remove_file(&temp_path).await {
            warn!(
                path = %temp_path.display(),
                error = %err,
                "Failed to remove temporary Mattermost image file"
            );
        }

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Mattermost image upload failed; falling back to text"
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
        "mattermost"
    }
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
