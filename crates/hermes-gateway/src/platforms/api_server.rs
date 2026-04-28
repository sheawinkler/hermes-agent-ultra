//! OpenAI-compatible HTTP API server platform adapter.
//!
//! Exposes `/v1/chat/completions` (streaming SSE + non-streaming) and
//! `/v1/responses` endpoints, allowing any OpenAI-compatible client to
//! interact with the Hermes Agent gateway.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::BasePlatformAdapter;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8090
}

fn ip_is_network_accessible(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !v4.is_loopback(),
        IpAddr::V6(v6) => {
            if v6.is_loopback() {
                return false;
            }
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return !mapped.is_loopback();
            }
            true
        }
    }
}

fn is_network_accessible_with_lookup<F>(host: &str, lookup: F) -> bool
where
    F: Fn(&str) -> std::io::Result<Vec<IpAddr>>,
{
    let trimmed = host.trim();
    let candidate = trimmed
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(trimmed);
    if candidate.is_empty() {
        return true;
    }

    if let Ok(ip) = candidate.parse::<IpAddr>() {
        return ip_is_network_accessible(ip);
    }

    match lookup(candidate) {
        Ok(ips) => {
            if ips.is_empty() {
                return true;
            }
            ips.into_iter().any(ip_is_network_accessible)
        }
        Err(_) => true,
    }
}

fn is_network_accessible(host: &str) -> bool {
    is_network_accessible_with_lookup(host, |candidate| {
        (candidate, 0_u16)
            .to_socket_addrs()
            .map(|iter| iter.map(|addr| addr.ip()).collect())
    })
}

fn requires_auth_token_for_bind(host: &str, auth_token: Option<&str>) -> bool {
    is_network_accessible(host) && auth_token.unwrap_or_default().trim().is_empty()
}

impl Default for ApiServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            auth_token: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: UsageInfo,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct UsageInfo {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct StreamChunkResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<StreamChoice>,
}

#[derive(Debug, Serialize)]
pub struct StreamChoice {
    pub index: u32,
    pub delta: StreamDelta,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StreamDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ApiError,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub message: String,
    pub r#type: String,
    pub code: String,
}

#[derive(Debug, Clone)]
pub struct ApiInboundRequest {
    pub request_id: String,
    pub session_id: String,
    pub user_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub personality: Option<String>,
    pub prompt: String,
}

// ---------------------------------------------------------------------------
// Pending response mailbox
// ---------------------------------------------------------------------------

/// Holds pending responses that will be sent back to HTTP callers.
#[derive(Default)]
struct ResponseMailbox {
    pending: HashMap<String, mpsc::Sender<String>>,
}

#[derive(Default)]
struct RunCancelRegistry {
    pending: HashMap<String, Arc<Notify>>,
}

// ---------------------------------------------------------------------------
// ApiServerAdapter
// ---------------------------------------------------------------------------

pub struct ApiServerAdapter {
    base: BasePlatformAdapter,
    config: ApiServerConfig,
    stop_signal: Arc<Notify>,
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
    mailbox: Arc<RwLock<ResponseMailbox>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
}

impl ApiServerAdapter {
    pub fn new(config: ApiServerConfig) -> Self {
        let token = config.auth_token.clone().unwrap_or_default();
        let base = BasePlatformAdapter::new(if token.is_empty() {
            "api-server"
        } else {
            &token
        });
        Self {
            base,
            config,
            stop_signal: Arc::new(Notify::new()),
            shutdown_tx: RwLock::new(None),
            mailbox: Arc::new(RwLock::new(ResponseMailbox::default())),
            run_cancels: Arc::new(RwLock::new(RunCancelRegistry::default())),
            inbound_tx: Arc::new(RwLock::new(None)),
        }
    }

    pub fn config(&self) -> &ApiServerConfig {
        &self.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<ApiInboundRequest>) {
        *self.inbound_tx.write().await = Some(tx);
    }

    fn make_completion_id() -> String {
        format!(
            "chatcmpl-{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
        )
    }

