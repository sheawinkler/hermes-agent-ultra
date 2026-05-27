//! Gateway orchestrator: starts, stops, and routes messages to platform adapters.
//!
//! Implements the full message flow:
//! 1. Platform adapter receives a message
//! 2. Gateway looks up or creates a session via `SessionManager`
//! 3. Gateway checks DM authorization via `DmManager`
//! 4. Gateway invokes the agent loop with the session's message history
//! 5. Gateway sends the response back via the platform adapter
//!
//! Also integrates `StreamManager` for progressive message editing.

use chrono::{DateTime, Utc};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, trace, warn};

/// Placeholder shown while the model is generating (WeCom native stream).
const WECOM_NATIVE_STREAM_THINKING: &str = "思考中...";

/// Interval between WeCom stream refreshes (full accumulated text), matching agent-demo.
fn wecom_native_stream_flush_interval_ms() -> u64 {
    std::env::var("HERMES_WECOM_STREAM_FLUSH_INTERVAL_MS")
        .or_else(|_| std::env::var("HERMES_WECOM_STREAM_CHAR_INTERVAL_MS"))
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(150)
}

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_core::types::{Message, MessageRole};
use hermes_core::{
    transport_fallback_message, InboundEvent, InboundMessagePreparer, InboundPrepareContext,
};

use crate::background::{BackgroundTaskManager, TaskStatus};
use crate::commands::{handle_command, GatewayCommandResult};
use crate::dm::{DmDecision, DmManager};
use crate::hooks::{HookEvent, HookRegistry};
use crate::platforms::helpers::extract_inline_images;
use hermes_config::resolve_outbound_media_path;
use hermes_tools::extract_media;
use hermes_tools::tools::messaging::MessagingSessionContext;
use crate::session::SessionManager;
use crate::stream::{StreamConfig, StreamManager};
use crate::voice::VoiceManager;

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Configuration for the Gateway orchestrator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    /// Enable SSRF protection on outbound URLs (default: true).
    #[serde(default = "default_true")]
    pub ssrf_protection: bool,

    /// Media cache directory path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_cache_dir: Option<String>,

    /// Maximum media cache size in bytes (0 = unlimited).
    #[serde(default)]
    pub media_cache_max_bytes: u64,

    /// Whether to enable streaming output (progressive message editing).
    #[serde(default)]
    pub streaming_enabled: bool,

    /// Streaming configuration.
    #[serde(default)]
    pub streaming: StreamConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            ssrf_protection: true,
            media_cache_dir: None,
            media_cache_max_bytes: 0,
            streaming_enabled: false,
            streaming: StreamConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

fn role_label(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

// ---------------------------------------------------------------------------
// IncomingMessage (platform-agnostic)
// ---------------------------------------------------------------------------

/// A platform-agnostic incoming message for gateway routing.
#[derive(Debug, Clone)]
pub struct IncomingMessage {
    /// Platform name (e.g., "telegram", "discord").
    pub platform: String,
    /// Chat/channel identifier.
    pub chat_id: String,
    /// User identifier.
    pub user_id: String,
    /// Message text content.
    pub text: String,
    /// Structured inbound media URLs or local cache paths.
    pub media_urls: Vec<String>,
    /// Structured inbound media types, aligned by index with `media_urls`.
    pub media_types: Vec<String>,
    /// Platform-specific message ID (for reply threading).
    pub message_id: Option<String>,
    /// Whether this is a DM (direct message) or group message.
    pub is_dm: bool,
    /// Discord interaction id when the message originated from a slash command.
    pub interaction_id: Option<String>,
    /// Discord interaction token for deferred slash follow-up responses.
    pub interaction_token: Option<String>,
}

impl IncomingMessage {
    pub fn new(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        user_id: impl Into<String>,
        text: impl Into<String>,
        is_dm: bool,
    ) -> Self {
        Self {
            platform: platform.into(),
            chat_id: chat_id.into(),
            user_id: user_id.into(),
            text: text.into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm,
            interaction_id: None,
            interaction_token: None,
        }
    }
}

// ---------------------------------------------------------------------------
// MessageHandler callback
// ---------------------------------------------------------------------------

/// Callback type for processing messages through the agent loop.
/// Takes the session messages and returns the agent's response text.
pub type MessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Structured runtime context passed to V2 handlers.
#[derive(Debug, Clone, Default)]
pub struct GatewayRuntimeContext {
    pub session_key: String,
    /// Rotatable session UUID (Python-style).  Equals `session_key` for sessions
    /// that have never been reset; becomes a fresh UUID after each `/new` / `/reset`.
    /// Use this — not `session_key` — as the agent's `session_id` for SQLite persistence.
    pub session_id: String,
    pub platform: String,
    pub chat_id: String,
    pub user_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub profile: Option<String>,
    pub branch: Option<String>,
    pub personality: Option<String>,
    pub home: Option<String>,
    pub verbose: bool,
    pub yolo: bool,
    pub reasoning: bool,
    pub mcp_reload_generation: u64,
    /// Messages queued by handlers to be delivered only after the main reply.
    pub deferred_post_delivery_messages: Option<Arc<StdMutex<Vec<String>>>>,
    /// Release flag shared with handlers for post-delivery gating.
    pub deferred_post_delivery_released: Option<Arc<AtomicBool>>,
}

/// Context-aware callback type for processing messages through the agent loop.
pub type MessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Callback type for streaming message processing.
/// Takes session messages and a chunk callback, returns the final response.
pub type StreamingMessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Context-aware callback type for streaming message processing.
pub type StreamingMessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Debug, Clone, Default)]
struct UsageStats {
    user_messages: u64,
    assistant_messages: u64,
    input_chars: u64,
    output_chars: u64,
    last_updated_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default)]
