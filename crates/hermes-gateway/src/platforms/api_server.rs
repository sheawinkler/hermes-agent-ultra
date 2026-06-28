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
use hermes_cron::{
    verify_nas_fire_token, ChronosConfig, CronError, CronJob, CronScheduler, DeliverConfig,
    DeliverTarget, JobStatus,
};
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
struct ApiCronJobCreateRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    deliver: Option<serde_json::Value>,
    #[serde(default)]
    repeat: Option<u32>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    script: Option<String>,
    #[serde(default)]
    no_agent: Option<bool>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ApiCronFireRequest {
    job_id: String,
    #[serde(default)]
    fire_at: Option<String>,
}

#[derive(Clone, Copy)]
struct ApiJobRequestContext<'a> {
    method: &'a str,
    raw_path: &'a str,
    headers: &'a str,
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
    #[serde(deserialize_with = "deserialize_chat_content")]
    pub content: String,
}

const MAX_CHAT_CONTENT_DEPTH: usize = 10;
const MAX_CHAT_CONTENT_LIST_SIZE: usize = 1000;
const MAX_CHAT_CONTENT_CHARS: usize = 65_536;

fn truncate_chat_content(input: &str) -> String {
    input.chars().take(MAX_CHAT_CONTENT_CHARS).collect()
}

