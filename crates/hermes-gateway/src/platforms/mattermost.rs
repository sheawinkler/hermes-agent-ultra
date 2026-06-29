//! Mattermost REST API + WebSocket adapter.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter, SendMessageOptions};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::platforms::helpers::download_media_url;

#[cfg(test)]
use std::future::Future;

const MATTERMOST_WS_BACKOFF_STEPS: &[u64] = &[2, 5, 10, 30, 60];

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
    pub thread_id: Option<String>,
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
    #[serde(default = "default_reply_to_mode")]
    pub reply_to_mode: String,
}

fn default_reply_to_mode() -> String {
    "off".to_string()
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

    fn websocket_url(&self) -> String {
        let trimmed = self.config.server_url.trim_end_matches('/');
        let ws_base = if let Some(rest) = trimmed.strip_prefix("https://") {
            format!("wss://{rest}")
        } else if let Some(rest) = trimmed.strip_prefix("http://") {
            format!("ws://{rest}")
        } else {
            trimmed.to_string()
        };
        format!("{ws_base}/api/v4/websocket")
    }

    /// Send a message via Mattermost REST API.
    pub async fn send_text(&self, channel_id: &str, text: &str) -> Result<String, GatewayError> {
        self.send_text_with_thread(channel_id, text, None, false)
            .await
    }

    async fn send_text_with_thread(
        &self,
        channel_id: &str,
        text: &str,
        thread_id: Option<&str>,
        notify: bool,
    ) -> Result<String, GatewayError> {
        let root_id = self.thread_root_for_send(thread_id).await;
        let body = mattermost_post_body(channel_id, text, None, root_id.as_deref());

        match self.post_body_once(&body).await {
            Ok(id) => Ok(id),
            Err(err) if notify && root_id.is_some() && err.is_broken_thread_root() => {
                let mut flat_body = mattermost_post_body(channel_id, text, None, None);
                flat_body["message"] = serde_json::json!(format!(
                    "Mattermost thread delivery failed; posting final reply in channel.\n\n{text}"
                )
                .trim());
                self.post_body_once(&flat_body)
                    .await
                    .map_err(MattermostPostFailure::into_gateway_error)
            }
            Err(err) => Err(err.into_gateway_error()),
        }
    }

    async fn post_body_once(
        &self,
        body: &serde_json::Value,
    ) -> Result<String, MattermostPostFailure> {
        let url = format!("{}/api/v4/posts", self.config.server_url);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .json(body)
            .send()
            .await
            .map_err(|e| MattermostPostFailure {
                status: None,
                body: format!("Mattermost send failed: {e}"),
            })?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            return Err(MattermostPostFailure {
                status: Some(status),
                body: text,
            });
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| MattermostPostFailure {
            status: None,
            body: format!("Mattermost parse failed: {e}"),
        })?;
        Ok(result
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    fn thread_candidate_for_send<'a>(&self, thread_id: Option<&'a str>) -> Option<&'a str> {
        if !self.config.reply_to_mode.eq_ignore_ascii_case("thread") {
            return None;
        }
        thread_id
            .map(str::trim)
            .filter(|id| mattermost_api_path_segment("thread id", id).is_ok())
    }

    async fn thread_root_for_send(&self, thread_id: Option<&str>) -> Option<String> {
        let candidate = self.thread_candidate_for_send(thread_id)?;
        self.resolve_root_id(candidate).await
    }

    async fn resolve_root_id(&self, post_id: &str) -> Option<String> {
        let candidate = match mattermost_api_path_segment("post id", post_id) {
            Ok(candidate) => candidate,
            Err(err) => {
                warn!(
                    error = %err,
                    "Mattermost refused unsafe thread root lookup path segment"
                );
                return None;
            }
        };
        let url = format!("{}/api/v4/posts/{}", self.config.server_url, candidate);
        let Ok(resp) = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.config.token))
            .send()
            .await
        else {
            return Some(candidate.to_string());
        };

        if !resp.status().is_success() {
            return Some(candidate.to_string());
        }

        let Ok(post) = resp.json::<serde_json::Value>().await else {
            return Some(candidate.to_string());
        };
        let root = post
            .get("root_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or(candidate)
            .to_string();
        match mattermost_api_path_segment("root id", &root) {
            Ok(_) => Some(root),
            Err(err) => {
                warn!(
                    error = %err,
                    "Mattermost refused unsafe resolved thread root path segment"
                );
                None
            }
        }
    }

    async fn send_file_with_thread(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        thread_id: Option<&str>,
        notify: bool,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::mime_from_extension;

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

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

        let root_id = self.thread_root_for_send(thread_id).await;
        let body = mattermost_post_body(
            chat_id,
            caption.unwrap_or(""),
            Some(file_ids.clone()),
            root_id.as_deref(),
        );

        match self.post_body_once(&body).await {
            Ok(_) => Ok(()),
            Err(err) if notify && root_id.is_some() && err.is_broken_thread_root() => {
                let mut flat_body =
                    mattermost_post_body(chat_id, caption.unwrap_or(""), Some(file_ids), None);
                flat_body["message"] = serde_json::json!(format!(
                    "Mattermost thread delivery failed; posting final reply in channel.\n\n{}",
                    caption.unwrap_or("")
                )
                .trim());
                self.post_body_once(&flat_body)
                    .await
                    .map(|_| ())
                    .map_err(MattermostPostFailure::into_gateway_error)
            }
            Err(err) => Err(err.into_gateway_error()),
        }
    }

    /// Parse a Mattermost WebSocket event into an incoming message.
    ///
    /// Only `posted` events that contain a valid post JSON are returned.
    pub fn parse_ws_event(event: &MattermostWsEvent) -> Option<IncomingMattermostMessage> {
        Self::parse_ws_event_with_reply_mode(event, "off")
    }

    pub fn parse_ws_event_with_reply_mode(
        event: &MattermostWsEvent,
        reply_to_mode: &str,
    ) -> Option<IncomingMattermostMessage> {
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
        let channel_type = data
            .get("channel_type")
            .or_else(|| post.get("channel_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut thread_id = post
            .get("root_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);
        if thread_id.is_none()
            && reply_to_mode.eq_ignore_ascii_case("thread")
            && channel_type != "D"
        {
            thread_id = Some(post_id.clone());
        }

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
            thread_id,
            is_bot,
        })
    }

    fn parse_ws_event_for_config(
        &self,
        event: &MattermostWsEvent,
    ) -> Option<IncomingMattermostMessage> {
        Self::parse_ws_event_with_reply_mode(event, &self.config.reply_to_mode)
    }

    fn map_ws_error(err: tokio_tungstenite::tungstenite::Error) -> GatewayError {
        match err {
            tokio_tungstenite::tungstenite::Error::Http(response)
                if matches!(response.status().as_u16(), 401 | 403) =>
            {
                GatewayError::Auth(format!(
                    "Mattermost WebSocket auth error: {}",
                    response.status()
                ))
            }
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                GatewayError::ConnectionFailed(format!(
                    "Mattermost WebSocket handshake error: {}",
                    response.status()
                ))
            }
            other => GatewayError::ConnectionFailed(format!("Mattermost WebSocket error: {other}")),
        }
    }

    /// Connect to the Mattermost real-time API and forward parsed post events.
    pub async fn ws_connect_and_listen<F>(&self, callback: &mut F) -> Result<(), GatewayError>
    where
        F: FnMut(IncomingMattermostMessage) + Send,
    {
        let url = self.websocket_url();
        let (mut socket, _) = connect_async(url.as_str())
            .await
            .map_err(Self::map_ws_error)?;

        let auth = serde_json::json!({
            "seq": 1,
            "action": "authentication_challenge",
            "data": { "token": self.config.token },
        });
        socket
            .send(Message::Text(auth.to_string().into()))
            .await
            .map_err(Self::map_ws_error)?;

        loop {
            tokio::select! {
                _ = self.stop_signal.notified() => {
                    info!("Mattermost WebSocket stop signal received");
                    break;
                }
                frame = socket.next() => {
                    match frame {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(event) = serde_json::from_str::<MattermostWsEvent>(text.as_str()) {
                                if let Some(message) = self.parse_ws_event_for_config(&event) {
                                    callback(message);
                                }
                            }
                        }
                        Some(Ok(Message::Ping(payload))) => {
                            socket
                                .send(Message::Pong(payload))
                                .await
                                .map_err(Self::map_ws_error)?;
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {}
                        Some(Err(err)) => return Err(Self::map_ws_error(err)),
                    }
                }
            }
        }

        Ok(())
    }

    /// Run the reconnect loop. Auth errors are terminal; transient errors back off.
    pub async fn ws_loop<F>(&self, mut callback: F) -> Result<(), GatewayError>
    where
        F: FnMut(IncomingMattermostMessage) + Send,
    {
        let mut backoff_idx = 0usize;

        while self.base.is_running() {
            match self.ws_connect_and_listen(&mut callback).await {
                Ok(()) => {
                    backoff_idx = 0;
                    if self.base.is_running() {
                        warn!("Mattermost WebSocket disconnected; reconnecting");
                    }
                }
                Err(GatewayError::Auth(msg)) => {
                    self.base.mark_stopped();
                    return Err(GatewayError::Auth(msg));
                }
                Err(err) => {
                    let delay_secs = MATTERMOST_WS_BACKOFF_STEPS
                        [backoff_idx.min(MATTERMOST_WS_BACKOFF_STEPS.len() - 1)];
                    warn!(
                        error = %err,
                        retry_in_secs = delay_secs,
                        "Mattermost WebSocket transient error, backing off"
                    );
                    backoff_idx =
                        (backoff_idx + 1).min(MATTERMOST_WS_BACKOFF_STEPS.len().saturating_sub(1));
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(delay_secs)) => {}
                        _ = self.stop_signal.notified() => break,
                    }
                }
            }
        }

        Ok(())
    }

    #[cfg(test)]
    async fn ws_reconnect_loop_with_backoff<F, Fut>(
        &self,
        backoff_steps: &[u64],
        mut connect_and_listen: F,
    ) -> Result<(), GatewayError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<(), GatewayError>>,
    {
        let mut backoff_idx = 0usize;

        while self.base.is_running() {
            match connect_and_listen().await {
                Ok(()) => {
                    backoff_idx = 0;
                    if self.base.is_running() {
                        warn!("Mattermost WebSocket disconnected; reconnecting");
                    }
                }
                Err(GatewayError::Auth(msg)) => {
                    self.base.mark_stopped();
                    return Err(GatewayError::Auth(msg));
                }
                Err(err) => {
                    let delay_secs = if backoff_steps.is_empty() {
                        0
                    } else {
                        backoff_steps[backoff_idx.min(backoff_steps.len() - 1)]
                    };
                    warn!(
                        error = %err,
                        retry_in_secs = delay_secs,
                        "Mattermost WebSocket transient error, backing off"
                    );
                    backoff_idx = (backoff_idx + 1).min(backoff_steps.len().saturating_sub(1));
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(delay_secs)) => {}
                        _ = self.stop_signal.notified() => break,
                    }
                }
            }
        }

        Ok(())
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
        let post_id = mattermost_api_path_segment("post id", post_id)?;
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

