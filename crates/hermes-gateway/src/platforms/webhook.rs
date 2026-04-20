//! Generic webhook platform adapter.
//!
//! Receives incoming HTTP webhooks with HMAC-SHA256 signature verification
//! and routes JSON payloads to the gateway. Outbound messages are queued
//! for the next poll from the external service.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::sync::{Notify, RwLock};
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::BasePlatformAdapter;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookConfig {
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    #[serde(default = "default_webhook_path")]
    pub path: String,
    pub secret: String,
}

fn default_webhook_port() -> u16 {
    9000
}
fn default_webhook_path() -> String {
    "/webhook".to_string()
}

// ---------------------------------------------------------------------------
// Incoming payload
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookPayload {
    pub chat_id: String,
    pub user_id: Option<String>,
    pub text: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Outbound message queue
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct OutboundMessage {
    pub chat_id: String,
    pub text: String,
}

// ---------------------------------------------------------------------------
// WebhookAdapter
// ---------------------------------------------------------------------------

pub struct WebhookAdapter {
    base: BasePlatformAdapter,
    config: WebhookConfig,
    stop_signal: Arc<Notify>,
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
    outbound_queue: Arc<RwLock<VecDeque<OutboundMessage>>>,
    inbound_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<WebhookPayload>>>>,
}

impl WebhookAdapter {
    pub fn new(config: WebhookConfig) -> Self {
        let base = BasePlatformAdapter::new(&config.secret);
        Self {
            base,
            config,
            stop_signal: Arc::new(Notify::new()),
            shutdown_tx: RwLock::new(None),
            outbound_queue: Arc::new(RwLock::new(VecDeque::new())),
            inbound_tx: Arc::new(RwLock::new(None)),
        }
    }

    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }

    /// Set a channel to forward inbound webhook payloads to.
    pub async fn set_inbound_sender(&self, tx: tokio::sync::mpsc::Sender<WebhookPayload>) {
        *self.inbound_tx.write().await = Some(tx);
    }

    /// Drain all queued outbound messages.
    pub async fn drain_outbound(&self) -> Vec<OutboundMessage> {
        let mut queue = self.outbound_queue.write().await;
        queue.drain(..).collect()
    }

    /// Verify HMAC-SHA256 signature.
    fn verify_signature(secret: &str, body: &[u8], signature: &str) -> bool {
        type HmacSha256 = Hmac<Sha256>;

        let sig_clean = signature
            .trim()
            .strip_prefix("sha256=")
            .unwrap_or(signature.trim());
        let expected_sig = match decode_hex(sig_clean) {
            Some(bytes) => bytes,
            None => return false,
        };

        let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
            Ok(mac) => mac,
            Err(_) => return false,
        };
        mac.update(body);
        mac.verify_slice(&expected_sig).is_ok()
    }
}

fn decode_hex(input: &str) -> Option<Vec<u8>> {
    if input.is_empty() || input.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    let mut chars = input.chars();
    while let (Some(hi), Some(lo)) = (chars.next(), chars.next()) {
        let hi = hi.to_digit(16)?;
        let lo = lo.to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

#[async_trait]
impl PlatformAdapter for WebhookAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Webhook adapter starting on port {} at path {}",
            self.config.port, self.config.path
        );

        let addr: SocketAddr = format!("0.0.0.0:{}", self.config.port)
            .parse()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid address: {e}")))?;

        let secret = self.config.secret.clone();
        let expected_path = self.config.path.clone();
        let outbound_queue = self.outbound_queue.clone();
        let inbound_tx = self.inbound_tx.clone();

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Webhook server failed to bind: {e}");
                    return;
                }
            };

            info!("Webhook server listening on {addr}");
            let mut shutdown_rx = shutdown_rx;

            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer)) => {
                                let secret = secret.clone();
                                let expected_path = expected_path.clone();
                                let outbound_queue = outbound_queue.clone();
                                let inbound_tx = inbound_tx.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_webhook_request(
                                        stream, peer, &secret, &expected_path,
                                        outbound_queue, inbound_tx,
                                    ).await {
                                        debug!("Webhook connection error from {peer}: {e}");
                                    }
                                });
                            }
                            Err(e) => warn!("Webhook accept error: {e}"),
                        }
                    }
                    _ = &mut shutdown_rx => {
                        info!("Webhook server shutting down");
                        break;
                    }
                }
            }
        });

        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Webhook adapter stopping");
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
        let mut queue = self.outbound_queue.write().await;
        queue.push_back(OutboundMessage {
            chat_id: chat_id.to_string(),
            text: text.to_string(),
        });
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
        let text = if let Some(cap) = caption {
            format!("[file:{}] {}", file_path, cap)
        } else {
            format!("[file:{}]", file_path)
        };
        self.send_message(chat_id, &text, None).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }
    fn platform_name(&self) -> &str {
        "webhook"
    }
}

// ---------------------------------------------------------------------------
// HTTP request handler
// ---------------------------------------------------------------------------

async fn handle_webhook_request(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    secret: &str,
    expected_path: &str,
    outbound_queue: Arc<RwLock<VecDeque<OutboundMessage>>>,
    inbound_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<WebhookPayload>>>>,
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

    let signature = request
        .lines()
        .find(|l| l.to_lowercase().starts_with("x-signature:"))
        .map(|l| l.splitn(2, ':').nth(1).unwrap_or("").trim().to_string())
        .unwrap_or_default();

    if method == "POST" && path == expected_path {
        let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(n);
        let body_bytes = &buf[body_start..n];

        if !WebhookAdapter::verify_signature(secret, body_bytes, &signature) {
            let resp =
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 22\r\n\r\n{\"error\":\"bad signature\"}";
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }

        let body_str = String::from_utf8_lossy(body_bytes);
        match serde_json::from_str::<WebhookPayload>(&body_str) {
            Ok(payload) => {
                if let Some(tx) = inbound_tx.read().await.as_ref() {
                    let _ = tx.send(payload).await;
                }
                let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"status\":\"ok\"}";
                writer.write_all(resp.as_bytes()).await?;
            }
            Err(e) => {
                let body = format!("{{\"error\":\"invalid payload: {e}\"}}");
                let resp = format!(
                    "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(), body
                );
                writer.write_all(resp.as_bytes()).await?;
            }
        }
    } else if method == "GET" && path == format!("{}/outbound", expected_path).as_str() {
        let messages = outbound_queue.write().await.drain(..).collect::<Vec<_>>();
        let body = serde_json::to_string(&messages)?;
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(resp.as_bytes()).await?;
    } else {
        let resp = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        writer.write_all(resp.as_bytes()).await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let bytes = mac.finalize().into_bytes();
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    }

    #[test]
    fn verify_signature_accepts_prefixed_sha256_digest() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        let sig = format!("sha256={}", sign(secret, body));
        assert!(WebhookAdapter::verify_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_accepts_raw_hex_digest() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        let sig = sign(secret, body);
        assert!(WebhookAdapter::verify_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_rejects_malformed_signature() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        assert!(!WebhookAdapter::verify_signature(
            secret,
            body,
            "sha256=xyz"
        ));
        assert!(!WebhookAdapter::verify_signature(secret, body, ""));
    }

    #[test]
    fn verify_signature_rejects_tampered_payload() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        let sig = format!("sha256={}", sign(secret, body));
        let tampered = br#"{"chat_id":"c1","text":"bye"}"#;
        assert!(!WebhookAdapter::verify_signature(secret, tampered, &sig));
    }
}