struct CompressionOutcome {
    removed_messages: usize,
    summary_warning: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionRuntimeState {
    model: Option<String>,
    provider: Option<String>,
    profile: Option<String>,
    branch: Option<String>,
    personality: Option<String>,
    home: Option<String>,
    /// Optional usage budget (same units as `/budget` input; gateway displays as-is).
    budget: Option<f64>,
    verbose: bool,
    yolo: bool,
    reasoning: bool,
}

impl Default for SessionRuntimeState {
    fn default() -> Self {
        Self {
            model: None,
            provider: None,
            profile: None,
            branch: None,
            personality: None,
            home: None,
            budget: None,
            verbose: false,
            yolo: false,
            reasoning: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Central orchestrator for all platform adapters.
///
/// The `Gateway` owns a collection of named `PlatformAdapter` instances,
/// a `SessionManager`, a `DmManager`, and a `StreamManager`. It provides
/// a unified interface to start/stop adapters and route messages.
pub struct Gateway {
    adapters: RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>,
    session_manager: Arc<SessionManager>,
    dm_manager: Arc<RwLock<DmManager>>,
    stream_manager: Arc<StreamManager>,
    config: GatewayConfig,
    /// Optional message handler for processing messages through the agent loop.
    message_handler: RwLock<Option<MessageHandler>>,
    /// Optional context-aware message handler for processing incoming messages.
    message_handler_with_context: RwLock<Option<MessageHandlerWithContext>>,
    /// Optional streaming message handler.
    streaming_handler: RwLock<Option<StreamingMessageHandler>>,
    /// Optional context-aware streaming message handler.
    streaming_handler_with_context: RwLock<Option<StreamingMessageHandlerWithContext>>,
    /// Runtime command state for each session.
    runtime_state: RwLock<HashMap<String, SessionRuntimeState>>,
    /// Basic usage counters for each session.
    usage_stats: RwLock<HashMap<String, UsageStats>>,
    /// Tracks async `/background` and `/btw` tasks.
    background_tasks: Arc<BackgroundTaskManager>,
    /// MCP reload generation number.
    mcp_reload_generation: RwLock<u64>,
    /// Optional hook registry for runtime event emission.
    hook_registry: RwLock<Option<Arc<HookRegistry>>>,
    /// Per-platform allowlist policy for group and slash-command traffic.
    platform_access_policies: RwLock<HashMap<String, PlatformAccessPolicy>>,
    /// Optional agent-layer inbound preparer (vision routing, transcription).
    inbound_preparer: RwLock<Option<Arc<dyn InboundMessagePreparer>>>,
    /// Optional voice/STT manager for inbound audio and outbound TTS.
    voice_manager: RwLock<Option<Arc<VoiceManager>>>,
    /// When false, inbound audio is not transcribed (Python `stt_enabled`).
    stt_enabled: RwLock<bool>,
    /// Current inbound channel for `send_message` session fallback.
    messaging_session: RwLock<Option<Arc<MessagingSessionContext>>>,
    /// Per-session mutex so concurrent inbound messages for one chat queue (Python `_running_agents` guard).
    session_serial: RwLock<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupAccessMode {
    Open,
    Allowlist,
    Disabled,
}

impl Default for GroupAccessMode {
    fn default() -> Self {
        Self::Open
    }
}

/// Direct-message access mode (`platforms.<name>.dm_policy` / `extra.dm_policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DmAccessMode {
    /// Anyone may DM; skip global pairing gate.
    Open,
    /// Unknown senders get pairing / approval prompt.
    #[default]
    Pairing,
    /// Only listed users may DM.
    Allowlist,
    /// All DMs dropped.
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct PlatformAccessPolicy {
    pub allowed_users: HashSet<String>,
    pub admin_users: HashSet<String>,
    pub group_mode: GroupAccessMode,
    pub slash_requires_allowlist: bool,
    pub dm_mode: DmAccessMode,
}

impl PlatformAccessPolicy {
    fn has_allowlist(&self) -> bool {
        !self.allowed_users.is_empty() || !self.admin_users.is_empty()
    }

    fn user_matches_any(user_id: &str, set: &HashSet<String>) -> bool {
        let candidate = user_id.trim();
        if candidate.is_empty() {
            return false;
        }
        let candidate_no_at = candidate.strip_prefix('@').unwrap_or(candidate);
        set.iter().any(|entry| {
            let allowed = entry.trim();
            if allowed.is_empty() {
                return false;
            }
            let allowed_no_at = allowed.strip_prefix('@').unwrap_or(allowed);
            allowed.eq_ignore_ascii_case(candidate)
                || allowed.eq_ignore_ascii_case(candidate_no_at)
                || allowed_no_at.eq_ignore_ascii_case(candidate)
                || allowed_no_at.eq_ignore_ascii_case(candidate_no_at)
        })
    }

    pub fn is_user_allowed(&self, user_id: &str) -> bool {
        Self::user_matches_any(user_id, &self.admin_users)
            || Self::user_matches_any(user_id, &self.allowed_users)
    }
}

impl Gateway {
    /// Create a new `Gateway` with the given session manager and config.
    pub fn new(
        session_manager: Arc<SessionManager>,
        dm_manager: DmManager,
        config: GatewayConfig,
    ) -> Self {
        let stream_manager = Arc::new(StreamManager::new(config.streaming.clone()));

        Self {
            adapters: RwLock::new(HashMap::new()),
            session_manager,
            dm_manager: Arc::new(RwLock::new(dm_manager)),
            stream_manager,
            config,
            message_handler: RwLock::new(None),
            message_handler_with_context: RwLock::new(None),
            streaming_handler: RwLock::new(None),
            streaming_handler_with_context: RwLock::new(None),
            runtime_state: RwLock::new(HashMap::new()),
            usage_stats: RwLock::new(HashMap::new()),
            background_tasks: Arc::new(BackgroundTaskManager::new(8)),
            mcp_reload_generation: RwLock::new(0),
            hook_registry: RwLock::new(None),
            platform_access_policies: RwLock::new(HashMap::new()),
            inbound_preparer: RwLock::new(None),
            voice_manager: RwLock::new(None),
            stt_enabled: RwLock::new(true),
            messaging_session: RwLock::new(None),
            session_serial: RwLock::new(HashMap::new()),
        }
    }

    /// Hold the per-session lock for the full `route_message` pipeline (agent + session writes).
    async fn acquire_session_serial(
        &self,
        session_key: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let mutex = {
            let mut map = self.session_serial.write().await;
            map.entry(session_key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        mutex.lock_owned().await
    }

    /// Register the agent inbound preparer (vision enrich, native multimodal, etc.).
    pub async fn set_inbound_preparer(&self, preparer: Arc<dyn InboundMessagePreparer>) {
        *self.inbound_preparer.write().await = Some(preparer);
    }

    /// Wire voice manager + STT gate from app `config.yaml` (`tts` / `stt` blocks).
    pub async fn set_voice_runtime(&self, manager: Arc<VoiceManager>, stt_enabled: bool) {
        *self.voice_manager.write().await = Some(manager);
        *self.stt_enabled.write().await = stt_enabled;
    }

    /// Share session context with `send_message` for automatic platform/recipient fallback.
    pub async fn set_messaging_session_context(&self, ctx: Arc<MessagingSessionContext>) {
        *self.messaging_session.write().await = Some(ctx);
    }

    fn incoming_to_event(incoming: &IncomingMessage) -> InboundEvent {
        InboundEvent {
            platform: incoming.platform.clone(),
            chat_id: incoming.chat_id.clone(),
            user_id: incoming.user_id.clone(),
            text: incoming.text.clone(),
            media_urls: incoming.media_urls.clone(),
            media_types: incoming.media_types.clone(),
            message_id: incoming.message_id.clone(),
            is_dm: incoming.is_dm,
        }
    }

    /// Create a Gateway with default DM manager (pair behavior).
    pub fn with_defaults(session_manager: Arc<SessionManager>, config: GatewayConfig) -> Self {
        Self::new(session_manager, DmManager::with_pair_behavior(), config)
    }

    /// Merge per-request runtime hints (HTTP API, webhooks) for the composed session key.
    pub async fn merge_request_runtime_overrides(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
        model: Option<String>,
        provider: Option<String>,
        personality: Option<String>,
    ) {
        let session_key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        let mut states = self.runtime_state.write().await;
        let s = states.entry(session_key).or_default();
        if let Some(m) = model {
            s.model = Some(m.clone());
            if m.contains(':') {
                s.provider = None;
            }
        }
        if let Some(p) = provider {
            s.provider = Some(p);
        }
        if let Some(pers) = personality {
            s.personality = Some(pers);
        }
    }

    /// Number of messages currently stored for the session (platform + chat + user).
    pub async fn session_transcript_len(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> usize {
        let key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        self.session_manager.get_messages(&key).await.len()
    }

    async fn clear_session_boundary_security_state(&self, session_key: &str) {
        if session_key.is_empty() {
            return;
        }
        let mut states = self.runtime_state.write().await;
        if let Some(state) = states.get_mut(session_key) {
            state.yolo = false;
        }
    }

    async fn should_apply_reaction_lifecycle(
        &self,
        incoming: &IncomingMessage,
    ) -> bool {
        if incoming.text.trim_start().starts_with('/') {
            return false;
        }
        if incoming
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
        {
            return false;
        }

        let platform = incoming.platform.trim().to_ascii_lowercase();
        if platform == "slack" {
            return incoming.is_dm || incoming.text.contains("<@");
        }
        if platform == "discord" {
            let Some(adapter) = self.get_adapter("discord").await else {
                return false;
            };
            if !adapter.reactions_enabled() {
                return false;
            }
            return incoming.is_dm
                || incoming.text.contains("<@")
                || incoming.text.contains("<@!");
        }
        false
    }

    /// Set the message handler for processing incoming messages.
    pub async fn set_message_handler(&self, handler: MessageHandler) {
        *self.message_handler.write().await = Some(handler);
        *self.message_handler_with_context.write().await = None;
    }

    /// Set a context-aware message handler for processing incoming messages.
    pub async fn set_message_handler_with_context(&self, handler: MessageHandlerWithContext) {
        *self.message_handler_with_context.write().await = Some(handler);
    }

    /// Set the streaming message handler.
    pub async fn set_streaming_handler(&self, handler: StreamingMessageHandler) {
        *self.streaming_handler.write().await = Some(handler);
        *self.streaming_handler_with_context.write().await = None;
    }

    /// Set a context-aware streaming message handler.
    pub async fn set_streaming_handler_with_context(
        &self,
        handler: StreamingMessageHandlerWithContext,
    ) {
        *self.streaming_handler_with_context.write().await = Some(handler);
    }

    /// Attach gateway hook registry for emitting lifecycle/progress events.
    pub async fn set_hook_registry(&self, registry: Arc<HookRegistry>) {
        *self.hook_registry.write().await = Some(registry);
    }

    /// Set per-platform access policies for non-DM and slash-command traffic.
    pub async fn set_platform_access_policies(
        &self,
        policies: HashMap<String, PlatformAccessPolicy>,
    ) {
        *self.platform_access_policies.write().await = policies
            .into_iter()
            .map(|(platform, policy)| (platform.to_ascii_lowercase(), policy))
            .collect();
    }

    async fn platform_access_policy(&self, platform: &str) -> Option<PlatformAccessPolicy> {
        let key = platform.trim().to_ascii_lowercase();
        self.platform_access_policies
            .read()
            .await
            .get(&key)
            .cloned()
    }

    /// Emit one hook event if a registry is configured.
    pub async fn emit_hook_event(&self, event_type: &str, context: serde_json::Value) {
        let registry = self.hook_registry.read().await.clone();
        if let Some(reg) = registry {
            reg.emit(&HookEvent::new(event_type, context)).await;
        }
    }

    /// Register a platform adapter under the given name.
    pub async fn register_adapter(
        &self,
        name: impl Into<String>,
        adapter: Arc<dyn PlatformAdapter>,
    ) {
        let name = name.into();
        info!("Registering platform adapter: {}", name);
        self.adapters.write().await.insert(name, adapter);
    }

    /// Retrieve a registered platform adapter by name.
    pub async fn get_adapter(&self, name: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.adapters.read().await.get(name).cloned()
    }

    /// Start all registered and enabled platform adapters.
    pub async fn start_all(&self) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Starting platform adapter: {}", name);
            if let Err(e) = adapter.start().await {
                error!("Failed to start adapter '{}': {}", name, e);
                return Err(e);
            }
        }
        info!("All platform adapters started successfully");
        Ok(())
    }

    /// Stop all platform adapters gracefully.
    pub async fn stop_all(&self) -> Result<(), GatewayError> {
        let adapters = self.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Stopping platform adapter: {}", name);
            if let Err(e) = adapter.stop().await {
                warn!("Error stopping adapter '{}': {}", name, e);
            }
        }
        info!("All platform adapters stopped");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Message routing
    // -----------------------------------------------------------------------

    /// Route an incoming message through the full pipeline:
    /// DM check → session lookup → agent loop → response.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        let route_start = Instant::now();
        if let Some(ctx) = self.messaging_session.read().await.as_ref() {
            ctx.set(&incoming.platform, &incoming.chat_id);
        }
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            user_id = %incoming.user_id,
            is_dm = incoming.is_dm,
            has_media = !incoming.media_urls.is_empty(),
            text_chars = incoming.text.chars().count(),
            "gateway route start"
        );
        let access_policy = self.platform_access_policy(&incoming.platform).await;
        let is_slash_command = incoming.text.trim_start().starts_with('/');
        if let Some(policy) = access_policy.as_ref() {
            if !incoming.is_dm {
                match policy.group_mode {
                    GroupAccessMode::Disabled => {
                        debug!(
                            platform = incoming.platform,
                            user_id = incoming.user_id,
                            "Group traffic denied by platform policy"
                        );
                        return Ok(());
                    }
                    GroupAccessMode::Allowlist => {
                        if !policy.is_user_allowed(&incoming.user_id) {
                            debug!(
                                platform = incoming.platform,
                                user_id = incoming.user_id,
                                "Group message denied: user not in allowlist"
                            );
                            return Ok(());
                        }
                    }
                    GroupAccessMode::Open => {}
                }
            }
            if is_slash_command
                && policy.slash_requires_allowlist
                && policy.has_allowlist()
                && !policy.is_user_allowed(&incoming.user_id)
            {
                debug!(
                    platform = incoming.platform,
                    user_id = incoming.user_id,
                    "Slash command denied: user not in platform allowlist"
                );
                return Ok(());
            }
        }

        // 1. Check DM authorization if this is a direct message
        if incoming.is_dm {
            let dm_mode = access_policy.as_ref().map(|p| p.dm_mode).unwrap_or_else(|| {
                match incoming.platform.trim().to_ascii_lowercase().as_str() {
                    "wecom" | "weixin" | "qqbot" => DmAccessMode::Open,
                    _ => DmAccessMode::Pairing,
                }
            });

            if dm_mode == DmAccessMode::Disabled {
                debug!(
                    user_id = incoming.user_id,
                    platform = incoming.platform,
                    "DM denied: platform dm_policy is disabled"
                );
                return Ok(());
            }

            if dm_mode == DmAccessMode::Allowlist {
                let allowed = access_policy
                    .as_ref()
                    .is_some_and(|p| p.is_user_allowed(&incoming.user_id));
                if !allowed {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied: user not in platform allowlist"
                    );
                    return Ok(());
                }
            } else if dm_mode != DmAccessMode::Open {
                let dm_manager = self.dm_manager.read().await;
                let decision = dm_manager
                    .handle_dm(&incoming.user_id, &incoming.platform)
                    .await;

                match decision {
                    DmDecision::Allow => {}
                    DmDecision::Pair { message } => {
                        if let Some(msg) = message {
                            warn!(
                                user_id = %incoming.user_id,
                                platform = %incoming.platform,
                                dm_mode = ?dm_mode,
                                "Sending DM pairing approval message"
                            );
                            self.send_incoming_reply(incoming, &msg, None)
                                .await?;
                        }
                        return Ok(());
                    }
                    DmDecision::Deny => {
                        debug!(
                            user_id = incoming.user_id,
                            platform = incoming.platform,
                            "DM denied for unauthorized user"
                        );
                        return Ok(());
                    }
                }
            }
        }

        let is_dm = Some(incoming.is_dm);
        let session_key = self.session_manager.compose_session_key_with_dm(
            &incoming.platform,
            &incoming.chat_id,
            &incoming.user_id,
            is_dm,
        );
        let _session_serial = self.acquire_session_serial(&session_key).await;

        // 2. Get or create session
        let existing_session = self.session_manager.get_session(&session_key).await;
        let session = self
            .session_manager
            .get_or_create_session_with_dm(
                &incoming.platform,
                &incoming.chat_id,
                &incoming.user_id,
                is_dm,
            )
            .await;
        trace!(
            platform = %incoming.platform,
            session_key = %session_key,
            session_started = existing_session.is_none(),
            "gateway session resolved"
        );
        let session_started = existing_session.is_none();
        let session_auto_reset = existing_session
            .as_ref()
            .map(|s| s.created_at != session.created_at)
            .unwrap_or(false);
        if session_started || session_auto_reset {
            self.emit_hook_event(
                "session:start",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "reason": if session_started { "new" } else { "auto_reset" }
                }),
            )
            .await;
        }

        // Slash commands are executed directly by the gateway command runtime.
        if is_slash_command {
            if self.execute_slash_command(incoming, &session_key).await? {
                return Ok(());
            }
        }

        if incoming.platform.eq_ignore_ascii_case("discord") {
            if let Some(adapter) = self.get_adapter("discord").await {
                let chat_id = incoming.chat_id.clone();
                tokio::spawn(async move {
                    let _ = adapter.trigger_typing(&chat_id).await;
                });
            }
        }

        let reaction_adapter = if self.should_apply_reaction_lifecycle(incoming).await {
            self.get_adapter(&incoming.platform).await
        } else {
            None
        };
        if let (Some(adapter), Some(message_id)) =
            (&reaction_adapter, incoming.message_id.as_deref())
        {
            if let Err(err) = adapter
                .add_reaction(&incoming.chat_id, message_id, "eyes")
                .await
            {
                debug!(
                    platform = incoming.platform,
                    chat_id = incoming.chat_id,
                    message_id = message_id,
                    "Failed to add start reaction: {}",
                    err
                );
            }
        }

        let user_message = self
            .prepare_inbound_user_message(incoming, &session_key)
            .await;
        let input_chars = user_message
            .content
            .as_deref()
            .unwrap_or("")
            .chars()
            .count();

        // 3. Add the user message to the session
        self.session_manager
            .add_message(&session_key, user_message)
            .await;
        self.bump_input_usage(&session_key, input_chars)
            .await;

        // 4. Get all session messages for the agent loop
        let messages = self.session_manager.get_messages(&session_key).await;

        // 5. Process through agent loop (streaming or non-streaming)
        let processing_result = if self.config.streaming_enabled {
            self.route_streaming(&incoming, messages, &session_key)
                .await
        } else {
            self.route_non_streaming(&incoming, messages, &session_key)
                .await
        };
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            elapsed_ms = route_start.elapsed().as_millis() as u64,
            success = processing_result.is_ok(),
            "gateway route finished"
        );

