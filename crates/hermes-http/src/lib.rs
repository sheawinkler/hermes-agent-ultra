//! HTTP and WebSocket API server for Hermes.
//!
//! Environment (see also `security` module):
//! - `HERMES_HTTP_MAX_BODY_BYTES` — max JSON body size for POST routes (default 2 MiB).
//!
//! Policy HTTP routes are intentionally omitted (Hermes Python does not expose them).

pub mod dashboard_oidc;
mod security;

pub use security::parse_allowed_ips;
pub use security::PolicyGuardConfig;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::Path as FsPath;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use async_trait::async_trait;
use axum::extract::ws::{Message as WsMessage, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::http::header;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::StreamExt;
use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::session_persistence::SessionPersistence;
use hermes_agent::smart_model_routing::ApiMode;
use hermes_agent::{
    build_auxiliary_client_with_main_runtime, leading_system_prompt_for_persist,
    run_oneshot_with_client, AgentConfig, AgentLoop, AuxiliaryConfig, AuxiliaryMainRuntime,
    MemorySubject, MemorySurface, OneShotError, OneShotRequest, RunRuntime,
};
use hermes_config::GatewayConfig;
use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_core::{AgentError, LlmProvider, Message, ResolvedExecutionMode, StreamChunk};
use hermes_gateway::gateway::{
    GatewayConfig as RuntimeGatewayConfig, IncomingMessage, IncomingSender,
};
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_provider_runtime::{
    build_provider as build_runtime_provider, select_startup_model_with_fallback,
};
use hermes_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const HTTP_PLATFORM: &str = "http";
const SESSION_KEY_HEADER: &str = "x-hermes-session-key";
#[derive(Clone, Default)]
pub struct ChatOutboundBuffer {
    inner: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl ChatOutboundBuffer {
    pub fn clear_chat(&self, chat_id: &str) {
        let mut g = self.inner.lock().unwrap();
        g.remove(chat_id);
    }

    pub fn drain_chat(&self, chat_id: &str) -> Vec<String> {
        let mut g = self.inner.lock().unwrap();
        g.remove(chat_id).unwrap_or_default()
    }
}

struct HttpPlatformAdapter {
    buf: ChatOutboundBuffer,
}

#[async_trait]
impl PlatformAdapter for HttpPlatformAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.buf
            .inner
            .lock()
            .unwrap()
            .entry(chat_id.to_string())
            .or_default()
            .push(text.to_string());
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        HTTP_PLATFORM
    }
}

#[derive(Clone)]
pub struct HttpServerState {
    pub config: Arc<GatewayConfig>,
    pub tool_registry: Arc<ToolRegistry>,
    run_runtime: Arc<RunRuntime>,
    #[cfg(test)]
    execution_mode: ResolvedExecutionMode,
    gateway: Arc<Gateway>,
    outbound: ChatOutboundBuffer,
}

impl HttpServerState {
    pub async fn build(config: GatewayConfig) -> Result<Self, AgentError> {
        // Validate every explicitly present execution-mode layer before opening
        // the durable runtime. `RunRuntime::open` can create/migrate sessions.db,
        // provision credential-store entries, recover runs, and initialize
        // artifact storage.
        let execution_mode = hermes_config::resolve_runtime_execution_mode(&config, None)?;
        let state_root = config
            .home_dir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(hermes_config::hermes_home);
        let run_runtime = Arc::new(
            RunRuntime::open(&state_root)
                .map_err(|error| AgentError::Io(format!("run-record startup failed: {error}")))?,
        );
        Self::build_with_run_runtime(config, run_runtime, execution_mode).await
    }