#[derive(Debug)]
struct MattermostPostFailure {
    status: Option<u16>,
    body: String,
}

impl MattermostPostFailure {
    fn is_broken_thread_root(&self) -> bool {
        if !matches!(self.status, Some(400 | 404)) {
            return false;
        }
        let body = self.body.to_ascii_lowercase();
        let rootish = ["root_id", "rootid", "root id", "thread", "post"]
            .iter()
            .any(|needle| body.contains(needle));
        let broken = ["invalid", "not found", "does not exist", "missing"]
            .iter()
            .any(|needle| body.contains(needle));
        rootish && broken
    }

    fn into_gateway_error(self) -> GatewayError {
        let body = self.body;
        GatewayError::SendFailed(match self.status {
            Some(status) => format!("Mattermost API error ({status}): {body}"),
            None => body,
        })
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

fn mattermost_api_path_segment<'a>(label: &str, raw: &'a str) -> Result<&'a str, GatewayError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains("..")
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
    {
        return Err(GatewayError::SendFailed(format!(
            "Mattermost {label} contains unsafe API path segment"
        )));
    }
    Ok(trimmed)
}

fn mattermost_post_body(
    channel_id: &str,
    message: &str,
    file_ids: Option<Vec<String>>,
    root_id: Option<&str>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "channel_id": channel_id,
        "message": message,
    });
    if let Some(file_ids) = file_ids {
        body["file_ids"] = serde_json::json!(file_ids);
    }
    if let Some(root_id) = root_id.map(str::trim).filter(|id| !id.is_empty()) {
        body["root_id"] = serde_json::json!(root_id);
    }
    body
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

    async fn send_message_threaded(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        thread_id: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_text_with_thread(chat_id, text, thread_id, false)
            .await?;
        Ok(())
    }

    async fn send_message_with_options(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let _ = parse_mode;
        self.send_text_with_thread(chat_id, text, options.thread_id.as_deref(), options.notify)
            .await
            .map(|_| ())
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
        self.send_file_with_thread(chat_id, file_path, caption, None, false)
            .await
    }

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        self.send_file_with_thread(
            chat_id,
            file_path,
            caption,
            options.thread_id.as_deref(),
            options.notify,
        )
        .await
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let downloaded = download_media_url(&self.client, image_url).await;

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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_adapter() -> MattermostAdapter {
        MattermostAdapter::new(MattermostConfig {
            server_url: "https://mattermost.example.com/".to_string(),
            token: "test-token".to_string(),
            team_id: None,
            proxy: AdapterProxyConfig::default(),
            reply_to_mode: "off".to_string(),
        })
        .unwrap()
    }

    #[test]
    fn websocket_url_uses_ws_scheme_and_realtime_path() {
        let adapter = test_adapter();
        assert_eq!(
            adapter.websocket_url(),
            "wss://mattermost.example.com/api/v4/websocket"
        );

        let local = MattermostAdapter::new(MattermostConfig {
            server_url: "http://127.0.0.1:8065".to_string(),
            token: "test-token".to_string(),
            team_id: None,
            proxy: AdapterProxyConfig::default(),
            reply_to_mode: "off".to_string(),
        })
        .unwrap();
        assert_eq!(
            local.websocket_url(),
            "ws://127.0.0.1:8065/api/v4/websocket"
        );
    }

    #[test]
    fn post_body_preserves_thread_root_when_present() {
        let body = mattermost_post_body(
            "channel-1",
            "hello",
            Some(vec!["file-1".to_string()]),
            Some("root-post-1"),
        );

        assert_eq!(body["channel_id"], "channel-1");
        assert_eq!(body["message"], "hello");
        assert_eq!(body["file_ids"][0], "file-1");
        assert_eq!(body["root_id"], "root-post-1");
    }

    #[test]
    fn thread_root_for_send_requires_thread_reply_mode() {
        let adapter = test_adapter();
        assert_eq!(adapter.thread_candidate_for_send(Some("root-post-1")), None);

        let mut threaded = test_adapter();
        threaded.config.reply_to_mode = "thread".to_string();
        assert_eq!(
            threaded.thread_candidate_for_send(Some(" root-post-1 ")),
            Some("root-post-1")
        );
        assert_eq!(threaded.thread_candidate_for_send(Some("   ")), None);
        assert_eq!(
            threaded.thread_candidate_for_send(Some("../users/me")),
            None
        );
        assert_eq!(
            threaded.thread_candidate_for_send(Some("posts/root-post-1")),
            None
        );
    }

    #[test]
    fn mattermost_api_path_segment_rejects_traversal_shapes() {
        assert_eq!(
            mattermost_api_path_segment("post id", " post-1 ").unwrap(),
            "post-1"
        );
        for unsafe_id in [
            "../users/me",
            "posts/post-1",
            r"posts\\post-1",
            "post\0id",
            "",
        ] {
            let err = mattermost_api_path_segment("post id", unsafe_id)
                .expect_err("unsafe segment should fail closed");
            assert!(err.to_string().contains("unsafe API path segment"));
        }
    }

    #[test]
    fn broken_thread_root_detection_is_specific() {
        assert!(MattermostPostFailure {
            status: Some(400),
            body: "api.context.invalid_param.app_error: invalid root_id".into(),
        }
        .is_broken_thread_root());
        assert!(!MattermostPostFailure {
            status: Some(500),
            body: "invalid root_id".into(),
        }
        .is_broken_thread_root());
        assert!(!MattermostPostFailure {
            status: Some(400),
            body: "Internal Server Error".into(),
        }
        .is_broken_thread_root());
    }

    #[tokio::test]
    async fn ws_reconnect_loop_stops_on_auth_error_without_retry() {
        let adapter = test_adapter();
        adapter.base.mark_running();
        let attempts = AtomicUsize::new(0);

        let result = adapter
            .ws_reconnect_loop_with_backoff(&[0], || {
                attempts.fetch_add(1, Ordering::SeqCst);
                std::future::ready(Err(GatewayError::Auth("401 Unauthorized".to_string())))
            })
            .await;

        assert!(matches!(result, Err(GatewayError::Auth(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(!adapter.is_running());
    }

    #[tokio::test]
    async fn ws_reconnect_loop_retries_transient_errors() {
        let adapter = test_adapter();
        adapter.base.mark_running();
        let attempts = AtomicUsize::new(0);

        let result = adapter
            .ws_reconnect_loop_with_backoff(&[0], || {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt >= 2 {
                    adapter.base.mark_stopped();
                    std::future::ready(Ok(()))
                } else {
                    std::future::ready(Err(GatewayError::ConnectionFailed(
                        "network timeout".to_string(),
                    )))
                }
            })
            .await;

        assert!(result.is_ok());
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn parse_ws_event_extracts_posted_message() {
        let event = MattermostWsEvent {
            event: "posted".to_string(),
            data: Some(serde_json::json!({
                "post": serde_json::json!({
                    "id": "post-1",
                    "channel_id": "channel-1",
                    "user_id": "user-1",
                    "message": "hello",
                    "props": { "from_bot": "true" }
                }).to_string()
            })),
            broadcast: None,
            seq: Some(2),
        };

        let msg = MattermostAdapter::parse_ws_event(&event).expect("posted event");
        assert_eq!(msg.post_id, "post-1");
        assert_eq!(msg.channel_id, "channel-1");
        assert_eq!(msg.user_id, "user-1");
        assert_eq!(msg.message, "hello");
        assert_eq!(msg.thread_id, None);
        assert!(msg.is_bot);
    }

    #[test]
    fn parse_ws_event_preserves_existing_thread_root() {
        let event = MattermostWsEvent {
            event: "posted".to_string(),
            data: Some(serde_json::json!({
                "channel_type": "O",
                "post": serde_json::json!({
                    "id": "reply-post-1",
                    "root_id": "root-post-1",
                    "channel_id": "channel-1",
                    "user_id": "user-1",
                    "message": "thread reply"
                }).to_string()
            })),
            broadcast: None,
            seq: Some(3),
        };

        let msg = MattermostAdapter::parse_ws_event_with_reply_mode(&event, "thread")
            .expect("thread reply event");
        assert_eq!(msg.post_id, "reply-post-1");
        assert_eq!(msg.thread_id.as_deref(), Some("root-post-1"));
    }

    #[test]
    fn parse_ws_event_treats_top_level_channel_post_as_thread_root_in_thread_mode() {
        let event = MattermostWsEvent {
            event: "posted".to_string(),
            data: Some(serde_json::json!({
                "channel_type": "O",
                "post": serde_json::json!({
                    "id": "top-post-1",
                    "root_id": "",
                    "channel_id": "channel-1",
                    "user_id": "user-1",
                    "message": "top level"
                }).to_string()
            })),
            broadcast: None,
            seq: Some(4),
        };

        let msg = MattermostAdapter::parse_ws_event_with_reply_mode(&event, "thread")
            .expect("top-level event");
        assert_eq!(msg.thread_id.as_deref(), Some("top-post-1"));
    }

    #[test]
    fn parse_ws_event_does_not_seed_dm_thread_root() {
        let event = MattermostWsEvent {
            event: "posted".to_string(),
            data: Some(serde_json::json!({
                "channel_type": "D",
                "post": serde_json::json!({
                    "id": "dm-post-1",
                    "root_id": "",
                    "channel_id": "dm-channel",
                    "user_id": "user-1",
                    "message": "hello"
                }).to_string()
            })),
            broadcast: None,
            seq: Some(5),
        };

        let msg =
            MattermostAdapter::parse_ws_event_with_reply_mode(&event, "thread").expect("dm event");
        assert_eq!(msg.thread_id, None);
    }

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