        if let (Some(adapter), Some(message_id)) =
            (&reaction_adapter, incoming.message_id.as_deref())
        {
            if let Err(err) = adapter
                .remove_reaction(&incoming.chat_id, message_id, "eyes")
                .await
            {
                debug!(
                    platform = incoming.platform,
                    chat_id = incoming.chat_id,
                    message_id = message_id,
                    "Failed to remove start reaction: {}",
                    err
                );
            }
            let emoji = if processing_result.is_ok() {
                "white_check_mark"
            } else {
                "x"
            };
            if let Err(err) = adapter
                .add_reaction(&incoming.chat_id, message_id, emoji)
                .await
            {
                debug!(
                    platform = incoming.platform,
                    chat_id = incoming.chat_id,
                    message_id = message_id,
                    "Failed to add completion reaction: {}",
                    err
                );
            }
        }

        processing_result?;
        Ok(())
    }

    async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<bool, GatewayError> {
        let result = handle_command(&incoming.text);
        if !matches!(result, GatewayCommandResult::Unknown(_)) {
            if let Some(command_name) = Self::extract_command_name(&incoming.text) {
                self.emit_hook_event(
                    &format!("command:{}", command_name),
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "command": command_name
                    }),
                )
                .await;
            }
        }
        let handled = self
            .apply_command_result(incoming, session_key, result)
            .await?;
        Ok(handled)
    }

    async fn apply_command_result(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        result: GatewayCommandResult,
    ) -> Result<bool, GatewayError> {
        match result {
            GatewayCommandResult::Reply(text)
            | GatewayCommandResult::ShowHelp(text)
            | GatewayCommandResult::Unknown(text) => {
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ResetSession(reply) => {
                self.emit_hook_event(
                    "session:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key
                    }),
                )
                .await;
                self.session_manager.reset_session(session_key).await;
                self.clear_session_boundary_security_state(session_key)
                    .await;
                self.emit_hook_event(
                    "session:reset",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key
                    }),
                )
                .await;
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { model, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().model = Some(model);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchPersonality { name, reply } => {
                let mut states = self.runtime_state.write().await;
                states
                    .entry(session_key.to_string())
                    .or_default()
                    .personality = Some(name);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ApproveUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /approve requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.authorize_user(user_id.clone());
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("✅ User '{}' has been approved for DM access.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::DenyUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /deny requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.deauthorize_user(&user_id);
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("⛔ User '{}' has been removed from DM allowlist.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::StopAgent(reply) => {
                for (task_id, status, _) in self.background_tasks.list_tasks() {
                    if status == TaskStatus::Running {
                        let _ = self.background_tasks.cancel(&task_id);
                    }
                }
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowUsage(_) => {
                let text = self.build_usage_text(session_key).await;
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::CompressContext(_) => {
                let outcome = self.compress_context(session_key, 24).await;
                let mut reply = format!(
                    "📦 Context compressed. Removed {} old messages.",
                    outcome.removed_messages
                );
                if let Some(warning) = outcome.summary_warning {
                    reply.push_str("\n\n");
                    reply.push_str(&warning);
                }
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowInsights(text) => {
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleVerbose(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.verbose = !state.verbose;
                let reply = format!(
                    "📝 Verbose mode: {}",
                    if state.verbose { "ON" } else { "OFF" }
                );
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleYolo(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.yolo = !state.yolo;
                let reply = format!("🤠 YOLO mode: {}", if state.yolo { "ON" } else { "OFF" });
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SetHome { path, reply } => {
                let target = std::path::Path::new(&path);
                let response = if target.exists() && target.is_dir() {
                    let mut states = self.runtime_state.write().await;
                    states.entry(session_key.to_string()).or_default().home = Some(path);
                    reply
                } else {
                    format!("❌ Path not found or not a directory: {}", path)
                };
                self.send_incoming_reply(incoming, &response, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowStatus(_) => {
                let text = self.build_status_text(session_key).await;
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ReloadMcp => {
                let mut generation = self.mcp_reload_generation.write().await;
                *generation += 1;
                let current = *generation;
                drop(generation);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("🔄 MCP registry reloaded (generation {}).", current),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProvider { provider, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProfile { profile, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().profile = Some(profile);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchBranch { branch } => {
                let reply = match branch {
                    Some(name) => {
                        let mut states = self.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().branch =
                            Some(name.clone());
                        format!("🌿 Branch context switched to: {}", name)
                    }
                    None => {
                        let branch = self
                            .runtime_state
                            .read()
                            .await
                            .get(session_key)
                            .and_then(|s| s.branch.clone())
                            .unwrap_or_else(|| "main".to_string());
                        format!("🌿 Current branch context: {}", branch)
                    }
                };
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Rollback { steps } => {
                let mut removed = 0usize;
                for _ in 0..steps {
                    if self
                        .session_manager
                        .pop_last_message(session_key)
                        .await
                        .is_some()
                    {
                        removed += 1;
                    } else {
                        break;
                    }
                }
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("↪️ Rolled back {} message(s).", removed),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::CheckUpdate => {
                let version =
                    std::env::var("HERMES_LATEST_VERSION").unwrap_or_else(|_| "latest".to_string());
                self.send_update_notification(&incoming.platform, &incoming.chat_id, &version)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::BackgroundTask { prompt } => {
                let handled = self
                    .handle_background_command(incoming, session_key, &prompt, false)
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::BtwTask { prompt } => {
                let handled = self
                    .handle_background_command(incoming, session_key, &prompt, true)
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::ToggleReasoning(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.reasoning = !state.reasoning;
                let reply = format!(
                    "🧠 Reasoning visibility: {}",
                    if state.reasoning { "ON" } else { "OFF" }
                );
                drop(states);
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchFast(_) => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().model =
                    Some("openai:gpt-4o-mini".to_string());
                drop(states);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "⚡ Fast model enabled: openai:gpt-4o-mini",
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::Retry => {
                let mut messages = self.session_manager.get_messages(session_key).await;
                if matches!(
                    messages.last().map(|m| m.role),
                    Some(MessageRole::Assistant)
                ) {
                    messages.pop();
                }
                if messages.is_empty() {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "No previous message to retry.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                self.session_manager
                    .replace_messages(session_key, messages.clone())
                    .await;
                self.route_non_streaming(incoming, messages, session_key)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Undo => {
                let mut removed = 0usize;
                if let Some(last) = self.session_manager.pop_last_message(session_key).await {
                    removed += 1;
                    if last.role == MessageRole::Assistant {
                        if let Some(prev) = self.session_manager.pop_last_message(session_key).await
                        {
                            if prev.role == MessageRole::User {
                                removed += 1;
                            }
                        }
                    }
                }
                let reply = if removed == 0 {
                    "Nothing to undo.".to_string()
                } else {
                    format!("↩️ Removed {} message(s) from current session.", removed)
                };
                self.send_incoming_reply(incoming, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ListTools { filter } => {
                let suffix = match &filter {
                    Some(f) => format!(" (filter: `{}`)", f),
                    None => String::new(),
                };
                let text = format!(
                    "🔧 Tools{}.\nRegistered MCP tools are resolved at runtime after reload.",
                    suffix
                );
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::EnableTool { name } => {
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!(
                        "✅ Tool enabled: `{}` (effective on next agent turn).",
                        name
                    ),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::DisableTool { name } => {
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!(
                        "⛔ Tool disabled: `{}` (effective on next agent turn).",
                        name
                    ),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::ListSessions => {
                let sessions = self
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let text = if sessions.is_empty() {
                    "📚 No sessions found for your user.".to_string()
                } else {
                    let mut out = String::from("📚 **Your sessions:**\n\n");
                    for s in sessions {
                        let key = self.session_manager.compose_session_key(
                            &s.platform,
                            &s.chat_id,
                            &s.user_id,
                        );
                        out.push_str(&format!(
                            "• `{}` — {} messages, platform `{}` (id `{}`)\n",
                            key,
                            s.messages.len(),
                            s.platform,
                            s.id
                        ));
                    }
                    out.push_str("\nUse `/sessions <key or id>` to switch.");
                    out
                };
                self.send_incoming_reply(incoming, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchSession { session_id } => {
                let sessions = self
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let matched = sessions.iter().find(|s| {
                    let key = self.session_manager.compose_session_key(
                        &s.platform,
                        &s.chat_id,
                        &s.user_id,
                    );
                    key == session_id || s.id == session_id
                });
                let msg = if let Some(target) = matched {
                    let copied = self
                        .session_manager
                        .replace_messages(session_key, target.messages.clone())
                        .await;
                    if copied {
                        self.clear_session_boundary_security_state(session_key)
                            .await;
                        format!(
                            "🔁 Switched to session `{}`.\nLoaded {} message(s) into this chat context.",
                            session_id,
                            target.messages.len()
                        )
                    } else {
                        format!(
                            "❌ Could not switch to `{}` because the current chat session key is missing.",
                            session_id
                        )
                    }
                } else {
                    format!(
                        "❌ No session matching `{}` for your user. Try `/sessions` to list keys.",
                        session_id
                    )
                };
                self.send_incoming_reply(incoming, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowBudget { new_budget } => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                let msg = match new_budget {
                    Some(b) => {
                        state.budget = Some(b);
                        format!("💰 Usage budget set to {:.4}.", b)
                    }
                    None => match state.budget {
                        Some(b) => format!("💰 Current usage budget: {:.4}.", b),
                        None => {
                            "💰 No usage budget set. Use `/budget <amount>` to set one.".to_string()
                        }
                    },
                };
                drop(states);
                self.send_incoming_reply(incoming, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }

    /// Non-streaming message routing: invoke agent, send complete response.
    async fn route_non_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        self.emit_hook_event(
            "agent:start",
            serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "streaming": false
            }),
        )
        .await;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self.build_runtime_context(incoming, session_key).await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.message_handler_with_context.read().await.clone();
        let agent_start = Instant::now();
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            message_count = messages.len(),
            "gateway non-streaming agent start"
        );
        let response_result = if let Some(handler) = context_handler {
            handler(messages, runtime_context).await
        } else {
            let handler = self.message_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No message handler configured".into()))?;
            let messages = self.inject_runtime_hints(session_key, messages).await;
            handler(messages).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                self.emit_hook_event(
                    "agent:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "streaming": false,
                        "success": false,
                        "error": e.to_string()
                    }),
                )
                .await;
                return Err(e);
            }
        };
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            elapsed_ms = agent_start.elapsed().as_millis() as u64,
            response_chars = response.chars().count(),
            "gateway non-streaming agent finished"
        );

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;

        // Send response back to the platform (text + MEDIA: local attachments)
        self.deliver_response_attachments(&incoming.platform, &incoming.chat_id, &response)
            .await;
        self.send_incoming_reply(incoming, &response, None)
            .await?;
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            deferred_messages,
            deferred_release,
        )
        .await;
        self.emit_hook_event(
            "agent:end",
            serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "streaming": false,
                "success": true,
                "response_chars": response.chars().count()
            }),
        )
        .await;

        Ok(())
    }

    /// Streaming message routing: progressively edit messages as tokens arrive.
    async fn route_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Vec<Message>,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        self.emit_hook_event(
            "agent:start",
            serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "streaming": true
            }),
        )
        .await;
        let deferred_messages = Arc::new(StdMutex::new(Vec::new()));
        let deferred_release = Arc::new(AtomicBool::new(false));
        let mut runtime_context = self.build_runtime_context(incoming, session_key).await;
        runtime_context.deferred_post_delivery_messages = Some(deferred_messages.clone());
        runtime_context.deferred_post_delivery_released = Some(deferred_release.clone());
        let context_handler = self.streaming_handler_with_context.read().await.clone();
        let legacy_messages = self
            .inject_runtime_hints(session_key, messages.clone())
            .await;

        let adapter_for_platform = self.adapters.read().await.get(&incoming.platform).cloned();
        let native_streaming = adapter_for_platform
            .as_ref()
            .map(|a| a.supports_native_streaming())
            .unwrap_or(false);

        let mut stream_id: Option<String> = None;
        let mut native_worker: Option<tokio::task::JoinHandle<()>> = None;
        let native_started = Arc::new(AtomicBool::new(false));
        let native_failed = Arc::new(AtomicBool::new(false));

        let on_chunk: Arc<dyn Fn(String) + Send + Sync> = if native_streaming {
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            let adapter = adapter_for_platform.clone().expect("adapter exists when native_streaming");
            let chat_id = incoming.chat_id.clone();
            let reply_to = incoming.message_id.clone();
            let started = native_started.clone();
            let failed = native_failed.clone();
            native_worker = Some(tokio::spawn(async move {
                let flush_interval =
                    Duration::from_millis(wecom_native_stream_flush_interval_ms());
                let mut ticker = tokio::time::interval(flush_interval);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

                let mut native_stream_id: Option<String> = None;
                let mut accumulated = String::new();
                let mut last_flushed = String::new();

                loop {
                    tokio::select! {
                        chunk = rx.recv() => {
                            match chunk {
                                None => break,
                                Some(chunk) if chunk.trim().is_empty() => {}
                                Some(chunk) => {
                                    if native_stream_id.is_none() {
                                        match adapter
                                            .start_native_stream(
                                                &chat_id,
                                                reply_to.as_deref(),
                                                Some(WECOM_NATIVE_STREAM_THINKING),
                                            )
                                            .await
                                        {
                                            Ok(Some(sid)) => {
                                                native_stream_id = Some(sid);
                                                started.store(true, Ordering::Release);
                                            }
                                            Ok(None) => {
                                                failed.store(true, Ordering::Release);
                                                return;
                                            }
                                            Err(err) => {
                                                warn!(error = %err, "native streaming start failed");
                                                failed.store(true, Ordering::Release);
                                                return;
                                            }
                                        }
                                    }
                                    accumulated.push_str(&chunk);
                                }
                            }
                        }
                        _ = ticker.tick() => {
                            let Some(sid) = native_stream_id.as_deref() else {
                                continue;
                            };
                            if accumulated.is_empty() || accumulated == last_flushed {
                                continue;
                            }
                            if let Err(err) = adapter
                                .send_native_stream_chunk(&chat_id, sid, &accumulated, false)
                                .await
                            {
                                warn!(error = %err, stream_id = %sid, "native streaming chunk failed");
                                failed.store(true, Ordering::Release);
                                return;
                            }
                            last_flushed.clone_from(&accumulated);
                        }
                    }
                }

                if let Some(sid) = native_stream_id.as_deref() {
                    let final_content = if accumulated.is_empty() {
                        WECOM_NATIVE_STREAM_THINKING.to_string()
                    } else {
                        accumulated.clone()
                    };
                    if let Err(err) = adapter
                        .send_native_stream_chunk(&chat_id, sid, &final_content, true)
                        .await
                    {
                        warn!(error = %err, stream_id = %sid, "native streaming finish failed");
                        failed.store(true, Ordering::Release);
                    }
                }
            }));

            Arc::new(move |chunk: String| {
                let _ = tx.send(chunk);
            })
        } else {
            // Start legacy stream manager session.
            let stream_handle = self
                .stream_manager
                .start_stream(&incoming.platform, &incoming.chat_id)
                .await;
            stream_id = Some(stream_handle.id.clone());

            let anchor_id = if let Some(adapter) =
                self.get_adapter(&incoming.platform).await
            {
                adapter
                    .send_message_with_id(&incoming.chat_id, "...", None)
                    .await?
            } else {
                self.send_message(&incoming.platform, &incoming.chat_id, "...", None)
                    .await?;
                None
            };
            if let Some(stream_id) = stream_id.as_ref() {
                if let Some(mid) = anchor_id.as_deref().or(Some("stream-anchor")) {
                    self.stream_manager
                        .set_message_id(stream_id, mid)
                        .await;
                }
            }

            let stream_manager = self.stream_manager.clone();
            let platform = incoming.platform.clone();
            let chat_id = incoming.chat_id.clone();
            let gateway_adapters = self.adapters.read().await.clone();
            let sid = stream_id.clone().unwrap_or_default();

            Arc::new(move |chunk: String| {
                let sm = stream_manager.clone();
                let sid = sid.clone();
                let platform = platform.clone();
                let chat_id = chat_id.clone();
                let adapters = gateway_adapters.clone();

                tokio::spawn(async move {
                    if let Some(should_flush) = sm.update_stream(&sid, &chunk).await {
                        if should_flush {
                            if let Some(content) = sm.get_stream_content(&sid).await {
                                if let Some(adapter) = adapters.get(&platform) {
                                    if let Some(message_id) =
                                        sm.get_message_id(&sid).await
                                    {
                                        let _ = adapter
                                            .edit_message(
                                                &chat_id,
                                                &message_id,
                                                &content,
                                            )
                                            .await;
                                    } else {
                                        let _ = adapter
                                            .send_message(&chat_id, &content, None)
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                });
            })
        };

        // Invoke the streaming handler
        let agent_start = Instant::now();
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            message_count = messages.len(),
            "gateway streaming agent start"
        );
        let response_result = if let Some(handler) = context_handler {
            handler(messages, runtime_context, on_chunk).await
        } else {
            let handler = self.streaming_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No streaming handler configured".into()))?;
            handler(legacy_messages, on_chunk).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                self.emit_hook_event(
                    "agent:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "streaming": true,
                        "success": false,
                        "error": e.to_string()
                    }),
                )
                .await;
                return Err(e);
            }
        };
        debug!(
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            elapsed_ms = agent_start.elapsed().as_millis() as u64,
            response_chars = response.chars().count(),
            "gateway streaming agent finished"
        );

        self.deliver_response_attachments(&incoming.platform, &incoming.chat_id, &response)
            .await;

        if let Some(worker) = native_worker {
            let _ = worker.await;
            // If native stream could not start, fall back to one-shot delivery.
            if !native_started.load(Ordering::Acquire) || native_failed.load(Ordering::Acquire) {
                self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
                    .await?;
            }
        } else if let Some(stream_id) = stream_id {
            let anchor_message_id = self.stream_manager.get_message_id(&stream_id).await;
            let accumulated = self
                .stream_manager
                .finish_stream(&stream_id)
                .await
                .unwrap_or_default();
            let acc = accumulated.trim();
            let trimmed_response = response.trim();
            let final_text = if !acc.is_empty() && acc != "..." && acc.len() >= trimmed_response.len() {
                accumulated
            } else {
                response.clone()
            };
            if !final_text.trim().is_empty() {
                if let Some(message_id) = anchor_message_id {
                    if let Some(adapter) = self.get_adapter(&incoming.platform).await {
                        let _ = adapter
                            .edit_message(
                                &incoming.chat_id,
                                &message_id,
                                final_text.trim(),
                            )
                            .await;
                    }
                } else {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        final_text.trim(),
                        None,
                    )
                    .await?;
                }
            }
        }

        // Add assistant response to session
        self.session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;
        self.flush_post_delivery_messages(
            &incoming.platform,
            &incoming.chat_id,
            deferred_messages,
            deferred_release,
        )
        .await;
        self.emit_hook_event(
            "agent:end",
            serde_json::json!({
                "platform": incoming.platform,
                "chat_id": incoming.chat_id,
                "user_id": incoming.user_id,
                "session_id": session_key,
                "streaming": true,
                "success": true,
                "response_chars": response.chars().count()
            }),
        )
        .await;

        Ok(())
    }

    async fn inject_runtime_hints(
        &self,
        session_key: &str,
        messages: Vec<Message>,
    ) -> Vec<Message> {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();

        let mut hints = Vec::new();
        if let Some(model) = state.model {
            hints.push(format!("model={}", model));
        }
        if let Some(provider) = state.provider {
            hints.push(format!("provider={}", provider));
        }
        if let Some(profile) = state.profile {
            hints.push(format!("profile={}", profile));
        }
        if let Some(branch) = state.branch {
            hints.push(format!("branch={}", branch));
        }
        if hints.is_empty() {
            return messages;
        }

        let mut out = Vec::with_capacity(messages.len() + 1);
        out.push(Message::system(format!(
            "[gateway_runtime]\n{}",
            hints.join("\n")
        )));
        out.extend(messages);
        out
    }

    async fn build_runtime_context(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> GatewayRuntimeContext {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let mcp_reload_generation = *self.mcp_reload_generation.read().await;

        let session_id = self
            .session_manager
            .get_session(session_key)
            .await
            .map(|s| s.id)
            .unwrap_or_else(|| session_key.to_string());

        GatewayRuntimeContext {
            session_key: session_key.to_string(),
            session_id,
            platform: incoming.platform.clone(),
            chat_id: incoming.chat_id.clone(),
            user_id: incoming.user_id.clone(),
            model: state.model,
            provider: state.provider,
            profile: state.profile,
            branch: state.branch,
            personality: state.personality,
            home: state.home,
            verbose: state.verbose,
            yolo: state.yolo,
            reasoning: state.reasoning,
            mcp_reload_generation,
            deferred_post_delivery_messages: None,
            deferred_post_delivery_released: None,
        }
    }

    async fn flush_post_delivery_messages(
        &self,
        platform: &str,
        chat_id: &str,
        pending: Arc<StdMutex<Vec<String>>>,
        released: Arc<AtomicBool>,
    ) {
        released.store(true, Ordering::Release);
        let queued = match pending.lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => Vec::new(),
        };
        for message in queued {
            if let Err(e) = self.send_message(platform, chat_id, &message, None).await {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    error = %e,
                    "Failed to flush deferred post-delivery message"
                );
            }
        }
    }

    async fn bump_input_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.user_messages += 1;
        stat.input_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    async fn bump_output_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.assistant_messages += 1;
        stat.output_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    async fn build_usage_text(&self, session_key: &str) -> String {
        let usage = self.usage_stats.read().await;
        let stat = usage.get(session_key).cloned().unwrap_or_default();
        let approx_input_tokens = stat.input_chars / 4;
        let approx_output_tokens = stat.output_chars / 4;
        format!(
            "📊 Usage\n- user messages: {}\n- assistant messages: {}\n- input chars: {} (~{} tokens)\n- output chars: {} (~{} tokens)",
            stat.user_messages,
            stat.assistant_messages,
            stat.input_chars,
            approx_input_tokens,
            stat.output_chars,
            approx_output_tokens
        )
    }

    fn summarize_removed_messages(messages: &[Message]) -> Result<String, String> {
        let mut bullets = Vec::new();
        for msg in messages {
            let Some(raw) = msg.content.as_ref() else {
                continue;
            };
            let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact.is_empty() {
                continue;
            }
            let truncated = if compact.chars().count() > 180 {
                let mut head = compact.chars().take(177).collect::<String>();
                head.push_str("...");
                head
            } else {
                compact
            };
            bullets.push(format!("• {}: {}", role_label(msg.role), truncated));
            if bullets.len() >= 6 {
                break;
            }
        }

        if bullets.is_empty() {
            return Err("no textual content available to summarize".to_string());
        }

        let mut out =
            String::from("[CONTEXT COMPACTION] Earlier conversation was compacted. Key points:\n");
        out.push_str(&bullets.join("\n"));
        Ok(out)
    }

    async fn compress_context(&self, session_key: &str, max_messages: usize) -> CompressionOutcome {
        let current = self.session_manager.get_messages(session_key).await;
        if current.len() <= max_messages {
            return CompressionOutcome::default();
        }

        let mut compressed = Vec::new();
        let mut head_count = 0usize;
        if let Some(first) = current.first() {
            if first.role == MessageRole::System {
                compressed.push(first.clone());
                head_count = 1;
            }
        }
        let keep_tail = max_messages.saturating_sub(compressed.len());
        let mut tail: Vec<Message> = current.iter().rev().take(keep_tail).cloned().collect();
        tail.reverse();
        let tail_start = current.len().saturating_sub(keep_tail);
        let middle = if tail_start > head_count {
            &current[head_count..tail_start]
        } else {
            &[]
        };
        let removed_messages = middle.len();

        let mut summary_warning = None;
        if removed_messages > 0 {
            match Self::summarize_removed_messages(middle) {
                Ok(summary) => compressed.push(Message::assistant(&summary)),
                Err(err) => {
                    compressed.push(Message::assistant(&format!(
                        "[CONTEXT COMPACTION] Summary generation was unavailable. {removed_messages} message(s) were removed to free context space but could not be summarized. Continue from recent messages and current workspace state."
                    )));
                    summary_warning = Some(format!(
                        "⚠️ Context compression summary failed ({err}). {removed_messages} historical message(s) were removed and replaced with a placeholder."
                    ));
                }
            }
        }
        compressed.extend(tail);

        self.session_manager
            .replace_messages(session_key, compressed)
            .await;
        CompressionOutcome {
            removed_messages,
            summary_warning,
        }
    }

    async fn build_status_text(&self, session_key: &str) -> String {
        let state = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let usage = self
            .usage_stats
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let messages = self.session_manager.get_messages(session_key).await;
        let running_tasks = self
            .background_tasks
            .list_tasks()
            .into_iter()
            .filter(|(_, status, _)| *status == TaskStatus::Running)
            .count();

        format!(
            "🧭 Gateway status\n- model: {}\n- provider: {}\n- profile: {}\n- branch: {}\n- personality: {}\n- reasoning: {}\n- verbose: {}\n- yolo: {}\n- home: {}\n- messages in session: {}\n- running background tasks: {}\n- mcp generation: {}\n- input/output chars: {}/{}",
            state.model.unwrap_or_else(|| "default".to_string()),
            state.provider.unwrap_or_else(|| "default".to_string()),
            state.profile.unwrap_or_else(|| "default".to_string()),
            state.branch.unwrap_or_else(|| "main".to_string()),
            state.personality.unwrap_or_else(|| "default".to_string()),
            if state.reasoning { "ON" } else { "OFF" },
            if state.verbose { "ON" } else { "OFF" },
            if state.yolo { "ON" } else { "OFF" },
            state.home.unwrap_or_else(|| "(not set)".to_string()),
            messages.len(),
            running_tasks,
            *self.mcp_reload_generation.read().await,
            usage.input_chars,
            usage.output_chars
        )
    }

    async fn handle_background_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        prompt: &str,
        isolated_context: bool,
    ) -> Result<bool, GatewayError> {
        let trimmed = prompt.trim();
        if trimmed.eq_ignore_ascii_case("list") {
            let tasks = self.background_tasks.list_tasks();
            let summary = if tasks.is_empty() {
                "No background tasks.".to_string()
            } else {
                let mut out = String::from("🧵 Background tasks:\n");
                for (id, status, task_prompt) in tasks {
                    out.push_str(&format!("- {} [{:?}] {}\n", id, status, task_prompt));
                }
                out
            };
            self.send_incoming_reply(incoming, &summary, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("cancel ").map(str::trim) {
            let ok = self.background_tasks.cancel(task_id);
            let msg = if ok {
                format!("Cancelled background task {}", task_id)
            } else {
                format!("Task {} was not running or not found", task_id)
            };
            self.send_incoming_reply(incoming, &msg, None)
                .await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("status ").map(str::trim) {
            let msg = match self.background_tasks.get_status(task_id) {
                Some(TaskStatus::Running) => format!("Task {} is running", task_id),
                Some(TaskStatus::Completed) => {
                    let result = self
                        .background_tasks
                        .get_result(task_id)
                        .unwrap_or_default();
                    format!("Task {} completed.\n{}", task_id, result)
                }
                Some(TaskStatus::Failed(err)) => format!("Task {} failed: {}", task_id, err),
                Some(TaskStatus::Cancelled) => format!("Task {} was cancelled", task_id),
                None => format!("Task {} not found", task_id),
            };
            self.send_incoming_reply(incoming, &msg, None)
                .await?;
            return Ok(true);
        }

        let task_id = if isolated_context {
            Self::python_async_task_id("btw")
        } else {
            Self::python_async_task_id("bg")
        };
        self.background_tasks
            .submit_with_id(task_id.clone(), trimmed.to_string())
            .map_err(GatewayError::Platform)?;

        let preview = Self::gateway_command_preview(trimmed);
        let ack = if isolated_context {
            format!("💬 /btw: \"{}\"\nReply will appear here shortly.", preview)
        } else {
            format!(
                "🔄 Background task started: \"{}\"\nTask ID: {}\nYou can keep chatting — results will appear when done.",
                preview, task_id
            )
        };
        self.send_incoming_reply(incoming, &ack, None)
            .await?;

        let legacy_handler = self.message_handler.read().await.as_ref().cloned();
        let context_handler = self
            .message_handler_with_context
            .read()
            .await
            .as_ref()
            .cloned();
        if context_handler.is_none() && legacy_handler.is_none() {
            return Err(GatewayError::Platform(
                "No message handler configured".into(),
            ));
        }
        let manager = self.background_tasks.clone();
        let task_id_for_task = task_id.clone();
        // Python `GatewayRunner._run_background_task`: only `user_message=prompt` (fresh session).
        // Python `_run_btw_task`: `conversation_history` snapshot + ephemeral user turn (no tools).
        let original_messages = if isolated_context {
            let mut history = self.session_manager.get_messages(session_key).await;
            let btw_user = format!(
                "[Ephemeral /btw side question. Answer using the conversation \
                 context. No tools available. Be direct and concise.]\n\n{}",
                trimmed
            );
            history.push(Message::user(btw_user));
            history
        } else {
            vec![Message::user(trimmed)]
        };
        let legacy_messages = original_messages.clone();
        let runtime_context = self.build_runtime_context(incoming, session_key).await;
        tokio::spawn(async move {
            let result = if let Some(handler) = context_handler {
                handler(original_messages, runtime_context).await
            } else if let Some(handler) = legacy_handler {
                handler(legacy_messages).await
            } else {
                Err(GatewayError::Platform(
                    "No message handler configured".into(),
                ))
            };

            match result {
                Ok(result) => manager.complete(&task_id_for_task, result),
                Err(err) => manager.fail(&task_id_for_task, err.to_string()),
            }
        });

        Ok(true)
    }

    /// `preview = prompt[:60] + ("..." if len(prompt) > 60 else "")` (Python gateway).
    fn gateway_command_preview(prompt: &str) -> String {
        let t = prompt.trim();
        let mut it = t.chars();
        let head: String = it.by_ref().take(60).collect();
        if it.next().is_some() {
            format!("{}...", head)
        } else {
            head
        }
    }

    /// Python: `f"{kind}_{%H%M%S}_{os.urandom(3).hex()}"` style task ids (`bg_…`, `btw_…`).
    fn python_async_task_id(kind: &str) -> String {
        let ts = chrono::Utc::now().format("%H%M%S");
        let salt = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.subsec_nanos() as u64) ^ d.as_secs().wrapping_mul(0x9e37_79b9_85f0_a7b5))
            .unwrap_or(0xABCDEF);
        format!("{}_{}_{:06x}", kind, ts, salt & 0xFFFFFF)
    }

    fn extract_command_name(text: &str) -> Option<String> {
        let trimmed = text.trim_start();
        if !trimmed.starts_with('/') {
            return None;
        }
        let token = trimmed[1..].split_whitespace().next()?.trim();
        if token.is_empty() {
            return None;
        }
        Some(token.to_ascii_lowercase())
    }

    // -----------------------------------------------------------------------
    // Message sending (delegates to adapters)
    // -----------------------------------------------------------------------

    /// Deliver `MEDIA:<path>` attachments and bare local paths from an agent response.
    pub async fn deliver_response_attachments(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
    ) {
        let (media_files, _cleaned) = extract_media(text);
        for (media_path, _is_voice) in media_files {
            match resolve_outbound_media_path(&media_path) {
                Ok(resolved) => {
                    let path_str = resolved.to_string_lossy().into_owned();
                    if let Err(err) = self.send_file(platform, chat_id, &path_str, None).await {
                        warn!(
                            platform = platform,
                            chat_id = chat_id,
                            path = %path_str,
                            error = %err,
                            "failed to deliver MEDIA attachment from agent response"
                        );
                    }
                }
                Err(err) => {
                    warn!(
                        platform = platform,
                        chat_id = chat_id,
                        path = %media_path,
                        error = %err,
                        "skipping MEDIA attachment (file missing or path invalid)"
                    );
                }
            }
        }
    }

    /// Send a reply for an inbound message (slash interaction follow-up when applicable).
    pub async fn send_incoming_reply(
        &self,
        incoming: &IncomingMessage,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        if let (Some(interaction_id), Some(interaction_token)) = (
            incoming.interaction_id.as_deref(),
            incoming.interaction_token.as_deref(),
        ) {
            if let Some(adapter) = self.get_adapter(&incoming.platform).await {
                return adapter
                    .respond_interaction(interaction_id, interaction_token, text)
                    .await;
            }
        }
        self.send_message(&incoming.platform, &incoming.chat_id, text, parse_mode)
            .await
    }

    /// Send a text message to a specific platform chat.
    pub async fn send_message(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        debug!(
            platform = %platform,
            chat_id = %chat_id,
            text_chars = text.chars().count(),
            has_parse_mode = parse_mode.is_some(),
            "gateway send_message dispatch"
        );
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        let (cleaned, images) = extract_inline_images(text);
        if images.is_empty() {
            return adapter.send_message(chat_id, text, parse_mode).await;
        }

        if !cleaned.is_empty() {
            adapter
                .send_message(chat_id, &cleaned, parse_mode.clone())
                .await?;
        }

        for image in images {
            if let Err(err) = adapter
                .send_image_url(chat_id, &image.url, image.alt_text.as_deref())
                .await
            {
                warn!(
                    platform = platform,
                    chat_id = chat_id,
                    image_url = %image.url,
                    error = %err,
                    "native image send failed; falling back to plain URL message"
                );

                let fallback = match image.alt_text.as_deref().map(str::trim) {
                    Some(caption) if !caption.is_empty() => format!("{caption}\n{}", image.url),
                    _ => image.url.clone(),
                };
                adapter
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await?;
            }
        }

        Ok(())
    }

    /// Edit an existing message on a specific platform chat.
    pub async fn edit_message(
        &self,
        platform: &str,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.edit_message(chat_id, message_id, text).await
    }

    /// Send a file to a specific platform chat with an optional caption.
    pub async fn send_file(
        &self,
        platform: &str,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter.send_file(chat_id, file_path, caption).await
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the stream manager.
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.stream_manager
    }

    /// Get a reference to the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// List the names of all registered adapters.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.adapters.read().await.keys().cloned().collect()
    }

    /// Periodically expires inactive sessions.
    pub async fn session_expiry_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(30)));
        loop {
            ticker.tick().await;
            let expired = self.session_manager.expire_idle_sessions().await;
            if expired > 0 {
                tracing::info!(expired, "Expired idle sessions");
            }
        }
    }

    /// Monitors adapter health and attempts reconnect through stop/start.
    pub async fn platform_reconnect_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(20)));
        loop {
            ticker.tick().await;
            let snapshot = self.adapters.read().await.clone();
            for (name, adapter) in snapshot {
                if !adapter.is_running() {
                    tracing::warn!(platform = %name, "Adapter appears offline, reconnecting");
                    let _ = adapter.start().await;
                }
            }
        }
    }

    /// Prepare the user turn via the injected agent preparer (vision routing, etc.).
    async fn prepare_inbound_user_message(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Message {
        let event = Self::incoming_to_event(incoming);
        let (provider, model) = {
            let states = self.runtime_state.read().await;
            states
                .get(session_key)
                .map(|s| (s.provider.clone(), s.model.clone()))
                .unwrap_or((None, None))
        };
        let ctx = InboundPrepareContext {
            session_key: session_key.to_string(),
            provider,
            model,
            image_input_mode: "auto".to_string(),
            aux_vision_provider: None,
            aux_vision_model: None,
            aux_vision_base_url: None,
        };
        let preparer = self.inbound_preparer.read().await.clone();
        let mut message = if let Some(preparer) = preparer {
            match preparer.prepare(&event, &ctx).await {
                Ok(message) => message,
                Err(err) => {
                    warn!(
                        platform = %incoming.platform,
                        session_key = %session_key,
                        error = %err,
                        "Inbound preparer failed; using transport fallback"
                    );
                    transport_fallback_message(&event)
                }
            }
        } else {
            transport_fallback_message(&event)
        };

        if let Some(enriched) = self.enrich_inbound_audio(&event, &message).await {
            message = enriched;
        }
        message
    }

    /// Transcribe inbound audio attachments (Python `_enrich_message_with_transcription`).
    async fn enrich_inbound_audio(&self, event: &InboundEvent, base: &Message) -> Option<Message> {
        if !*self.stt_enabled.read().await {
            return None;
        }
        let voice = self.voice_manager.read().await.clone()?;
        let mut transcripts = Vec::new();
        for (idx, url) in event.media_urls.iter().enumerate() {
            let media_type = event
                .media_types
                .get(idx)
                .map(String::as_str)
                .unwrap_or("")
                .to_ascii_lowercase();
            if !media_type.starts_with("audio/") && media_type != "voice" && media_type != "audio" {
                continue;
            }
            let path = url.trim();
            if path.is_empty() {
                continue;
            }
            match voice.transcribe_path(path).await {
                Ok(text) if !text.trim().is_empty() => {
                    transcripts.push(format!(
                        "[The user sent a voice message~ Here's what they said: \"{}\"]",
                        text.trim()
                    ));
                }
                Ok(_) => {}
                Err(err) => {
                    warn!(
                        path = path,
                        error = %err,
                        "Inbound audio transcription failed"
                    );
                    transcripts.push(
                        "[The user sent a voice message but transcription failed.]".into(),
                    );
                }
            }
        }
        if transcripts.is_empty() {
            return None;
        }
        let prefix = transcripts.join("\n");
        let body = base
            .content
            .as_deref()
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .map(|c| format!("{prefix}\n{c}"))
            .unwrap_or(prefix);
        Some(Message::user(body))
    }

    /// Build deterministic signature for config-change detection.
    pub fn agent_config_signature(&self) -> String {
        let s = serde_json::to_string(&self.config).unwrap_or_default();
        format!("{:x}", md5::compute(s))
    }

    /// Load optional prefill messages.
    pub fn load_prefill_messages(&self, path: &std::path::Path) -> Vec<Message> {
        let Ok(content) = std::fs::read_to_string(path) else {
            return vec![];
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(Message::user)
            .collect()
    }

    /// Load optional ephemeral system prompt.
    pub fn load_ephemeral_system_prompt(&self, path: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Authorize user based on DM manager and platform context.
    pub async fn is_user_authorized(&self, user_id: &str, platform: &str) -> bool {
        let dm = self.dm_manager.read().await;
        dm.is_authorized(user_id) || dm.handle_dm(user_id, platform).await == DmDecision::Allow
    }

    /// Send update notification message to a chat.
    pub async fn send_update_notification(
        &self,
        platform: &str,
        chat_id: &str,
        latest_version: &str,
    ) -> Result<(), GatewayError> {
        let msg = format!("Update available: Hermes {}", latest_version);
        self.send_message(platform, chat_id, &msg, None).await
    }

    /// Watch external process output and forward to a callback.
    pub async fn run_process_watcher(
        &self,
        mut child: tokio::process::Child,
        on_output: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<(), GatewayError> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| GatewayError::Platform("Process has no stdout".into()))?;
        let mut lines = BufReader::new(stdout).lines();
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|e| GatewayError::Platform(format!("Watcher read error: {}", e)))?
        {
            on_output(line);
        }
        Ok(())
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::{HookEvent, HookHandler, HookRegistry};
    use crate::session::SessionManager;
    use async_trait::async_trait;
    use hermes_config::session::SessionConfig;
    use std::sync::Mutex;

    struct TestAdapter {
        messages: Arc<Mutex<Vec<(String, String)>>>,
    }

    struct NativeStreamTestAdapter {
        messages: Arc<Mutex<Vec<(String, String)>>>,
        chunks: Arc<Mutex<Vec<(String, bool)>>>,
    }

    struct ReactionTestAdapter {
        messages: Arc<Mutex<Vec<(String, String)>>>,
        reactions: Arc<Mutex<Vec<String>>>,
    }

    struct RecordingHook {
        seen: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }

    #[async_trait]
    impl HookHandler for RecordingHook {
        async fn handle(&self, event: &HookEvent) -> Result<(), String> {
            self.seen
                .lock()
                .unwrap()
                .push((event.event_type.clone(), event.context.clone()));
            Ok(())
        }

        fn name(&self) -> &str {
            "recording-hook"
        }
    }

    #[async_trait]
    impl PlatformAdapter for TestAdapter {
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
            self.messages
                .lock()
                .unwrap()
                .push((chat_id.to_string(), text.to_string()));
            Ok(())
        }

        async fn edit_message(
            &self,
            chat_id: &str,
            _message_id: &str,
            text: &str,
        ) -> Result<(), GatewayError> {
            let mut msgs = self.messages.lock().unwrap();
            if let Some(pos) = msgs
                .iter()
                .rposition(|(c, t)| c == chat_id && t == "...")
            {
                msgs[pos].1 = text.to_string();
            }
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

        async fn send_image_url(
            &self,
            chat_id: &str,
            image_url: &str,
            caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            let mut marker = format!("[image] {image_url}");
            if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
                marker.push_str(&format!(" | caption={cap}"));
            }
            self.messages
                .lock()
                .unwrap()
                .push((chat_id.to_string(), marker));
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "test"
        }
    }

    #[async_trait]
    impl PlatformAdapter for ReactionTestAdapter {
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
            self.messages
                .lock()
                .unwrap()
                .push((chat_id.to_string(), text.to_string()));
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

        async fn add_reaction(
            &self,
            chat_id: &str,
            message_id: &str,
            emoji: &str,
        ) -> Result<(), GatewayError> {
            self.reactions
                .lock()
                .unwrap()
                .push(format!("add:{chat_id}:{message_id}:{emoji}"));
            Ok(())
        }

        async fn remove_reaction(
            &self,
            chat_id: &str,
            message_id: &str,
            emoji: &str,
        ) -> Result<(), GatewayError> {
            self.reactions
                .lock()
                .unwrap()
                .push(format!("remove:{chat_id}:{message_id}:{emoji}"));
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "slack"
        }
    }

    #[async_trait]
    impl PlatformAdapter for NativeStreamTestAdapter {
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
            self.messages
                .lock()
                .unwrap()
                .push((chat_id.to_string(), text.to_string()));
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

        fn supports_native_streaming(&self) -> bool {
            true
        }

        async fn start_native_stream(
            &self,
            _chat_id: &str,
            _reply_to: Option<&str>,
            initial_content: Option<&str>,
        ) -> Result<Option<String>, GatewayError> {
            self.chunks.lock().unwrap().push((
                initial_content.unwrap_or_default().to_string(),
                false,
            ));
            Ok(Some("sid-1".to_string()))
        }

        async fn send_native_stream_chunk(
            &self,
            _chat_id: &str,
            _stream_id: &str,
            content: &str,
            finish: bool,
        ) -> Result<(), GatewayError> {
            self.chunks
                .lock()
                .unwrap()
                .push((content.to_string(), finish));
            Ok(())
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            "wecom"
        }
    }

    #[test]
    fn gateway_config_default() {
        let cfg = GatewayConfig::default();
        assert!(cfg.ssrf_protection);
        assert!(cfg.media_cache_dir.is_none());
        assert_eq!(cfg.media_cache_max_bytes, 0);
        assert!(!cfg.streaming_enabled);
    }

    #[tokio::test]
    async fn gateway_register_and_list_adapters() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());

        assert!(gw.adapter_names().await.is_empty());
    }

    #[tokio::test]
    async fn gateway_send_message_extracts_inline_images() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        gw.send_message(
            "test",
            "chat1",
            "Here ![diagram](https://cdn.example.com/x.png) and <img src=\"https://fal.media/abc\"> done",
            Some(ParseMode::Markdown),
        )
        .await
        .expect("send should succeed");

        let sent = sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0].0, "chat1");
        assert_eq!(sent[0].1, "Here and done");
        assert_eq!(
            sent[1].1,
            "[image] https://cdn.example.com/x.png | caption=diagram"
        );
        assert_eq!(sent[2].1, "[image] https://fal.media/abc");
    }

    #[tokio::test]
    async fn gateway_route_dm_denied() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "unknown_user".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        // Should succeed (deny silently)
        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn gateway_route_dm_open_skips_pairing_message() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_pair_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("wecom", adapter).await;

        let mut policies = HashMap::new();
        policies.insert(
            "wecom".to_string(),
            PlatformAccessPolicy {
                dm_mode: DmAccessMode::Open,
                ..PlatformAccessPolicy::default()
            },
        );
        gw.set_platform_access_policies(policies).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Err(GatewayError::Platform("handler reached".to_string())) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "wecom".into(),
            chat_id: "user-1".into(),
            user_id: "user-1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        let result = gw.route_message(&incoming).await;
        assert!(result.is_err());
        assert!(
            sent.lock().unwrap().is_empty(),
            "dm_policy open must not send pairing approval text"
        );
    }

    #[tokio::test]
    async fn gateway_route_no_handler() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        // Should fail because no message handler is set
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn gateway_route_group_message_skips_dm_check() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "-group1".into(),
            user_id: "unknown_user".into(),
            text: "hello group".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: false, // Group message, no DM check
            interaction_id: None,
            interaction_token: None,
        };

        // Should fail because no handler, but DM check is skipped
        let result = gw.route_message(&incoming).await;
        assert!(result.is_err()); // No handler configured
    }

    #[tokio::test]
    async fn gateway_group_allowlist_denies_unauthorized_user() {
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        let mut policies = HashMap::new();
        let mut policy = PlatformAccessPolicy {
            group_mode: GroupAccessMode::Allowlist,
            ..PlatformAccessPolicy::default()
        };
        policy.allowed_users.insert("allowed_user".to_string());
        policies.insert("telegram".to_string(), policy);
        gw.set_platform_access_policies(policies).await;

        let incoming = IncomingMessage {
            platform: "telegram".into(),
            chat_id: "-100123".into(),
            user_id: "other_user".into(),
            text: "hello group".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
        };

        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());
        assert_eq!(
            gw.session_transcript_len("telegram", "-100123", "other_user")
                .await,
            0
        );
    }

    #[tokio::test]
    async fn gateway_discord_slash_requires_allowlist() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_ignore_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("discord", adapter).await;

        let mut policies = HashMap::new();
        let mut policy = PlatformAccessPolicy {
            group_mode: GroupAccessMode::Open,
            slash_requires_allowlist: true,
            ..PlatformAccessPolicy::default()
        };
        policy.allowed_users.insert("allowed_user".to_string());
        policies.insert("discord".to_string(), policy);
        gw.set_platform_access_policies(policies).await;

        let denied = IncomingMessage {
            platform: "discord".into(),
            chat_id: "guild:1".into(),
            user_id: "random_user".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("m1".into()),
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&denied).await.is_ok());
        assert_eq!(
            gw.session_transcript_len("discord", "guild:1", "random_user")
                .await,
            0
        );

        let allowed = IncomingMessage {
            platform: "discord".into(),
            chat_id: "guild:1".into(),
            user_id: "allowed_user".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("m2".into()),
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&allowed).await.is_ok());
        let sent_msgs = sent.lock().unwrap();
        assert_eq!(sent_msgs.len(), 1);
        assert_eq!(sent_msgs[0].0, "guild:1");
        assert!(!sent_msgs[0].1.trim().is_empty());
    }

    #[tokio::test]
    async fn gateway_executes_status_command_without_agent_handler() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        let result = gw.route_message(&incoming).await;
        assert!(result.is_ok());

        let msgs = sent.lock().unwrap();
        assert!(msgs.iter().any(|(_, text)| text.contains("Gateway status")));
    }

    #[tokio::test]
    async fn gateway_compress_command_appends_warning_when_summary_unavailable() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let session_key = gw
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let _ = gw
            .session_manager
            .get_or_create_session("test", "chat1", "user1")
            .await;
        gw.session_manager
            .add_message(&session_key, Message::system("sys"))
            .await;
        for _ in 0..40 {
            gw.session_manager
                .add_message(
                    &session_key,
                    Message {
                        role: MessageRole::Tool,
                        content: None,
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                        reasoning_content: None,
                        cache_control: None,
                    },
                )
                .await;
        }

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/compress".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
        assert!(reply.contains("Context compressed"));
        assert!(reply.contains("⚠️ Context compression summary failed"));
        assert!(reply.contains("historical message(s) were removed"));
    }

    #[tokio::test]
    async fn gateway_compress_command_emits_summary_without_warning() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let session_key = gw
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let _ = gw
            .session_manager
            .get_or_create_session("test", "chat1", "user1")
            .await;
        gw.session_manager
            .add_message(&session_key, Message::system("sys"))
            .await;
        for i in 0..40 {
            let message = if i % 2 == 0 {
                Message::user(format!("turn {i} content"))
            } else {
                Message::assistant(format!("turn {i} content"))
            };
            gw.session_manager.add_message(&session_key, message).await;
        }

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/compress".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };

        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
        assert!(reply.contains("Context compressed"));
        assert!(!reply.contains("⚠️"));
        drop(msgs);

        let updated = gw.session_manager.get_messages(&session_key).await;
        assert!(
            updated.iter().any(|m| {
                m.content
                    .as_deref()
                    .unwrap_or("")
                    .contains("[CONTEXT COMPACTION] Earlier conversation was compacted")
            }),
            "summary marker should be persisted into compressed transcript"
        );
    }

    #[tokio::test]
    async fn gateway_background_task_lifecycle_commands_work() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let prompt = messages
                    .last()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "none".to_string());
                Ok(format!("done: {}", prompt))
            })
        }))
        .await;

        let start = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/background ping".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&start).await.is_ok());

        let task_id = {
            let msgs = sent.lock().unwrap();
            let queued = msgs
                .iter()
                .find(|(_, text)| text.contains("Background task started"))
                .expect("queue ack should exist");
            queued
                .1
                .lines()
                .find_map(|line| line.strip_prefix("Task ID: ").map(str::trim))
                .expect("task id line")
                .to_string()
        };

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        let status = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: format!("/background status {}", task_id),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&status).await.is_ok());

        let msgs = sent.lock().unwrap();
        assert!(msgs.iter().any(|(_, text)| text.contains("completed")));
    }

    #[tokio::test]
    async fn gateway_admin_approve_and_deny_affects_dm_authorization() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_ignore_behavior();
        dm_manager.add_admin("admin1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let approve = IncomingMessage {
            platform: "test".into(),
            chat_id: "admin-chat".into(),
            user_id: "admin1".into(),
            text: "/approve user2".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&approve).await.is_ok());

        // user2 should now pass DM authorization, then fail because no handler is configured.
        let authorized_dm = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat-u2".into(),
            user_id: "user2".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&authorized_dm).await.is_err());

        let deny = IncomingMessage {
            platform: "test".into(),
            chat_id: "admin-chat".into(),
            user_id: "admin1".into(),
            text: "/deny user2".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&deny).await.is_ok());

        // user2 should be denied again, and route should return Ok (silently denied).
        let denied_dm = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat-u2".into(),
            user_id: "user2".into(),
            text: "hello again".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&denied_dm).await.is_ok());
    }

    #[tokio::test]
    async fn gateway_reload_mcp_and_status_reflect_runtime_state() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let provider = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/provider openrouter".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&provider).await.is_ok());

        let profile = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/profile prod".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&profile).await.is_ok());

        let reload = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/reload_mcp".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&reload).await.is_ok());

        let status = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&status).await.is_ok());

        let msgs = sent.lock().unwrap();
        let status_text = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.contains("Gateway status") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("status response should exist");
        assert!(status_text.contains("provider: openrouter"));
        assert!(status_text.contains("profile: prod"));
        assert!(status_text.contains("mcp generation: 1"));
    }

    #[tokio::test]
    async fn gateway_runtime_state_is_injected_into_agent_messages() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|messages| {
            Box::pin(async move {
                let hint = messages
                    .iter()
                    .find(|m| {
                        m.role == MessageRole::System
                            && m.content
                                .as_deref()
                                .unwrap_or("")
                                .contains("[gateway_runtime]")
                    })
                    .and_then(|m| m.content.clone())
                    .unwrap_or_else(|| "no-runtime-hints".to_string());
                Ok(hint)
            })
        }))
        .await;

        let set_provider = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/provider openai".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&set_provider).await.is_ok());

        let set_model = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/model gpt-4o".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&set_model).await.is_ok());

        let set_profile = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/profile prod".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&set_profile).await.is_ok());

        let set_branch = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/branch feature/parity".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&set_branch).await.is_ok());

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let msgs = sent.lock().unwrap();
        let echoed = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.contains("[gateway_runtime]") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("runtime hint response should exist");

        assert!(echoed.contains("model=gpt-4o"));
        assert!(echoed.contains("provider=openai"));
        assert!(echoed.contains("profile=prod"));
        assert!(echoed.contains("branch=feature/parity"));
    }

    #[tokio::test]
    async fn gateway_new_clears_yolo_only_for_target_session() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let session_key_1 = gw
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let session_key_2 = gw
            .session_manager
            .compose_session_key("test", "chat2", "user1");

        let yolo_chat1 = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/yolo".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&yolo_chat1).await.is_ok());

        let yolo_chat2 = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat2".into(),
            user_id: "user1".into(),
            text: "/yolo".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&yolo_chat2).await.is_ok());

        {
            let states = gw.runtime_state.read().await;
            assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(true));
            assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
        }

        let reset_chat1 = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/new".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&reset_chat1).await.is_ok());

        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(false));
        assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
    }

    #[tokio::test]
    async fn gateway_switch_session_clears_yolo_for_current_chat_context() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;

        let current_key = gw
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let target_key = gw
            .session_manager
            .compose_session_key("test", "chat2", "user1");

        let _ = gw
            .session_manager
            .get_or_create_session("test", "chat2", "user1")
            .await;
        gw.session_manager
            .add_message(&target_key, Message::user("history from another session"))
            .await;

        let yolo_chat1 = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/yolo".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&yolo_chat1).await.is_ok());
        {
            let states = gw.runtime_state.read().await;
            assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(true));
        }

        let switch = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: format!("/sessions {}", target_key),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&switch).await.is_ok());

        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(false));
    }

    #[tokio::test]
    async fn gateway_slack_reaction_lifecycle_success() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("slack", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "slack".into(),
            chat_id: "C123".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("1710000000.123".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let got = reactions.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                "add:C123:1710000000.123:eyes".to_string(),
                "remove:C123:1710000000.123:eyes".to_string(),
                "add:C123:1710000000.123:white_check_mark".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_slack_reaction_lifecycle_failure_sets_error_reaction() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("slack", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Err(GatewayError::Platform("boom".to_string())) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "slack".into(),
            chat_id: "C123".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("1710000000.456".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_err());

        let got = reactions.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                "add:C123:1710000000.456:eyes".to_string(),
                "remove:C123:1710000000.456:eyes".to_string(),
                "add:C123:1710000000.456:x".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_slack_reactions_skip_non_dm_non_mentions() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let dm_manager = DmManager::with_pair_behavior();
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("slack", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "slack".into(),
            chat_id: "C123".into(),
            user_id: "user1".into(),
            text: "general channel chatter".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("1710000000.789".into()),
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());
        assert!(reactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gateway_context_handler_receives_structured_runtime_context() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler_with_context(Arc::new(|messages, ctx| {
            Box::pin(async move {
                let payload = format!(
                    "ctx model={:?} provider={:?} profile={:?} branch={:?} platform={} user={} session={} has_legacy_hint={}",
                    ctx.model,
                    ctx.provider,
                    ctx.profile,
                    ctx.branch,
                    ctx.platform,
                    ctx.user_id,
                    ctx.session_key,
                    messages.iter().any(|m| m
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .contains("[gateway_runtime]"))
                );
                Ok(payload)
            })
        }))
        .await;

        let setup_cmds = vec![
            "/provider openai",
            "/model gpt-4o-mini",
            "/profile prod",
            "/branch feat-123",
        ];
        for cmd in setup_cmds {
            let incoming = IncomingMessage {
                platform: "test".into(),
                chat_id: "chat1".into(),
                user_id: "user1".into(),
                text: cmd.to_string(),
                media_urls: vec![],
                media_types: vec![],
                message_id: None,
                is_dm: true,
                interaction_id: None,
                interaction_token: None,
            };
            assert!(gw.route_message(&incoming).await.is_ok());
        }

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "run".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let msgs = sent.lock().unwrap();
        let echoed = msgs
            .iter()
            .rev()
            .find_map(|(_, text)| {
                if text.starts_with("ctx model=") {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .expect("context response should exist");
        assert!(echoed.contains("Some(\"gpt-4o-mini\")"));
        assert!(echoed.contains("Some(\"openai\")"));
        assert!(echoed.contains("Some(\"prod\")"));
        assert!(echoed.contains("Some(\"feat-123\")"));
        assert!(echoed.contains("platform=test"));
        assert!(echoed.contains("user=user1"));
        assert!(echoed.contains("has_legacy_hint=false"));
    }

    #[tokio::test]
    async fn gateway_deferred_post_delivery_messages_flush_after_main_reply() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                let released = ctx
                    .deferred_post_delivery_released
                    .expect("release flag should be present");
                assert!(
                    !released.load(std::sync::atomic::Ordering::Acquire),
                    "release must remain false before main reply delivery"
                );
                pending
                    .lock()
                    .unwrap()
                    .push("💾 deferred-memory-update".to_string());
                Ok("main-response".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec![
                "main-response".to_string(),
                "💾 deferred-memory-update".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_status_then_main_then_deferred_order_matches_python_chain() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Arc::new(Gateway::new(
            session_mgr,
            dm_manager,
            GatewayConfig::default(),
        ));
        gw.register_adapter("test", adapter).await;

        let gw_for_handler = gw.clone();
        gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
            let gw = gw_for_handler.clone();
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                pending.lock().unwrap().push("💾 bg-review".to_string());

                // Mirrors Python's status_callback: status is forwarded immediately.
                gw.send_message(&ctx.platform, &ctx.chat_id, "⚠️ context pressure", None)
                    .await
                    .expect("status callback send should succeed");

                Ok("main-response".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec![
                "⚠️ context pressure".to_string(),
                "main-response".to_string(),
                "💾 bg-review".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_streaming_flushes_deferred_after_stream_finishes() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let mut cfg = GatewayConfig::default();
        cfg.streaming_enabled = true;
        let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
        gw.register_adapter("test", adapter).await;

        gw.set_streaming_handler_with_context(Arc::new(|_messages, ctx, _on_chunk| {
            Box::pin(async move {
                let pending = ctx
                    .deferred_post_delivery_messages
                    .expect("deferred queue should be present");
                let released = ctx
                    .deferred_post_delivery_released
                    .expect("release flag should be present");
                assert!(
                    !released.load(std::sync::atomic::Ordering::Acquire),
                    "release must stay false while stream handler is running"
                );
                pending
                    .lock()
                    .unwrap()
                    .push("💾 stream-bg-review".to_string());
                Ok("stream-final".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(
            ordered,
            vec![
                "stream-final".to_string(),
                "💾 stream-bg-review".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_native_streaming_sends_full_refresh_chunks() {
        unsafe { std::env::set_var("HERMES_WECOM_STREAM_FLUSH_INTERVAL_MS", "1") };

        let sent = Arc::new(Mutex::new(Vec::new()));
        let chunks = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(NativeStreamTestAdapter {
            messages: sent.clone(),
            chunks: chunks.clone(),
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let mut cfg = GatewayConfig::default();
        cfg.streaming_enabled = true;
        let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
        gw.register_adapter("wecom", adapter).await;

        gw.set_streaming_handler(Arc::new(|_messages, on_chunk| {
            Box::pin(async move {
                on_chunk("你".to_string());
                on_chunk("好".to_string());
                Ok("你好".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "wecom".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let sent = sent.lock().unwrap();
        assert!(
            sent.is_empty(),
            "native stream path should not fall back to one-shot send_message"
        );

        let chunks = chunks.lock().unwrap().clone();
        unsafe { std::env::remove_var("HERMES_WECOM_STREAM_FLUSH_INTERVAL_MS") };

        assert!(!chunks.is_empty());
        assert_eq!(chunks.first().map(|c| c.0.as_str()), Some("思考中..."));
        assert_eq!(chunks.last().map(|c| c.0.as_str()), Some("你好"));
        assert_eq!(chunks.last().map(|c| c.1), Some(true));
        let bodies: Vec<&str> = chunks.iter().map(|c| c.0.as_str()).collect();
        assert!(bodies.contains(&"你好"));
    }

    #[tokio::test]
    async fn gateway_emits_agent_start_and_end_hooks() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "agent:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async move { Ok("main-response".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            vec!["agent:start".to_string(), "agent:end".to_string()]
        );
        let end_payload = events
            .iter()
            .find(|(name, _)| name == "agent:end")
            .map(|(_, ctx)| ctx.clone())
            .expect("agent:end payload should exist");
        assert_eq!(end_payload["success"], serde_json::json!(true));
    }

    #[tokio::test]
    async fn gateway_hook_event_order_captures_start_status_step_end() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "agent:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Arc::new(Gateway::new(
            session_mgr,
            dm_manager,
            GatewayConfig::default(),
        ));
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;

        let gw_for_handler = gw.clone();
        gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
            let gw = gw_for_handler.clone();
            Box::pin(async move {
                gw.emit_hook_event(
                    "agent:status",
                    serde_json::json!({
                        "platform": ctx.platform,
                        "user_id": ctx.user_id,
                        "session_id": ctx.session_key,
                        "event_type": "lifecycle",
                        "message": "Context pressure 85%"
                    }),
                )
                .await;
                gw.emit_hook_event(
                    "agent:step",
                    serde_json::json!({
                        "platform": ctx.platform,
                        "user_id": ctx.user_id,
                        "session_id": ctx.session_key,
                        "iteration": 1,
                        "tool_names": ["memory"],
                        "tools": [{"name":"memory","result":"ok"}]
                    }),
                )
                .await;
                Ok("done".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "agent:start".to_string(),
                "agent:status".to_string(),
                "agent:step".to_string(),
                "agent:end".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_emits_session_start_and_command_hook_events() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "session:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );
        hooks.register_in_process(
            "command:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;

        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"session:start".to_string()));
        assert!(names.contains(&"command:status".to_string()));
    }

    #[tokio::test]
    async fn gateway_emits_session_end_and_reset_for_reset_command() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(TestAdapter {
            messages: sent.clone(),
        });
        let hook_seen = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = HookRegistry::new();
        hooks.register_in_process(
            "session:*",
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.set_hook_registry(Arc::new(hooks)).await;
        gw.register_adapter("test", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async move { Ok("assistant".to_string()) })
        }))
        .await;

        let normal = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&normal).await.is_ok());

        let reset = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "/reset".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
        };
        assert!(gw.route_message(&reset).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"session:end".to_string()));
        assert!(names.contains(&"session:reset".to_string()));
    }
}