    fn make_non_streaming_response(
        request_id: &str,
        model: &str,
        content: &str,
    ) -> ChatCompletionResponse {
        let prompt_tokens = 0_u32;
        let completion_tokens = content.len() as u32 / 4;
        ChatCompletionResponse {
            id: request_id.to_string(),
            object: "chat.completion".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".to_string(),
                    content: content.to_string(),
                },
                finish_reason: "stop".to_string(),
            }],
            usage: UsageInfo {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            },
        }
    }

    fn make_stream_chunk(
        request_id: &str,
        model: &str,
        content: Option<&str>,
        finish: bool,
    ) -> StreamChunkResponse {
        StreamChunkResponse {
            id: request_id.to_string(),
            object: "chat.completion.chunk".to_string(),
            created: chrono::Utc::now().timestamp(),
            model: model.to_string(),
            choices: vec![StreamChoice {
                index: 0,
                delta: StreamDelta {
                    role: if content.is_none() && !finish {
                        Some("assistant".to_string())
                    } else {
                        None
                    },
                    content: content.map(|s| s.to_string()),
                },
                finish_reason: if finish {
                    Some("stop".to_string())
                } else {
                    None
                },
            }],
        }
    }
}

#[async_trait]
impl PlatformAdapter for ApiServerAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "API server adapter starting on {}:{}",
            self.config.host, self.config.port
        );

        if requires_auth_token_for_bind(&self.config.host, self.config.auth_token.as_deref()) {
            return Err(GatewayError::Auth(format!(
                "Refusing to bind API server to '{}' without auth token. Set api_server.auth_token (or API_SERVER_KEY) or bind to loopback.",
                self.config.host
            )));
        }

        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid address: {e}")))?;

        let mailbox = self.mailbox.clone();
        let run_cancels = self.run_cancels.clone();
        let inbound_tx = self.inbound_tx.clone();
        let auth_token = self.config.auth_token.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        tokio::spawn(async move {
            let mailbox = mailbox;
            let auth_token = auth_token;

            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("API server failed to bind: {e}");
                    return;
                }
            };

            info!("API server listening on {addr}");

            let mut shutdown_rx = shutdown_rx;

            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer)) => {
                                let mailbox = mailbox.clone();
                                let run_cancels = run_cancels.clone();
                                let inbound_tx = inbound_tx.clone();
                                let auth_token = auth_token.clone();
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        handle_connection(
                                            stream,
                                            peer,
                                            mailbox,
                                            run_cancels,
                                            inbound_tx,
                                            auth_token,
                                        )
                                        .await
                                    {
                                        debug!("API connection error from {peer}: {e}");
                                    }
                                });
                            }
                            Err(e) => {
                                warn!("API server accept error: {e}");
                            }
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("API server shutting down");
                        break;
                    }
                }
            }
        });

        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("API server adapter stopping");
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }
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
        let mailbox = self.mailbox.read().await;
        if let Some(tx) = mailbox.pending.get(chat_id) {
            let _ = tx.send(text.to_string()).await;
        } else {
            debug!(chat_id = chat_id, "No pending API request for chat_id");
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        _message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.send_message(chat_id, text, None).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let msg = if let Some(cap) = caption {
            format!("[File: {}] {}", file_path, cap)
        } else {
            format!("[File: {}]", file_path)
        };
        self.send_message(chat_id, &msg, None).await
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let marker = image_marker_message(image_url, caption);
        self.send_message(chat_id, &marker, Some(ParseMode::Plain))
            .await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "api-server"
    }
}

fn image_marker_message(image_url: &str, caption: Option<&str>) -> String {
    let mut marker = format!("[image] {image_url}");
    if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
        marker.push_str(&format!(" | caption={cap}"));
    }
    marker
}

// ---------------------------------------------------------------------------
// Connection handler (minimal HTTP/1.1 without axum dep for compilation)
// ---------------------------------------------------------------------------

