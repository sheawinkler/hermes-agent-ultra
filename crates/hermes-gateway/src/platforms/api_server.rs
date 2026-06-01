//! OpenAI-compatible HTTP API server platform adapter.
//!
//! Exposes `/v1/chat/completions` (streaming SSE + non-streaming) and
//! `/v1/responses` endpoints, allowing any OpenAI-compatible client to
//! interact with the Hermes Agent gateway.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_tools::{ToolRegistry, ToolsetManager};

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
    #[serde(default, deserialize_with = "deserialize_boolish")]
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

#[derive(Debug, Deserialize)]
pub struct ResponsesRequest {
    pub model: Option<String>,
    pub input: ResponseInput,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    pub stream: bool,
    #[serde(
        default = "default_store_response",
        deserialize_with = "deserialize_boolish"
    )]
    pub store: bool,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub previous_response_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RunRequest {
    pub input: ResponseInput,
    #[serde(default)]
    pub conversation_history: Option<Vec<ChatMessage>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub personality: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunApprovalRequest {
    #[serde(default)]
    choice: Option<String>,
    #[serde(default, deserialize_with = "deserialize_boolish")]
    all: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum ResponseInput {
    Text(String),
    Messages(Vec<ChatMessage>),
}

fn default_store_response() -> bool {
    true
}

fn deserialize_boolish<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Boolish {
        Bool(bool),
        String(String),
        Number(u64),
    }

    match Option::<Boolish>::deserialize(deserializer)? {
        None => Ok(false),
        Some(Boolish::Bool(value)) => Ok(value),
        Some(Boolish::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "" | "0" | "false" | "off" | "no" | "none" | "null" => Ok(false),
            "1" | "true" | "on" | "yes" => Ok(true),
            other => Err(de::Error::custom(format!(
                "invalid bool-like value {other:?}"
            ))),
        },
        Some(Boolish::Number(value)) => Ok(value != 0),
    }
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

#[derive(Debug, Clone, Serialize)]
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

#[derive(Clone)]
struct ApiServerRuntime {
    mailbox: Arc<RwLock<ResponseMailbox>>,
    response_store: Arc<RwLock<ResponseStore>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    auth_token: Option<String>,
}

// ---------------------------------------------------------------------------
// Pending response mailbox
// ---------------------------------------------------------------------------

/// Holds pending responses that will be sent back to HTTP callers.
#[derive(Default)]
struct ResponseMailbox {
    pending: HashMap<String, mpsc::Sender<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredApiResponse {
    response: serde_json::Value,
    conversation_history: Vec<ChatMessage>,
}

#[derive(Debug)]
struct ResponseStore {
    max_size: usize,
    entries: HashMap<String, StoredApiResponse>,
    lru: VecDeque<String>,
    conversation_to_response: HashMap<String, String>,
}

impl ResponseStore {
    fn new(max_size: usize) -> Self {
        Self {
            max_size,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            conversation_to_response: HashMap::new(),
        }
    }

    fn put(&mut self, id: impl Into<String>, response: StoredApiResponse) {
        let id = id.into();
        self.entries.insert(id.clone(), response);
        self.touch(&id);
        self.evict_if_needed();
    }

    fn get(&mut self, id: &str) -> Option<StoredApiResponse> {
        if self.entries.contains_key(id) {
            self.touch(id);
        }
        self.entries.get(id).cloned()
    }

    fn delete(&mut self, id: &str) -> bool {
        let existed = self.entries.remove(id).is_some();
        if existed {
            self.lru.retain(|entry| entry != id);
            self.conversation_to_response
                .retain(|_, response_id| response_id != id);
        }
        existed
    }

    fn set_conversation(
        &mut self,
        conversation: impl Into<String>,
        response_id: impl Into<String>,
    ) {
        self.conversation_to_response
            .insert(conversation.into(), response_id.into());
    }

    fn get_conversation(&mut self, conversation: &str) -> Option<String> {
        let response_id = self.conversation_to_response.get(conversation)?.clone();
        if self.entries.contains_key(&response_id) {
            self.touch(&response_id);
            Some(response_id)
        } else {
            self.conversation_to_response.remove(conversation);
            None
        }
    }

    fn touch(&mut self, id: &str) {
        self.lru.retain(|entry| entry != id);
        self.lru.push_back(id.to_string());
    }

    fn evict_if_needed(&mut self) {
        while self.entries.len() > self.max_size {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
            self.conversation_to_response
                .retain(|_, response_id| response_id != &oldest);
        }
    }
}

impl Default for ResponseStore {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[derive(Default)]
struct RunCancelRegistry {
    pending: HashMap<String, Arc<Notify>>,
}

#[derive(Debug, Clone)]
struct RunRecord {
    run_id: String,
    session_id: String,
    user_id: String,
    model: Option<String>,
    provider: Option<String>,
    personality: Option<String>,
    status: String,
    output: Option<String>,
    usage: UsageInfo,
    last_event: Option<String>,
    events: Vec<serde_json::Value>,
    created_at: i64,
    completed_at: Option<i64>,
}

impl RunRecord {
    fn new(
        run_id: String,
        session_id: String,
        user_id: String,
        model: Option<String>,
        provider: Option<String>,
        personality: Option<String>,
    ) -> Self {
        let mut record = Self {
            run_id,
            session_id,
            user_id,
            model,
            provider,
            personality,
            status: "queued".to_string(),
            output: None,
            usage: UsageInfo {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
            last_event: None,
            events: Vec::new(),
            created_at: chrono::Utc::now().timestamp(),
            completed_at: None,
        };
        record.push_event("run.queued", None);
        record
    }

    fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running" | "stopping")
    }

    fn is_terminal(&self) -> bool {
        matches!(self.status.as_str(), "completed" | "cancelled" | "failed")
    }

    fn push_event(&mut self, event_type: &str, extra: Option<serde_json::Value>) {
        self.last_event = Some(event_type.to_string());
        let mut event = serde_json::json!({
            "type": event_type,
            "run_id": self.run_id,
            "status": self.status,
            "created_at": chrono::Utc::now().timestamp(),
        });
        if let Some(extra) = extra {
            if let (Some(target), Some(source)) = (event.as_object_mut(), extra.as_object()) {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
        }
        self.events.push(event);
    }
}

#[derive(Default)]
struct RunStore {
    records: HashMap<String, RunRecord>,
    notifiers: HashMap<String, Arc<Notify>>,
}

impl RunStore {
    fn insert(&mut self, record: RunRecord) {
        let run_id = record.run_id.clone();
        self.notifiers
            .insert(run_id.clone(), Arc::new(Notify::new()));
        self.records.insert(run_id, record);
    }

    fn get(&self, run_id: &str) -> Option<RunRecord> {
        self.records.get(run_id).cloned()
    }

    fn notifier(&self, run_id: &str) -> Option<Arc<Notify>> {
        self.notifiers.get(run_id).cloned()
    }

    fn update<F>(&mut self, run_id: &str, f: F) -> Option<RunRecord>
    where
        F: FnOnce(&mut RunRecord),
    {
        let record = self.records.get_mut(run_id)?;
        f(record);
        if let Some(notifier) = self.notifiers.get(run_id) {
            notifier.notify_waiters();
        }
        Some(record.clone())
    }
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
    response_store: Arc<RwLock<ResponseStore>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
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
            response_store: Arc::new(RwLock::new(ResponseStore::default())),
            run_cancels: Arc::new(RwLock::new(RunCancelRegistry::default())),
            run_store: Arc::new(RwLock::new(RunStore::default())),
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
        let response_store = self.response_store.clone();
        let run_cancels = self.run_cancels.clone();
        let run_store = self.run_store.clone();
        let inbound_tx = self.inbound_tx.clone();
        let auth_token = self.config.auth_token.clone();
        let runtime = ApiServerRuntime {
            mailbox,
            response_store,
            run_cancels,
            run_store,
            inbound_tx,
            auth_token,
        };

        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        tokio::spawn(async move {
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
                                let runtime = runtime.clone();
                                tokio::spawn(async move {
                                    if let Err(e) =
                                        handle_connection(
                                            stream,
                                            peer,
                                            runtime,
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

const SECURITY_HEADERS: &[(&str, &str)] = &[
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Content-Security-Policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
    (
        "Permissions-Policy",
        "camera=(), microphone=(), geolocation=()",
    ),
    (
        "Strict-Transport-Security",
        "max-age=31536000; includeSubDomains",
    ),
    ("X-Frame-Options", "DENY"),
    ("X-XSS-Protection", "0"),
];

#[derive(Clone, Copy)]
struct HttpStatus {
    code: u16,
    reason: &'static str,
}

const HTTP_OK: HttpStatus = HttpStatus {
    code: 200,
    reason: "OK",
};
const HTTP_BAD_REQUEST: HttpStatus = HttpStatus {
    code: 400,
    reason: "Bad Request",
};
const HTTP_UNAUTHORIZED: HttpStatus = HttpStatus {
    code: 401,
    reason: "Unauthorized",
};
const HTTP_NOT_FOUND: HttpStatus = HttpStatus {
    code: 404,
    reason: "Not Found",
};
const HTTP_CONFLICT: HttpStatus = HttpStatus {
    code: 409,
    reason: "Conflict",
};
const HTTP_BAD_GATEWAY: HttpStatus = HttpStatus {
    code: 502,
    reason: "Bad Gateway",
};
const HTTP_SERVICE_UNAVAILABLE: HttpStatus = HttpStatus {
    code: 503,
    reason: "Service Unavailable",
};
const HTTP_GATEWAY_TIMEOUT: HttpStatus = HttpStatus {
    code: 504,
    reason: "Gateway Timeout",
};

fn append_security_headers(response: &mut String) {
    for (name, value) in SECURITY_HEADERS {
        response.push_str(name);
        response.push_str(": ");
        response.push_str(value);
        response.push_str("\r\n");
    }
}

fn http_response(status: HttpStatus, content_type: &str, body: &str) -> String {
    let mut response = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\n",
        status.code, status.reason, content_type
    );
    append_security_headers(&mut response);
    response.push_str(&format!("Content-Length: {}\r\n\r\n{}", body.len(), body));
    response
}

fn json_http_response(status: HttpStatus, body: &serde_json::Value) -> serde_json::Result<String> {
    let payload = serde_json::to_string(body)?;
    Ok(http_response(status, "application/json", &payload))
}

fn api_error(message: impl Into<String>, error_type: &str, code: u16) -> serde_json::Value {
    serde_json::json!({
        "error": {
            "message": message.into(),
            "type": error_type,
            "code": code.to_string(),
        }
    })
}

fn sse_http_header() -> String {
    let mut response = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n".to_string();
    append_security_headers(&mut response);
    response.push_str("\r\n");
    response
}

fn split_path_query(raw_path: &str) -> (&str, Option<&str>) {
    raw_path
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((raw_path, None))
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    query?.split('&').find_map(|pair| {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        (name == key).then(|| urlencoding::decode(value).unwrap_or_default().to_string())
    })
}

fn models_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "list",
        "data": [
            {
                "id": "hermes-agent",
                "object": "model",
                "owned_by": "hermes",
            },
            {
                "id": "hermes",
                "object": "model",
                "owned_by": "hermes",
            }
        ]
    })
}

fn capabilities_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "hermes.api_server.capabilities",
        "features": {
            "chat_completions": true,
            "responses": true,
            "response_store": true,
            "runs": true,
            "conversation_mapping": true,
            "session_continuity_header": "X-Hermes-Session-Id",
            "toolsets": true,
            "skills": true,
        },
        "endpoints": {
            "health": {"method": "GET", "path": "/health"},
            "models": {"method": "GET", "path": "/v1/models"},
            "chat_completions": {"method": "POST", "path": "/v1/chat/completions"},
            "responses": {"method": "POST", "path": "/v1/responses"},
            "response_get": {"method": "GET", "path": "/v1/responses/{response_id}"},
            "response_delete": {"method": "DELETE", "path": "/v1/responses/{response_id}"},
            "run_start": {"method": "POST", "path": "/v1/runs"},
            "run_status": {"method": "GET", "path": "/v1/runs/{run_id}"},
            "run_events": {"method": "GET", "path": "/v1/runs/{run_id}/events"},
            "run_approval": {"method": "POST", "path": "/v1/runs/{run_id}/approval"},
            "run_stop": {"method": "POST", "path": "/v1/runs/{run_id}/stop"},
            "skills": {"method": "GET", "path": "/v1/skills"},
            "toolsets": {"method": "GET", "path": "/v1/toolsets"},
        }
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct SkillListEntry {
    name: String,
    description: String,
    category: String,
}

fn discover_skill_entries() -> Vec<SkillListEntry> {
    let mut roots = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd.join("skills"));
        roots.push(cwd.join("optional-skills"));
    }
    roots.push(hermes_config::paths::skills_dir());

    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        collect_skill_entries(&root, &root, &mut seen, &mut out);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn collect_skill_entries(
    root: &Path,
    dir: &Path,
    seen: &mut HashSet<String>,
    out: &mut Vec<SkillListEntry>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_md = path.join("SKILL.md");
        if skill_md.is_file() {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown")
                .to_string();
            if seen.insert(name.clone()) {
                let category = path
                    .parent()
                    .and_then(|parent| parent.strip_prefix(root).ok())
                    .and_then(|relative| relative.components().next())
                    .and_then(|component| component.as_os_str().to_str())
                    .filter(|value| !value.is_empty())
                    .unwrap_or("general")
                    .to_string();
                out.push(SkillListEntry {
                    name,
                    description: skill_description(&skill_md),
                    category,
                });
            }
        } else {
            collect_skill_entries(root, &path, seen, out);
        }
    }
}

fn skill_description(path: &Path) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("")
        .chars()
        .take(240)
        .collect()
}

fn skills_response_body() -> serde_json::Value {
    serde_json::json!({
        "object": "list",
        "data": discover_skill_entries(),
    })
}

fn toolsets_response_body() -> serde_json::Value {
    let registry = Arc::new(ToolRegistry::new());
    let manager = ToolsetManager::new(registry);
    let default_api_toolset = "hermes-api-server";
    let data: Vec<serde_json::Value> = manager
        .list_toolsets()
        .into_iter()
        .map(|name| {
            let tools = manager
                .resolve_toolset_unfiltered(&name)
                .unwrap_or_else(|_| Vec::new());
            serde_json::json!({
                "name": name,
                "title": name.replace('-', " "),
                "description": "Built-in Hermes toolset",
                "enabled": name == default_api_toolset,
                "configured": name == default_api_toolset,
                "tools": tools,
            })
        })
        .collect();

    serde_json::json!({
        "object": "list",
        "platform": "api_server",
        "data": data,
    })
}

fn response_input_messages(input: ResponseInput) -> Vec<ChatMessage> {
    match input {
        ResponseInput::Text(text) => vec![ChatMessage {
            role: "user".to_string(),
            content: text,
        }],
        ResponseInput::Messages(messages) => messages,
    }
}

fn input_messages_have_non_empty_user_content(messages: &[ChatMessage]) -> bool {
    messages.iter().any(|message| {
        message.role.trim().eq_ignore_ascii_case("user") && !message.content.trim().is_empty()
    })
}

fn estimated_usage(prompt: &str, content: &str) -> UsageInfo {
    let prompt_tokens = (prompt.len() as u32 / 4).max(1);
    let completion_tokens = (content.len() as u32 / 4).max(1);
    UsageInfo {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens + completion_tokens,
    }
}

fn run_response_body(record: &RunRecord) -> serde_json::Value {
    let mut body = serde_json::json!({
        "id": record.run_id,
        "run_id": record.run_id,
        "object": "hermes.run",
        "status": record.status,
        "session_id": record.session_id,
        "user": record.user_id,
        "created_at": record.created_at,
        "last_event": record.last_event,
        "usage": record.usage,
    });

    if let Some(model) = record.model.as_deref() {
        body["model"] = serde_json::json!(model);
    }
    if let Some(provider) = record.provider.as_deref() {
        body["provider"] = serde_json::json!(provider);
    }
    if let Some(personality) = record.personality.as_deref() {
        body["personality"] = serde_json::json!(personality);
    }
    if let Some(output) = record.output.as_deref() {
        body["output"] = serde_json::json!(output);
    }
    if let Some(completed_at) = record.completed_at {
        body["completed_at"] = serde_json::json!(completed_at);
    }
    body
}

fn run_events_sse_body(record: &RunRecord) -> String {
    let mut out = String::new();
    for event in &record.events {
        let event_type = event["type"].as_str().unwrap_or("run.event");
        out.push_str("event: ");
        out.push_str(event_type);
        out.push('\n');
        out.push_str("data: ");
        out.push_str(&event.to_string());
        out.push_str("\n\n");
    }
    out.push_str("data: [DONE]\n\n");
    out
}

async fn wait_for_run_event_snapshot(
    run_store: Arc<RwLock<RunStore>>,
    run_id: &str,
) -> Option<RunRecord> {
    loop {
        let (record, notifier) = {
            let guard = run_store.read().await;
            (guard.get(run_id)?, guard.notifier(run_id))
        };
        if record.is_terminal() {
            return Some(record);
        }

        let Some(notifier) = notifier else {
            return Some(record);
        };
        let notified = notifier.notified();
        tokio::pin!(notified);

        if let Some(latest) = run_store.read().await.get(run_id) {
            if latest.is_terminal() {
                return Some(latest);
            }
        } else {
            return None;
        }

        if tokio::time::timeout(Duration::from_secs(120), &mut notified)
            .await
            .is_err()
        {
            return run_store.read().await.get(run_id);
        }
    }
}

fn make_run_id() -> String {
    format!(
        "run_{}",
        uuid::Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
    )
}

fn make_responses_api_body(
    response_id: &str,
    model: &str,
    content: &str,
    previous_response_id: Option<&str>,
) -> serde_json::Value {
    let prompt_tokens = 0_u32;
    let completion_tokens = content.len() as u32 / 4;
    serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": chrono::Utc::now().timestamp(),
        "model": model,
        "previous_response_id": previous_response_id,
        "output": [
            {
                "id": format!("msg_{response_id}"),
                "type": "message",
                "role": "assistant",
                "content": [
                    {
                        "type": "output_text",
                        "text": content,
                    }
                ]
            }
        ],
        "usage": {
            "input_tokens": prompt_tokens,
            "output_tokens": completion_tokens,
            "total_tokens": prompt_tokens + completion_tokens,
        }
    })
}

async fn cleanup_api_request(
    mailbox: &Arc<RwLock<ResponseMailbox>>,
    run_cancels: &Arc<RwLock<RunCancelRegistry>>,
    request_id: &str,
    mailbox_key: &str,
) {
    mailbox.write().await.pending.remove(mailbox_key);
    let mut cancels = run_cancels.write().await;
    cancels.pending.remove(request_id);
    cancels.pending.remove(mailbox_key);
}

async fn run_api_request(
    mailbox: Arc<RwLock<ResponseMailbox>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    inbound: ApiInboundRequest,
    mailbox_key: String,
) -> Result<String, (HttpStatus, serde_json::Value)> {
    let request_id = inbound.request_id.clone();
    let (reply_tx, mut reply_rx) = mpsc::channel::<String>(1);
    let cancel_waiter = Arc::new(Notify::new());

    mailbox
        .write()
        .await
        .pending
        .insert(mailbox_key.clone(), reply_tx);
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
        cleanup_api_request(&mailbox, &run_cancels, &request_id, &mailbox_key).await;
        return Err((
            HTTP_SERVICE_UNAVAILABLE,
            api_error(
                "Gateway inbound pipeline is not configured",
                "service_unavailable",
                503,
            ),
        ));
    };

    if tx.send(inbound).await.is_err() {
        cleanup_api_request(&mailbox, &run_cancels, &request_id, &mailbox_key).await;
        return Err((
            HTTP_SERVICE_UNAVAILABLE,
            api_error(
                "Gateway inbound queue is unavailable",
                "service_unavailable",
                503,
            ),
        ));
    }

    let reply = tokio::select! {
        _ = cancel_waiter.notified() => {
            Err((
                HTTP_CONFLICT,
                api_error("Run stopped", "cancelled_error", 409),
            ))
        }
        timeout_result = tokio::time::timeout(Duration::from_secs(120), reply_rx.recv()) => {
            match timeout_result {
                Ok(Some(msg)) => Ok(msg),
                Ok(None) => Err((
                    HTTP_BAD_GATEWAY,
                    api_error("Gateway closed response channel", "internal_error", 502),
                )),
                Err(_) => Err((
                    HTTP_GATEWAY_TIMEOUT,
                    api_error("Gateway response timeout", "timeout_error", 504),
                )),
            }
        }
    };

    cleanup_api_request(&mailbox, &run_cancels, &request_id, &mailbox_key).await;
    reply
}

async fn run_background_request(
    mailbox: Arc<RwLock<ResponseMailbox>>,
    run_cancels: Arc<RwLock<RunCancelRegistry>>,
    run_store: Arc<RwLock<RunStore>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    inbound: ApiInboundRequest,
    mailbox_key: String,
) {
    let run_id = inbound.request_id.clone();
    let prompt = inbound.prompt.clone();

    let should_start = run_store
        .write()
        .await
        .update(&run_id, |record| {
            if record.status == "stopping" {
                record.status = "cancelled".to_string();
                record.completed_at = Some(chrono::Utc::now().timestamp());
                record.push_event(
                    "run.failed",
                    Some(serde_json::json!({"error": "Run stopped before dispatch"})),
                );
            } else {
                record.status = "running".to_string();
                record.push_event("run.running", None);
            }
        })
        .map(|record| record.status == "running")
        .unwrap_or(false);

    if !should_start {
        return;
    }

    let result = run_api_request(mailbox, run_cancels, inbound_tx, inbound, mailbox_key).await;
    match result {
        Ok(reply) => {
            let usage = estimated_usage(&prompt, &reply);
            run_store.write().await.update(&run_id, |record| {
                record.status = "completed".to_string();
                record.output = Some(reply.clone());
                record.usage = usage;
                record.completed_at = Some(chrono::Utc::now().timestamp());
                record.push_event(
                    "run.completed",
                    Some(serde_json::json!({
                        "output": reply,
                        "usage": record.usage,
                    })),
                );
            });
        }
        Err((status, body)) => {
            let message = body["error"]["message"]
                .as_str()
                .unwrap_or("Gateway request failed")
                .to_string();
            run_store.write().await.update(&run_id, |record| {
                record.status = if status.code == HTTP_CONFLICT.code || record.status == "stopping"
                {
                    "cancelled".to_string()
                } else {
                    "failed".to_string()
                };
                record.completed_at = Some(chrono::Utc::now().timestamp());
                record.push_event(
                    "run.failed",
                    Some(serde_json::json!({
                        "error": message,
                        "code": status.code,
                    })),
                );
            });
        }
    }
}

async fn handle_connection(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    runtime: ApiServerRuntime,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::AsyncWriteExt;

    let ApiServerRuntime {
        mailbox,
        response_store,
        run_cancels,
        run_store,
        inbound_tx,
        auth_token,
    } = runtime;

    let (mut reader, mut writer) = stream.into_split();
    let raw = read_http_request(&mut reader).await?;
    if raw.is_empty() {
        return Ok(());
    }

    let Some(header_end) = find_bytes(&raw, b"\r\n\r\n") else {
        let resp = json_http_response(
            HTTP_BAD_REQUEST,
            &api_error("Invalid HTTP request", "invalid_request_error", 400),
        )?;
        writer.write_all(resp.as_bytes()).await?;
        return Ok(());
    };

    let header_text = String::from_utf8_lossy(&raw[..header_end]);
    let body_bytes = &raw[(header_end + 4).min(raw.len())..];
    let first_line = header_text.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let raw_path = parts.get(1).copied().unwrap_or("/");
    let (path, query) = split_path_query(raw_path);

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
            let resp = json_http_response(
                HTTP_UNAUTHORIZED,
                &api_error("Unauthorized", "auth_error", 401),
            )?;
            writer.write_all(resp.as_bytes()).await?;
            return Ok(());
        }
    }

    if method == "POST" {
        if let Some(run_id) = parse_stop_run_path(path) {
            let stop_record = run_store.write().await.update(run_id, |record| {
                if record.is_active() {
                    record.status = "stopping".to_string();
                    record.push_event("run.stopping", None);
                }
            });

            if let Some(record) = stop_record.filter(|record| record.status == "stopping") {
                if let Some(waiter) = run_cancels.read().await.pending.get(run_id).cloned() {
                    waiter.notify_waiters();
                }
                let body = serde_json::json!({
                    "id": run_id,
                    "run_id": run_id,
                    "object": "hermes.run",
                    "status": record.status,
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else if let Some(waiter) = run_cancels.read().await.pending.get(run_id).cloned() {
                waiter.notify_waiters();
                let body = serde_json::json!({
                    "id": run_id,
                    "run_id": run_id,
                    "object": "hermes.run",
                    "status": "stopping"
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
            return Ok(());
        }
    }

    match (method, path) {
        ("GET", "/health") | ("GET", "/") => {
            let body = serde_json::json!({"status":"ok","adapter":"api-server"});
            let resp = json_http_response(HTTP_OK, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/health/detailed") | ("GET", "/v1/health") => {
            let body = serde_json::json!({
                "status": "ok",
                "adapter": "api-server",
                "features": capabilities_response_body()["features"].clone(),
            });
            let resp = json_http_response(HTTP_OK, &body)?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/models") => {
            let resp = json_http_response(HTTP_OK, &models_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/capabilities") => {
            let resp = json_http_response(HTTP_OK, &capabilities_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/skills") => {
            let _category_filter = query_param(query, "category");
            let resp = json_http_response(HTTP_OK, &skills_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", "/v1/toolsets") => {
            let _enabled_filter = query_param(query, "enabled");
            let resp = json_http_response(HTTP_OK, &toolsets_response_body())?;
            writer.write_all(resp.as_bytes()).await?;
        }
        ("GET", _) if path.starts_with("/v1/responses/") => {
            let response_id = path.trim_start_matches("/v1/responses/");
            let stored = response_store.write().await.get(response_id);
            if let Some(stored) = stored {
                let resp = json_http_response(HTTP_OK, &stored.response)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Response not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("DELETE", _) if path.starts_with("/v1/responses/") => {
            let response_id = path.trim_start_matches("/v1/responses/");
            let deleted = response_store.write().await.delete(response_id);
            if deleted {
                let body = serde_json::json!({
                    "id": response_id,
                    "object": "response.deleted",
                    "deleted": true,
                });
                let resp = json_http_response(HTTP_OK, &body)?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Response not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("POST", "/v1/runs") => {
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<RunRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let run_id = make_run_id();
                    let mut input_messages = response_input_messages(req.input);
                    if !input_messages_have_non_empty_user_content(&input_messages) {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include non-empty input",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let mut messages = req.conversation_history.unwrap_or_default();
                    messages.append(&mut input_messages);
                    let prompt = build_prompt_from_messages(&messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include at least one user message",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req.session_id.clone().unwrap_or_else(|| run_id.clone());
                    let user_id = req
                        .user
                        .clone()
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let record = RunRecord::new(
                        run_id.clone(),
                        session_id.clone(),
                        user_id.clone(),
                        req.model.clone(),
                        req.provider.clone(),
                        req.personality.clone(),
                    );
                    run_store.write().await.insert(record);

                    let inbound = ApiInboundRequest {
                        request_id: run_id.clone(),
                        session_id: session_id.clone(),
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    tokio::spawn(run_background_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        run_store.clone(),
                        inbound_tx.clone(),
                        inbound,
                        session_id.clone(),
                    ));

                    let body = serde_json::json!({
                        "id": run_id,
                        "run_id": run_id,
                        "object": "hermes.run",
                        "status": "started",
                        "session_id": session_id,
                    });
                    let resp = json_http_response(
                        HttpStatus {
                            code: 202,
                            reason: "Accepted",
                        },
                        &body,
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("GET", _) if parse_run_events_path(path).is_some() => {
            let run_id = parse_run_events_path(path).expect("guard checked path");
            let record = wait_for_run_event_snapshot(run_store.clone(), run_id).await;
            if let Some(record) = record {
                let header = sse_http_header();
                writer.write_all(header.as_bytes()).await?;
                let data = run_events_sse_body(&record);
                writer.write_all(data.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("GET", _) if parse_get_run_path(path).is_some() => {
            let run_id = parse_get_run_path(path).expect("guard checked path");
            let record = run_store.read().await.get(run_id);
            if let Some(record) = record {
                let resp = json_http_response(HTTP_OK, &run_response_body(&record))?;
                writer.write_all(resp.as_bytes()).await?;
            } else {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
            }
        }
        ("POST", _) if parse_run_approval_path(path).is_some() => {
            let run_id = parse_run_approval_path(path).expect("guard checked path");
            if run_store.read().await.get(run_id).is_none() {
                let resp = json_http_response(
                    HTTP_NOT_FOUND,
                    &api_error("Run not found", "not_found", 404),
                )?;
                writer.write_all(resp.as_bytes()).await?;
                return Ok(());
            }
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<RunApprovalRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let _choice = req.choice;
                    let _all = req.all;
                    let resp = json_http_response(
                        HTTP_CONFLICT,
                        &api_error("Run has no pending approval", "approval_not_pending", 409),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("POST", "/v1/chat/completions") => {
            let body_str = String::from_utf8_lossy(body_bytes);

            let parsed: Result<ChatCompletionRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let request_id = ApiServerAdapter::make_completion_id();
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let prompt = build_prompt_from_messages(&req.messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include at least one user message",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
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

                    let reply = match run_api_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        inbound_tx.clone(),
                        inbound,
                        mailbox_key,
                    )
                    .await
                    {
                        Ok(reply) => reply,
                        Err((status, body)) => {
                            let resp = json_http_response(status, &body)?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                    };

                    if req.stream {
                        let header = sse_http_header();
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
                        let resp = http_response(
                            HTTP_OK,
                            "application/json",
                            &serde_json::to_string(&response)?,
                        );
                        writer.write_all(resp.as_bytes()).await?;
                    }
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        ("POST", "/v1/responses") => {
            let body_str = String::from_utf8_lossy(body_bytes);
            let parsed: Result<ResponsesRequest, _> = serde_json::from_str(&body_str);
            match parsed {
                Ok(req) => {
                    let request_id = format!(
                        "resp_{}",
                        uuid::Uuid::new_v4().to_string().replace('-', "")[..24].to_string()
                    );
                    let model = req.model.as_deref().unwrap_or("hermes").to_string();
                    let mut messages = response_input_messages(req.input);
                    let previous_response_id = if let Some(id) = req.previous_response_id.clone() {
                        let exists = response_store.write().await.get(&id).is_some();
                        if !exists {
                            let resp = json_http_response(
                                HTTP_NOT_FOUND,
                                &api_error("Previous response not found", "not_found", 404),
                            )?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                        Some(id)
                    } else if let Some(conversation) = req.conversation.as_deref() {
                        response_store.write().await.get_conversation(conversation)
                    } else {
                        None
                    };

                    if let Some(previous_id) = previous_response_id.as_deref() {
                        if let Some(previous) = response_store.write().await.get(previous_id) {
                            let mut history = previous.conversation_history;
                            history.append(&mut messages);
                            messages = history;
                        }
                    }

                    let prompt = build_prompt_from_messages(&messages).unwrap_or_default();
                    if prompt.trim().is_empty() {
                        let resp = json_http_response(
                            HTTP_BAD_REQUEST,
                            &api_error(
                                "Request must include non-empty input",
                                "invalid_request_error",
                                400,
                            ),
                        )?;
                        writer.write_all(resp.as_bytes()).await?;
                        return Ok(());
                    }

                    let session_id = req
                        .session_id
                        .clone()
                        .or_else(|| req.conversation.clone())
                        .unwrap_or_else(|| request_id.clone());
                    let user_id = req
                        .user
                        .clone()
                        .filter(|u| !u.trim().is_empty())
                        .unwrap_or_else(|| "api-client".to_string());
                    let inbound = ApiInboundRequest {
                        request_id: request_id.clone(),
                        session_id: session_id.clone(),
                        user_id,
                        model: req.model.clone(),
                        provider: req.provider.clone(),
                        personality: req.personality.clone(),
                        prompt,
                    };

                    let reply = match run_api_request(
                        mailbox.clone(),
                        run_cancels.clone(),
                        inbound_tx.clone(),
                        inbound,
                        session_id,
                    )
                    .await
                    {
                        Ok(reply) => reply,
                        Err((status, body)) => {
                            let resp = json_http_response(status, &body)?;
                            writer.write_all(resp.as_bytes()).await?;
                            return Ok(());
                        }
                    };

                    let response = make_responses_api_body(
                        &request_id,
                        &model,
                        &reply,
                        previous_response_id.as_deref(),
                    );

                    if req.store {
                        let mut conversation_history = messages;
                        conversation_history.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: reply.clone(),
                        });
                        let mut guard = response_store.write().await;
                        guard.put(
                            request_id.clone(),
                            StoredApiResponse {
                                response: response.clone(),
                                conversation_history,
                            },
                        );
                        if let Some(conversation) = req.conversation {
                            guard.set_conversation(conversation, request_id.clone());
                        }
                    }

                    if req.stream {
                        let header = sse_http_header();
                        writer.write_all(header.as_bytes()).await?;
                        let data = format!("data: {}\n\ndata: [DONE]\n\n", response);
                        writer.write_all(data.as_bytes()).await?;
                    } else {
                        let resp = json_http_response(HTTP_OK, &response)?;
                        writer.write_all(resp.as_bytes()).await?;
                    }
                }
                Err(e) => {
                    let resp = json_http_response(
                        HTTP_BAD_REQUEST,
                        &api_error(
                            format!("Invalid request: {e}"),
                            "invalid_request_error",
                            400,
                        ),
                    )?;
                    writer.write_all(resp.as_bytes()).await?;
                }
            }
        }
        _ => {
            let resp =
                json_http_response(HTTP_NOT_FOUND, &api_error("Not found", "not_found", 404))?;
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

fn parse_run_events_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/events")?;
    if run_id.is_empty() || run_id.contains('/') {
        None
    } else {
        Some(run_id)
    }
}

fn parse_run_approval_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?.strip_suffix("/approval")?;
    if run_id.is_empty() || run_id.contains('/') {
        None
    } else {
        Some(run_id)
    }
}

fn parse_get_run_path(path: &str) -> Option<&str> {
    let run_id = path.strip_prefix("/v1/runs/")?;
    if run_id.is_empty() || run_id.contains('/') {
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn spawn_one_request_server(
        mailbox: Arc<RwLock<ResponseMailbox>>,
        response_store: Arc<RwLock<ResponseStore>>,
        run_cancels: Arc<RwLock<RunCancelRegistry>>,
        run_store: Arc<RwLock<RunStore>>,
        inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
        auth_token: Option<String>,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.expect("accept");
            let runtime = ApiServerRuntime {
                mailbox,
                response_store,
                run_cancels,
                run_store,
                inbound_tx,
                auth_token,
            };
            handle_connection(stream, peer, runtime)
                .await
                .expect("handle connection");
        });
        (addr, handle)
    }

    async fn read_http_response(mut stream: tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.expect("read response");
        String::from_utf8(bytes).expect("utf8 response")
    }

    struct ApiTestState {
        mailbox: Arc<RwLock<ResponseMailbox>>,
        response_store: Arc<RwLock<ResponseStore>>,
        run_cancels: Arc<RwLock<RunCancelRegistry>>,
        run_store: Arc<RwLock<RunStore>>,
        inbound_tx: Arc<RwLock<Option<mpsc::Sender<ApiInboundRequest>>>>,
    }

    impl ApiTestState {
        fn new(tx: mpsc::Sender<ApiInboundRequest>) -> Self {
            Self {
                mailbox: Arc::new(RwLock::new(ResponseMailbox::default())),
                response_store: Arc::new(RwLock::new(ResponseStore::default())),
                run_cancels: Arc::new(RwLock::new(RunCancelRegistry::default())),
                run_store: Arc::new(RwLock::new(RunStore::default())),
                inbound_tx: Arc::new(RwLock::new(Some(tx))),
            }
        }

        async fn roundtrip(&self, request: String, auth_token: Option<String>) -> String {
            let (addr, handle) = spawn_one_request_server(
                Arc::clone(&self.mailbox),
                Arc::clone(&self.response_store),
                Arc::clone(&self.run_cancels),
                Arc::clone(&self.run_store),
                Arc::clone(&self.inbound_tx),
                auth_token,
            )
            .await;

            let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
            client
                .write_all(request.as_bytes())
                .await
                .expect("write request");
            client.shutdown().await.expect("shutdown write side");
            let response = read_http_response(client).await;
            handle.await.expect("server task");
            response
        }
    }

    fn json_body(response: &str) -> serde_json::Value {
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("http body");
        serde_json::from_str(body).expect("json body")
    }

    fn json_request(method: &str, path: &str, body: serde_json::Value) -> String {
        let body = body.to_string();
        format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    }

    fn empty_request(method: &str, path: &str) -> String {
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n\r\n")
    }

    async fn wait_for_run_status(
        run_store: &Arc<RwLock<RunStore>>,
        run_id: &str,
        expected: &str,
    ) -> RunRecord {
        for _ in 0..50 {
            if let Some(record) = run_store.read().await.get(run_id) {
                if record.status == expected {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        run_store
            .read()
            .await
            .get(run_id)
            .expect("run should exist")
    }

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

    #[test]
    fn parse_run_routes_accept_only_expected_subresources() {
        assert_eq!(
            parse_get_run_path("/v1/runs/run_abc123"),
            Some("run_abc123")
        );
        assert_eq!(
            parse_run_events_path("/v1/runs/run_abc123/events"),
            Some("run_abc123")
        );
        assert_eq!(
            parse_run_approval_path("/v1/runs/run_abc123/approval"),
            Some("run_abc123")
        );
        assert_eq!(parse_get_run_path("/v1/runs/run_abc123/events"), None);
        assert_eq!(parse_run_events_path("/v1/runs/run_abc123"), None);
    }

    #[test]
    fn api_boolish_fields_accept_quoted_false() {
        let chat: ChatCompletionRequest = serde_json::from_value(serde_json::json!({
            "model": "hermes-agent",
            "messages": [{"role": "user", "content": "hello"}],
            "stream": "false",
        }))
        .expect("quoted false stream should deserialize");
        assert!(!chat.stream);

        let responses: ResponsesRequest = serde_json::from_value(serde_json::json!({
            "model": "hermes-agent",
            "input": "hello",
            "stream": "false",
            "store": "false",
        }))
        .expect("quoted false stream/store should deserialize");
        assert!(!responses.stream);
        assert!(!responses.store);

        let approval: RunApprovalRequest = serde_json::from_value(serde_json::json!({
            "choice": "once",
            "all": "false",
        }))
        .expect("quoted false all should deserialize");
        assert!(!approval.all);
    }

    #[test]
    fn response_store_deletes_and_evicts_conversation_mappings() {
        let mut store = ResponseStore::new(2);
        let stored = |text: &str| StoredApiResponse {
            response: serde_json::json!({"id": text}),
            conversation_history: vec![ChatMessage {
                role: "assistant".into(),
                content: text.into(),
            }],
        };

        store.put("resp_1", stored("one"));
        store.set_conversation("chat-a", "resp_1");
        assert_eq!(store.get_conversation("chat-a").as_deref(), Some("resp_1"));
        assert!(store.delete("resp_1"));
        assert_eq!(store.get_conversation("chat-a"), None);

        store.put("resp_2", stored("two"));
        store.set_conversation("chat-b", "resp_2");
        store.put("resp_3", stored("three"));
        store.set_conversation("chat-c", "resp_3");
        store.put("resp_4", stored("four"));

        assert!(store.get("resp_2").is_none());
        assert_eq!(store.get_conversation("chat-b"), None);
        assert_eq!(store.get_conversation("chat-c").as_deref(), Some("resp_3"));
    }

    #[test]
    fn api_discovery_bodies_expose_capabilities_toolsets_and_headers() {
        let capabilities = capabilities_response_body();
        assert_eq!(
            capabilities["endpoints"]["skills"],
            serde_json::json!({"method": "GET", "path": "/v1/skills"})
        );
        assert_eq!(
            capabilities["endpoints"]["toolsets"],
            serde_json::json!({"method": "GET", "path": "/v1/toolsets"})
        );

        let toolsets = toolsets_response_body();
        let data = toolsets["data"].as_array().expect("toolset list");
        let api_server = data
            .iter()
            .find(|entry| entry["name"] == "hermes-api-server")
            .expect("hermes-api-server toolset");
        assert_eq!(api_server["enabled"], true);
        assert!(api_server["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|tool| tool == "terminal"));

        let response = json_http_response(HTTP_OK, &serde_json::json!({"status": "ok"}))
            .expect("json response");
        assert!(response.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(response.contains("Referrer-Policy: no-referrer\r\n"));
        assert!(response.contains("X-Frame-Options: DENY\r\n"));
        assert!(response
            .contains("Content-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n"));
    }

    #[test]
    fn skill_discovery_reads_nested_skill_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let skill_dir = tmp.path().join("creative").join("ascii-art");
        std::fs::create_dir_all(&skill_dir).expect("create skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "# ASCII Art\n\nGenerate terminal-friendly drawings.\n",
        )
        .expect("write skill");

        let mut seen = HashSet::new();
        let mut entries = Vec::new();
        collect_skill_entries(tmp.path(), tmp.path(), &mut seen, &mut entries);

        assert_eq!(
            entries,
            vec![SkillListEntry {
                name: "ascii-art".into(),
                description: "Generate terminal-friendly drawings.".into(),
                category: "creative".into(),
            }]
        );
    }

    #[test]
    fn responses_body_shape_matches_openai_responses_contract() {
        let body = make_responses_api_body("resp_abc", "hermes-agent", "done", Some("resp_prev"));
        assert_eq!(body["id"], "resp_abc");
        assert_eq!(body["object"], "response");
        assert_eq!(body["previous_response_id"], "resp_prev");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(body["output"][0]["content"][0]["text"], "done");
    }

    #[tokio::test]
    async fn api_discovery_endpoint_serves_capabilities_with_security_headers() {
        let (tx, _rx) = mpsc::channel(1);
        let (addr, handle) = spawn_one_request_server(
            Arc::new(RwLock::new(ResponseMailbox::default())),
            Arc::new(RwLock::new(ResponseStore::default())),
            Arc::new(RwLock::new(RunCancelRegistry::default())),
            Arc::new(RwLock::new(RunStore::default())),
            Arc::new(RwLock::new(Some(tx))),
            None,
        )
        .await;

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(b"GET /v1/capabilities HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .expect("write request");
        client.shutdown().await.expect("shutdown write side");
        let response = read_http_response(client).await;
        handle.await.expect("server task");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(response.contains("\"skills\":{\"method\":\"GET\",\"path\":\"/v1/skills\"}"));
        assert!(response.contains("\"toolsets\":{\"method\":\"GET\",\"path\":\"/v1/toolsets\"}"));
    }

    #[tokio::test]
    async fn responses_endpoint_accepts_input_and_quoted_false_without_storing() {
        let mailbox = Arc::new(RwLock::new(ResponseMailbox::default()));
        let response_store = Arc::new(RwLock::new(ResponseStore::default()));
        let run_cancels = Arc::new(RwLock::new(RunCancelRegistry::default()));
        let (tx, mut rx) = mpsc::channel(1);
        let (addr, handle) = spawn_one_request_server(
            Arc::clone(&mailbox),
            Arc::clone(&response_store),
            Arc::clone(&run_cancels),
            Arc::new(RwLock::new(RunStore::default())),
            Arc::new(RwLock::new(Some(tx))),
            None,
        )
        .await;

        let body = serde_json::json!({
            "model": "hermes-agent",
            "input": "hello",
            "store": "false",
            "stream": "false",
        })
        .to_string();
        let request = format!(
            "POST /v1/responses HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );

        let mut client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        client
            .write_all(request.as_bytes())
            .await
            .expect("write request");

        let inbound = rx.recv().await.expect("inbound request");
        assert_eq!(inbound.model.as_deref(), Some("hermes-agent"));
        assert_eq!(inbound.prompt, "hello");
        let sender = mailbox
            .read()
            .await
            .pending
            .get(&inbound.session_id)
            .cloned()
            .expect("pending response sender");
        sender.send("done".to_string()).await.expect("send reply");

        let response = read_http_response(client).await;
        handle.await.expect("server task");

        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(!response.contains("text/event-stream"));
        assert!(response.contains("\"object\":\"response\""));
        assert!(response.contains("\"text\":\"done\""));
        assert!(response_store.read().await.entries.is_empty());
    }

    #[tokio::test]
    async fn runs_endpoint_starts_completes_status_and_events() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        let start_response = state
            .roundtrip(
                json_request(
                    "POST",
                    "/v1/runs",
                    serde_json::json!({
                        "model": "hermes-agent",
                        "input": "hello",
                        "session_id": "space-session",
                    }),
                ),
                None,
            )
            .await;

        assert!(start_response.starts_with("HTTP/1.1 202 Accepted"));
        let start = json_body(&start_response);
        assert_eq!(start["status"], "started");
        let run_id = start["run_id"].as_str().expect("run id").to_string();
        assert!(run_id.starts_with("run_"));

        let inbound = rx.recv().await.expect("inbound run");
        assert_eq!(inbound.request_id, run_id);
        assert_eq!(inbound.session_id, "space-session");
        assert_eq!(inbound.prompt, "hello");

        let sender = state
            .mailbox
            .read()
            .await
            .pending
            .get("space-session")
            .cloned()
            .expect("pending run response");
        sender.send("done".to_string()).await.expect("send reply");
        let record = wait_for_run_status(&state.run_store, &run_id, "completed").await;
        assert_eq!(record.output.as_deref(), Some("done"));

        let status_response = state
            .roundtrip(empty_request("GET", &format!("/v1/runs/{run_id}")), None)
            .await;
        assert!(status_response.starts_with("HTTP/1.1 200 OK"));
        let status = json_body(&status_response);
        assert_eq!(status["run_id"], run_id);
        assert_eq!(status["status"], "completed");
        assert_eq!(status["session_id"], "space-session");
        assert_eq!(status["output"], "done");
        assert_eq!(status["last_event"], "run.completed");
        assert!(status["usage"]["total_tokens"].as_u64().unwrap_or_default() > 0);

        let events_response = state
            .roundtrip(
                empty_request("GET", &format!("/v1/runs/{run_id}/events")),
                None,
            )
            .await;
        assert!(events_response.starts_with("HTTP/1.1 200 OK"));
        assert!(events_response.contains("Content-Type: text/event-stream"));
        assert!(events_response.contains("event: run.completed"));
        assert!(events_response.contains("\"output\":\"done\""));
        assert!(events_response.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn runs_endpoint_rejects_invalid_history_without_allocating_run() {
        let (tx, _rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        let response = state
            .roundtrip(
                json_request(
                    "POST",
                    "/v1/runs",
                    serde_json::json!({
                        "input": "hello",
                        "conversation_history": {"role": "user"},
                    }),
                ),
                None,
            )
            .await;

        assert!(response.starts_with("HTTP/1.1 400 Bad Request"));
        assert!(state.run_store.read().await.records.is_empty());
    }

    #[tokio::test]
    async fn runs_stop_marks_active_run_cancelled_and_events_emit_failure() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        let start_response = state
            .roundtrip(
                json_request("POST", "/v1/runs", serde_json::json!({"input": "hold"})),
                None,
            )
            .await;
        let run_id = json_body(&start_response)["run_id"]
            .as_str()
            .expect("run id")
            .to_string();
        let inbound = rx.recv().await.expect("inbound run");
        assert_eq!(inbound.request_id, run_id);

        let stop_response = state
            .roundtrip(
                json_request(
                    "POST",
                    &format!("/v1/runs/{run_id}/stop"),
                    serde_json::json!({}),
                ),
                None,
            )
            .await;
        assert!(stop_response.starts_with("HTTP/1.1 200 OK"));
        let stop = json_body(&stop_response);
        assert_eq!(stop["run_id"], run_id);
        assert_eq!(stop["status"], "stopping");

        let record = wait_for_run_status(&state.run_store, &run_id, "cancelled").await;
        assert_eq!(record.last_event.as_deref(), Some("run.failed"));

        let status_response = state
            .roundtrip(empty_request("GET", &format!("/v1/runs/{run_id}")), None)
            .await;
        let status = json_body(&status_response);
        assert_eq!(status["status"], "cancelled");

        let events_response = state
            .roundtrip(
                empty_request("GET", &format!("/v1/runs/{run_id}/events")),
                None,
            )
            .await;
        assert!(events_response.contains("event: run.failed"));
        assert!(events_response.contains("Run stopped"));
    }

    #[tokio::test]
    async fn runs_approval_without_pending_returns_conflict() {
        let (tx, mut rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        let start_response = state
            .roundtrip(
                json_request("POST", "/v1/runs", serde_json::json!({"input": "hello"})),
                None,
            )
            .await;
        let run_id = json_body(&start_response)["run_id"]
            .as_str()
            .expect("run id")
            .to_string();

        let inbound = rx.recv().await.expect("inbound run");
        let sender = state
            .mailbox
            .read()
            .await
            .pending
            .get(&inbound.session_id)
            .cloned()
            .expect("pending run response");
        sender.send("done".to_string()).await.expect("send reply");
        let _record = wait_for_run_status(&state.run_store, &run_id, "completed").await;

        let approval_response = state
            .roundtrip(
                json_request(
                    "POST",
                    &format!("/v1/runs/{run_id}/approval"),
                    serde_json::json!({"choice": "once", "all": "false"}),
                ),
                None,
            )
            .await;
        assert!(approval_response.starts_with("HTTP/1.1 409 Conflict"));
        let body = json_body(&approval_response);
        assert_eq!(body["error"]["code"], "409");
        assert_eq!(body["error"]["type"], "approval_not_pending");
    }

    #[tokio::test]
    async fn runs_endpoints_require_bearer_auth_when_configured() {
        let (tx, _rx) = mpsc::channel(1);
        let state = ApiTestState::new(tx);
        let response = state
            .roundtrip(
                json_request("POST", "/v1/runs", serde_json::json!({"input": "hello"})),
                Some("sk-secret".to_string()),
            )
            .await;
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(state.run_store.read().await.records.is_empty());
    }
}