    /// Builds HTTP state around an already-opened runtime.
    ///
    /// The resolved mode is supplied explicitly so test callers do not consult
    /// process-global configuration after constructing their injected runtime.
    #[doc(hidden)]
    pub async fn build_with_run_runtime(
        config: GatewayConfig,
        run_runtime: Arc<RunRuntime>,
        execution_mode: ResolvedExecutionMode,
    ) -> Result<Self, AgentError> {
        run_sessions_db_auto_maintenance(&config);
        let runtime_gateway_config = RuntimeGatewayConfig {
            streaming_enabled: config.streaming.enabled,
            display: config.display.clone(),
            service_tier: config.agent.normalized_service_tier(),
            resource_limits_enabled: config.resource_limits_enabled,
            ..RuntimeGatewayConfig::default()
        };
        let session_manager = Arc::new(SessionManager::with_resource_limits(
            config.session.clone(),
            config.resource_limits_enabled,
        ));
        let gateway = Arc::new(Gateway::new(
            session_manager,
            DmManager::with_ignore_behavior(),
            runtime_gateway_config,
        ));

        let outbound = ChatOutboundBuffer::default();
        let adapter = Arc::new(HttpPlatformAdapter {
            buf: outbound.clone(),
        });
        gateway.register_adapter(HTTP_PLATFORM, adapter).await;

        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_tools = Arc::new(bridge_tool_registry(&tool_registry));
        let config_arc = Arc::new(config.clone());
        let config_arc_stream = config_arc.clone();
        let agent_tools_stream = agent_tools.clone();
        let run_runtime_for_message = run_runtime.clone();
        let run_runtime_for_stream = run_runtime.clone();
        let execution_mode_for_message_handler = execution_mode;
        let execution_mode_for_stream_handler = execution_mode;

        gateway
            .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                let config = config_arc.clone();
                let agent_tools = agent_tools.clone();
                let run_runtime = run_runtime_for_message.clone();
                Box::pin(async move {
                    hermes_telemetry::record_llm_request();
                    let effective_model = resolve_model_for_gateway(
                        config.model.as_deref().unwrap_or("dynamic"),
                        &ctx,
                    );
                    let agent = build_agent_for_gateway_context(
                        config.as_ref(),
                        &ctx,
                        agent_tools,
                        run_runtime,
                        execution_mode_for_message_handler,
                    )?;
                    let result = agent
                        .run(messages, None)
                        .await
                        .map_err(|e| GatewayError::Platform(e.to_string()))?;
                    let home = ctx
                        .home
                        .as_deref()
                        .or(config.home_dir.as_deref())
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    if let Some(h) = home {
                        if !ctx.session_key.trim().is_empty() {
                            let sp = SessionPersistence::new(FsPath::new(h));
                            let sys = leading_system_prompt_for_persist(&result.messages);
                            let _ = sp.persist_session(
                                &ctx.session_key,
                                &result.messages,
                                Some(&effective_model),
                                Some(ctx.platform.as_str()),
                                None,
                                sys.as_deref(),
                            );
                        }
                    }
                    Ok(extract_last_assistant_reply(&result.messages))
                })
            }))
            .await;

        gateway
            .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
                let config = config_arc_stream.clone();
                let agent_tools = agent_tools_stream.clone();
                let run_runtime = run_runtime_for_stream.clone();
                Box::pin(async move {
                    hermes_telemetry::record_llm_request();
                    let effective_model = resolve_model_for_gateway(
                        config.model.as_deref().unwrap_or("dynamic"),
                        &ctx,
                    );
                    let agent = build_agent_for_gateway_context(
                        config.as_ref(),
                        &ctx,
                        agent_tools,
                        run_runtime,
                        execution_mode_for_stream_handler,
                    )?;
                    let emit = on_chunk.clone();
                    let muted = Arc::new(Mutex::new(false));
                    let muted_cb = muted.clone();
                    let (observer_sink, mut observer_rx) =
                        hermes_agent::AgentObserverSink::channel(256);
                    let mut observer_task = tokio::spawn(async move {
                        while let Some(event) = observer_rx.recv().await {
                            let hermes_agent::AgentObserverEvent::RenderDelta(delta) = event else {
                                continue;
                            };
                            if let Some(enabled) = delta.mute_post_response {
                                if let Ok(mut state) = muted_cb.lock() {
                                    *state = enabled;
                                }
                            }
                            let is_muted = muted_cb.lock().map(|state| *state).unwrap_or(false);
                            if is_muted {
                                continue;
                            }
                            if let Some(text) = delta.text {
                                if delta.break_before {
                                    emit("\n\n".to_string());
                                }
                                emit(text);
                            }
                        }
                    });

                    let result = agent
                        .run_stream(messages, None, observer_sink.clone())
                        .await
                        .map_err(|e| GatewayError::Platform(e.to_string()));
                    let flushed = observer_sink.flush().await;
                    drop(observer_sink);
                    if !flushed
                        || tokio::time::timeout(
                            std::time::Duration::from_millis(100),
                            &mut observer_task,
                        )
                        .await
                        .is_err()
                    {
                        observer_task.abort();
                    }
                    let result = result?;
                    let home = ctx
                        .home
                        .as_deref()
                        .or(config.home_dir.as_deref())
                        .map(str::trim)
                        .filter(|s| !s.is_empty());
                    if let Some(h) = home {
                        if !ctx.session_key.trim().is_empty() {
                            let sp = SessionPersistence::new(FsPath::new(h));
                            let sys = leading_system_prompt_for_persist(&result.messages);
                            let _ = sp.persist_session(
                                &ctx.session_key,
                                &result.messages,
                                Some(&effective_model),
                                Some(ctx.platform.as_str()),
                                None,
                                sys.as_deref(),
                            );
                        }
                    }
                    Ok(extract_last_assistant_reply(&result.messages))
                })
            }))
            .await;

        gateway
            .start_all()
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;

        Ok(Self {
            config: Arc::new(config),
            tool_registry,
            run_runtime,
            #[cfg(test)]
            execution_mode,
            gateway,
            outbound,
        })
    }
}