async fn handle_connection(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    mailbox: Arc<RwLock<ResponseMailbox>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    auth_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncWriteExt;

    let (mut reader, mut writer) = stream.into_split();
    let raw = read_http_request(&mut reader).await?;
    if raw.is_empty() {
        return Ok(());
    }

    let Some(header_end) = find_bytes(&raw, b"\r\n\r\n") else {
        let body = r#"{"error":{"message":"Invalid HTTP request","type":"invalid_request_error","code":"400"}}"#;
        let resp = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    };

    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let body_bytes = &raw[(header_end + 4).min(raw.len())..];
    let first_line = header_text.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let path = parts.get(1).copied().unwrap_or("/");

    // Extract Authorization header
    let auth_header = header_text
        .lines()
        .find(|l| l.to_lowercase().starts_with("authorization:"))
        .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string());

    if let Some(ref expected) = auth_token {
        let valid = auth_header
            .as_deref()
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(|t| t == expected)
            .unwrap_or(false);
        if !valid {
            let err = serde_json::json!({"error":{"message":"Unauthorized","type":"auth_error","code":"401"}});
            let body = serde_json::to_string(&err)?;
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(), body
            );
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    if method == "POST" {
        if let Some(run_id) = parse_stop_run_path(path) {
            let stop_waiter = {
                let guard = run_cancels.read().await;
                guard.pending.get(run_id).cloned()
            };

            if let Some(waiter) = stop_waiter {
                waiter.notify_waiters();
                let body = serde_json::json!({
                    "id": run_id,
                    "object": "run",
                    "status": "stopped"
                });
                let payload = serde_json::to_string(&body)?;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let err = serde_json::json!({
                    "error": {
                        "message":"Run not found",
                        "type":"not_found",
                        "code":"404"
                    }
                });
                let payload = serde_json::to_string(&err)?;
                let resp = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    payload.len(),
                    payload
                );
                writer.write_all(resp.as_bytes()).await?;
            }
            return Ok(());
        }
    }

    match (method, path) {
        ("POST", "/v1/chat/completions") | ("POST", "/v1/responses") => {
            let body_str = String::from_utf8_lossy(body_bytes);

            let parsed: Result<ChatCompletionRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let request_id = ApiServerAdapter::make_completion_id();
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let prompt = build_prompt_from_messages(&req.messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let err = serde_json::json!({
                            "error": {
                                "message": "Request must include at least one user message",
                                "type":"invalid_request_error",
                                "code":"400"
                            }
                        });
                        let body = serde_json::to_string(&err)?;
                        let resp = format!(
                            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req.session_id.unwrap_or_else(|| request_id.clone());
                    let mailbox_key = session_id.clone();
                    let user_id = req
                        .user
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let inbound = ApiInboundRequest {
                        request_id: request_id.clone(),
                        session_id,
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    let (reply_tx, mut reply_rx) = mpsc::channel::<String>(1);
                    let cancel_waiter = Arc::new(Notify::new());
                    {
                        let mut guard = mailbox.write().await;
                        guard.pending.insert(mailbox_key.clone(), reply_tx);
                    }
                    {
                        let mut guard = run_cancels.write().await;
                        guard
                            .pending
                            .insert(request_id.clone(), cancel_waiter.clone());
                        guard
                            .pending
                            .insert(mailbox_key.clone(), cancel_waiter.clone());
                    }

                    let maybe_inbound = inbound_tx.read().await.clone();
                    let Some(tx) = maybe_inbound else {
                        let mut guard = mailbox.write().await;
                        guard.pending.remove(&mailbox_key);
                        let err = serde_json::json!({
                            "error": {
                                "message":"Gateway inbound pipeline is not configured",
                                "type":"service_unavailable",
                                "code":"503"
                            }
                        });
                        let body = serde_json::to_string(&err)?;
                        let resp = format!(
                            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    };

                    if tx.send(inbound).await.is_err() {
                        let mut guard = mailbox.write().await;
                        guard.pending.remove(&mailbox_key);
                        let err = serde_json::json!({
                            "error": {
                                "message":"Gateway inbound queue is unavailable",
                                "type":"service_unavailable",
                                "code":"503"
                            }
                        });
                        let body = serde_json::to_string(&err)?;
                        let resp = format!(
                            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let reply = tokio::select! {
                        _ = cancel_waiter.notified() => {
                            let err = serde_json::json!({
                                "error": {
                                    "message":"Run stopped",
                                    "type":"cancelled_error",
                                    "code":"409"
                                }
                            });
                            let body = serde_json::to_string(&err)?;
                            let resp = format!(
                                "HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            writer.write_all(resp.as_bytes()).await?;
                            let mut guard = mailbox.write().await;
                            guard.pending.remove(&mailbox_key);
                            let mut cancels = run_cancels.write().await;
                            cancels.pending.remove(&request_id);
                            cancels.pending.remove(&mailbox_key);
                            return Ok(());
                        }
                        timeout_result = tokio::time::timeout(Duration::from_secs(120), reply_rx.recv()) => {
                            match timeout_result {
                                Ok(Some(msg)) => msg,
                                Ok(None) => {
                                    let err = serde_json::json!({
                                        "error": {
                                            "message":"Gateway closed response channel",
                                            "type":"internal_error",
                                            "code":"502"
                                        }
                                    });
                                    let body = serde_json::to_string(&err)?;
                                    let resp = format!(
                                        "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                        body.len(),
                                        body
                                    );
                                    writer.write_all(resp.as_bytes()).await?;
                                    let mut guard = mailbox.write().await;
                                    guard.pending.remove(&mailbox_key);
                                    let mut cancels = run_cancels.write().await;
                                    cancels.pending.remove(&request_id);
                                    cancels.pending.remove(&mailbox_key);
                                    return Ok(());
                                }
                                Err(_) => {
                                    let err = serde_json::json!({
                                        "error": {
                                            "message":"Gateway response timeout",
                                            "type":"timeout_error",
                                            "code":"504"
                                        }
                                    });
                                    let body = serde_json::to_string(&err)?;
                                    let resp = format!(
                                        "HTTP/1.1 504 Gateway Timeout\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                                        body.len(),
                                        body
                                    );
                                    writer.write_all(resp.as_bytes()).await?;
                                    let mut guard = mailbox.write().await;
                                    guard.pending.remove(&mailbox_key);
                                    let mut cancels = run_cancels.write().await;
                                    cancels.pending.remove(&request_id);
                                    cancels.pending.remove(&mailbox_key);
                                    return Ok(());
                                }
                            }
                        }
                    };

                    {
                        let mut guard = mailbox.write().await;
                        guard.pending.remove(&mailbox_key);
                    }
                    {
                        let mut guard = run_cancels.write().await;
                        guard.pending.remove(&request_id);
                        guard.pending.remove(&mailbox_key);
                    }

                    if req.stream {
                        let header = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n";
                        writer.write_all(header.as_bytes()).await?;

                        // Role chunk
                        let role_chunk =
                            ApiServerAdapter::make_stream_chunk(&request_id, &model, None, false);
                        let data = format!("data: {}\n\n", serde_json::to_string(&role_chunk)?);
                        writer.write_all(data.as_bytes()).await?;

                        // Content chunks
                        for chunk in reply.as_bytes().chunks(20) {
                            let text = String::from_utf8_lossy(chunk);
                            let sc = ApiServerAdapter::make_stream_chunk(
                                &request_id,
                                &model,
                                Some(&text),
                                false,
                            );
                            let data = format!("data: {}\n\n", serde_json::to_string(&sc)?);
                            writer.write_all(data.as_bytes()).await?;
                        }

                        // Finish chunk
                        let done_chunk =
                            ApiServerAdapter::make_stream_chunk(&request_id, &model, None, true);
                        let data = format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            serde_json::to_string(&done_chunk)?
                        );
                        writer.write_all(data.as_bytes()).await?;
                    } else {
                        let response = ApiServerAdapter::make_non_streaming_response(
                            &request_id,
                            &model,
                            &reply,
                        );
                        let body = serde_json::to_string(&response)?;
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(), body
                        );
                        writer.write_all(resp.as_bytes()).await?;
                    }
                }
                Err(e) => {
                    let err = serde_json::json!({"error":{"message":format!("Invalid request: {e}"),"type":"invalid_request_error","code":"400"}});
                    let body = serde_json::to_string(&err)?;
                    let resp = format!(
                        "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(), body
                    );
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("GET", "/health") | ("GET", "/") => {
            let body = r#"{"status":"ok","adapter":"api-server"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            writer.write_all(resp.as_bytes()).await?;
        }
        _ => {
            let body = r#"{"error":{"message":"Not found","type":"not_found","code":"404"}}"#;
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(), body
            );
            writer.write_all(resp.as_bytes()).await?;
        }
    }

    Ok(())
}

fn build_prompt_from_messages(messages: &[ChatMessage]) -> Option<String> {
    if messages.is_empty() {
        return None;
    }

    let has_user_message = messages
        .iter()
        .any(|m| m.role.trim().eq_ignore_ascii_case("user"));
    if !has_user_message {
        return None;
    }

    if messages.len() == 1 {
        let only = &messages[0];
        if only.role.trim().eq_ignore_ascii_case("user") {
            return Some(only.content.clone());
        }
    }

    let mut prompt = String::new();
    for (idx, msg) in messages.iter().enumerate() {
        let role = msg.role.trim();
        let role_upper = role.to_ascii_uppercase();
        if idx > 0 {
            prompt.push_str("\n\n");
        }
        prompt.push('[');
        prompt.push_str(if role.is_empty() {
            "MESSAGE"
        } else {
            role_upper.as_str()
        });
        prompt.push_str("]\n");
        prompt.push_str(&msg.content);
    }

    if prompt.trim().is_empty() {
        None
    } else {
        Some(prompt)
    }
}

fn parse_stop_run_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/stop")?;
    if run_id.is_empty() {
        None
    } else {
        Some(run_id)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn parse_content_length(headers: &str) -> usize {
    headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0)
}

async fn read_http_request(
    reader: &mut tokio::net::tcp::OwnedReadHalf,
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncReadExt;

    let mut buf = Vec::with_capacity(16 * 1024);
    let mut chunk = [0_u8; 8192];
    let mut expected_total: Option<usize> = None;

    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 2 * 1024 * 1024 {
            break;
        }

        if expected_total.is_none() {
            if let Some(header_end) = find_bytes(&buf, b"\r\n\r\n") {
                let header_text = String::from_utf8_lossy(&buf[..header_end]);
                let body_len = parse_content_length(&header_text);
                expected_total = Some(header_end + 4 + body_len);
            }
        }
        if let Some(total) = expected_total {
            if buf.len() >= total {
                break;
            }
        }
    }

    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_content_length_is_case_insensitive() {
        let h = "POST /x HTTP/1.1\r\nHost: localhost\r\nContent-Length: 42\r\n\r\n";
        assert_eq!(parse_content_length(h), 42);
        let h2 = "POST /x HTTP/1.1\r\ncontent-length: 9\r\n\r\n";
        assert_eq!(parse_content_length(h2), 9);
    }

    #[test]
    fn build_prompt_from_messages_preserves_single_user_prompt() {
        let msgs = vec![ChatMessage {
            role: "user".into(),
            content: "final prompt".into(),
        }];
        assert_eq!(
            build_prompt_from_messages(&msgs).as_deref(),
            Some("final prompt")
        );
    }

    #[test]
    fn build_prompt_from_messages_preserves_multi_message_transcript() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "rules".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "hello".into(),
            },
            ChatMessage {
                role: "user".into(),
                content: "final prompt".into(),
            },
        ];
        let rendered = build_prompt_from_messages(&msgs).expect("prompt should exist");
        assert!(rendered.contains("[SYSTEM]\nrules"));
        assert!(rendered.contains("[ASSISTANT]\nhello"));
        assert!(rendered.contains("[USER]\nfinal prompt"));
    }

    #[test]
    fn build_prompt_from_messages_requires_user_message() {
        let msgs = vec![
            ChatMessage {
                role: "system".into(),
                content: "rules".into(),
            },
            ChatMessage {
                role: "assistant".into(),
                content: "hello".into(),
            },
        ];
        assert!(build_prompt_from_messages(&msgs).is_none());
    }

    #[test]
    fn network_accessibility_classifies_ip_binds() {
        assert!(!is_network_accessible("127.0.0.1"));
        assert!(!is_network_accessible("::1"));
        assert!(!is_network_accessible("::ffff:127.0.0.1"));
        assert!(is_network_accessible("0.0.0.0"));
        assert!(is_network_accessible("::"));
        assert!(is_network_accessible("10.0.0.1"));
        assert!(is_network_accessible("::ffff:0.0.0.0"));
    }

    #[test]
    fn network_accessibility_hostname_resolution_is_fail_closed() {
        assert!(!is_network_accessible_with_lookup("localhost", |_| {
            Ok(vec!["127.0.0.1".parse().expect("loopback should parse")])
        }));

        assert!(is_network_accessible_with_lookup(
            "dual-stack.local",
            |_| {
                Ok(vec![
                    "127.0.0.1".parse().expect("loopback should parse"),
                    "10.0.0.7".parse().expect("private ip should parse"),
                ])
            }
        ));

        assert!(is_network_accessible_with_lookup(
            "nonexistent.invalid",
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "resolution failed",
                ))
            }
        ));
    }

    #[test]
    fn bind_guard_requires_token_only_for_network_accessible_hosts() {
        assert!(!requires_auth_token_for_bind("127.0.0.1", None));
        assert!(!requires_auth_token_for_bind("::1", Some(" ")));
        assert!(requires_auth_token_for_bind("0.0.0.0", None));
        assert!(requires_auth_token_for_bind("::", Some("")));
        assert!(!requires_auth_token_for_bind("0.0.0.0", Some("sk-test")));
    }

    #[test]
    fn image_marker_message_with_caption() {
        let marker = image_marker_message("https://cdn.example.com/a.png", Some("Diagram"));
        assert_eq!(
            marker,
            "[image] https://cdn.example.com/a.png | caption=Diagram"
        );
    }

    #[test]
    fn image_marker_message_without_caption() {
        let marker = image_marker_message("https://cdn.example.com/a.png", Some("   "));
        assert_eq!(marker, "[image] https://cdn.example.com/a.png");
    }

    #[test]
    fn parse_stop_run_path_accepts_valid_route() {
        assert_eq!(
            parse_stop_run_path("/v1/runs/run_abc123/stop"),
            Some("run_abc123")
        );
    }

    #[test]
    fn parse_stop_run_path_rejects_invalid_route() {
        assert_eq!(parse_stop_run_path("/v1/runs//stop"), None);
        assert_eq!(parse_stop_run_path("/v1/runs/run_abc123"), None);
        assert_eq!(parse_stop_run_path("/v1/chat/completions"), None);
    }
}
