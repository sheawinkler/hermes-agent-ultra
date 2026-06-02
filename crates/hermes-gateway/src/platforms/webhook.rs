//! Generic webhook platform adapter.
//!
//! Receives incoming HTTP webhooks with HMAC-SHA256 signature verification
//! and routes JSON payloads to the gateway. Outbound messages are queued
//! for the next poll from the external service.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use tokio::io::AsyncWriteExt;
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
    #[serde(default = "default_webhook_host")]
    pub host: String,
    #[serde(default = "default_webhook_port")]
    pub port: u16,
    #[serde(default = "default_webhook_path")]
    pub path: String,
    pub secret: String,
    #[serde(default = "default_webhook_rate_limit")]
    pub rate_limit: u32,
    #[serde(default = "default_webhook_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default)]
    pub routes: BTreeMap<String, WebhookRouteConfig>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebhookRouteConfig {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub event_types: Vec<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub deliver: Option<String>,
    #[serde(default)]
    pub deliver_only: bool,
    #[serde(default)]
    pub deliver_extra: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub idempotency_header: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<u32>,
}

const INSECURE_NO_AUTH: &str = "INSECURE_NO_AUTH";
const DELIVERY_TTL: Duration = Duration::from_secs(60 * 60);
const RATE_WINDOW: Duration = Duration::from_secs(60);

fn default_webhook_host() -> String {
    "0.0.0.0".to_string()
}
fn default_webhook_port() -> u16 {
    9000
}
fn default_webhook_path() -> String {
    "/webhook".to_string()
}
fn default_webhook_rate_limit() -> u32 {
    30
}
fn default_webhook_max_body_bytes() -> usize {
    1_048_576
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub chat_id: String,
    pub text: String,
}

#[derive(Debug, Clone)]
struct RateWindow {
    started_at: Instant,
    count: u32,
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
    seen_deliveries: Arc<RwLock<HashMap<String, Instant>>>,
    rate_windows: Arc<RwLock<HashMap<String, RateWindow>>>,
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
            seen_deliveries: Arc::new(RwLock::new(HashMap::new())),
            rate_windows: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn config(&self) -> &WebhookConfig {
        &self.config
    }

