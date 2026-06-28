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

include!("api_server/state.rs");

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

include!("api_server/http_catalog.rs");
include!("api_server/job_helpers.rs");

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
