//! OpenAI-compatible HTTP API server platform adapter.
//!
//! Exposes `/v1/chat/completions` (streaming SSE + non-streaming) and
//! `/v1/responses` endpoints, allowing any OpenAI-compatible client to
//! interact with the Hermes Agent gateway.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

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
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    8090
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

// ---------------------------------------------------------------------------
// Pending response mailbox
// ---------------------------------------------------------------------------

/// Holds pending responses that will be sent back to HTTP callers.
#[derive(Default)]
struct ResponseMailbox {
    pending: HashMap<String, mpsc::Sender<String>>,
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
        }
    }

    pub fn config(&self) -> &ApiServerConfig {
        &self.config
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

        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid address: {e}")))?;

        let mailbox = self.mailbox.clone();
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
                                let auth_token = auth_token.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_connection(stream, peer, mailbox, auth_token).await {
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

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "api-server"
    }
}

// ---------------------------------------------------------------------------
// Connection handler (minimal HTTP/1.1 without axum dep for compilation)
// ---------------------------------------------------------------------------

async fn handle_connection(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    _mailbox: Arc<RwLock<ResponseMailbox>>,
    auth_token: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 65536];
    let (mut reader, mut writer) = stream.into_split();
    let n = reader.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let path = parts.get(1).copied().unwrap_or("/");

    // Extract Authorization header
    let auth_header = request
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

    match (method, path) {
        ("POST", "/v1/chat/completions") | ("POST", "/v1/responses") => {
            let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(n);
            let body_str = &request[body_start..];

            let parsed: Result<ChatCompletionRequest, _> = serde_json::from_str(body_str);
            match parsed {
                Ok(req) => {
                    let request_id = ApiServerAdapter::make_completion_id();
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let last_msg = req
                        .messages
                        .last()
                        .map(|m| m.content.clone())
                        .unwrap_or_default();

                    // Echo back with a placeholder response for now
                    let reply = format!("Received: {}", last_msg);

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