    fn validate_auth_safety(config: &WebhookConfig) -> Result<(), GatewayError> {
        if config.secret.trim() == INSECURE_NO_AUTH && !is_loopback_host(&config.host) {
            return Err(GatewayError::Auth(
                "webhook secret INSECURE_NO_AUTH is refused on non-loopback binds; unauthenticated webhook routes are only safe on loopback test binds"
                    .to_string(),
            ));
        }
        Ok(())
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
    fn verify_hmac_signature(secret: &str, body: &[u8], signature: &str) -> bool {
        if secret == INSECURE_NO_AUTH {
            return true;
        }

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

    fn validate_signature(headers: &HashMap<String, String>, body: &[u8], secret: &str) -> bool {
        if secret == INSECURE_NO_AUTH {
            return true;
        }

        if let Some(token) = headers.get("x-gitlab-token") {
            return constant_time_eq(token.as_bytes(), secret.as_bytes());
        }

        for key in ["x-hub-signature-256", "x-webhook-signature", "x-signature"] {
            if let Some(sig) = headers.get(key) {
                return Self::verify_hmac_signature(secret, body, sig);
            }
        }

        if let (Some(msg_id), Some(timestamp), Some(signature)) = (
            headers.get("svix-id"),
            headers.get("svix-timestamp"),
            headers.get("svix-signature"),
        ) {
            return verify_svix_signature(secret, body, msg_id, timestamp, signature);
        }

        false
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(
        host.trim().to_ascii_lowercase().as_str(),
        "127.0.0.1" | "localhost" | "::1"
    )
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn verify_svix_signature(
    secret: &str,
    body: &[u8],
    msg_id: &str,
    timestamp: &str,
    signature: &str,
) -> bool {
    type HmacSha256 = Hmac<Sha256>;
    let key = if let Some(encoded) = secret.strip_prefix("whsec_") {
        match base64::engine::general_purpose::STANDARD.decode(encoded) {
            Ok(bytes) => bytes,
            Err(_) => return false,
        }
    } else {
        secret.as_bytes().to_vec()
    };

    let signed = [msg_id.as_bytes(), b".", timestamp.as_bytes(), b".", body].concat();
    let mut mac = match HmacSha256::new_from_slice(&key) {
        Ok(mac) => mac,
        Err(_) => return false,
    };
    mac.update(&signed);
    let digest = mac.finalize().into_bytes();

    signature.split_whitespace().any(|part| {
        part.strip_prefix("v1,")
            .or_else(|| part.strip_prefix("v1="))
            .map(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.trim())
                    .map(|candidate| constant_time_eq(&candidate, &digest))
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    })
}

fn lookup_payload_template_value(payload: &serde_json::Value, key: &str) -> Option<String> {
    if let Some(value) = lookup_string(payload, key) {
        return Some(value);
    }
    key.strip_prefix("payload.")
        .and_then(|stripped| lookup_string(payload, stripped))
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
        Self::validate_auth_safety(&self.config)?;

        info!(
            "Webhook adapter starting on port {} at path {}",
            self.config.port, self.config.path
        );

        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid address: {e}")))?;

        let config = self.config.clone();
        let outbound_queue = self.outbound_queue.clone();
        let inbound_tx = self.inbound_tx.clone();
        let seen_deliveries = self.seen_deliveries.clone();
        let rate_windows = self.rate_windows.clone();

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
                                let config = config.clone();
                                let outbound_queue = outbound_queue.clone();
                                let inbound_tx = inbound_tx.clone();
                                let seen_deliveries = seen_deliveries.clone();
                                let rate_windows = rate_windows.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_webhook_request(
                                        stream, peer, config,
                                        outbound_queue, inbound_tx, seen_deliveries, rate_windows,
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
            platform: None,
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
        "webhook"
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
// HTTP request handler
// ---------------------------------------------------------------------------

async fn handle_webhook_request(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    config: WebhookConfig,
    outbound_queue: Arc<RwLock<VecDeque<OutboundMessage>>>,
    inbound_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<WebhookPayload>>>>,
    seen_deliveries: Arc<RwLock<HashMap<String, Instant>>>,
    rate_windows: Arc<RwLock<HashMap<String, RateWindow>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncReadExt;

    let mut buf = vec![0u8; 8192];
    let (mut reader, mut writer) = stream.into_split();
    let n = reader.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    buf.truncate(n);

    let header_end = find_header_end(&buf).unwrap_or(buf.len());
    let headers = parse_headers(&buf[..header_end]);
    let content_len = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| buf.len().saturating_sub(header_end.saturating_add(4)));
    if content_len > config.max_body_bytes {
        write_json_response(&mut writer, 413, r#"{"error":"body too large"}"#).await?;
        return Ok(());
    }
    let body_start = (header_end + 4).min(buf.len());
    while buf.len().saturating_sub(body_start) < content_len {
        let remaining = content_len - buf.len().saturating_sub(body_start);
        let mut chunk = vec![0u8; remaining.min(8192)];
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
    }

    let request = String::from_utf8_lossy(&buf[..header_end]);
    let first_line = request.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let path = parts.get(1).copied().unwrap_or("/");
    let body_bytes = &buf[body_start..buf.len().min(body_start + content_len)];

    if method == "POST" && path == config.path {
        if !WebhookAdapter::validate_signature(&headers, body_bytes, &config.secret) {
            write_json_response(&mut writer, 403, r#"{"error":"bad signature"}"#).await?;
            return Ok(());
        }
        handle_simple_payload(&mut writer, body_bytes, inbound_tx).await?;
    } else if method == "POST" {
        match route_name_for_path(path, &config.path) {
            Some(route_name) => {
                let route_ctx = RoutePayloadContext {
                    config: &config,
                    outbound_queue: outbound_queue.clone(),
                    inbound_tx: inbound_tx.clone(),
                    seen_deliveries: seen_deliveries.clone(),
                    rate_windows: rate_windows.clone(),
                };
                handle_route_payload(&mut writer, route_name, &headers, body_bytes, route_ctx)
                    .await?;
            }
            None => write_empty_response(&mut writer, 404).await?,
        }
    } else if method == "GET" && path == format!("{}/outbound", config.path).as_str() {
        let messages = outbound_queue.write().await.drain(..).collect::<Vec<_>>();
        let body = serde_json::to_string(&messages)?;
        write_json_response(&mut writer, 200, &body).await?;
    } else {
        write_empty_response(&mut writer, 404).await?;
    }

    Ok(())
}

async fn handle_simple_payload(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    body_bytes: &[u8],
    inbound_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<WebhookPayload>>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match serde_json::from_slice::<WebhookPayload>(body_bytes) {
        Ok(payload) => {
            if let Some(tx) = inbound_tx.read().await.as_ref() {
                let _ = tx.send(payload).await;
            }
            write_json_response(writer, 200, r#"{"status":"ok"}"#).await?;
        }
        Err(e) => {
            let body = format!("{{\"error\":\"invalid payload: {e}\"}}");
            write_json_response(writer, 400, &body).await?;
        }
    }
    Ok(())
}

struct RoutePayloadContext<'a> {
    config: &'a WebhookConfig,
    outbound_queue: Arc<RwLock<VecDeque<OutboundMessage>>>,
    inbound_tx: Arc<RwLock<Option<tokio::sync::mpsc::Sender<WebhookPayload>>>>,
    seen_deliveries: Arc<RwLock<HashMap<String, Instant>>>,
    rate_windows: Arc<RwLock<HashMap<String, RateWindow>>>,
}

async fn handle_route_payload(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    route_name: &str,
    headers: &HashMap<String, String>,
    body_bytes: &[u8],
    ctx: RoutePayloadContext<'_>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = ctx.config;
    let Some(route) = config.routes.get(route_name) else {
        write_empty_response(writer, 404).await?;
        return Ok(());
    };
    let secret = route.secret.as_deref().unwrap_or(&config.secret);
    if secret.trim() != INSECURE_NO_AUTH
        && !WebhookAdapter::validate_signature(headers, body_bytes, secret)
    {
        write_json_response(writer, 403, r#"{"error":"bad signature"}"#).await?;
        return Ok(());
    }

    let payload: serde_json::Value = match serde_json::from_slice(body_bytes) {
        Ok(value) => value,
        Err(e) => {
            let body = format!("{{\"error\":\"invalid payload: {e}\"}}");
            write_json_response(writer, 400, &body).await?;
            return Ok(());
        }
    };

    let event_type = event_type(headers, &payload);
    if !route.event_types.is_empty()
        && !route
            .event_types
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&event_type))
    {
        write_json_response(writer, 202, r#"{"status":"ignored"}"#).await?;
        return Ok(());
    }

    if let Some(delivery_id) = delivery_id(headers, route) {
        let key = format!("{route_name}:{delivery_id}");
        let mut seen = ctx.seen_deliveries.write().await;
        prune_seen(&mut seen);
        if seen.contains_key(&key) {
            write_json_response(writer, 202, r#"{"status":"duplicate"}"#).await?;
            return Ok(());
        }
        seen.insert(key, Instant::now());
    }

    if !check_rate_limit(
        route_name,
        route.rate_limit.unwrap_or(config.rate_limit),
        ctx.rate_windows.clone(),
    )
    .await
    {
        write_json_response(writer, 429, r#"{"error":"rate limit"}"#).await?;
        return Ok(());
    }

    let rendered = render_prompt(route.prompt.as_deref(), &payload, &event_type, route_name);

    if route.deliver_only {
        let chat_id = route
            .deliver_extra
            .get("chat_id")
            .and_then(|v| render_value(v, &payload, &event_type, route_name))
            .or_else(|| {
                route
                    .chat_id
                    .as_deref()
                    .map(|s| render_template(s, &payload, &event_type, route_name))
            })
            .filter(|s| !s.trim().is_empty());
        let Some(chat_id) = chat_id else {
            write_json_response(writer, 502, r#"{"error":"missing delivery chat_id"}"#).await?;
            return Ok(());
        };
        ctx.outbound_queue.write().await.push_back(OutboundMessage {
            platform: route.deliver.clone(),
            chat_id,
            text: rendered,
        });
        write_json_response(writer, 200, r#"{"status":"delivered"}"#).await?;
        return Ok(());
    }

    let chat_id = route
        .chat_id
        .as_deref()
        .map(|s| render_template(s, &payload, &event_type, route_name))
        .or_else(|| lookup_string(&payload, "chat_id"))
        .unwrap_or_else(|| route_name.to_string());
    let user_id = route
        .user_id
        .as_deref()
        .map(|s| render_template(s, &payload, &event_type, route_name))
        .or_else(|| lookup_string(&payload, "user_id"))
        .unwrap_or_else(|| "webhook-client".to_string());
    let payload = WebhookPayload {
        chat_id,
        user_id: Some(user_id),
        text: rendered,
        metadata: payload,
    };
    if let Some(tx) = ctx.inbound_tx.read().await.as_ref() {
        let _ = tx.send(payload).await;
    }
    write_json_response(writer, 202, r#"{"status":"accepted"}"#).await?;
    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_headers(header_bytes: &[u8]) -> HashMap<String, String> {
    let header_text = String::from_utf8_lossy(header_bytes);
    header_text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect()
}

fn route_name_for_path<'a>(path: &'a str, base_path: &str) -> Option<&'a str> {
    if let Some(rest) = path.strip_prefix("/webhooks/") {
        return rest.split('/').next().filter(|s| !s.is_empty());
    }
    let prefix = format!("{}/", base_path.trim_end_matches('/'));
    path.strip_prefix(&prefix)
        .and_then(|rest| rest.split('/').next())
        .filter(|s| !s.is_empty())
}

fn event_type(headers: &HashMap<String, String>, payload: &serde_json::Value) -> String {
    for key in ["x-github-event", "x-gitlab-event", "x-webhook-event"] {
        if let Some(value) = headers.get(key).filter(|v| !v.trim().is_empty()) {
            return value.trim().to_string();
        }
    }
    for key in ["event_type", "event", "action", "type"] {
        if let Some(value) = lookup_string(payload, key).filter(|v| !v.trim().is_empty()) {
            return value;
        }
    }
    "webhook".to_string()
}

fn delivery_id(headers: &HashMap<String, String>, route: &WebhookRouteConfig) -> Option<String> {
    if let Some(header) = route.idempotency_header.as_deref() {
        if let Some(value) = headers
            .get(&header.to_ascii_lowercase())
            .filter(|v| !v.trim().is_empty())
        {
            return Some(value.trim().to_string());
        }
    }
    for key in [
        "x-github-delivery",
        "x-gitlab-event-uuid",
        "x-webhook-delivery",
        "svix-id",
        "idempotency-key",
    ] {
        if let Some(value) = headers.get(key).filter(|v| !v.trim().is_empty()) {
            return Some(value.trim().to_string());
        }
    }
    None
}

fn prune_seen(seen: &mut HashMap<String, Instant>) {
    let now = Instant::now();
    seen.retain(|_, at| now.duration_since(*at) <= DELIVERY_TTL);
}

async fn check_rate_limit(
    route_name: &str,
    limit: u32,
    rate_windows: Arc<RwLock<HashMap<String, RateWindow>>>,
) -> bool {
    if limit == 0 {
        return true;
    }
    let now = Instant::now();
    let mut windows = rate_windows.write().await;
    let window = windows.entry(route_name.to_string()).or_insert(RateWindow {
        started_at: now,
        count: 0,
    });
    if now.duration_since(window.started_at) >= RATE_WINDOW {
        window.started_at = now;
        window.count = 0;
    }
    if window.count >= limit {
        return false;
    }
    window.count += 1;
    true
}

fn render_prompt(
    template: Option<&str>,
    payload: &serde_json::Value,
    event_type: &str,
    route_name: &str,
) -> String {
    match template.map(str::trim).filter(|s| !s.is_empty()) {
        Some(template) => render_template(template, payload, event_type, route_name),
        None => format!(
            "Webhook event {event_type} on {route_name}:\n{}",
            serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
        ),
    }
}

fn render_template(
    template: &str,
    payload: &serde_json::Value,
    event_type: &str,
    route_name: &str,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);
        if let Some(end) = after_start.find('}') {
            let key = after_start[1..end].trim();
            if let Some(value) = lookup_template_value(payload, key, event_type, route_name) {
                out.push_str(&value);
            } else {
                out.push_str(&after_start[..=end]);
            }
            rest = &after_start[end + 1..];
        } else {
            out.push_str(after_start);
            rest = "";
        }
    }
    out.push_str(rest);
    out
}

fn lookup_template_value(
    payload: &serde_json::Value,
    key: &str,
    event_type: &str,
    route_name: &str,
) -> Option<String> {
    match key {
        "event" | "event_type" => return Some(event_type.to_string()),
        "route" | "route_name" => return Some(route_name.to_string()),
        _ => {}
    }
    lookup_payload_template_value(payload, key)
}

fn lookup_string(payload: &serde_json::Value, path: &str) -> Option<String> {
    let mut current = payload;
    for part in path.split('.') {
        current = current.get(part)?;
    }
    match current {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Null => None,
        other => Some(other.to_string()),
    }
}

fn render_value(
    value: &serde_json::Value,
    payload: &serde_json::Value,
    event_type: &str,
    route_name: &str,
) -> Option<String> {
    match value {
        serde_json::Value::String(s) => Some(render_template(s, payload, event_type, route_name)),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

async fn write_empty_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
) -> Result<(), std::io::Error> {
    let reason = reason_phrase(status);
    let resp = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\n\r\n");
    writer.write_all(resp.as_bytes()).await
}

async fn write_json_response(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    status: u16,
    body: &str,
) -> Result<(), std::io::Error> {
    let reason = reason_phrase(status);
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(), body
    );
    writer.write_all(resp.as_bytes()).await
}

fn reason_phrase(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        _ => "OK",
    }
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
        assert!(WebhookAdapter::verify_hmac_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_accepts_raw_hex_digest() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        let sig = sign(secret, body);
        assert!(WebhookAdapter::verify_hmac_signature(secret, body, &sig));
    }

    #[test]
    fn verify_signature_rejects_malformed_signature() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        assert!(!WebhookAdapter::verify_hmac_signature(
            secret,
            body,
            "sha256=xyz"
        ));
        assert!(!WebhookAdapter::verify_hmac_signature(secret, body, ""));
    }

    #[test]
    fn verify_signature_rejects_tampered_payload() {
        let secret = "s3cr3t";
        let body = br#"{"chat_id":"c1","text":"hello"}"#;
        let sig = format!("sha256={}", sign(secret, body));
        let tampered = br#"{"chat_id":"c1","text":"bye"}"#;
        assert!(!WebhookAdapter::verify_hmac_signature(
            secret, tampered, &sig
        ));
    }

    #[test]
    fn insecure_no_auth_is_refused_for_public_bind_adapter() {
        let config = WebhookConfig {
            port: 9000,
            host: "0.0.0.0".to_string(),
            path: "/webhook".to_string(),
            secret: INSECURE_NO_AUTH.to_string(),
            rate_limit: default_webhook_rate_limit(),
            max_body_bytes: default_webhook_max_body_bytes(),
            routes: BTreeMap::new(),
        };
        let err = WebhookAdapter::validate_auth_safety(&config).unwrap_err();
        assert!(err.to_string().contains("INSECURE_NO_AUTH"));
        assert!(err.to_string().contains("non-loopback"));
    }

    #[test]
    fn insecure_no_auth_allowed_for_loopback_bind() {
        let config = WebhookConfig {
            port: 9000,
            host: "127.0.0.1".to_string(),
            path: "/webhook".to_string(),
            secret: INSECURE_NO_AUTH.to_string(),
            rate_limit: default_webhook_rate_limit(),
            max_body_bytes: default_webhook_max_body_bytes(),
            routes: BTreeMap::new(),
        };
        WebhookAdapter::validate_auth_safety(&config).unwrap();
    }

    #[test]
    fn insecure_no_auth_signature_bypass_is_explicit_sentinel_only() {
        assert!(WebhookAdapter::verify_hmac_signature(
            INSECURE_NO_AUTH,
            br#"{"ok":true}"#,
            ""
        ));
        assert!(!WebhookAdapter::verify_hmac_signature(
            "INSECURE_NO_AUTH ",
            br#"{"ok":true}"#,
            "bad"
        ));
    }

    #[test]
    fn validate_signature_accepts_github_gitlab_generic_and_svix_shapes() {
        let body = br#"{"event_type":"message.received"}"#;
        let secret = "webhook-secret";
        let mut headers = HashMap::new();
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", sign(secret, body)),
        );
        assert!(WebhookAdapter::validate_signature(&headers, body, secret));

        headers.clear();
        headers.insert("x-gitlab-token".into(), secret.into());
        assert!(WebhookAdapter::validate_signature(&headers, body, secret));

        headers.clear();
        headers.insert("x-webhook-signature".into(), sign(secret, body));
        assert!(WebhookAdapter::validate_signature(&headers, body, secret));

        let svix_secret = format!(
            "whsec_{}",
            base64::engine::general_purpose::STANDARD.encode(b"agentmail-secret")
        );
        let msg_id = "msg_123";
        let timestamp = "1710000000";
        type HmacSha256 = Hmac<Sha256>;
        let mut mac =
            HmacSha256::new_from_slice(b"agentmail-secret").expect("hmac key should be valid");
        mac.update(format!("{msg_id}.{timestamp}.").as_bytes());
        mac.update(body);
        let sig = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
        headers.clear();
        headers.insert("svix-id".into(), msg_id.into());
        headers.insert("svix-timestamp".into(), timestamp.into());
        headers.insert("svix-signature".into(), format!("v1,{sig}"));
        assert!(WebhookAdapter::validate_signature(
            &headers,
            body,
            &svix_secret
        ));
    }

    #[test]
    fn render_prompt_supports_dot_notation_and_fallback() {
        let payload = serde_json::json!({
            "pull_request": {"number": 42, "title": "Fix bug"},
            "sender": {"login": "octocat"}
        });
        let rendered = render_prompt(
            Some("PR #{pull_request.number}: {payload.pull_request.title} by {sender.login}"),
            &payload,
            "pull_request",
            "github",
        );
        assert_eq!(rendered, "PR #42: Fix bug by octocat");

        let fallback = render_prompt(None, &payload, "push", "github");
        assert!(fallback.contains("Webhook event push on github"));
        assert!(fallback.contains("octocat"));
    }

    #[tokio::test]
    async fn route_payload_filters_duplicates_and_rate_limits() {
        let mut routes = BTreeMap::new();
        routes.insert(
            "github".into(),
            WebhookRouteConfig {
                prompt: Some("opened {pull_request.title}".into()),
                event_types: vec!["pull_request".into()],
                secret: Some("route-secret".into()),
                rate_limit: Some(1),
                ..WebhookRouteConfig::default()
            },
        );
        let config = WebhookConfig {
            host: "127.0.0.1".into(),
            port: 0,
            path: "/webhook".into(),
            secret: "global-secret".into(),
            rate_limit: 30,
            max_body_bytes: default_webhook_max_body_bytes(),
            routes,
        };
        let body = br#"{"pull_request":{"title":"Rust parity"},"chat_id":"chat-1"}"#;
        let mut headers = HashMap::new();
        headers.insert("x-github-event".into(), "pull_request".into());
        headers.insert("x-github-delivery".into(), "delivery-1".into());
        headers.insert(
            "x-hub-signature-256".into(),
            format!("sha256={}", sign("route-secret", body)),
        );

        let payload: serde_json::Value = serde_json::from_slice(body).unwrap();
        assert_eq!(
            event_type(&headers, &payload),
            "pull_request",
            "provider event header wins"
        );
        let seen = Arc::new(RwLock::new(HashMap::new()));
        let rate = Arc::new(RwLock::new(HashMap::new()));
        assert!(check_rate_limit("github", 1, rate.clone()).await);
        assert!(!check_rate_limit("github", 1, rate).await);

        let key = format!(
            "github:{}",
            delivery_id(&headers, config.routes.get("github").unwrap()).unwrap()
        );
        seen.write().await.insert(key.clone(), Instant::now());
        assert!(seen.read().await.contains_key(&key));
    }

    #[test]
    fn deliver_only_route_renders_target_chat() {
        let payload = serde_json::json!({"payload":{"user":"a","other":"b"}});
        let route = WebhookRouteConfig {
            prompt: Some("{payload.user} matched {payload.other}".into()),
            deliver: Some("telegram".into()),
            deliver_only: true,
            deliver_extra: BTreeMap::from([(
                "chat_id".to_string(),
                serde_json::Value::String("chat-{payload.user}".to_string()),
            )]),
            ..WebhookRouteConfig::default()
        };
        let text = render_prompt(route.prompt.as_deref(), &payload, "match", "match-alert");
        let chat = route
            .deliver_extra
            .get("chat_id")
            .and_then(|v| render_value(v, &payload, "match", "match-alert"))
            .unwrap();
        assert_eq!(text, "a matched b");
        assert_eq!(chat, "chat-a");
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
}
