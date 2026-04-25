//! HTTP and WebSocket API server for Hermes.
//!
//! Environment (see also `security` module):
//! - `HERMES_HTTP_MAX_BODY_BYTES` — max JSON body size for POST routes (default 2 MiB).
//! Policy HTTP routes are intentionally omitted (Hermes Python does not expose them).

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
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use futures::StreamExt;
use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::provider::{
    AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
};
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_agent::session_persistence::SessionPersistence;
use hermes_agent::{leading_system_prompt_for_persist, AgentConfig, AgentLoop};
use hermes_config::GatewayConfig;
use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_core::{AgentError, LlmProvider, Message, MessageRole, StreamChunk};
use hermes_gateway::gateway::{GatewayConfig as RuntimeGatewayConfig, IncomingMessage};
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_tools::ToolRegistry;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HTTP_PLATFORM: &str = "http";

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
    gateway: Arc<Gateway>,
    outbound: ChatOutboundBuffer,
}

impl HttpServerState {
    pub async fn build(config: GatewayConfig) -> Result<Self, AgentError> {
        run_sessions_db_auto_maintenance(&config);
        let runtime_gateway_config = RuntimeGatewayConfig {
            streaming_enabled: config.streaming.enabled,
            ..RuntimeGatewayConfig::default()
        };
        let session_manager = Arc::new(SessionManager::new(config.session.clone()));
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

        gateway
            .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                let config = config_arc.clone();
                let agent_tools = agent_tools.clone();
                Box::pin(async move {
                    hermes_telemetry::record_llm_request();
                    let effective_model = resolve_model_for_gateway(
                        config.model.as_deref().unwrap_or("gpt-4o"),
                        &ctx,
                    );
                    let agent = build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
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
                Box::pin(async move {
                    hermes_telemetry::record_llm_request();
                    let effective_model = resolve_model_for_gateway(
                        config.model.as_deref().unwrap_or("gpt-4o"),
                        &ctx,
                    );
                    let agent = build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
                    let emit = on_chunk.clone();
                    let ui_state = Arc::new(Mutex::new((false, false))); // (muted, needs_break)
                    let ui_state_cb = ui_state.clone();
                    let stream_cb: Box<dyn Fn(StreamChunk) + Send + Sync> =
                        Box::new(move |chunk: StreamChunk| {
                            if let Some(delta) = chunk.delta {
                                if let Some(extra) = delta.extra.as_ref() {
                                    if let Some(control) =
                                        extra.get("control").and_then(|v| v.as_str())
                                    {
                                        if control == "mute_post_response" {
                                            let enabled = extra
                                                .get("enabled")
                                                .and_then(|v| v.as_bool())
                                                .unwrap_or(false);
                                            if let Ok(mut st) = ui_state_cb.lock() {
                                                st.0 = enabled;
                                            }
                                        } else if control == "stream_break" {
                                            if let Ok(mut st) = ui_state_cb.lock() {
                                                st.1 = true;
                                            }
                                        }
                                    }
                                }
                                if let Some(text) = delta.content {
                                    if let Ok(mut st) = ui_state_cb.lock() {
                                        if st.0 {
                                            return;
                                        }
                                        if st.1 {
                                            emit("\n\n".to_string());
                                            st.1 = false;
                                        }
                                    }
                                    emit(text);
                                }
                            }
                        });

                    let result = agent
                        .run_stream(messages, None, Some(stream_cb))
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
            .start_all()
            .await
            .map_err(|e| AgentError::Io(e.to_string()))?;

        Ok(Self {
            config: Arc::new(config),
            tool_registry,
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

fn max_request_body_bytes() -> usize {
    const DEFAULT: usize = 2 * 1024 * 1024;
    std::env::var("HERMES_HTTP_MAX_BODY_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT)
}

pub fn router(state: HttpServerState) -> Router {
    let security = Arc::new(security::HttpSecurity::from_env());
    let rate = Arc::new(security::RateLimiter::new(security.rate_limit_per_minute));
    let sec_guard = security.clone();
    let rate_guard = rate.clone();
    let body_limit = max_request_body_bytes();

    Router::new()
        .route("/health", get(health))
        .route("/metrics", get(metrics_prometheus))
        .route("/v1/sessions/{session_id}/messages", post(send_message))
        .route("/v1/commands", post(exec_command))
        .route("/v1/ws/{session_id}", get(ws_upgrade))
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
    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        hermes_telemetry::prometheus_text(),
    )
}

fn http_user_id(explicit: Option<String>) -> String {
    explicit
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HERMES_HTTP_USER_ID").ok())
        .unwrap_or_else(|| "http".to_string())
}

async fn send_message(
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
    Json(req): Json<SendMessageRequest>,
) -> Result<Json<SendMessageResponse>, HttpError> {
    hermes_telemetry::record_http_request();
    let user_id = http_user_id(req.user_id.clone());

    if req.model.is_some() || req.provider.is_some() {
        let full = resolve_model(
            state
                .config
                .model
                .as_deref()
                .unwrap_or("openai:gpt-4o-mini"),
            req.provider.as_deref(),
            req.model.as_deref(),
        );
        state
            .gateway
            .merge_request_runtime_overrides(
                HTTP_PLATFORM,
                &session_id,
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
                &session_id,
                &user_id,
                None,
                None,
                req.personality.clone(),
            )
            .await;
    }

    state.outbound.clear_chat(&session_id);
    let incoming = IncomingMessage {
        platform: HTTP_PLATFORM.to_string(),
        chat_id: session_id.clone(),
        user_id,
        text: req.text,
        message_id: None,
        is_dm: false,
    };

    state
        .gateway
        .route_message(&incoming)
        .await
        .map_err(|e| HttpError {
            status: StatusCode::BAD_GATEWAY,
            message: e.to_string(),
        })?;

    let parts = state.outbound.drain_chat(&session_id);
    let reply = if parts.is_empty() {
        "(no gateway output)".to_string()
    } else {
        parts.join("\n")
    };

    let message_count = state
        .gateway
        .session_transcript_len(HTTP_PLATFORM, &session_id, &incoming.user_id)
        .await;

    Ok(Json(SendMessageResponse {
        session_id,
        reply,
        message_count,
    }))
}

async fn exec_command(
    State(state): State<HttpServerState>,
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

    let session_id = req
        .session_id
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let user_id = http_user_id(req.user_id.clone());

    state.outbound.clear_chat(&session_id);
    let incoming = IncomingMessage {
        platform: HTTP_PLATFORM.to_string(),
        chat_id: session_id,
        user_id,
        text: cmd,
        message_id: None,
        is_dm: false,
    };

    state
        .gateway
        .route_message(&incoming)
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

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    Path(session_id): Path<String>,
    State(state): State<HttpServerState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_ws(socket, state, session_id))
}

async fn handle_ws(mut socket: WebSocket, state: HttpServerState, session_id: String) {
    let _ = socket
        .send(WsMessage::Text(
            format!("connected session={}", session_id).into(),
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
                    Path(session_id.clone()),
                    State(state.clone()),
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

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
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
        skip_context_files: config.agent.skip_context_files || skip_context_files_env,
        platform: Some("http".to_string()),
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
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
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
        ..AgentConfig::default()
    }
}

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        let name = schema.name.clone();
        let tools_clone = tools.clone();
        agent_registry.register(
            name.clone(),
            schema,
            Arc::new(
                move |params: Value| -> Result<String, hermes_core::ToolError> {
                    Ok(tools_clone.dispatch(&name, params))
                },
            ),
        );
    }
    agent_registry
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    let (provider_name, model_name) = model.split_once(':').unwrap_or(("openai", model));
    let provider_config = config.llm_providers.get(provider_name);
    let api_key = provider_config
        .and_then(|c| c.api_key.clone())
        .unwrap_or_default();
    if api_key.is_empty() {
        return Arc::new(GenericProvider::new(
            "https://api.openai.com/v1".to_string(),
            "missing-api-key",
            model_name,
        ));
    }

    let base_url = provider_config.and_then(|c| c.base_url.clone());
    match provider_name {
        "openai" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => Arc::new(OpenRouterProvider::new(&api_key).with_model(model_name)),
        "qwen" => Arc::new(QwenProvider::new(&api_key).with_model(model_name)),
        "kimi" | "moonshot" => Arc::new(KimiProvider::new(&api_key).with_model(model_name)),
        "minimax" => Arc::new(MiniMaxProvider::new(&api_key).with_model(model_name)),
        "nous" => Arc::new(NousProvider::new(&api_key).with_model(model_name)),
        "copilot" => Arc::new(
            CopilotProvider::new(
                base_url.unwrap_or_else(|| "https://api.github.com/copilot".to_string()),
                &api_key,
            )
            .with_model(model_name),
        ),
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name))
        }
    }
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
) -> AgentLoop {
    let effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), ctx);
    let provider = build_provider(config, &effective_model);
    let mut agent_config = build_agent_config(config, &effective_model);
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
    AgentLoop::new(agent_config, agent_tools, provider)
}

fn extract_last_assistant_reply(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|m| {
            if m.role == MessageRole::Assistant {
                m.content.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "(no assistant reply)".to_string())
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