fn run_sessions_db_auto_maintenance(config: &GatewayConfig) {
    if !config.sessions.auto_prune {
        return;
    }
    let home = config
        .home_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let sp = SessionPersistence::new(home);
    let result = sp.maybe_auto_prune_and_vacuum(
        config.sessions.retention_days,
        config.sessions.min_interval_hours,
        config.sessions.vacuum_after_prune,
    );
    if let Some(err) = result.error {
        tracing::debug!("sessions db auto-maintenance skipped: {}", err);
    } else if !result.skipped && result.pruned > 0 {
        tracing::info!(
            "sessions db auto-maintenance pruned {} session(s){}",
            result.pruned,
            if result.vacuumed { " + vacuum" } else { "" }
        );
    }
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    pub text: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub personality: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SendMessageResponse {
    pub session_id: String,
    pub reply: String,
    pub message_count: usize,
}

#[derive(Debug, Deserialize)]
pub struct CommandRequest {
    pub command: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandResponse {
    pub accepted: bool,
    pub output: String,
}

fn default_rpc_params() -> Value {
    Value::Object(Map::new())
}

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default = "default_rpc_params")]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct RpcErrorBody {
    pub code: i64,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct RpcResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcErrorBody>,
}

const MAX_WS_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

fn max_request_body_bytes() -> usize {
    const DEFAULT: usize = 2 * 1024 * 1024;
    std::env::var("HERMES_HTTP_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
}
fn websocket_message_limit(config: &GatewayConfig) -> Option<usize> {
    config
        .resource_limits_enabled
        .then_some(MAX_WS_MESSAGE_BYTES)
}

pub fn router(state: HttpServerState) -> Router {
    let security = Arc::new(security::HttpSecurity::from_env());
    let configured_callback_path = security
        .dashboard_auth
        .oidc
        .as_ref()
        .map(|oidc| oidc.callback_path.clone())
        .filter(|path| path != "/auth/oidc/callback");
    let rate = Arc::new(security::RateLimiter::new(security.rate_limit_per_minute));
    let sec_guard = security.clone();
    let rate_guard = rate.clone();
    let body_limit = max_request_body_bytes();

    let routes = Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_prometheus))
        .route("/auth/status", get(dashboard_oidc::auth_status))
        .route("/auth/logout", get(dashboard_oidc::auth_logout))
        .route("/auth/oidc/login", get(dashboard_oidc::oidc_login))
        .route("/auth/oidc/callback", get(dashboard_oidc::oidc_callback))
        .route("/auth/oidc/refresh", post(dashboard_oidc::oidc_refresh))
        .route("/v1/sessions/{session_id}/messages", post(send_message))
        .route("/v1/commands", post(exec_command))
        .route("/v1/rpc", post(exec_rpc))
        .route("/v1/ws/{session_id}", get(ws_upgrade));
    let routes = if let Some(path) = configured_callback_path {
        routes.route(&path, get(dashboard_oidc::oidc_callback))
    } else {
        routes
    };

    routes
        .with_state(state)
        .layer(middleware::from_fn(move |req, next| {
            let sec = sec_guard.clone();
            let rl = rate_guard.clone();
            async move { security::request_guard(sec, rl, req, next).await }
        }))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(body_limit))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .layer(tower_http::cors::CorsLayer::permissive())
}

pub async fn run_server(addr: SocketAddr, config: GatewayConfig) -> Result<(), AgentError> {
    let state = HttpServerState::build(config).await?;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| AgentError::Io(e.to_string()))?;
    tracing::info!("hermes-http listening on {}", addr);
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("hermes-http graceful shutdown");
    };
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(|e| AgentError::Io(e.to_string()))
}

async fn health() -> impl IntoResponse {
    Json(HealthResponse {
        status: "ok",
        timestamp: Utc::now().to_rfc3339(),
    })
}

async fn metrics_prometheus() -> impl IntoResponse {
    let content_type = format!(
        "text/plain; version={}; charset=utf-8",
        env!("CARGO_PKG_VERSION")
    );
    (
        [(header::CONTENT_TYPE, content_type)],
        hermes_telemetry::prometheus_text(),
    )
}

fn http_user_id(explicit: Option<String>) -> String {
    explicit
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HERMES_HTTP_USER_ID").ok())
        .unwrap_or_else(|| "http".to_string())
}