fn normalize_chat_content_value(value: &serde_json::Value, depth: usize) -> String {
    if depth > MAX_CHAT_CONTENT_DEPTH {
        return String::new();
    }
    match value {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(text) => truncate_chat_content(text),
        serde_json::Value::Bool(value) => {
            if *value {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        serde_json::Value::Number(value) => value.to_string(),
        serde_json::Value::Array(items) => items
            .iter()
            .take(MAX_CHAT_CONTENT_LIST_SIZE)
            .map(|item| normalize_chat_content_value(item, depth + 1))
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        serde_json::Value::Object(map) => {
            let part_type = map
                .get("type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if matches!(part_type, "text" | "input_text" | "output_text") {
                map.get("text")
                    .map(|text| normalize_chat_content_value(text, depth + 1))
                    .unwrap_or_default()
            } else {
                String::new()
            }
        }
    }
}

fn deserialize_chat_content<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(normalize_chat_content_value(&value, 0))
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
    cron_scheduler: Arc<CronScheduler>,
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
    cron_scheduler: Arc<CronScheduler>,
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
            cron_scheduler: Arc::new(hermes_cron::cron_scheduler_for_data_dir(
                hermes_config::paths::cron_dir(),
            )),
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
        let cron_scheduler = self.cron_scheduler.clone();
        let inbound_tx = self.inbound_tx.clone();
        let auth_token = self.config.auth_token.clone();
        if let Err(err) = cron_scheduler.load_persisted_jobs().await {
            warn!("API server cron jobs failed to load persisted jobs: {err}");
        }
        let runtime = ApiServerRuntime {
            mailbox,
            response_store,
            run_cancels,
            run_store,
            cron_scheduler,
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
const HTTP_ACCEPTED: HttpStatus = HttpStatus {
    code: 202,
    reason: "Accepted",
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
            "cron_jobs": true,
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
            "jobs_list": {"method": "GET", "path": "/api/jobs"},
            "jobs_create": {"method": "POST", "path": "/api/jobs"},
            "jobs_get": {"method": "GET", "path": "/api/jobs/{job_id}"},
            "jobs_update": {"method": "PATCH", "path": "/api/jobs/{job_id}"},
            "jobs_delete": {"method": "DELETE", "path": "/api/jobs/{job_id}"},
            "jobs_pause": {"method": "POST", "path": "/api/jobs/{job_id}/pause"},
            "jobs_resume": {"method": "POST", "path": "/api/jobs/{job_id}/resume"},
            "jobs_run": {"method": "POST", "path": "/api/jobs/{job_id}/run"},
            "cron_fire": {"method": "POST", "path": "/api/cron/fire"},
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

fn boolish_query_param(query: Option<&str>, key: &str) -> bool {
    query_param(query, key)
        .as_deref()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn validate_api_job_id(job_id: &str) -> Result<(), serde_json::Value> {
    if job_id.is_empty() || job_id.len() > 64 {
        return Err(api_error("Invalid job id", "invalid_request_error", 400));
    }
    if job_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(())
    } else {
        Err(api_error("Invalid job id", "invalid_request_error", 400))
    }
}

fn clean_audit_log_value(raw: Option<&str>, max_len: usize) -> String {
    raw.unwrap_or_default()
        .replace(['\r', '\n'], " ")
        .trim()
        .chars()
        .take(max_len)
        .collect()
}

fn request_header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .trim()
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn audit_log_suffix(ctx: ApiJobRequestContext<'_>) -> String {
    let mut fields = Vec::new();
    let forwarded_for =
        clean_audit_log_value(request_header_value(ctx.headers, "X-Forwarded-For"), 200);
    let real_ip = clean_audit_log_value(request_header_value(ctx.headers, "X-Real-IP"), 200);
    let user_agent = clean_audit_log_value(request_header_value(ctx.headers, "User-Agent"), 300);
    let method = clean_audit_log_value(Some(ctx.method), 16);
    let path = clean_audit_log_value(Some(ctx.raw_path), 500);

    if !forwarded_for.is_empty() {
        fields.push(format!("forwarded_for={forwarded_for:?}"));
    }
    if !real_ip.is_empty() {
        fields.push(format!("real_ip={real_ip:?}"));
    }
    if !method.is_empty() {
        fields.push(format!("method={method:?}"));
    }
    if !path.is_empty() {
        fields.push(format!("path={path:?}"));
    }
    if !user_agent.is_empty() {
        fields.push(format!("user_agent={user_agent:?}"));
    }
    if fields.is_empty() {
        "source='unknown'".to_string()
    } else {
        fields.join(" ")
    }
}

fn invalid_api_job_id_response(
    job_id: &str,
    ctx: Option<ApiJobRequestContext<'_>>,
) -> Option<(HttpStatus, serde_json::Value)> {
    match validate_api_job_id(job_id) {
        Ok(()) => None,
        Err(body) => {
            if let Some(ctx) = ctx {
                warn!(
                    "Cron jobs API rejected invalid job_id {:?}: {}",
                    job_id,
                    audit_log_suffix(ctx)
                );
            }
            Some((HTTP_BAD_REQUEST, body))
        }
    }
}

fn deliver_target_name(target: &DeliverTarget) -> &'static str {
    match target {
        DeliverTarget::Origin => "origin",
        DeliverTarget::Local => "local",
        DeliverTarget::Telegram => "telegram",
        DeliverTarget::Discord => "discord",
        DeliverTarget::Slack => "slack",
        DeliverTarget::Email => "email",
        DeliverTarget::WhatsApp => "whatsapp",
        DeliverTarget::Signal => "signal",
        DeliverTarget::Matrix => "matrix",
        DeliverTarget::Mattermost => "mattermost",
        DeliverTarget::DingTalk => "dingtalk",
        DeliverTarget::Feishu => "feishu",
        DeliverTarget::WeCom => "wecom",
        DeliverTarget::Weixin => "weixin",
        DeliverTarget::BlueBubbles => "bluebubbles",
        DeliverTarget::Sms => "sms",
        DeliverTarget::HomeAssistant => "homeassistant",
        DeliverTarget::Ntfy => "ntfy",
    }
}

fn parse_deliver_target(raw: &str) -> Option<DeliverTarget> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "origin" => Some(DeliverTarget::Origin),
        "local" => Some(DeliverTarget::Local),
        "telegram" => Some(DeliverTarget::Telegram),
        "discord" => Some(DeliverTarget::Discord),
        "slack" => Some(DeliverTarget::Slack),
        "email" => Some(DeliverTarget::Email),
        "whatsapp" => Some(DeliverTarget::WhatsApp),
        "signal" => Some(DeliverTarget::Signal),
        "matrix" => Some(DeliverTarget::Matrix),
        "mattermost" => Some(DeliverTarget::Mattermost),
        "dingtalk" => Some(DeliverTarget::DingTalk),
        "feishu" => Some(DeliverTarget::Feishu),
        "wecom" => Some(DeliverTarget::WeCom),
        "weixin" | "wechat" => Some(DeliverTarget::Weixin),
        "bluebubbles" | "blue_bubbles" => Some(DeliverTarget::BlueBubbles),
        "sms" => Some(DeliverTarget::Sms),
        "homeassistant" | "home_assistant" => Some(DeliverTarget::HomeAssistant),
        "ntfy" => Some(DeliverTarget::Ntfy),
        _ => None,
    }
}

fn parse_api_deliver_config(
    raw: Option<&serde_json::Value>,
) -> Result<Option<DeliverConfig>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if let Some(value) = raw.as_str() {
        let target = parse_deliver_target(value)
            .ok_or_else(|| format!("Unknown deliver target '{value}'"))?;
        return Ok(Some(DeliverConfig {
            target,
            platform: None,
        }));
    }
    let Some(obj) = raw.as_object() else {
        return Err("deliver must be a string or object".to_string());
    };
    let target_raw = obj
        .get("target")
        .or_else(|| obj.get("type"))
        .or_else(|| obj.get("name"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "deliver.target is required".to_string())?;
    let target = parse_deliver_target(target_raw)
        .ok_or_else(|| format!("Unknown deliver target '{target_raw}'"))?;
    let platform = obj
        .get("platform")
        .or_else(|| obj.get("recipient"))
        .or_else(|| obj.get("chat_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Ok(Some(DeliverConfig { target, platform }))
}

fn api_cron_job_body(job: &CronJob) -> serde_json::Value {
    let deliver = job.deliver.as_ref().map(|deliver| {
        let mut value = serde_json::json!(deliver_target_name(&deliver.target));
        if let Some(platform) = deliver.platform.as_deref() {
            value = serde_json::json!({
                "target": deliver_target_name(&deliver.target),
                "platform": platform,
            });
        }
        value
    });

    serde_json::json!({
        "id": job.id,
        "name": job.name,
        "schedule": job.schedule,
        "prompt": job.prompt,
        "deliver": deliver,
        "enabled": job.status == JobStatus::Active,
        "status": job.status.to_string(),
        "created_at": job.created_at,
        "last_run": job.last_run,
        "next_run": job.next_run,
        "repeat": job.repeat,
        "run_count": job.run_count,
        "skills": job.skills,
        "script": job.script,
        "no_agent": job.no_agent,
        "script_timeout_seconds": job.script_timeout_seconds,
        "script_shell": job.script_shell,
        "workdir": job.workdir,
        "context_from": job.context_from,
        "last_output": job.last_output,
    })
}

fn cron_error_to_http(error: CronError) -> (HttpStatus, serde_json::Value) {
    match error {
        CronError::JobNotFound(id) => (
            HTTP_NOT_FOUND,
            api_error(format!("Job not found: {id}"), "not_found", 404),
        ),
        CronError::InvalidJob(message) => (
            HTTP_BAD_REQUEST,
            api_error(message, "invalid_request_error", 400),
        ),
        CronError::JobAlreadyExists(id) => (
            HTTP_CONFLICT,
            api_error(format!("Job already exists: {id}"), "conflict_error", 409),
        ),
        other => (
            HTTP_BAD_GATEWAY,
            api_error(other.to_string(), "internal_error", 502),
        ),
    }
}

fn validate_api_cron_create(req: ApiCronJobCreateRequest) -> Result<CronJob, serde_json::Value> {
    let name = req
        .name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error("name is required", "invalid_request_error", 400))?;
    if name.chars().count() > 200 {
        return Err(api_error(
            "Name must be 200 characters or fewer",
            "invalid_request_error",
            400,
        ));
    }

    let schedule = req
        .schedule
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| api_error("schedule is required", "invalid_request_error", 400))?;

    let prompt = req.prompt.unwrap_or_default().trim().to_string();
    if prompt.chars().count() > 5000 {
        return Err(api_error(
            "Prompt must be 5000 characters or fewer",
            "invalid_request_error",
            400,
        ));
    }
    if prompt.is_empty() && req.script.as_ref().is_none_or(|v| v.trim().is_empty()) {
        return Err(api_error(
            "prompt is required",
            "invalid_request_error",
            400,
        ));
    }
    if matches!(req.repeat, Some(0)) {
        return Err(api_error(
            "repeat must be greater than zero",
            "invalid_request_error",
            400,
        ));
    }

    let mut job = CronJob::new(schedule, prompt);
    job.name = Some(name);
    job.deliver = match req.deliver.as_ref() {
        Some(raw) => parse_api_deliver_config(Some(raw))
            .map_err(|message| api_error(message, "invalid_request_error", 400))?,
        None => Some(DeliverConfig {
            target: DeliverTarget::Local,
            platform: None,
        }),
    };
    job.repeat = req.repeat;
    job.skills = req
        .skills
        .map(|skills| {
            skills
                .into_iter()
                .map(|skill| skill.trim().to_string())
                .filter(|skill| !skill.is_empty())
                .collect::<Vec<_>>()
        })
        .filter(|skills| !skills.is_empty());
    job.script = req
        .script
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    job.no_agent = req.no_agent.unwrap_or(false);
    if req.enabled == Some(false) {
        job.status = JobStatus::Paused;
        job.next_run = None;
    }
    Ok(job)
}

fn apply_api_cron_updates(
    mut job: CronJob,
    updates: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<CronJob>, serde_json::Value> {
    let mut changed = false;
    for (key, value) in updates {
        match key.as_str() {
            "name" => {
                let name = value.as_str().unwrap_or_default().trim().to_string();
                if name.chars().count() > 200 {
                    return Err(api_error(
                        "Name must be 200 characters or fewer",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.name = (!name.is_empty()).then_some(name);
                changed = true;
            }
            "schedule" => {
                let schedule = value.as_str().unwrap_or_default().trim().to_string();
                if schedule.is_empty() {
                    return Err(api_error(
                        "schedule is required",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.schedule = schedule;
                job.next_run = None;
                changed = true;
            }
            "prompt" => {
                let prompt = value.as_str().unwrap_or_default().trim().to_string();
                if prompt.chars().count() > 5000 {
                    return Err(api_error(
                        "Prompt must be 5000 characters or fewer",
                        "invalid_request_error",
                        400,
                    ));
                }
                job.prompt = prompt;
                changed = true;
            }
            "deliver" => {
                job.deliver = parse_api_deliver_config(Some(value))
                    .map_err(|message| api_error(message, "invalid_request_error", 400))?;
                changed = true;
            }
            "enabled" => {
                if let Some(enabled) = value.as_bool() {
                    job.status = if enabled {
                        JobStatus::Active
                    } else {
                        JobStatus::Paused
                    };
                    if enabled {
                        job.next_run = None;
                    }
                    changed = true;
                }
            }
            "repeat" => {
                let repeat = if value.is_null() {
                    None
                } else {
                    let Some(raw) = value.as_u64() else {
                        return Err(api_error(
                            "repeat must be an integer",
                            "invalid_request_error",
                            400,
                        ));
                    };
                    if raw == 0 {
                        return Err(api_error(
                            "repeat must be greater than zero",
                            "invalid_request_error",
                            400,
                        ));
                    }
                    let repeat = u32::try_from(raw).map_err(|_| {
                        api_error("repeat is too large", "invalid_request_error", 400)
                    })?;
                    Some(repeat)
                };
                job.repeat = repeat;
                changed = true;
            }
            "skills" => {
                if value.is_null() {
                    job.skills = None;
                } else {
                    let Some(items) = value.as_array() else {
                        return Err(api_error(
                            "skills must be an array",
                            "invalid_request_error",
                            400,
                        ));
                    };
                    let skills = items
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(str::trim)
                        .filter(|skill| !skill.is_empty())
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>();
                    job.skills = (!skills.is_empty()).then_some(skills);
                }
                changed = true;
            }
            "skill" => {
                job.skills = value
                    .as_str()
                    .map(str::trim)
                    .filter(|skill| !skill.is_empty())
                    .map(|skill| vec![skill.to_string()]);
                changed = true;
            }
            "script" => {
                job.script = value
                    .as_str()
                    .map(str::trim)
                    .filter(|script| !script.is_empty())
                    .map(ToOwned::to_owned);
                changed = true;
            }
            "no_agent" => {
                if let Some(no_agent) = value.as_bool() {
                    job.no_agent = no_agent;
                    changed = true;
                }
            }
            _ => {}
        }
    }
    Ok(changed.then_some(job))
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

async fn api_jobs_list_response(
    cron_scheduler: Arc<CronScheduler>,
    include_disabled: bool,
) -> (HttpStatus, serde_json::Value) {
    let mut jobs = cron_scheduler.list_jobs().await;
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    if !include_disabled {
        jobs.retain(|job| job.status == JobStatus::Active);
    }
    let jobs = jobs.iter().map(api_cron_job_body).collect::<Vec<_>>();
    (HTTP_OK, serde_json::json!({ "jobs": jobs }))
}

async fn api_jobs_create_response(
    cron_scheduler: Arc<CronScheduler>,
    body_bytes: &[u8],
) -> (HttpStatus, serde_json::Value) {
    let body_str = String::from_utf8_lossy(body_bytes);
    let parsed: Result<ApiCronJobCreateRequest, _> = serde_json::from_str(&body_str);
    let req = match parsed {
        Ok(req) => req,
        Err(err) => {
            return (
                HTTP_BAD_REQUEST,
                api_error(
                    format!("Invalid request: {err}"),
                    "invalid_request_error",
                    400,
                ),
            );
        }
    };
    let job = match validate_api_cron_create(req) {
        Ok(job) => job,
        Err(body) => return (HTTP_BAD_REQUEST, body),
    };
    let job_id = match cron_scheduler.create_job(job).await {
        Ok(job_id) => job_id,
        Err(err) => return cron_error_to_http(err),
    };
    let Some(job) = cron_scheduler.get_job(&job_id).await else {
        return (
            HTTP_BAD_GATEWAY,
            api_error("Created job could not be loaded", "internal_error", 502),
        );
    };
    (
        HTTP_OK,
        serde_json::json!({ "job": api_cron_job_body(&job) }),
    )
}

async fn api_jobs_get_response(
    cron_scheduler: Arc<CronScheduler>,
    job_id: &str,
    request_context: Option<ApiJobRequestContext<'_>>,
) -> (HttpStatus, serde_json::Value) {
    if let Some(response) = invalid_api_job_id_response(job_id, request_context) {
        return response;
    }
    match cron_scheduler.get_job(job_id).await {
        Some(job) => (
            HTTP_OK,
            serde_json::json!({ "job": api_cron_job_body(&job) }),
        ),
        None => (HTTP_NOT_FOUND, api_error("Job not found", "not_found", 404)),
    }
}

async fn api_jobs_update_response(
    cron_scheduler: Arc<CronScheduler>,
    job_id: &str,
    body_bytes: &[u8],
    request_context: Option<ApiJobRequestContext<'_>>,
) -> (HttpStatus, serde_json::Value) {
    if let Some(response) = invalid_api_job_id_response(job_id, request_context) {
        return response;
    }
    let body_str = String::from_utf8_lossy(body_bytes);
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(&body_str);
    let value = match parsed {
        Ok(value) => value,
        Err(err) => {
            return (
                HTTP_BAD_REQUEST,
                api_error(
                    format!("Invalid request: {err}"),
                    "invalid_request_error",
                    400,
                ),
            );
        }
    };
    let Some(updates) = value.as_object() else {
        return (
            HTTP_BAD_REQUEST,
            api_error(
                "Request body must be an object",
                "invalid_request_error",
                400,
            ),
        );
    };
    let Some(job) = cron_scheduler.get_job(job_id).await else {
        return (HTTP_NOT_FOUND, api_error("Job not found", "not_found", 404));
    };
    let Some(updated) = (match apply_api_cron_updates(job, updates) {
        Ok(updated) => updated,
        Err(body) => return (HTTP_BAD_REQUEST, body),
    }) else {
        return (
            HTTP_BAD_REQUEST,
            api_error("No valid fields to update", "invalid_request_error", 400),
        );
    };
    if let Err(err) = cron_scheduler.update_job(job_id, updated).await {
        return cron_error_to_http(err);
    }
    let Some(job) = cron_scheduler.get_job(job_id).await else {
        return (
            HTTP_BAD_GATEWAY,
            api_error("Updated job could not be loaded", "internal_error", 502),
        );
    };
    (
        HTTP_OK,
        serde_json::json!({ "job": api_cron_job_body(&job) }),
    )
}

async fn api_jobs_delete_response(
    cron_scheduler: Arc<CronScheduler>,
    job_id: &str,
    request_context: Option<ApiJobRequestContext<'_>>,
) -> (HttpStatus, serde_json::Value) {
    if let Some(response) = invalid_api_job_id_response(job_id, request_context) {
        return response;
    }
    match cron_scheduler.remove_job(job_id).await {
        Ok(()) => (HTTP_OK, serde_json::json!({ "ok": true })),
        Err(err) => cron_error_to_http(err),
    }
}

async fn api_jobs_action_response(
    cron_scheduler: Arc<CronScheduler>,
    job_id: &str,
    action: &str,
    request_context: Option<ApiJobRequestContext<'_>>,
) -> (HttpStatus, serde_json::Value) {
    if let Some(response) = invalid_api_job_id_response(job_id, request_context) {
        return response;
    }
    let result = match action {
        "pause" => cron_scheduler.pause_job(job_id).await,
        "resume" => cron_scheduler.resume_job(job_id).await,
        "run" => cron_scheduler.run_job(job_id).await.map(|_| ()),
        _ => {
            return (HTTP_NOT_FOUND, api_error("Not found", "not_found", 404));
        }
    };
    if let Err(err) = result {
        return cron_error_to_http(err);
    }
    let Some(job) = cron_scheduler.get_job(job_id).await else {
        return (HTTP_NOT_FOUND, api_error("Job not found", "not_found", 404));
    };
    (
        HTTP_OK,
        serde_json::json!({ "job": api_cron_job_body(&job) }),
    )
}

async fn api_cron_fire_response(
    cron_scheduler: Arc<CronScheduler>,
    auth_header: Option<&str>,
    body_bytes: &[u8],
) -> (HttpStatus, serde_json::Value) {
    let Some(token) = auth_header
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return (
            HTTP_UNAUTHORIZED,
            api_error("Missing Chronos bearer token", "auth_error", 401),
        );
    };

    let config = ChronosConfig::load();
    if let Err(err) = verify_nas_fire_token(token, &config).await {
        tracing::warn!(error = %err, "Chronos cron fire token rejected");
        return (
            HTTP_UNAUTHORIZED,
            api_error("Unauthorized", "auth_error", 401),
        );
    }

    let body_str = String::from_utf8_lossy(body_bytes);
    let parsed: Result<ApiCronFireRequest, _> = serde_json::from_str(&body_str);
    let req = match parsed {
        Ok(req) => req,
        Err(err) => {
            return (
                HTTP_BAD_REQUEST,
                api_error(
                    format!("Invalid request: {err}"),
                    "invalid_request_error",
                    400,
                ),
            );
        }
    };
    let job_id = req.job_id.trim();
    if job_id.is_empty() {
        return (
            HTTP_BAD_REQUEST,
            api_error("job_id is required", "invalid_request_error", 400),
        );
    }
    let fire_at = match req
        .fire_at
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(raw) => match chrono::DateTime::parse_from_rfc3339(raw) {
            Ok(dt) => Some(dt.with_timezone(&chrono::Utc)),
            Err(err) => {
                return (
                    HTTP_BAD_REQUEST,
                    api_error(
                        format!("fire_at must be RFC3339: {err}"),
                        "invalid_request_error",
                        400,
                    ),
                );
            }
        },
        None => None,
    };

    match cron_scheduler.fire_managed_job(job_id, fire_at).await {
        Ok(accepted) => (
            HTTP_ACCEPTED,
            serde_json::json!({
                "status": "accepted",
                "job_id": job_id,
                "dispatched": accepted,
            }),
        ),
        Err(CronError::JobNotFound(_)) => {
            (HTTP_NOT_FOUND, api_error("Job not found", "not_found", 404))
        }
        Err(err) => (
            HTTP_BAD_GATEWAY,
            api_error(err.to_string(), "internal_error", 502),
        ),
    }
}

include!("api_server/http_runtime.rs");

#[cfg(test)]
mod tests;