fn session_key_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(SESSION_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

async fn send_message(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    headers: HeaderMap,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, HttpError> {
    hermes_telemetry::record_http_request();
    let user_id = http_user_id(req.user_id.clone());
    let effective_session_id = session_key_from_headers(&headers).unwrap_or(session_id);

    if req.model.is_some() || req.provider.is_some() {
        let full = resolve_model(
            state.config.model.as_deref().unwrap_or("dynamic"),
            req.provider.as_deref(),
            req.model.as_deref(),
        );
        state
            .gateway
            .merge_request_runtime_overrides(
                HTTP_PLATFORM,
                &effective_session_id,
                &user_id,
                Some(full),
                None,
                req.personality.clone(),
            )
            .await;
    } else if req.personality.is_some() {
        state
            .gateway
            .merge_request_runtime_overrides(
                HTTP_PLATFORM,
                &effective_session_id,
                &user_id,
                None,
                None,
                req.personality.clone(),
            )
            .await;
    }

    state.outbound.clear_chat(&effective_session_id);
    let incoming = IncomingMessage {
        platform: HTTP_PLATFORM.to_string(),
        chat_id: effective_session_id.clone(),
        user_id,
        text: req.text,
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    state
        .gateway
        .route_message_from_sender(&incoming, IncomingSender::unknown())
        .await
        .map_err(|e| HttpError {
            status: StatusCode::BAD_GATEWAY,
            message: e.to_string(),
        })?;

    let parts = state.outbound.drain_chat(&effective_session_id);
    let reply = if parts.is_empty() {
        "(no gateway output)".to_string()
    } else {
        parts.join("\n")
    };

    let message_count = state
        .gateway
        .session_transcript_len(HTTP_PLATFORM, &effective_session_id, &incoming.user_id)
        .await;

    Ok(Json(SendMessageResponse {
        session_id: effective_session_id,
        reply,
        message_count,
    }))
}

async fn exec_command(
    State(state): State<HttpServerState>,
    headers: HeaderMap,
    Json(req): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, HttpError> {
    hermes_telemetry::record_http_request();
    let trimmed = req.command.trim();
    if trimmed.is_empty() {
        return Ok(Json(CommandResponse {
            accepted: false,
            output: "empty command".to_string(),
        }));
    }

    let cmd = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };

    let session_id = session_key_from_headers(&headers).unwrap_or_else(|| {
        req.session_id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "default".to_string())
    });
    let user_id = http_user_id(req.user_id.clone());

    state.outbound.clear_chat(&session_id);
    let incoming = IncomingMessage {
        platform: HTTP_PLATFORM.to_string(),
        chat_id: session_id,
        user_id,
        text: cmd,
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    state
        .gateway
        .route_message_from_sender(&incoming, IncomingSender::unknown())
        .await
        .map_err(|e| HttpError {
            status: StatusCode::BAD_GATEWAY,
            message: e.to_string(),
        })?;

    let chat_id = incoming.chat_id.clone();
    let parts = state.outbound.drain_chat(&chat_id);
    let output = if parts.is_empty() {
        "(no gateway output)".to_string()
    } else {
        parts.join("\n")
    };

    Ok(Json(CommandResponse {
        accepted: true,
        output,
    }))
}

fn rpc_ok(id: Option<Value>, result: Value) -> Json<RpcResponse> {
    Json(RpcResponse {
        id,
        result: Some(result),
        error: None,
    })
}

fn rpc_err(id: Option<Value>, code: i64, message: impl Into<String>) -> Json<RpcResponse> {
    Json(RpcResponse {
        id,
        result: None,
        error: Some(RpcErrorBody {
            code,
            message: message.into(),
        }),
    })
}

fn rpc_param_str(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn rpc_param_u32(params: &Value, key: &str, default: u32) -> u32 {
    params
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn rpc_param_f64(params: &Value, key: &str, default: Option<f64>) -> Option<f64> {
    match params.get(key) {
        Some(Value::Number(number)) => number.as_f64(),
        Some(Value::String(raw)) => raw.trim().parse::<f64>().ok(),
        Some(Value::Null) | None => default,
        _ => default,
    }
}

fn rpc_param_variables(params: &Value) -> Map<String, Value> {
    params
        .get("variables")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn rpc_harness_autonomy_payload(params: &Value) -> Result<Value, String> {
    let cwd = rpc_param_str(params, "cwd").map(PathBuf::from);
    hermes_tools::tools::harness_cockpit::harness_cockpit_action_snapshot(
        "autonomy",
        cwd.as_deref(),
    )
}

fn main_runtime_from_model(model: Option<String>) -> Option<AuxiliaryMainRuntime> {
    let model = model?;
    let (provider, model_part) = model.split_once(':')?;
    let provider = provider.trim();
    let model_part = model_part.trim();
    if provider.is_empty() || model_part.is_empty() {
        return None;
    }
    Some(AuxiliaryMainRuntime::new(provider, model_part))
}

async fn exec_rpc(
    State(state): State<HttpServerState>,
    Json(req): Json<RpcRequest>,
) -> Json<RpcResponse> {
    hermes_telemetry::record_http_request();
    let RpcRequest { id, method, params } = req;
    let method = method.trim();
    match method {
        "project.facts" => {
            let cwd = rpc_param_str(&params, "cwd").map(PathBuf::from);
            let facts =
                hermes_tools::tools::project_workspace::project_facts_snapshot(cwd.as_deref());
            rpc_ok(id, serde_json::json!({ "facts": facts }))
        }
        "project.tree" => {
            let cwd = rpc_param_str(&params, "cwd").map(PathBuf::from);
            let max_depth = rpc_param_u32(&params, "max_depth", 3) as usize;
            let max_entries = rpc_param_u32(&params, "max_entries", 200) as usize;
            let include_hidden = params
                .get("include_hidden")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let tree = hermes_tools::tools::project_workspace::project_tree_snapshot(
                cwd.as_deref(),
                max_depth,
                max_entries,
                include_hidden,
            );
            rpc_ok(id, serde_json::json!({ "tree": tree }))
        }
        "verification.status" => {
            let cwd = rpc_param_str(&params, "cwd").map(PathBuf::from);
            let session_id = rpc_param_str(&params, "session_id")
                .or_else(|| rpc_param_str(&params, "session_key"));
            let verification = hermes_tools::verification_evidence::verification_status(
                session_id.as_deref(),
                cwd.as_deref(),
            );
            rpc_ok(id, serde_json::json!({ "verification": verification }))
        }
        "harness.autonomy" => match rpc_harness_autonomy_payload(&params) {
            Ok(payload) => rpc_ok(id, serde_json::json!({ "autonomy": payload })),
            Err(err) => rpc_err(id, 4220, err),
        },
        "llm.oneshot" => {
            let template = rpc_param_str(&params, "template");
            let instructions = rpc_param_str(&params, "instructions").unwrap_or_default();
            let user_input = rpc_param_str(&params, "input")
                .or_else(|| rpc_param_str(&params, "user_input"))
                .unwrap_or_default();
            if template.is_none() && instructions.trim().is_empty() && user_input.trim().is_empty()
            {
                return rpc_err(
                    id,
                    4030,
                    "llm.oneshot requires a template or instructions/input",
                );
            }

            let session_id = rpc_param_str(&params, "session_id")
                .or_else(|| rpc_param_str(&params, "session_key"));
            let user_id = http_user_id(rpc_param_str(&params, "user_id"));
            let main_runtime = match session_id.as_deref() {
                Some(session_id) => main_runtime_from_model(
                    state
                        .gateway
                        .effective_model_for_session(HTTP_PLATFORM, session_id, &user_id)
                        .await,
                ),
                None => main_runtime_from_model(state.config.model.clone()),
            };
            let (client, _summary) =
                build_auxiliary_client_with_main_runtime(AuxiliaryConfig::default(), main_runtime);
            let request = OneShotRequest {
                instructions,
                user_input,
                template,
                variables: rpc_param_variables(&params),
                task: rpc_param_str(&params, "task").unwrap_or_else(|| "title".to_string()),
                max_tokens: rpc_param_u32(&params, "max_tokens", 1024),
                temperature: rpc_param_f64(&params, "temperature", Some(0.3)),
                timeout_secs: params
                    .get("timeout")
                    .and_then(Value::as_u64)
                    .filter(|value| *value > 0)
                    .or(Some(60)),
            };
            match run_oneshot_with_client(&client, request).await {
                Ok(text) => rpc_ok(id, serde_json::json!({ "text": text })),
                Err(OneShotError::UnknownTemplate(template)) => {
                    rpc_err(id, 4031, format!("unknown one-shot template: {template}"))
                }
                Err(OneShotError::EmptyPrompt) => rpc_err(
                    id,
                    4032,
                    "llm.oneshot requires a template or instructions/input",
                ),
                Err(err) => rpc_err(id, 5030, format!("one-shot generation failed: {err}")),
            }
        }
        _ => rpc_err(id, -32601, format!("unknown method: {method}")),
    }
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    headers: HeaderMap,
) -> Response {
    let session_key_override = session_key_from_headers(&headers);
    let ws = match websocket_message_limit(&state.config) {
        Some(limit) => ws.max_message_size(limit).max_frame_size(limit),
        None => ws,
    };
    ws.on_upgrade(move |socket| handle_ws(socket, state, session_id, session_key_override))
}

async fn handle_ws(
    mut socket: WebSocket,
    state: HttpServerState,
    session_id: String,
    session_key_override: Option<String>,
) {
    let effective_session_id = session_key_override.unwrap_or(session_id);
    let _ = socket
        .send(WsMessage::Text(
            format!("connected session={}", effective_session_id).into(),
        ))
        .await;
    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            WsMessage::Text(text) => {
                let parsed: Option<SendMessageRequest> = serde_json::from_str(&text).ok();
                let request = parsed.unwrap_or_else(|| SendMessageRequest {
                    text: text.to_string(),
                    model: None,
                    provider: None,
                    personality: None,
                    user_id: None,
                });
                let result = send_message(
                    Path(effective_session_id.clone()),
                    State(state.clone()),
                    HeaderMap::new(),
                    Json(request),
                )
                .await;
                match result {
                    Ok(Json(ok)) => {
                        let _ = socket.send(WsMessage::Text(ok.reply.into())).await;
                    }
                    Err(err) => {
                        let _ = socket.send(WsMessage::Text(err.to_string().into())).await;
                    }
                }
            }
            WsMessage::Close(_) => break,
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct HttpError {
    pub status: StatusCode,
    pub message: String,
}

impl From<AgentError> for HttpError {
    fn from(value: AgentError) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: value.to_string(),
        }
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

fn parse_provider_api_mode(value: &str) -> Option<ApiMode> {
    match value.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "chat_completions" => Some(ApiMode::ChatCompletions),
        "anthropic_messages" => Some(ApiMode::AnthropicMessages),
        "codex_responses" => Some(ApiMode::CodexResponses),
        "bedrock_converse" => Some(ApiMode::BedrockConverse),
        _ => None,
    }
}

pub fn build_agent_config(
    config: &GatewayConfig,
    model: &str,
    execution_mode: ResolvedExecutionMode,
) -> AgentConfig {
    let provider_from_model = model.split_once(':').map(|(p, _)| p.to_string());
    let skip_context_files_env = std::env::var("HERMES_SKIP_CONTEXT_FILES")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        hermes_home: config.home_dir.clone(),
        provider: provider_from_model,
        stream: config.streaming.enabled,
        delegation_model: config
            .delegation
            .model
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_provider: config
            .delegation
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_base_url: config
            .delegation
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        delegation_api_key: config
            .delegation
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        skip_context_files: config.agent.skip_context_files || skip_context_files_env,
        platform: Some("http".to_string()),
        execution_mode,
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    hermes_agent::agent_loop::RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        base_url: cfg.base_url.clone(),
                        request_timeout_seconds: cfg.request_timeout_seconds,
                        api_mode: cfg.api_mode.as_deref().and_then(parse_provider_api_mode),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        prefill_messages: hermes_config::load_prefill_messages(config),
        smart_model_routing: hermes_agent::agent_loop::SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                hermes_agent::agent_loop::CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        code_index_enabled: config.agent.code_index_enabled,
        code_index_max_files: config.agent.code_index_max_files,
        code_index_max_symbols: config.agent.code_index_max_symbols,
        lsp_context_enabled: config.agent.lsp_context_enabled,
        lsp_context_max_chars: config.agent.lsp_context_max_chars,
        preflight_context_compress: config.agent.preflight_context_compress,
        ..AgentConfig::default()
    }
}

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    let tools = Arc::new(tools.clone());
    for definition in tools.runtime_definitions() {
        agent_registry.register_runtime_tool(Arc::clone(&tools), definition);
    }
    agent_registry
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    build_runtime_provider(config, model)
}

fn resolve_model_for_gateway(default_model: &str, ctx: &GatewayRuntimeContext) -> String {
    if let Some(model) = &ctx.model {
        if model.contains(':') {
            return model.clone();
        }
        if let Some(provider) = &ctx.provider {
            return format!("{}:{}", provider, model);
        }
        return model.clone();
    }

    if let Some(provider) = &ctx.provider {
        if default_model.contains(':') {
            if let Some((_, model_part)) = default_model.split_once(':') {
                return format!("{}:{}", provider, model_part);
            }
        }
        return format!("{}:{}", provider, default_model);
    }

    default_model.to_string()
}

fn build_agent_for_gateway_context(
    config: &GatewayConfig,
    ctx: &GatewayRuntimeContext,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
    run_runtime: Arc<RunRuntime>,
    execution_mode: ResolvedExecutionMode,
) -> Result<AgentLoop, GatewayError> {
    let effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("dynamic"), ctx);
    let effective_model =
        select_startup_model_with_fallback(config, &effective_model).selected_model;
    let memory_subject = MemorySubject::local_owner(MemorySurface::Http, ctx.session_key.clone())
        .map_err(|err| GatewayError::Platform(err.to_string()))?;
    let provider = build_provider(config, &effective_model);
    let mut agent_config = build_agent_config(config, &effective_model, execution_mode);
    if let Some(personality) = ctx.personality.clone() {
        agent_config.personality = Some(personality);
    }
    if !ctx.session_key.trim().is_empty() {
        agent_config.session_id = Some(ctx.session_key.clone());
    }
    let home = ctx
        .home
        .as_deref()
        .or(config.home_dir.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(h) = home {
        let _ = AgentLoop::hydrate_stored_system_prompt_from_hermes_home(
            &mut agent_config,
            FsPath::new(h),
        );
    }
    hermes_agent::attach_discovered_memory(
        AgentLoop::new(agent_config, agent_tools, provider).with_run_runtime(run_runtime),
        memory_subject,
    )
    .map_err(|err| GatewayError::Platform(err.to_string()))
}

fn extract_last_assistant_reply(messages: &[Message]) -> String {
    hermes_gateway::platforms::helpers::extract_current_turn_gateway_reply(messages)
}

fn resolve_model(default_model: &str, provider: Option<&str>, model: Option<&str>) -> String {
    match (provider, model) {
        (Some(p), Some(m)) if !m.contains(':') => format!("{}:{}", p, m),
        (Some(_), Some(m)) => m.to_string(),
        (Some(p), None) => {
            let m = default_model
                .split_once(':')
                .map(|(_, mm)| mm)
                .unwrap_or(default_model);
            format!("{}:{}", p, m)
        }
        (None, Some(m)) => m.to_string(),
        (None, None) => default_model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::{ExecutionModeConfigValue, LlmProviderConfig, EXECUTION_MODE_ENV_KEY};
    use hermes_core::{ExecutionMode, ExecutionModeSource};
    use hermes_provider_runtime::{
        normalize_runtime_provider_name, provider_default_base_url, ARCEE_BASE_URL,
        GEMINI_BASE_URL, GMI_BASE_URL, TENCENT_TOKENHUB_BASE_URL, XIAOMI_BASE_URL,
    };

    fn execution_mode_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
            std::sync::LazyLock::new(|| std::sync::Mutex::new(()));
        LOCK.lock().expect("execution mode env lock poisoned")
    }

    struct ExecutionModeEnvGuard {
        original: Option<std::ffi::OsString>,
    }

    impl ExecutionModeEnvGuard {
        fn capture() -> Self {
            Self {
                original: std::env::var_os(EXECUTION_MODE_ENV_KEY),
            }
        }
    }

    impl Drop for ExecutionModeEnvGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => std::env::set_var(EXECUTION_MODE_ENV_KEY, value),
                None => std::env::remove_var(EXECUTION_MODE_ENV_KEY),
            }
        }
    }

    fn config_for_state_root(state_root: &std::path::Path) -> GatewayConfig {
        GatewayConfig {
            home_dir: Some(state_root.to_string_lossy().into_owned()),
            ..GatewayConfig::default()
        }
    }

    fn assert_runtime_was_not_opened(state_root: &std::path::Path) {
        assert!(
            !state_root.exists(),
            "invalid mode must not create the state root: {}",
            state_root.display()
        );
        assert!(
            !state_root.join("sessions.db").exists(),
            "invalid mode must not create or migrate sessions.db"
        );
    }

    #[test]
    fn websocket_buffer_limits_are_opt_in() {
        let mut config = GatewayConfig::default();
        assert_eq!(websocket_message_limit(&config), None);

        config.resource_limits_enabled = true;
        assert_eq!(websocket_message_limit(&config), Some(MAX_WS_MESSAGE_BYTES));
    }

    #[test]
    fn gateway_provider_aliases_resolve_to_direct_provider_defaults() {
        let cases = [
            ("google-ai-studio", "gemini", GEMINI_BASE_URL),
            ("gmicloud", "gmi", GMI_BASE_URL),
            ("arcee-ai", "arcee", ARCEE_BASE_URL),
            ("mimo", "xiaomi", XIAOMI_BASE_URL),
            ("tencentmaas", "tencent-tokenhub", TENCENT_TOKENHUB_BASE_URL),
        ];

        for (alias, canonical, base_url) in cases {
            assert_eq!(normalize_runtime_provider_name(alias), canonical);
            assert_eq!(provider_default_base_url(canonical), Some(base_url));
        }
    }

    #[test]
    fn build_agent_config_maps_provider_request_timeout_seconds() {
        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                request_timeout_seconds: Some(45.5),
                ..Default::default()
            },
        );

        let agent_config =
            build_agent_config(&config, "openai:dynamic", ResolvedExecutionMode::default());

        assert_eq!(
            agent_config
                .runtime_providers
                .get("openai")
                .and_then(|cfg| cfg.request_timeout_seconds),
            Some(45.5)
        );
    }

    #[test]
    fn rpc_harness_autonomy_exposes_dashboard_surface() {
        let payload = rpc_harness_autonomy_payload(&serde_json::json!({})).expect("autonomy");
        let rendered = serde_json::to_string(&payload).expect("render");
        assert!(rendered.contains("ultra_autonomy"));
        assert!(rendered.contains("contextlattice"));
        assert!(rendered.contains("resource_plan"));
    }

    #[test]
    fn build_agent_config_maps_preflight_context_compress() {
        let mut config = GatewayConfig::default();
        config.agent.preflight_context_compress = false;

        let agent_config =
            build_agent_config(&config, "openai:dynamic", ResolvedExecutionMode::default());

        assert!(!agent_config.preflight_context_compress);
    }

    #[test]
    fn build_agent_config_loads_prefill_messages() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("prefill.json"),
            r#"[{"role":"system","content":"http prefill"},{"role":"user","content":"http example"}]"#,
        )
        .unwrap();
        let config = GatewayConfig {
            home_dir: Some(dir.path().to_string_lossy().to_string()),
            prefill_messages_file: Some("prefill.json".to_string()),
            ..GatewayConfig::default()
        };

        let agent_config =
            build_agent_config(&config, "openai:dynamic", ResolvedExecutionMode::default());

        assert_eq!(agent_config.prefill_messages.len(), 2);
        assert_eq!(
            agent_config.prefill_messages[0].content.as_deref(),
            Some("http prefill")
        );
        assert_eq!(
            agent_config.prefill_messages[1].content.as_deref(),
            Some("http example")
        );
    }

    #[test]
    fn build_agent_config_maps_delegation_provider_model_runtime_overrides() {
        let mut config = GatewayConfig::default();
        config.delegation.model = Some(" meta-llama/llama-4-scout ".to_string());
        config.delegation.provider = Some(" openrouter ".to_string());
        config.delegation.base_url = Some(" https://openrouter.ai/api/v1 ".to_string());
        config.delegation.api_key = Some(" sk-or-test ".to_string());

        let agent_config =
            build_agent_config(&config, "nous:hermes-3", ResolvedExecutionMode::default());

        assert_eq!(
            agent_config.delegation_model.as_deref(),
            Some("meta-llama/llama-4-scout")
        );
        assert_eq!(
            agent_config.delegation_provider.as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            agent_config.delegation_base_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
        assert_eq!(
            agent_config.delegation_api_key.as_deref(),
            Some("sk-or-test")
        );
    }

    #[tokio::test]
    async fn startup_rejects_empty_and_unknown_config_modes_before_opening_runtime() {
        let _env_lock = execution_mode_env_lock();
        let _env_guard = ExecutionModeEnvGuard::capture();
        std::env::remove_var(EXECUTION_MODE_ENV_KEY);

        for raw in ["", "unknown_mode"] {
            let temp_dir = tempfile::tempdir().expect("test state parent");
            let state_root = temp_dir.path().join("state");
            let mut config = config_for_state_root(&state_root);
            config.execution_mode = ExecutionModeConfigValue::value(raw);
            config.sessions.auto_prune = true;

            let error = HttpServerState::build(config)
                .await
                .err()
                .expect("invalid config execution mode must fail startup");

            assert!(
                error
                    .to_string()
                    .contains(ExecutionModeSource::ConfigFile.as_str()),
                "expected config-file validation for {raw:?}, got {error}"
            );
            assert_runtime_was_not_opened(&state_root);
        }
    }

    #[tokio::test]
    async fn startup_rejects_empty_and_unknown_environment_modes_before_opening_runtime() {
        let _env_lock = execution_mode_env_lock();
        let _env_guard = ExecutionModeEnvGuard::capture();

        for raw in ["", "unknown_mode"] {
            let temp_dir = tempfile::tempdir().expect("test state parent");
            let state_root = temp_dir.path().join("state");
            std::env::set_var(EXECUTION_MODE_ENV_KEY, raw);

            let mut config = config_for_state_root(&state_root);
            config.sessions.auto_prune = true;
            let error = HttpServerState::build(config)
                .await
                .err()
                .expect("invalid environment execution mode must fail startup");

            assert!(
                error
                    .to_string()
                    .contains(ExecutionModeSource::Environment.as_str()),
                "expected environment validation for {raw:?}, got {error}"
            );
            assert_runtime_was_not_opened(&state_root);
        }
    }

    #[tokio::test]
    async fn startup_manual_smoke_propagates_the_resolved_execution_mode() {
        let _env_lock = execution_mode_env_lock();
        let _env_guard = ExecutionModeEnvGuard::capture();
        std::env::remove_var(EXECUTION_MODE_ENV_KEY);

        let temp_dir = tempfile::tempdir().expect("test state parent");
        let state_root = temp_dir.path().join("state");
        let mut config = config_for_state_root(&state_root);
        config.execution_mode = ExecutionModeConfigValue::value("manual");
        let resolved = hermes_config::resolve_runtime_execution_mode(&config, None)
            .expect("resolve explicit manual mode");
        assert_eq!(
            resolved,
            ResolvedExecutionMode {
                mode: ExecutionMode::Manual,
                source: ExecutionModeSource::ConfigFile,
            }
        );

        let run_runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(&state_root).expect("open test runtime"),
        );
        let state = HttpServerState::build_with_run_runtime(config, run_runtime, resolved)
            .await
            .expect("build HTTP state with manual mode");

        assert!(state_root.join("sessions.db").is_file());
        assert_eq!(state.execution_mode, resolved);
        assert_eq!(
            build_agent_config(
                state.config.as_ref(),
                "openai:dynamic",
                state.execution_mode
            )
            .execution_mode,
            resolved
        );
    }
}
