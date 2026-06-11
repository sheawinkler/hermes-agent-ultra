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

use chrono::Utc;

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex as TokioMutex, mpsc};
use tokio::time::MissedTickBehavior;
use tracing::{debug, error, info, warn};

use crate::delivery_layer::{DeliveryLayer, TurnOutboundTracker};
use crate::extension_bus::ExtensionBus;
use crate::message_router::MessageRouter;
use crate::session_layer::SessionLayer;

// Re-export so external code using `hermes_gateway::gateway::*` keeps working.
pub use crate::message_router::{
    DmAccessMode, GatewayRuntimeContext, GroupAccessMode, MessageHandler,
    MessageHandlerWithContext, PlatformAccessPolicy, StreamingMessageHandler,
    StreamingMessageHandlerWithContext,
};
pub use crate::session_layer::{SessionTeardownContext, SessionTeardownHandler};

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

fn non_streaming_feedback_delay_ms() -> u64 {
    std::env::var("HERMES_GATEWAY_PROCESSING_FEEDBACK_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(1200)
}

fn platform_wants_processing_ack(platform: &str) -> bool {
    matches!(
        platform.trim().to_ascii_lowercase().as_str(),
        "wecom" | "weixin" | "feishu"
    )
}

fn streaming_feedback_delay_ms() -> u64 {
    std::env::var("HERMES_GATEWAY_STREAMING_FEEDBACK_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&ms| ms > 0)
        .unwrap_or(10_000)
}

/// Weixin iLink typing refresh interval when the `weixin` feature is disabled at compile time.
#[cfg(not(feature = "weixin"))]
const WEIXIN_TYPING_REFRESH_SECS_FALLBACK: u64 = 5;

/// WhatsApp composing indicator refresh (wa-rs `chatstate` / "typing…").
const WHATSAPP_TYPING_REFRESH_SECS: u64 = 5;

/// Cancels a platform typing keepalive started by [`Gateway::spawn_route_typing`].
pub(crate) struct RouteTypingGuard {
    cancel: Option<Arc<AtomicBool>>,
    join: Option<tokio::task::JoinHandle<()>>,
}

impl RouteTypingGuard {
    pub(crate) fn none() -> Self {
        Self {
            cancel: None,
            join: None,
        }
    }

    pub(crate) async fn finish(self) {
        if let Some(cancel) = self.cancel {
            cancel.store(true, Ordering::Release);
        }
        if let Some(join) = self.join {
            let _ = tokio::time::timeout(Duration::from_secs(3), join).await;
        }
    }
}

use hermes_config::{DisplayConfig, QuickCommandConfig, normalize_service_tier};
use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};
use hermes_core::types::{Message, MessageRole};
use hermes_core::{
    InboundEvent, InboundMessagePreparer, InboundPrepareContext, transport_fallback_message,
};

use crate::background::TaskStatus;
use crate::commands::{
    BatchCommandClass, BatchedCommand, GatewayCommandResult, handle_command, parse_batch_commands,
};
use crate::dm::{DmDecision, DmManager};
use crate::hooks::{HookEvent, HookRegistry};
use crate::platforms::helpers::extract_inline_images;
use crate::session::{SessionManager, SessionTeardownSnapshot};
use crate::stream::{StreamConfig, StreamManager};
use crate::tool_backends::ClarifyDispatcher;
use crate::voice::VoiceManager;
use hermes_config::resolve_outbound_media_path;
use hermes_tools::extract_media;
use hermes_tools::tools::messaging::MessagingSessionContext;

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

    /// Default provider service tier for gateway agent turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// Display command/runtime settings.
    #[serde(default)]
    pub display: DisplayConfig,

    /// User-defined slash commands that bypass the agent loop.
    #[serde(default)]
    pub quick_commands: BTreeMap<String, QuickCommandConfig>,

    /// Whether this gateway process owns Kanban dispatch/notifier duties.
    #[serde(default = "default_true")]
    pub kanban_dispatch_in_gateway: bool,

    /// Curator engine configuration.
    #[serde(default)]
    pub curator: hermes_skills::CuratorConfig,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            ssrf_protection: true,
            media_cache_dir: None,
            media_cache_max_bytes: 0,
            streaming_enabled: false,
            streaming: StreamConfig::default(),
            service_tier: None,
            display: DisplayConfig::default(),
            quick_commands: BTreeMap::new(),
            kanban_dispatch_in_gateway: true,
            curator: hermes_skills::CuratorConfig::default(),
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
#[derive(Debug, Clone, Default)]
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
    /// Discord member role snowflakes (guild messages / slash interactions).
    pub role_ids: Vec<String>,
    /// Parent channel when `chat_id` is a thread (Discord).
    pub parent_channel_id: Option<String>,
    /// Per-channel ephemeral system prompt (Discord P2-5).
    pub channel_prompt: Option<String>,
    /// Channel-bound skills (Discord P2-6).
    pub channel_skills: Vec<String>,
    /// Channel topic string when known (Discord P2-7).
    pub channel_topic: Option<String>,
    /// Telegram DM topic thread id (`message_thread_id`).
    pub message_thread_id: Option<String>,
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
            role_ids: vec![],
            parent_channel_id: None,
            channel_prompt: None,
            channel_skills: Vec::new(),
            channel_topic: None,
            message_thread_id: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct CompressionOutcome {
    removed_messages: usize,
    summary_warning: Option<String>,
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

/// Central orchestrator for all platform adapters.
///
/// State is partitioned into four sub-systems; `Gateway` itself is a thin
/// facade that delegates to each and exposes the full public API.
pub struct Gateway {
    /// Platform adapter registry, handler callbacks, and access policies.
    pub(crate) router: MessageRouter,
    /// Session management, per-session runtime state, concurrency locks.
    pub(crate) session: SessionLayer,
    /// Stream manager, outbound file tracking, live messaging context.
    pub(crate) delivery: DeliveryLayer,
    /// Optional extensions: hooks, voice/STT, inbound preparer, clarify.
    pub(crate) extensions: ExtensionBus,
    pub(crate) config: GatewayConfig,
}

pub(crate) fn inbound_text_log_fields(text: &str) -> (usize, String, String) {
    let trimmed = text.trim();
    let chars = trimmed.chars().count();
    let preview: String = trimmed
        .chars()
        .take(48)
        .collect::<String>()
        .replace('\n', " ");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    trimmed.hash(&mut hasher);
    let text_fp = format!("{:016x}", hasher.finish());
    (chars, preview, text_fp)
}

fn outbound_text_log_tail(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    let count = trimmed.chars().count();
    if count <= max_chars {
        return trimmed.replace('\n', " ");
    }
    let tail: String = trimmed
        .chars()
        .skip(count.saturating_sub(max_chars))
        .collect();
    format!("…{}", tail.replace('\n', " "))
}

impl Gateway {
    pub(crate) fn route_correlation_id(incoming: &IncomingMessage, session_key: &str) -> String {
        if let Some(mid) = incoming
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return format!("{}:{mid}", incoming.platform);
        }
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        incoming.platform.hash(&mut hasher);
        incoming.chat_id.hash(&mut hasher);
        incoming.user_id.hash(&mut hasher);
        session_key.hash(&mut hasher);
        format!("{}:{:016x}", incoming.platform, hasher.finish())
    }

    /// Create a new `Gateway` with the given session manager and config.
    pub fn new(
        session_manager: Arc<SessionManager>,
        dm_manager: DmManager,
        config: GatewayConfig,
    ) -> Self {
        Self {
            router: MessageRouter::new(dm_manager),
            session: SessionLayer::new(session_manager),
            delivery: DeliveryLayer::new(config.streaming.clone()),
            extensions: ExtensionBus::new(),
            config,
        }
    }

    /// Wire the shared clarify dispatcher so inbound IM replies can fulfill an
    /// active sync `clarify` wait without waiting for the per-session route lock.
    pub async fn set_clarify_dispatcher(&self, dispatcher: ClarifyDispatcher) {
        *self.extensions.clarify_dispatcher.write().await = Some(dispatcher);
    }

    pub(crate) fn begin_turn_outbound_tracking(
        &self,
        session_key: &str,
        platform: &str,
        chat_id: &str,
    ) {
        let mut guard = self
            .delivery
            .turn_outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.insert(
            session_key.to_string(),
            TurnOutboundTracker::new(platform, chat_id),
        );
    }

    pub(crate) fn clear_turn_outbound_tracking(&self, session_key: &str) {
        let mut guard = self
            .delivery
            .turn_outbound
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.remove(session_key);
    }

    fn turn_outbound_file_count(&self, session_key: &str) -> usize {
        self.delivery
            .turn_outbound
            .lock()
            .ok()
            .and_then(|g| g.get(session_key).map(TurnOutboundTracker::count))
            .unwrap_or(0)
    }

    fn record_turn_outbound_file(&self, platform: &str, chat_id: &str, file_path: &str) {
        let path = hermes_config::resolve_outbound_media_path(file_path)
            .unwrap_or_else(|_| PathBuf::from(file_path));
        let canonical = path.canonicalize().unwrap_or(path);
        if let Ok(guard) = self.delivery.turn_outbound.lock() {
            for tracker in guard.values() {
                if tracker.matches(platform, chat_id) {
                    tracker.record(canonical);
                    return;
                }
            }
        }
    }

    /// Register Discord adapter and retain a handle for channel backfill (P2-8).
    #[cfg(feature = "discord")]
    pub async fn register_discord_adapter(
        &self,
        adapter: Arc<crate::platforms::discord::DiscordAdapter>,
    ) {
        *self.extensions.discord_adapter.write().await = Some(adapter.clone());
        self.register_adapter("discord", adapter).await;
    }

    async fn abort_active_route(&self, session_key: &str) -> bool {
        let handle = self.session.active_routes.write().await.remove(session_key);
        if let Some(handle) = handle {
            handle.abort();
            true
        } else {
            false
        }
    }

    /// Register the agent inbound preparer (vision enrich, native multimodal, etc.).
    pub async fn set_inbound_preparer(&self, preparer: Arc<dyn InboundMessagePreparer>) {
        *self.extensions.inbound_preparer.write().await = Some(preparer);
    }

    /// Wire voice manager + STT gate from app `config.yaml` (`tts` / `stt` blocks).
    pub async fn set_voice_runtime(&self, manager: Arc<VoiceManager>, stt_enabled: bool) {
        *self.extensions.voice_manager.write().await = Some(manager);
        *self.extensions.stt_enabled.write().await = stt_enabled;
    }

    /// Share session context with `send_message` for automatic platform/recipient fallback.
    pub async fn set_messaging_session_context(&self, ctx: Arc<MessagingSessionContext>) {
        *self.delivery.messaging_session.write().await = Some(ctx);
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
            .session
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        let mut states = self.session.runtime_state.write().await;
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
            .session
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        self.session.session_manager.get_messages(&key).await.len()
    }

    async fn clear_session_boundary_security_state(&self, session_key: &str) {
        if session_key.is_empty() {
            return;
        }
        let mut states = self.session.runtime_state.write().await;
        if let Some(state) = states.get_mut(session_key) {
            state.yolo = false;
        }
        hermes_tools::approval::clear_session(session_key);
    }

    pub(crate) async fn should_apply_reaction_lifecycle(&self, incoming: &IncomingMessage) -> bool {
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
            return incoming.is_dm || incoming.text.contains("<@") || incoming.text.contains("<@!");
        }
        false
    }

    /// Set the message handler for processing incoming messages.
    pub async fn set_message_handler(&self, handler: MessageHandler) {
        *self.router.message_handler.write().await = Some(handler);
        *self.router.message_handler_with_context.write().await = None;
    }

    /// Set a context-aware message handler for processing incoming messages.
    pub async fn set_message_handler_with_context(&self, handler: MessageHandlerWithContext) {
        *self.router.message_handler_with_context.write().await = Some(handler);
    }

    /// Set the streaming message handler.
    pub async fn set_streaming_handler(&self, handler: StreamingMessageHandler) {
        *self.router.streaming_handler.write().await = Some(handler);
        *self.router.streaming_handler_with_context.write().await = None;
    }

    /// Set a context-aware streaming message handler.
    pub async fn set_streaming_handler_with_context(
        &self,
        handler: StreamingMessageHandlerWithContext,
    ) {
        *self.router.streaming_handler_with_context.write().await = Some(handler);
    }

    /// Attach gateway hook registry for emitting lifecycle/progress events.
    pub async fn set_hook_registry(&self, registry: Arc<HookRegistry>) {
        *self.extensions.hook_registry.write().await = Some(registry);
    }

    /// Register agent-layer session teardown (POI flush, memory `on_session_end`, etc.).
    pub async fn set_session_teardown_handler(&self, handler: SessionTeardownHandler) {
        *self.session.session_teardown_handler.write().await = Some(handler);
    }

    async fn build_teardown_context(
        &self,
        snapshot: &SessionTeardownSnapshot,
        reason: &str,
    ) -> SessionTeardownContext {
        let state = self
            .session
            .runtime_state
            .read()
            .await
            .get(&snapshot.session_key)
            .cloned()
            .unwrap_or_default();
        SessionTeardownContext {
            session_key: snapshot.session_key.clone(),
            session_id: snapshot.session.id.clone(),
            platform: snapshot.session.platform.clone(),
            chat_id: snapshot.session.chat_id.clone(),
            user_id: snapshot.session.user_id.clone(),
            messages: snapshot.session.message_snapshot(),
            reason: reason.to_string(),
            model: state.model,
            provider: state.provider,
            personality: state.personality,
            home: state.home,
        }
    }

    async fn invoke_session_teardown(&self, ctx: SessionTeardownContext) {
        let handler = self.session.session_teardown_handler.read().await.clone();
        if let Some(handler) = handler {
            handler(ctx).await;
        }
    }

    async fn teardown_session_snapshot(&self, snapshot: SessionTeardownSnapshot, reason: &str) {
        let ctx = self.build_teardown_context(&snapshot, reason).await;
        self.invoke_session_teardown(ctx).await;
    }

    async fn teardown_session_key(&self, session_key: &str, reason: &str) {
        let session = self.session.session_manager.get_session(session_key).await;
        let Some(session) = session else {
            return;
        };
        self.teardown_session_snapshot(
            SessionTeardownSnapshot {
                session_key: session_key.to_string(),
                session,
            },
            reason,
        )
        .await;
    }

    /// Flush agent session-end hooks for every in-memory session (gateway shutdown).
    pub async fn teardown_all_sessions(&self, reason: &str) {
        let snapshots = self.session.session_manager.drain_all_sessions().await;
        for snapshot in snapshots {
            self.teardown_session_snapshot(snapshot, reason).await;
        }
    }

    /// Set per-platform access policies for non-DM and slash-command traffic.
    pub async fn set_platform_access_policies(
        &self,
        policies: HashMap<String, PlatformAccessPolicy>,
    ) {
        *self.router.platform_access_policies.write().await = policies
            .into_iter()
            .map(|(platform, policy)| (platform.to_ascii_lowercase(), policy))
            .collect();
    }

    pub(crate) async fn platform_access_policy(
        &self,
        platform: &str,
    ) -> Option<PlatformAccessPolicy> {
        let key = platform.trim().to_ascii_lowercase();
        self.router
            .platform_access_policies
            .read()
            .await
            .get(&key)
            .cloned()
    }

    /// Emit one hook event if a registry is configured.
    pub async fn emit_hook_event(&self, event_type: &str, context: serde_json::Value) {
        let registry = self.extensions.hook_registry.read().await.clone();
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
        self.router.adapters.write().await.insert(name, adapter);
    }

    /// Retrieve a registered platform adapter by name.
    pub async fn get_adapter(&self, name: &str) -> Option<Arc<dyn PlatformAdapter>> {
        self.router.adapters.read().await.get(name).cloned()
    }

    /// Start all registered and enabled platform adapters.
    pub async fn start_all(&self) -> Result<(), GatewayError> {
        let adapters = self.router.adapters.read().await;
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
        let adapters = self.router.adapters.read().await;
        for (name, adapter) in adapters.iter() {
            info!("Stopping platform adapter: {}", name);
            if let Err(e) = adapter.stop().await {
                warn!("Error stopping adapter '{}': {}", name, e);
            }
        }
        info!("All platform adapters stopped");
        Ok(())
    }

    /// Start platform typing indicators for the duration of a route (best-effort).
    ///
    /// Discord: single `trigger_typing`. Weixin / WhatsApp: START immediately, refresh
    /// every ~5s, then STOP when [`RouteTypingGuard::finish`] is called.
    pub(crate) fn spawn_route_typing(
        platform: &str,
        adapter: Arc<dyn PlatformAdapter>,
        chat_id: String,
    ) -> RouteTypingGuard {
        if platform.eq_ignore_ascii_case("discord") {
            tokio::spawn(async move {
                let _ = adapter.trigger_typing(&chat_id).await;
            });
            return RouteTypingGuard::none();
        }

        let refresh_secs = if platform.eq_ignore_ascii_case("whatsapp") {
            WHATSAPP_TYPING_REFRESH_SECS
        } else if platform.eq_ignore_ascii_case("weixin") {
            #[cfg(feature = "weixin")]
            {
                crate::platforms::weixin::WEIXIN_TYPING_REFRESH_SECS
            }
            #[cfg(not(feature = "weixin"))]
            {
                WEIXIN_TYPING_REFRESH_SECS_FALLBACK
            }
        } else {
            return RouteTypingGuard::none();
        };

        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancelled.clone();
        let join = tokio::spawn(async move {
            loop {
                let _ = adapter.trigger_typing(&chat_id).await;
                tokio::select! {
                    () = async {
                        while !cancel_flag.load(Ordering::Acquire) {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    } => break,
                    () = tokio::time::sleep(Duration::from_secs(refresh_secs)) => {}
                }
                if cancel_flag.load(Ordering::Acquire) {
                    break;
                }
            }
            let _ = adapter.stop_typing(&chat_id).await;
        });

        RouteTypingGuard {
            cancel: Some(cancelled),
            join: Some(join),
        }
    }

    // -----------------------------------------------------------------------
    // Message routing
    // -----------------------------------------------------------------------

    /// Route an incoming message through the full pipeline:
    /// DM check → session lookup → agent loop → response.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        crate::inbound_pipeline::route_inbound(self, incoming).await
    }

    fn quick_command_key(raw: &str) -> String {
        raw.trim()
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .replace('-', "_")
    }

    fn split_slash_command(input: &str) -> (String, String) {
        let trimmed = input.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or(trimmed).to_string();
        let args = parts.next().unwrap_or_default().trim().to_string();
        (cmd, args)
    }

    async fn run_quick_exec(
        name: &str,
        command: &str,
        timeout_secs: u64,
    ) -> Result<String, GatewayError> {
        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), child).await {
            Ok(result) => result.map_err(|e| {
                GatewayError::Platform(format!("quick command `{name}` failed: {e}"))
            })?,
            Err(_) => {
                return Ok(format!(
                    "Quick command `{name}` timed out after {timeout_secs}s."
                ));
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string();
        if !stdout.trim().is_empty() {
            return Ok(stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_string();
        if !stderr.trim().is_empty() {
            return Ok(stderr);
        }
        Ok("Quick command completed with no output.".to_string())
    }

    async fn resolve_quick_command(&self, input: &str) -> Result<Option<String>, GatewayError> {
        let (cmd, args) = Self::split_slash_command(input);
        let key = Self::quick_command_key(&cmd);
        let Some(quick) = self.config.quick_commands.get(&key).cloned() else {
            return Ok(None);
        };

        match quick.kind.trim().to_ascii_lowercase().as_str() {
            "exec" => {
                let Some(command) = quick.command.as_deref().filter(|v| !v.trim().is_empty())
                else {
                    return Ok(Some(format!(
                        "Quick command `{key}` has no command defined."
                    )));
                };
                Ok(Some(
                    Self::run_quick_exec(&key, command, quick.timeout_secs()).await?,
                ))
            }
            "alias" => {
                let Some(target) = quick.target.as_deref().filter(|v| !v.trim().is_empty()) else {
                    return Ok(Some(format!(
                        "Quick command `{key}` has no target defined."
                    )));
                };
                let mut rewritten = target.trim().to_string();
                if !args.is_empty() {
                    rewritten.push(' ');
                    rewritten.push_str(&args);
                }
                Ok(match handle_command(&rewritten) {
                    GatewayCommandResult::Reply(text)
                    | GatewayCommandResult::ShowHelp(text)
                    | GatewayCommandResult::Unknown(text)
                    | GatewayCommandResult::ResetSession(text)
                    | GatewayCommandResult::ToggleVerbose(text)
                    | GatewayCommandResult::ToggleYolo(text)
                    | GatewayCommandResult::ToggleReasoning(text)
                    | GatewayCommandResult::ShowUsage(text)
                    | GatewayCommandResult::ShowStatus(text)
                    | GatewayCommandResult::CompressContext(text)
                    | GatewayCommandResult::StopAgent(text) => Some(text),
                    GatewayCommandResult::SwitchModel { reply, .. }
                    | GatewayCommandResult::SwitchFast { reply, .. }
                    | GatewayCommandResult::SwitchPersonality { reply, .. }
                    | GatewayCommandResult::SetHome { reply, .. } => Some(reply),
                    _ => Some(format!("Quick command `{key}` routed to `{rewritten}`.")),
                })
            }
            other => Ok(Some(format!(
                "Quick command `{key}` has unsupported type `{other}`."
            ))),
        }
    }

    fn normalize_tool_progress_mode(raw: &str) -> Option<String> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "false" | "0" => Some("off".to_string()),
            "new" => Some("new".to_string()),
            "all" | "true" | "1" => Some("all".to_string()),
            "verbose" => Some("verbose".to_string()),
            _ => None,
        }
    }

    fn default_tool_progress_for_platform(&self, platform: &str) -> String {
        let platform_key = platform.trim().to_ascii_lowercase().replace('-', "_");
        self.config
            .display
            .platform_tool_progress(&platform_key)
            .and_then(Self::normalize_tool_progress_mode)
            .unwrap_or_else(|| match platform_key.as_str() {
                "telegram" | "slack" => "off".to_string(),
                _ => "all".to_string(),
            })
    }

    fn next_tool_progress_mode(current: &str) -> &'static str {
        match current {
            "off" => "new",
            "new" => "all",
            "all" => "verbose",
            _ => "off",
        }
    }

    async fn apply_verbose_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<String, GatewayError> {
        if !self.config.display.tool_progress_command_enabled() {
            return Ok(
                "Tool progress command is not enabled. Set `display.tool_progress_command: true` to use `/verbose`."
                    .to_string(),
            );
        }

        let platform = incoming
            .platform
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_");
        let default_mode = self.default_tool_progress_for_platform(&platform);
        let next = {
            let mut modes = self.router.tool_progress_modes.write().await;
            let current = modes
                .get(&platform)
                .cloned()
                .unwrap_or_else(|| default_mode.clone());
            let next = Self::next_tool_progress_mode(&current).to_string();
            modes.insert(platform.clone(), next.clone());
            next
        };

        let mut states = self.session.runtime_state.write().await;
        let state = states.entry(session_key.to_string()).or_default();
        state.tool_progress = Some(next.clone());
        state.verbose = next == "verbose";
        drop(states);

        Ok(format!(
            "📝 Tool progress for {platform}: {}",
            next.to_ascii_uppercase()
        ))
    }

    pub(crate) async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<bool, GatewayError> {
        if let Some(reply) = self.resolve_quick_command(&incoming.text).await? {
            self.send_incoming_reply(incoming, &reply, None).await?;
            return Ok(true);
        }

        // Batch fast-path: when the message contains 2+ slash commands, route to the
        // generalised batch dispatcher instead of the single-command path.
        let batch = parse_batch_commands(&incoming.text);
        if !batch.is_empty() {
            return self
                .execute_batch_commands(incoming, session_key, batch)
                .await;
        }

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

    pub(crate) async fn apply_command_result(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        result: GatewayCommandResult,
    ) -> Result<bool, GatewayError> {
        match result {
            GatewayCommandResult::Reply(text)
            | GatewayCommandResult::ShowHelp(text)
            | GatewayCommandResult::Unknown(text) => {
                self.send_incoming_reply(incoming, &text, None).await?;
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
                self.teardown_session_key(session_key, "reset").await;
                self.session
                    .session_manager
                    .reset_session(session_key)
                    .await;
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
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { model, reply } => {
                let mut states = self.session.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().model = Some(model);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchPersonality { name, reply } => {
                let mut states = self.session.runtime_state.write().await;
                states
                    .entry(session_key.to_string())
                    .or_default()
                    .personality = Some(name);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ApproveUser { user_id } => {
                let mut dm = self.router.dm_manager.write().await;
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
                let mut dm = self.router.dm_manager.write().await;
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
                let _ = self.abort_active_route(session_key).await;
                for (task_id, status, _) in self.router.background_tasks.list_tasks() {
                    if status == TaskStatus::Running {
                        let _ = self.router.background_tasks.cancel(&task_id);
                    }
                }
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ShowUsage(_) => {
                let text = self.build_usage_text(session_key).await;
                self.send_incoming_reply(incoming, &text, None).await?;
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
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ShowInsights(text) => {
                self.send_incoming_reply(incoming, &text, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleVerbose(_) => {
                let reply = self.apply_verbose_command(incoming, session_key).await?;
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleYolo(_) => {
                let mut states = self.session.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.yolo = !state.yolo;
                if state.yolo {
                    hermes_tools::approval::enable_session_yolo(session_key);
                } else {
                    hermes_tools::approval::disable_session_yolo(session_key);
                }
                let reply = format!("🤠 YOLO mode: {}", if state.yolo { "ON" } else { "OFF" });
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ResolveCommandApproval {
                choice,
                resolve_all,
            } => {
                let count = hermes_tools::approval::resolve_gateway_approval(
                    session_key,
                    choice,
                    resolve_all,
                );
                let reply = if count == 0 {
                    "No pending command approval for this session.".to_string()
                } else if choice == hermes_tools::approval::ApprovalChoice::Deny {
                    if count == 1 {
                        "Denied pending command. The blocked agent will resume with a denial."
                            .to_string()
                    } else {
                        format!("Denied {count} pending commands.")
                    }
                } else if count == 1 {
                    format!(
                        "Approved pending command with `{}` scope. Resuming.",
                        choice.as_str()
                    )
                } else {
                    format!(
                        "Approved {count} pending commands with `{}` scope.",
                        choice.as_str()
                    )
                };
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SetHome { path, reply } => {
                let target = std::path::Path::new(&path);
                let response = if target.exists() && target.is_dir() {
                    let mut states = self.session.runtime_state.write().await;
                    states.entry(session_key.to_string()).or_default().home = Some(path);
                    reply
                } else {
                    format!("❌ Path not found or not a directory: {}", path)
                };
                self.send_incoming_reply(incoming, &response, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ShowStatus(_) => {
                let text = self.build_status_text(session_key).await;
                self.send_incoming_reply(incoming, &text, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ReloadMcp => {
                let mut generation = self.router.mcp_reload_generation.write().await;
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
                let mut states = self.session.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProfile { profile, reply } => {
                let mut states = self.session.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().profile = Some(profile);
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchBranch { branch } => {
                let reply = match branch {
                    Some(name) => {
                        let mut states = self.session.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().branch =
                            Some(name.clone());
                        format!("🌿 Branch context switched to: {}", name)
                    }
                    None => {
                        let branch = self
                            .session
                            .runtime_state
                            .read()
                            .await
                            .get(session_key)
                            .and_then(|s| s.branch.clone())
                            .unwrap_or_else(|| "main".to_string());
                        format!("🌿 Current branch context: {}", branch)
                    }
                };
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::Rollback { steps } => {
                let mut removed = 0usize;
                for _ in 0..steps {
                    if self
                        .session
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
                let mut states = self.session.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.reasoning = !state.reasoning;
                let reply = format!(
                    "🧠 Reasoning visibility: {}",
                    if state.reasoning { "ON" } else { "OFF" }
                );
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchFast {
                service_tier,
                reply,
            } => {
                let mut states = self.session.runtime_state.write().await;
                states
                    .entry(session_key.to_string())
                    .or_default()
                    .service_tier = service_tier.clone();
                drop(states);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::Retry => {
                let mut messages = self.session.session_manager.get_messages(session_key).await;
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
                self.session
                    .session_manager
                    .replace_messages(session_key, messages)
                    .await;
                let snapshot = self
                    .session
                    .session_manager
                    .snapshot_messages(session_key)
                    .await;
                let route_id = Self::route_correlation_id(incoming, session_key);
                self.route_non_streaming(incoming, snapshot, session_key, &route_id)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Undo => {
                let mut removed = 0usize;
                if let Some(last) = self
                    .session
                    .session_manager
                    .pop_last_message(session_key)
                    .await
                {
                    removed += 1;
                    if last.role == MessageRole::Assistant {
                        if let Some(prev) = self
                            .session
                            .session_manager
                            .pop_last_message(session_key)
                            .await
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
                self.send_incoming_reply(incoming, &reply, None).await?;
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
                self.send_incoming_reply(incoming, &text, None).await?;
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
                    .session
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let text = if sessions.is_empty() {
                    "📚 No sessions found for your user.".to_string()
                } else {
                    let mut out = String::from("📚 **Your sessions:**\n\n");
                    for s in sessions {
                        let key = self.session.session_manager.compose_session_key(
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
                self.send_incoming_reply(incoming, &text, None).await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchSession { session_id } => {
                let sessions = self
                    .session
                    .session_manager
                    .get_user_sessions(&incoming.user_id)
                    .await;
                let matched = sessions.iter().find(|s| {
                    let key = self.session.session_manager.compose_session_key(
                        &s.platform,
                        &s.chat_id,
                        &s.user_id,
                    );
                    key == session_id || s.id == session_id
                });
                let msg = if let Some(target) = matched {
                    let copied = self
                        .session
                        .session_manager
                        .replace_messages(session_key, target.messages.as_ref().clone())
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
                self.send_incoming_reply(incoming, &msg, None).await?;
                Ok(true)
            }
            GatewayCommandResult::ShowBudget { new_budget } => {
                let mut states = self.session.runtime_state.write().await;
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
                self.send_incoming_reply(incoming, &msg, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorStatus => {
                let reply = self.execute_curator_status();
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorRun { dry_run } => {
                let reply = self.execute_curator_run(dry_run);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorPause => {
                let reply = self.execute_curator_pause_resume(true);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorResume => {
                let reply = self.execute_curator_pause_resume(false);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorPin { name } => {
                let reply = self.execute_curator_pin_unpin(&name, true);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorUnpin { name } => {
                let reply = self.execute_curator_pin_unpin(&name, false);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorArchive { name } => {
                let reply = self.execute_curator_archive(&name);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorRestore { name } => {
                let reply = self.execute_curator_restore(&name);
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::CuratorListArchived => {
                let reply = self.execute_curator_list_archived();
                self.send_incoming_reply(incoming, &reply, None).await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }

    // -----------------------------------------------------------------------
    // Curator helper methods
    // -----------------------------------------------------------------------

    fn execute_curator_status(&self) -> String {
        let skills_dir = hermes_config::hermes_home().join("skills");
        let state = hermes_skills::load_curator_state(&skills_dir);
        let config = &self.config.curator;
        let report = hermes_skills::agent_created_report(&skills_dir);

        let status = if state.paused {
            "PAUSED"
        } else if config.enabled {
            "ENABLED"
        } else {
            "DISABLED"
        };

        let mut lines = vec![format!("curator: {status}")];
        lines.push(format!("  runs: {}", state.run_count));
        if let Some(ref last) = state.last_run_at {
            lines.push(format!("  last run: {last}"));
        }
        if let Some(ref summary) = state.last_run_summary {
            lines.push(format!("  last summary: {summary}"));
        }
        if let Some(ref report_path) = state.last_report_path {
            lines.push(format!("  last report: {report_path}"));
        }
        lines.push(format!("  interval: every {}h", config.interval_hours));
        lines.push(format!("  stale after: {}d", config.stale_after_days));
        lines.push(format!("  archive after: {}d", config.archive_after_days));
        lines.push(String::new());
        lines.push(format!("agent-created skills: {} total", report.len()));

        let active = report.iter().filter(|r| r.state == "active").count();
        let stale = report.iter().filter(|r| r.state == "stale").count();
        let archived = report.iter().filter(|r| r.state == "archived").count();
        lines.push(format!("  active: {active}"));
        lines.push(format!("  stale: {stale}"));
        lines.push(format!("  archived: {archived}"));

        lines.join("\n")
    }

    fn execute_curator_run(&self, dry_run: bool) -> String {
        let skills_dir = hermes_config::hermes_home().join("skills");
        let config = &self.config.curator;
        let result = hermes_skills::apply_automatic_transitions(&skills_dir, config);

        let mut lines = vec![];
        if dry_run {
            lines.push("── curator dry-run ──".to_string());
        } else {
            lines.push("── curator run ──".to_string());
        }
        lines.push(format!("  checked: {}", result.checked));
        lines.push(format!("  marked stale: {}", result.marked_stale));
        lines.push(format!("  archived: {}", result.archived));
        lines.push(format!("  reactivated: {}", result.reactivated));

        if !dry_run {
            let mut state = hermes_skills::load_curator_state(&skills_dir);
            state.last_run_at = Some(chrono::Utc::now().to_rfc3339());
            state.run_count += 1;
            state.last_run_summary = Some(format!(
                "auto: {} checked, {} stale, {} archived, {} reactivated",
                result.checked, result.marked_stale, result.archived, result.reactivated
            ));
            let _ = hermes_skills::save_curator_state(&skills_dir, &state);
            lines.push("State updated.".to_string());
        }

        lines.join("\n")
    }

    fn execute_curator_pause_resume(&self, pause: bool) -> String {
        let skills_dir = hermes_config::hermes_home().join("skills");
        match hermes_skills::set_paused(&skills_dir, pause) {
            Ok(()) => {
                if pause {
                    "✓ Curator paused.".to_string()
                } else {
                    "✓ Curator resumed.".to_string()
                }
            }
            Err(e) => format!("Failed: {e}"),
        }
    }

    fn execute_curator_pin_unpin(&self, name: &str, pin: bool) -> String {
        if name.is_empty() {
            return if pin {
                "Usage: /curator pin <skill-name>".to_string()
            } else {
                "Usage: /curator unpin <skill-name>".to_string()
            };
        }
        let skills_dir = hermes_config::hermes_home().join("skills");
        match hermes_skills::set_pinned(&skills_dir, name, pin) {
            Ok(()) => {
                if pin {
                    format!("✓ {name} pinned.")
                } else {
                    format!("✓ {name} unpinned.")
                }
            }
            Err(e) => format!("Failed: {e}"),
        }
    }

    fn execute_curator_archive(&self, name: &str) -> String {
        if name.is_empty() {
            return "Usage: /curator archive <skill-name>".to_string();
        }
        let skills_dir = hermes_config::hermes_home().join("skills");
        match hermes_skills::archive_skill(&skills_dir, name) {
            Ok((true, msg)) => format!("✓ {msg}"),
            Ok((false, msg)) => format!("✗ {msg}"),
            Err(e) => format!("Failed: {e}"),
        }
    }

    fn execute_curator_restore(&self, name: &str) -> String {
        if name.is_empty() {
            return "Usage: /curator restore <skill-name>".to_string();
        }
        let skills_dir = hermes_config::hermes_home().join("skills");
        match hermes_skills::restore_skill(&skills_dir, name) {
            Ok((true, msg)) => format!("✓ {msg}"),
            Ok((false, msg)) => format!("✗ {msg}"),
            Err(e) => format!("Failed: {e}"),
        }
    }

    fn execute_curator_list_archived(&self) -> String {
        let skills_dir = hermes_config::hermes_home().join("skills");
        let archive_dir = skills_dir.join(".archive");
        let mut names = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&archive_dir) {
            for entry in entries.flatten() {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if let Some(name) = entry.file_name().to_str() {
                        names.push(name.to_string());
                    }
                }
            }
        }
        if names.is_empty() {
            "No archived skills.".to_string()
        } else {
            names.sort();
            format!(
                "Archived skills ({}):\n{}",
                names.len(),
                names
                    .iter()
                    .map(|n| format!("  • {n}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }

    /// Non-streaming message routing: invoke agent, send complete response.
    pub(crate) async fn route_non_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Arc<Vec<Message>>,
        session_key: &str,
        route_id: &str,
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
        let context_handler = self
            .router
            .message_handler_with_context
            .read()
            .await
            .clone();
        let feedback_done = Arc::new(AtomicBool::new(false));
        if platform_wants_processing_ack(&incoming.platform) {
            let adapter = self.get_adapter(&incoming.platform).await;
            let chat_id = incoming.chat_id.clone();
            let done = feedback_done.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(non_streaming_feedback_delay_ms())).await;
                if !done.load(Ordering::Acquire) {
                    if let Some(adapter) = adapter {
                        let _ = adapter
                            .send_message(&chat_id, "处理中，请稍候...", None)
                            .await;
                    }
                }
            });
        }
        let agent_start = Instant::now();
        info!(
            route_id = %route_id,
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            message_count = messages.len(),
            "gateway non-streaming agent start"
        );
        let messages = Self::inject_discord_channel_context(incoming, messages);
        let response_result = if let Some(handler) = context_handler {
            handler(messages, runtime_context).await
        } else {
            let handler = self.router.message_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No message handler configured".into()))?;
            let messages = self.inject_runtime_hints(session_key, messages).await;
            handler(messages).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                feedback_done.store(true, Ordering::Release);
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
        feedback_done.store(true, Ordering::Release);
        info!(
            route_id = %route_id,
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            elapsed_ms = agent_start.elapsed().as_millis() as u64,
            response_chars = response.chars().count(),
            "gateway non-streaming agent finished"
        );

        // Add assistant response to session
        self.session
            .session_manager
            .add_message(session_key, Message::assistant(&response))
            .await;
        self.bump_output_usage(session_key, response.chars().count())
            .await;

        // Send response back to the platform (text + MEDIA: local attachments)
        let (attachment_failures, cleaned, attachments_delivered) = self
            .finalize_outbound_attachments(
                &incoming.platform,
                &incoming.chat_id,
                &incoming.text,
                &response,
                session_key,
            )
            .await;
        let reply_text = Self::reply_text_with_attachment_outcome(
            cleaned,
            &attachment_failures,
            &incoming.text,
            attachments_delivered,
        );
        self.send_incoming_reply(incoming, &reply_text, None)
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

    /// Merge streamed deltas with the handler's final string (WeCom native stream only
    /// flushes `on_chunk` text; tool rounds and the final answer may exist only in `response`).
    fn streaming_delivery_text(streamed: &str, handler_response: &str) -> String {
        let trimmed_response = handler_response.trim();
        if trimmed_response.is_empty() {
            return streamed.to_string();
        }
        let acc = streamed.trim();
        if acc.is_empty() || acc == "..." {
            return handler_response.to_string();
        }
        if trimmed_response.chars().count() >= acc.chars().count() {
            handler_response.to_string()
        } else {
            streamed.to_string()
        }
    }

    /// Streaming message routing: progressively edit messages as tokens arrive.
    pub(crate) async fn route_streaming(
        &self,
        incoming: &IncomingMessage,
        messages: Arc<Vec<Message>>,
        session_key: &str,
        route_id: &str,
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
        let context_handler = self
            .router
            .streaming_handler_with_context
            .read()
            .await
            .clone();
        let messages = Self::inject_discord_channel_context(incoming, messages);
        let message_count = messages.len();
        let legacy_messages = self.inject_runtime_hints(session_key, messages).await;

        let adapter_for_platform = self
            .router
            .adapters
            .read()
            .await
            .get(&incoming.platform)
            .cloned();
        let native_streaming = adapter_for_platform
            .as_ref()
            .map(|a| a.supports_native_streaming())
            .unwrap_or(false);

        let mut stream_id: Option<String> = None;
        let mut stream_edit_lock: Option<Arc<TokioMutex<()>>> = None;
        let mut stream_finalized: Option<Arc<AtomicBool>> = None;
        let mut native_worker: Option<tokio::task::JoinHandle<()>> = None;
        let native_started = Arc::new(AtomicBool::new(false));
        let native_failed = Arc::new(AtomicBool::new(false));
        let first_visible_emitted = Arc::new(AtomicBool::new(false));
        let first_visible_chunk_ms = Arc::new(AtomicU64::new(u64::MAX));
        let streaming_finished = Arc::new(AtomicBool::new(false));
        let stream_visible_start = Instant::now();

        let on_chunk: Arc<dyn Fn(String) + Send + Sync> = if native_streaming {
            let (tx, mut rx) = mpsc::unbounded_channel::<String>();
            let adapter = adapter_for_platform
                .clone()
                .expect("adapter exists when native_streaming");
            let chat_id = incoming.chat_id.clone();
            let reply_to = incoming.message_id.clone();
            let started = native_started.clone();
            let failed = native_failed.clone();
            let visible_emitted = first_visible_emitted.clone();
            let visible_ms = first_visible_chunk_ms.clone();
            native_worker = Some(tokio::spawn(async move {
                let flush_interval = Duration::from_millis(wecom_native_stream_flush_interval_ms());
                let mut ticker = tokio::time::interval(flush_interval);
                ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

                let native_stream_id: Option<String> = match adapter
                    .start_native_stream(
                        &chat_id,
                        reply_to.as_deref(),
                        Some(WECOM_NATIVE_STREAM_THINKING),
                    )
                    .await
                {
                    Ok(Some(sid)) => {
                        started.store(true, Ordering::Release);
                        if !visible_emitted.swap(true, Ordering::AcqRel) {
                            visible_ms.store(0, Ordering::Release);
                        }
                        Some(sid)
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
                };
                let mut accumulated = String::new();
                let mut last_flushed = String::new();

                loop {
                    tokio::select! {
                        chunk = rx.recv() => {
                            match chunk {
                                None => break,
                                Some(chunk) if chunk.trim().is_empty() => {}
                                Some(chunk) => {
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
                .delivery
                .stream_manager
                .start_stream(&incoming.platform, &incoming.chat_id)
                .await;
            stream_id = Some(stream_handle.id.clone());

            let reply_to = incoming
                .interaction_id
                .is_none()
                .then(|| incoming.message_id.as_deref())
                .flatten()
                .filter(|id| !id.is_empty());
            let anchor_id = if incoming.platform == "feishu" {
                None
            } else if let Some(adapter) = self.get_adapter(&incoming.platform).await {
                adapter
                    .send_message_in_thread(
                        &incoming.chat_id,
                        "...",
                        None,
                        reply_to,
                        incoming.message_thread_id.as_deref(),
                    )
                    .await?
            } else {
                self.send_message(&incoming.platform, &incoming.chat_id, "...", None)
                    .await?;
                None
            };
            if let Some(stream_id) = stream_id.as_ref() {
                if incoming.platform == "feishu" {
                    // Feishu does not use placeholder message edits in legacy streaming.
                    // Keep anchor unset so finalize sends the final text directly.
                } else if let Some(mid) = anchor_id.as_deref().or(Some("stream-anchor")) {
                    self.delivery
                        .stream_manager
                        .set_message_id(stream_id, mid)
                        .await;
                }
            }
            if incoming.platform != "feishu" {
                first_visible_emitted.store(true, Ordering::Release);
                first_visible_chunk_ms.store(0, Ordering::Release);
            }

            let stream_manager = self.delivery.stream_manager.clone();
            let platform = incoming.platform.clone();
            let chat_id = incoming.chat_id.clone();
            let gateway_adapters = self.router.adapters.read().await.clone();
            let sid = stream_id.clone().unwrap_or_default();
            // Serialize progressive edits; block chunk flushes after finalize so a
            // late in-flight edit cannot overwrite the final message (Discord race).
            let edit_lock = Arc::new(TokioMutex::new(()));
            let finalized_flag = Arc::new(AtomicBool::new(false));
            stream_edit_lock = Some(edit_lock.clone());
            stream_finalized = Some(finalized_flag.clone());
            let visible_emitted = first_visible_emitted.clone();
            let visible_ms = first_visible_chunk_ms.clone();
            let visible_start = stream_visible_start;
            #[cfg(feature = "discord")]
            let discord_progressive_edit_gate = if incoming.platform == "discord" {
                Some(Arc::new(TokioMutex::new(
                    Instant::now()
                        - Duration::from_millis(
                            crate::platforms::discord::stream_finalize::DISCORD_PROGRESSIVE_EDIT_MIN_MS,
                        ),
                )))
            } else {
                None
            };
            #[cfg(not(feature = "discord"))]
            let discord_progressive_edit_gate: Option<Arc<TokioMutex<Instant>>> = None;

            Arc::new(move |chunk: String| {
                if !chunk.trim().is_empty() && !visible_emitted.swap(true, Ordering::AcqRel) {
                    visible_ms.store(
                        visible_start.elapsed().as_millis() as u64,
                        Ordering::Release,
                    );
                }
                let sm = stream_manager.clone();
                let sid = sid.clone();
                let platform = platform.clone();
                let chat_id = chat_id.clone();
                let adapters = gateway_adapters.clone();
                let edit_lock = edit_lock.clone();
                let finalized = finalized_flag.clone();
                #[cfg(feature = "discord")]
                let discord_progressive_edit_gate = discord_progressive_edit_gate.clone();

                tokio::spawn(async move {
                    let Some(should_flush) = sm.update_stream(&sid, &chunk).await else {
                        return;
                    };
                    if !should_flush {
                        return;
                    }
                    if platform == "feishu" {
                        return;
                    }
                    let _guard = edit_lock.lock().await;
                    if finalized.load(Ordering::Acquire) {
                        return;
                    }
                    #[cfg(feature = "discord")]
                    if platform == "discord" {
                        if let Some(gate) = discord_progressive_edit_gate.as_ref() {
                            let last = *gate.lock().await;
                            if last.elapsed()
                                < Duration::from_millis(
                                    crate::platforms::discord::stream_finalize::DISCORD_PROGRESSIVE_EDIT_MIN_MS,
                                )
                            {
                                return;
                            }
                        }
                    }
                    let Some(content) = sm.get_stream_content(&sid).await else {
                        return;
                    };
                    if finalized.load(Ordering::Acquire) {
                        return;
                    }
                    let Some(adapter) = adapters.get(&platform) else {
                        return;
                    };
                    if let Some(message_id) = sm.get_message_id(&sid).await {
                        let edit_result =
                            adapter.edit_message(&chat_id, &message_id, &content).await;
                        if edit_result.is_ok() {
                            #[cfg(feature = "discord")]
                            if platform == "discord" {
                                if let Some(gate) = discord_progressive_edit_gate.as_ref() {
                                    *gate.lock().await = Instant::now();
                                }
                            }
                        } else if let Err(err) = edit_result {
                            let content_chars = content.chars().count();
                            warn!(
                                platform = %platform,
                                chat_id = %chat_id,
                                error = %err,
                                content_chars,
                                content_tail = %outbound_text_log_tail(&content, 64),
                                "streaming progressive edit failed"
                            );
                            if platform.eq_ignore_ascii_case("wecom") {
                                info!(
                                    chat_id = %chat_id,
                                    content_chars,
                                    content_tail = %outbound_text_log_tail(&content, 64),
                                    "wecom streaming: edit unsupported, chunk not delivered until finalize"
                                );
                            }
                        }
                    } else if let Err(err) = adapter.send_message(&chat_id, &content, None).await {
                        warn!(
                            platform = %platform,
                            chat_id = %chat_id,
                            error = %err,
                            "streaming progressive send failed"
                        );
                    }
                });
            })
        };

        // Invoke the streaming handler
        let agent_start = Instant::now();
        if incoming.platform.eq_ignore_ascii_case("wecom") {
            let adapter = adapter_for_platform.clone();
            let chat_id = incoming.chat_id.clone();
            let visible_emitted = first_visible_emitted.clone();
            let visible_ms = first_visible_chunk_ms.clone();
            let finished = streaming_finished.clone();
            let visible_start = stream_visible_start;
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(streaming_feedback_delay_ms())).await;
                if finished.load(Ordering::Acquire) || visible_emitted.load(Ordering::Acquire) {
                    return;
                }
                if let Some(adapter) = adapter {
                    if adapter
                        .send_message(&chat_id, "处理中，请稍候...", None)
                        .await
                        .is_ok()
                    {
                        if !visible_emitted.swap(true, Ordering::AcqRel) {
                            visible_ms.store(
                                visible_start.elapsed().as_millis() as u64,
                                Ordering::Release,
                            );
                        }
                        info!(
                            chat_id = %chat_id,
                            elapsed_ms = visible_start.elapsed().as_millis() as u64,
                            "gateway streaming delayed first visible output; sent progress notice"
                        );
                    }
                }
            });
        }
        info!(
            route_id = %route_id,
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            message_count = message_count,
            "gateway streaming agent start"
        );
        let response_result = if let Some(handler) = context_handler {
            handler(legacy_messages, runtime_context, on_chunk).await
        } else {
            let handler = self.router.streaming_handler.read().await;
            let handler = handler
                .as_ref()
                .ok_or_else(|| GatewayError::Platform("No streaming handler configured".into()))?;
            handler(legacy_messages, on_chunk).await
        };
        let response = match response_result {
            Ok(text) => text,
            Err(e) => {
                streaming_finished.store(true, Ordering::Release);
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
        streaming_finished.store(true, Ordering::Release);
        let first_visible_ms = {
            let raw = first_visible_chunk_ms.load(Ordering::Acquire);
            if raw == u64::MAX { None } else { Some(raw) }
        };
        info!(
            route_id = %route_id,
            platform = %incoming.platform,
            chat_id = %incoming.chat_id,
            session_key = %session_key,
            elapsed_ms = agent_start.elapsed().as_millis() as u64,
            first_user_visible_chunk_ms = ?first_visible_ms,
            response_chars = response.chars().count(),
            "gateway streaming agent finished"
        );

        let (attachment_failures, cleaned, attachments_delivered) = self
            .finalize_outbound_attachments(
                &incoming.platform,
                &incoming.chat_id,
                &incoming.text,
                &response,
                session_key,
            )
            .await;
        let fallback_text = Self::reply_text_with_attachment_outcome(
            cleaned,
            &attachment_failures,
            &incoming.text,
            attachments_delivered,
        );

        if let Some(worker) = native_worker {
            let _ = worker.await;
            // If native stream could not start, fall back to one-shot delivery.
            if !native_started.load(Ordering::Acquire) || native_failed.load(Ordering::Acquire) {
                self.send_message(&incoming.platform, &incoming.chat_id, &fallback_text, None)
                    .await?;
            }
            // Native stream success: final body was already sent via send_native_stream_chunk
            // (finish=true). Do not send_message again — it duplicates the streamed reply.
        } else if let Some(stream_id) = stream_id {
            if let (Some(edit_lock), Some(finalized)) =
                (stream_edit_lock.as_ref(), stream_finalized.as_ref())
            {
                finalized.store(true, Ordering::Release);
                let _guard = edit_lock.lock().await;
            }
            let anchor_message_id = self
                .delivery
                .stream_manager
                .get_message_id(&stream_id)
                .await;
            let accumulated = self
                .delivery
                .stream_manager
                .finish_stream(&stream_id)
                .await
                .unwrap_or_default();
            let final_text = Self::streaming_delivery_text(&accumulated, &response);
            if incoming.platform.eq_ignore_ascii_case("wecom") {
                info!(
                    route_id = %route_id,
                    chat_id = %incoming.chat_id,
                    streamed_chars = accumulated.chars().count(),
                    handler_chars = response.chars().count(),
                    final_chars = final_text.chars().count(),
                    streamed_tail = %outbound_text_log_tail(&accumulated, 80),
                    handler_tail = %outbound_text_log_tail(&response, 80),
                    final_tail = %outbound_text_log_tail(&final_text, 80),
                    anchor_message_id = ?anchor_message_id,
                    "wecom streaming finalize: choosing delivery text"
                );
            }
            if !final_text.trim().is_empty() {
                let trimmed = final_text.trim();
                #[cfg(feature = "discord")]
                let chunks = if incoming.platform == "discord" {
                    crate::platforms::discord::split_message(
                        trimmed,
                        crate::platforms::discord::MAX_MESSAGE_LENGTH,
                    )
                } else {
                    vec![trimmed.to_string()]
                };
                #[cfg(not(feature = "discord"))]
                let chunks = vec![trimmed.to_string()];

                if let Some(adapter) = self.get_adapter(&incoming.platform).await {
                    #[cfg(feature = "discord")]
                    {
                        crate::platforms::discord::stream_finalize::deliver_legacy_stream_final(
                            adapter.as_ref(),
                            &incoming.platform,
                            &incoming.chat_id,
                            anchor_message_id.as_deref(),
                            &chunks,
                        )
                        .await?;
                    }
                    #[cfg(not(feature = "discord"))]
                    {
                        if let Some(message_id) = anchor_message_id.as_deref() {
                            if let Some(first) = chunks.first() {
                                if let Err(err) = adapter
                                    .edit_message(&incoming.chat_id, message_id, first)
                                    .await
                                {
                                    warn!(
                                        platform = %incoming.platform,
                                        chat_id = %incoming.chat_id,
                                        message_id = %message_id,
                                        error = %err,
                                        reply_chars = first.chars().count(),
                                        reply_tail = %outbound_text_log_tail(first, 80),
                                        "streaming final edit failed; sending full reply"
                                    );
                                    adapter.send_message(&incoming.chat_id, first, None).await?;
                                    if incoming.platform.eq_ignore_ascii_case("wecom") {
                                        info!(
                                            chat_id = %incoming.chat_id,
                                            reply_chars = first.chars().count(),
                                            reply_tail = %outbound_text_log_tail(first, 80),
                                            "wecom streaming finalize: sent via send_message fallback"
                                        );
                                    }
                                } else if incoming.platform.eq_ignore_ascii_case("wecom") {
                                    info!(
                                        chat_id = %incoming.chat_id,
                                        message_id = %message_id,
                                        reply_chars = first.chars().count(),
                                        reply_tail = %outbound_text_log_tail(first, 80),
                                        "wecom streaming finalize: final edit succeeded"
                                    );
                                }
                            }
                            for chunk in chunks.iter().skip(1) {
                                adapter.send_message(&incoming.chat_id, chunk, None).await?;
                                if incoming.platform.eq_ignore_ascii_case("wecom") {
                                    info!(
                                        chat_id = %incoming.chat_id,
                                        chunk_chars = chunk.chars().count(),
                                        chunk_tail = %outbound_text_log_tail(chunk, 80),
                                        "wecom streaming finalize: sent extra chunk"
                                    );
                                }
                            }
                        } else {
                            for chunk in &chunks {
                                adapter.send_message(&incoming.chat_id, chunk, None).await?;
                                if incoming.platform.eq_ignore_ascii_case("wecom") {
                                    info!(
                                        chat_id = %incoming.chat_id,
                                        chunk_chars = chunk.chars().count(),
                                        chunk_tail = %outbound_text_log_tail(chunk, 80),
                                        "wecom streaming finalize: sent chunk (no anchor)"
                                    );
                                }
                            }
                        }
                    }
                } else {
                    for chunk in &chunks {
                        self.send_message(&incoming.platform, &incoming.chat_id, chunk, None)
                            .await?;
                    }
                }
            }
        }

        // Add assistant response to session
        self.session
            .session_manager
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

    fn inject_discord_channel_context(
        incoming: &IncomingMessage,
        messages: Arc<Vec<Message>>,
    ) -> Arc<Vec<Message>> {
        if incoming.platform != "discord" {
            return messages;
        }
        let mut hints = Vec::new();
        if let Some(prompt) = incoming.channel_prompt.as_deref().filter(|s| !s.is_empty()) {
            hints.push(format!("[channel_prompt]\n{prompt}"));
        }
        if let Some(topic) = incoming.channel_topic.as_deref().filter(|s| !s.is_empty()) {
            hints.push(format!("[channel_topic]\n{topic}"));
        }
        if !incoming.channel_skills.is_empty() {
            hints.push(format!(
                "[channel_skills]\n{}",
                incoming.channel_skills.join(", ")
            ));
        }
        if hints.is_empty() {
            return messages;
        }
        let mut out = Vec::with_capacity(messages.len() + hints.len());
        for hint in hints {
            out.push(Message::system(hint));
        }
        out.extend_from_slice(&messages);
        Arc::new(out)
    }

    async fn inject_runtime_hints(
        &self,
        session_key: &str,
        messages: Arc<Vec<Message>>,
    ) -> Arc<Vec<Message>> {
        let state = self
            .session
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
        if let Some(service_tier) = state
            .service_tier
            .or_else(|| normalize_service_tier(self.config.service_tier.as_deref()))
        {
            hints.push(format!("service_tier={service_tier}"));
        }
        if hints.is_empty() {
            return messages;
        }

        let mut out = Vec::with_capacity(messages.len() + 1);
        out.push(Message::system(format!(
            "[gateway_runtime]\n{}",
            hints.join("\n")
        )));
        out.extend_from_slice(&messages);
        Arc::new(out)
    }

    async fn build_runtime_context(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> GatewayRuntimeContext {
        let state = self
            .session
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let mcp_reload_generation = *self.router.mcp_reload_generation.read().await;

        let session_id = self
            .session
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
            service_tier: state
                .service_tier
                .or_else(|| normalize_service_tier(self.config.service_tier.as_deref())),
            tool_progress: state.tool_progress.clone(),
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
            if platform.eq_ignore_ascii_case("wecom") {
                info!(
                    chat_id = chat_id,
                    message_chars = message.chars().count(),
                    message_tail = %outbound_text_log_tail(&message, 80),
                    "wecom flushing deferred post-delivery message"
                );
            }
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

    pub(crate) async fn bump_input_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.session.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.user_messages += 1;
        stat.input_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    async fn bump_output_usage(&self, session_key: &str, chars: usize) {
        let mut usage = self.session.usage_stats.write().await;
        let stat = usage.entry(session_key.to_string()).or_default();
        stat.assistant_messages += 1;
        stat.output_chars += chars as u64;
        stat.last_updated_at = Some(Utc::now());
    }

    /// Cache agent-reported token totals for `/usage` between gateway turns.
    pub async fn sync_session_token_usage(
        &self,
        session_key: &str,
        display: hermes_agent::SessionUsageDisplay,
    ) {
        self.session
            .session_token_usage
            .write()
            .await
            .insert(session_key.to_string(), display);
    }

    async fn build_usage_text(&self, session_key: &str) -> String {
        if let Some(display) = self
            .session
            .session_token_usage
            .read()
            .await
            .get(session_key)
            .cloned()
        {
            if display.calls > 0 {
                return hermes_agent::format_gateway_usage_text(&display);
            }
        }
        let usage = self.session.usage_stats.read().await;
        let stat = usage.get(session_key).cloned().unwrap_or_default();
        let approx_input_tokens = stat.input_chars / 4;
        let approx_output_tokens = stat.output_chars / 4;
        format!(
            "📊 Usage\n- user messages: {}\n- assistant messages: {}\n- input chars: {} (~{} tokens)\n- output chars: {} (~{} tokens)\n(API token totals appear after the first model call.)",
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
        let current = self.session.session_manager.get_messages(session_key).await;
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

        self.session
            .session_manager
            .replace_messages(session_key, compressed)
            .await;
        CompressionOutcome {
            removed_messages,
            summary_warning,
        }
    }

    async fn build_status_text(&self, session_key: &str) -> String {
        let state = self
            .session
            .runtime_state
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let usage = self
            .session
            .usage_stats
            .read()
            .await
            .get(session_key)
            .cloned()
            .unwrap_or_default();
        let messages = self.session.session_manager.get_messages(session_key).await;
        let running_tasks = self
            .router
            .background_tasks
            .list_tasks()
            .into_iter()
            .filter(|(_, status, _)| *status == TaskStatus::Running)
            .count();

        format!(
            "🧭 Gateway status\n- model: {}\n- provider: {}\n- profile: {}\n- branch: {}\n- personality: {}\n- service_tier: {}\n- reasoning: {}\n- verbose: {}\n- tool_progress: {}\n- yolo: {}\n- home: {}\n- messages in session: {}\n- running background tasks: {}\n- mcp generation: {}\n- input/output chars: {}/{}",
            state.model.unwrap_or_else(|| "default".to_string()),
            state.provider.unwrap_or_else(|| "default".to_string()),
            state.profile.unwrap_or_else(|| "default".to_string()),
            state.branch.unwrap_or_else(|| "main".to_string()),
            state.personality.unwrap_or_else(|| "default".to_string()),
            state
                .service_tier
                .or_else(|| normalize_service_tier(self.config.service_tier.as_deref()))
                .unwrap_or_else(|| "default".to_string()),
            if state.reasoning { "ON" } else { "OFF" },
            if state.verbose { "ON" } else { "OFF" },
            state.tool_progress.unwrap_or_else(|| "default".to_string()),
            if state.yolo { "ON" } else { "OFF" },
            state.home.unwrap_or_else(|| "(not set)".to_string()),
            messages.len(),
            running_tasks,
            *self.router.mcp_reload_generation.read().await,
            usage.input_chars,
            usage.output_chars
        )
    }

    /// Dispatch a validated batch of 2+ commands parsed by [`parse_batch_commands`].
    ///
    /// Policy:
    /// - Any `Control` command in the batch → reject entire batch.
    /// - Any `SessionMutation` command in the batch → reject entire batch.
    /// - Mixed `FireAndForget` + `ReadOnly` → reject (ambiguous ordering).
    /// - All `FireAndForget` → parallel spawn, single upfront ack message.
    /// - All `ReadOnly` → sequential execution (each sends its own reply).
    async fn execute_batch_commands(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        commands: Vec<BatchedCommand>,
    ) -> Result<bool, GatewayError> {
        let has_control = commands
            .iter()
            .any(|c| c.class == BatchCommandClass::Control);
        let has_mutation = commands
            .iter()
            .any(|c| c.class == BatchCommandClass::SessionMutation);

        if has_control || has_mutation {
            let bad_names: Vec<String> = commands
                .iter()
                .filter(|c| {
                    matches!(
                        c.class,
                        BatchCommandClass::Control | BatchCommandClass::SessionMutation
                    )
                })
                .map(|c| format!("/{}", c.name))
                .collect();
            let msg = format!(
                "⚠️ Batch rejected: {} cannot be used in a multi-command message — \
                 these commands modify session state or require exclusive control. \
                 Send each command separately.",
                bad_names.join(", ")
            );
            self.send_incoming_reply(incoming, &msg, None).await?;
            return Ok(true);
        }

        let ff_count = commands
            .iter()
            .filter(|c| c.class == BatchCommandClass::FireAndForget)
            .count();
        let ro_count = commands
            .iter()
            .filter(|c| c.class == BatchCommandClass::ReadOnly)
            .count();

        if ff_count > 0 && ro_count > 0 {
            let msg = "⚠️ Mixed batch (fire-and-forget + read-only commands) is not supported. \
                       Send each group in a separate message."
                .to_string();
            self.send_incoming_reply(incoming, &msg, None).await?;
            return Ok(true);
        }

        if ff_count > 0 {
            // ── All FireAndForget: parallel dispatch ────────────────────────
            let legacy_handler = self.router.message_handler.read().await.as_ref().cloned();
            let context_handler = self
                .router
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

            let n = commands.len();
            let mut ack = format!(
                "🔄 Received {} background tasks — starting in parallel:\n",
                n
            );
            let mut dispatches: Vec<(String, Arc<Vec<Message>>)> = Vec::with_capacity(n);

            for (i, cmd) in commands.iter().enumerate() {
                let is_btw = cmd.name == "btw";
                let task_id = Self::python_async_task_id(if is_btw { "btw" } else { "bg" });
                let preview = Self::gateway_command_preview(&cmd.args);
                ack.push_str(&format!("{}. [{}] \"{}\"\n", i + 1, task_id, preview));
                let _ = self
                    .router
                    .background_tasks
                    .submit_with_id(task_id.clone(), cmd.args.clone());

                let messages: Arc<Vec<Message>> = if is_btw {
                    let mut history = self.session.session_manager.get_messages(session_key).await;
                    history.push(Message::user(format!(
                        "[Ephemeral /btw side question. Answer using the conversation \
                         context. No tools available. Be direct and concise.]\n\n{}",
                        cmd.args
                    )));
                    Arc::new(history)
                } else {
                    Arc::new(vec![Message::user(cmd.args.clone())])
                };
                dispatches.push((task_id, messages));
            }

            self.send_incoming_reply(incoming, &ack, None).await?;

            for (task_id, messages) in dispatches {
                let manager = self.router.background_tasks.clone();
                let task_id_inner = task_id;
                let runtime_context = self.build_runtime_context(incoming, session_key).await;
                let ctx_handler = context_handler.clone();
                let leg_handler = legacy_handler.clone();
                tokio::spawn(async move {
                    let legacy_messages = Arc::clone(&messages);
                    let result = if let Some(handler) = ctx_handler {
                        handler(messages, runtime_context).await
                    } else if let Some(handler) = leg_handler {
                        handler(legacy_messages).await
                    } else {
                        Err(GatewayError::Platform(
                            "No message handler configured".into(),
                        ))
                    };
                    match result {
                        Ok(r) => manager.complete(&task_id_inner, r),
                        Err(e) => manager.fail(&task_id_inner, e.to_string()),
                    }
                });
            }
            return Ok(true);
        }

        // ── All ReadOnly: sequential dispatch ──────────────────────────────
        for cmd in commands {
            let full_input = if cmd.args.is_empty() {
                format!("/{}", cmd.name)
            } else {
                format!("/{} {}", cmd.name, cmd.args)
            };
            let result = handle_command(&full_input);
            self.apply_command_result(incoming, session_key, result)
                .await?;
        }
        Ok(true)
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
            let tasks = self.router.background_tasks.list_tasks();
            let summary = if tasks.is_empty() {
                "No background tasks.".to_string()
            } else {
                let mut out = String::from("🧵 Background tasks:\n");
                for (id, status, task_prompt) in tasks {
                    out.push_str(&format!("- {} [{:?}] {}\n", id, status, task_prompt));
                }
                out
            };
            self.send_incoming_reply(incoming, &summary, None).await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("cancel ").map(str::trim) {
            let ok = self.router.background_tasks.cancel(task_id);
            let msg = if ok {
                format!("Cancelled background task {}", task_id)
            } else {
                format!("Task {} was not running or not found", task_id)
            };
            self.send_incoming_reply(incoming, &msg, None).await?;
            return Ok(true);
        }
        if let Some(task_id) = trimmed.strip_prefix("status ").map(str::trim) {
            let msg = match self.router.background_tasks.get_status(task_id) {
                Some(TaskStatus::Running) => format!("Task {} is running", task_id),
                Some(TaskStatus::Completed) => {
                    let result = self
                        .router
                        .background_tasks
                        .get_result(task_id)
                        .unwrap_or_default();
                    format!("Task {} completed.\n{}", task_id, result)
                }
                Some(TaskStatus::Failed(err)) => format!("Task {} failed: {}", task_id, err),
                Some(TaskStatus::Cancelled) => format!("Task {} was cancelled", task_id),
                None => format!("Task {} not found", task_id),
            };
            self.send_incoming_reply(incoming, &msg, None).await?;
            return Ok(true);
        }

        let task_id = if isolated_context {
            Self::python_async_task_id("btw")
        } else {
            Self::python_async_task_id("bg")
        };
        self.router
            .background_tasks
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
        self.send_incoming_reply(incoming, &ack, None).await?;

        let legacy_handler = self.router.message_handler.read().await.as_ref().cloned();
        let context_handler = self
            .router
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
        let manager = self.router.background_tasks.clone();
        let task_id_for_task = task_id.clone();
        // Python `GatewayRunner._run_background_task`: only `user_message=prompt` (fresh session).
        // Python `_run_btw_task`: `conversation_history` snapshot + ephemeral user turn (no tools).
        let original_messages: Arc<Vec<Message>> = if isolated_context {
            let mut history = self.session.session_manager.get_messages(session_key).await;
            let btw_user = format!(
                "[Ephemeral /btw side question. Answer using the conversation \
                 context. No tools available. Be direct and concise.]\n\n{}",
                trimmed
            );
            history.push(Message::user(btw_user));
            Arc::new(history)
        } else {
            Arc::new(vec![Message::user(trimmed)])
        };
        let legacy_messages = Arc::clone(&original_messages);
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

    /// Deliver `MEDIA:<path>` attachments from an agent response.
    ///
    /// Returns `(failure summaries, cleaned text, successfully delivered count)`.
    pub async fn deliver_response_attachments(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
    ) -> (Vec<String>, String, usize) {
        let (media_files, cleaned) = extract_media(text);
        let mut failures = Vec::new();
        let mut delivered = 0usize;
        for (media_path, _is_voice) in media_files {
            match resolve_outbound_media_path(&media_path) {
                Ok(resolved) => {
                    let path_str = resolved.to_string_lossy().into_owned();
                    match self.send_file(platform, chat_id, &path_str, None).await {
                        Ok(()) => delivered += 1,
                        Err(err) => {
                            warn!(
                                platform = platform,
                                chat_id = chat_id,
                                path = %path_str,
                                error = %err,
                                "failed to deliver MEDIA attachment from agent response"
                            );
                            let label = std::path::Path::new(&path_str)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(path_str.as_str());
                            failures.push(format!("{label} ({err})"));
                        }
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
                    failures.push(format!("{media_path} ({err})"));
                }
            }
        }
        (failures, cleaned, delivered)
    }

    /// Deliver `MEDIA:` tags, then infer missing attachments only when nothing was sent yet.
    async fn finalize_outbound_attachments(
        &self,
        platform: &str,
        chat_id: &str,
        user_text: &str,
        response: &str,
        session_key: &str,
    ) -> (Vec<String>, String, usize) {
        let (mut attachment_failures, cleaned, media_delivered) = self
            .deliver_response_attachments(platform, chat_id, response)
            .await;
        let mut attachments_delivered =
            media_delivered.max(self.turn_outbound_file_count(session_key));
        if attachments_delivered == 0 {
            let (inferred_failures, inferred_delivered) = self
                .deliver_inferred_attachments(platform, chat_id, user_text)
                .await;
            attachment_failures.extend(inferred_failures);
            attachments_delivered += inferred_delivered;
        }
        (attachment_failures, cleaned, attachments_delivered)
    }

    /// When the user asked for an attachment but the agent omitted `MEDIA:` tags, try to
    /// infer paths from the current user message (filenames and absolute paths only).
    pub async fn deliver_inferred_attachments(
        &self,
        platform: &str,
        chat_id: &str,
        user_text: &str,
    ) -> (Vec<String>, usize) {
        let paths = crate::attachment_inference::infer_attachment_paths(user_text);
        if paths.is_empty() {
            return (Vec::new(), 0);
        }

        let mut failures = Vec::new();
        let mut delivered = 0usize;
        for path in paths {
            let path_str = path.to_string_lossy().into_owned();
            match self.send_file(platform, chat_id, &path_str, None).await {
                Ok(()) => {
                    delivered += 1;
                    info!(
                        platform = platform,
                        chat_id = chat_id,
                        path = %path_str,
                        "gateway inferred attachment delivered"
                    );
                }
                Err(err) => {
                    warn!(
                        platform = platform,
                        chat_id = chat_id,
                        path = %path_str,
                        error = %err,
                        "failed to deliver inferred attachment"
                    );
                    let label = std::path::Path::new(&path_str)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(path_str.as_str());
                    failures.push(format!("{label} ({err})"));
                }
            }
        }
        (failures, delivered)
    }

    fn reply_text_with_attachment_outcome(
        cleaned: String,
        failures: &[String],
        user_text: &str,
        attachments_delivered: usize,
    ) -> String {
        let mut text = Self::reply_text_with_attachment_failures(cleaned, failures);
        if attachments_delivered == 0
            && crate::attachment_inference::user_requests_attachment(user_text)
            && failures.is_empty()
        {
            let hint = "⚠️ 未能找到可发送的附件文件，请说明文件名（例如 AGENTS.md）或完整路径。";
            if text.trim().is_empty() {
                text = hint.to_string();
            } else {
                text = format!("{text}\n\n{hint}");
            }
        }
        text
    }

    fn reply_text_with_attachment_failures(cleaned: String, failures: &[String]) -> String {
        if failures.is_empty() {
            return cleaned;
        }
        let notice = failures
            .iter()
            .map(|f| format!("⚠️ 附件发送失败: {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        if cleaned.trim().is_empty() {
            notice
        } else {
            format!("{cleaned}\n\n{notice}")
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
        let reply_to = incoming
            .interaction_id
            .is_none()
            .then(|| incoming.message_id.as_deref())
            .flatten()
            .filter(|id| !id.is_empty());
        if let Some(adapter) = self.get_adapter(&incoming.platform).await {
            return adapter
                .send_message_in_thread(
                    &incoming.chat_id,
                    text,
                    parse_mode,
                    reply_to,
                    incoming.message_thread_id.as_deref(),
                )
                .await
                .map(|_| ());
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

    /// Send a text message and return the platform message id when available.
    pub async fn send_message_with_id(
        &self,
        platform: &str,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<Option<String>, GatewayError> {
        let adapter = self.get_adapter(platform).await.ok_or_else(|| {
            GatewayError::Platform(format!("No adapter registered for platform: {}", platform))
        })?;
        adapter
            .send_message_with_id(chat_id, text, parse_mode)
            .await
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
        adapter.send_file(chat_id, file_path, caption).await?;
        self.record_turn_outbound_file(platform, chat_id, file_path);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session.session_manager
    }

    /// Get a reference to the stream manager.
    pub fn stream_manager(&self) -> &Arc<StreamManager> {
        &self.delivery.stream_manager
    }

    /// Get a reference to the gateway config.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// List the names of all registered adapters.
    pub async fn adapter_names(&self) -> Vec<String> {
        self.router.adapters.read().await.keys().cloned().collect()
    }

    /// Periodically expires inactive sessions.
    pub async fn session_expiry_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(30)));
        loop {
            ticker.tick().await;
            let expired = self.session.session_manager.expire_idle_sessions().await;
            let expired_count = expired.len();
            for snapshot in expired {
                self.teardown_session_snapshot(snapshot, "idle_expiry")
                    .await;
            }
            if expired_count > 0 {
                tracing::info!(expired = expired_count, "Expired idle sessions");
            }
        }
    }

    /// Monitors adapter health and attempts reconnect through stop/start.
    pub async fn platform_reconnect_watcher(&self, interval_secs: u64) {
        let mut ticker =
            tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(20)));
        loop {
            ticker.tick().await;
            let snapshot = self.router.adapters.read().await.clone();
            for (name, adapter) in snapshot {
                if !adapter.is_running() {
                    tracing::warn!(platform = %name, "Adapter appears offline, reconnecting");
                    let _ = adapter.start().await;
                }
            }
        }
    }

    /// Prepare the user turn via the injected agent preparer (vision routing, etc.).
    pub(crate) async fn prepare_inbound_user_message(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Message {
        let event = Self::incoming_to_event(incoming);
        let (provider, model) = {
            let states = self.session.runtime_state.read().await;
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
        let preparer = self.extensions.inbound_preparer.read().await.clone();
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
        if !*self.extensions.stt_enabled.read().await {
            return None;
        }
        let voice = self.extensions.voice_manager.read().await.clone()?;
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
                    transcripts
                        .push("[The user sent a voice message but transcription failed.]".into());
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
        let dm = self.router.dm_manager.read().await;
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

    #[test]
    fn platform_wants_processing_ack_for_chat_platforms() {
        assert!(platform_wants_processing_ack("weixin"));
        assert!(platform_wants_processing_ack("wecom"));
        assert!(platform_wants_processing_ack("feishu"));
        assert!(!platform_wants_processing_ack("discord"));
    }

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
        typing: Arc<Mutex<Vec<String>>>,
        typing_stops: Arc<Mutex<Vec<String>>>,
        platform: &'static str,
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
            if let Some(pos) = msgs.iter().rposition(|(c, t)| c == chat_id && t == "...") {
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

        async fn trigger_typing(&self, chat_id: &str) -> Result<(), GatewayError> {
            self.typing.lock().unwrap().push(chat_id.to_string());
            Ok(())
        }

        async fn stop_typing(&self, chat_id: &str) -> Result<(), GatewayError> {
            self.typing_stops.lock().unwrap().push(chat_id.to_string());
            Ok(())
        }

        fn reactions_enabled(&self) -> bool {
            true
        }

        fn is_running(&self) -> bool {
            true
        }

        fn platform_name(&self) -> &str {
            self.platform
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
            self.chunks
                .lock()
                .unwrap()
                .push((initial_content.unwrap_or_default().to_string(), false));
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            .session
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let _ = gw
            .session
            .session_manager
            .get_or_create_session("test", "chat1", "user1")
            .await;
        gw.session
            .session_manager
            .add_message(&session_key, Message::system("sys"))
            .await;
        for _ in 0..40 {
            gw.session
                .session_manager
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
            role_ids: vec![],
            ..Default::default()
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
            .session
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let _ = gw
            .session
            .session_manager
            .get_or_create_session("test", "chat1", "user1")
            .await;
        gw.session
            .session_manager
            .add_message(&session_key, Message::system("sys"))
            .await;
        for i in 0..40 {
            let message = if i % 2 == 0 {
                Message::user(format!("turn {i} content"))
            } else {
                Message::assistant(format!("turn {i} content"))
            };
            gw.session
                .session_manager
                .add_message(&session_key, message)
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
            role_ids: vec![],
            ..Default::default()
        };

        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
        assert!(reply.contains("Context compressed"));
        assert!(!reply.contains("⚠️"));
        drop(msgs);

        let updated = gw.session.session_manager.get_messages(&session_key).await;
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            .session
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let session_key_2 = gw
            .session
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&yolo_chat2).await.is_ok());

        {
            let states = gw.session.runtime_state.read().await;
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&reset_chat1).await.is_ok());

        let states = gw.session.runtime_state.read().await;
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
            .session
            .session_manager
            .compose_session_key("test", "chat1", "user1");
        let target_key = gw
            .session
            .session_manager
            .compose_session_key("test", "chat2", "user1");

        let _ = gw
            .session
            .session_manager
            .get_or_create_session("test", "chat2", "user1")
            .await;
        gw.session
            .session_manager
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&yolo_chat1).await.is_ok());
        {
            let states = gw.session.runtime_state.read().await;
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&switch).await.is_ok());

        let states = gw.session.runtime_state.read().await;
        assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(false));
    }

    #[tokio::test]
    async fn gateway_slack_reaction_lifecycle_success() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
            typing: Arc::new(Mutex::new(Vec::new())),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "slack",
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
            role_ids: vec![],
            ..Default::default()
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
            typing: Arc::new(Mutex::new(Vec::new())),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "slack",
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
            role_ids: vec![],
            ..Default::default()
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
            typing: Arc::new(Mutex::new(Vec::new())),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "slack",
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());
        assert!(reactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gateway_discord_reaction_lifecycle_success() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let typing = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
            typing: typing.clone(),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "discord",
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("discord", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "discord".into(),
            chat_id: "dm-ch".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-1".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let got = reactions.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                "add:dm-ch:msg-1:eyes".to_string(),
                "remove:dm-ch:msg-1:eyes".to_string(),
                "add:dm-ch:msg-1:white_check_mark".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_discord_reaction_lifecycle_failure_sets_error_reaction() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: sent.clone(),
            reactions: reactions.clone(),
            typing: Arc::new(Mutex::new(Vec::new())),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "discord",
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("discord", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Err(GatewayError::Platform("boom".to_string())) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "discord".into(),
            chat_id: "dm-ch".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-2".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_err());

        let got = reactions.lock().unwrap().clone();
        assert_eq!(
            got,
            vec![
                "add:dm-ch:msg-2:eyes".to_string(),
                "remove:dm-ch:msg-2:eyes".to_string(),
                "add:dm-ch:msg-2:x".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn gateway_discord_reactions_skip_slash_and_plain_guild() {
        let reactions = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: Arc::new(Mutex::new(Vec::new())),
            reactions: reactions.clone(),
            typing: Arc::new(Mutex::new(Vec::new())),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "discord",
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("discord", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let slash = IncomingMessage {
            platform: "discord".into(),
            chat_id: "ch1".into(),
            user_id: "user1".into(),
            text: "/status".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-slash".into()),
            is_dm: false,
            interaction_id: Some("ix-1".into()),
            interaction_token: Some("tok".into()),
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&slash).await.is_ok());
        assert!(reactions.lock().unwrap().is_empty());

        reactions.lock().unwrap().clear();

        let guild_plain = IncomingMessage {
            platform: "discord".into(),
            chat_id: "guild-ch".into(),
            user_id: "user1".into(),
            text: "general chatter".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-guild".into()),
            is_dm: false,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&guild_plain).await.is_ok());
        assert!(reactions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn gateway_discord_spawns_trigger_typing_on_route() {
        let typing = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: Arc::new(Mutex::new(Vec::new())),
            reactions: Arc::new(Mutex::new(Vec::new())),
            typing: typing.clone(),
            typing_stops: Arc::new(Mutex::new(Vec::new())),
            platform: "discord",
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("discord", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "discord".into(),
            chat_id: "dm-typing".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-t".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert_eq!(typing.lock().unwrap().as_slice(), &["dm-typing"]);
    }

    #[tokio::test]
    async fn gateway_weixin_spawns_typing_on_route() {
        let typing = Arc::new(Mutex::new(Vec::new()));
        let typing_stops = Arc::new(Mutex::new(Vec::new()));
        let adapter = Arc::new(ReactionTestAdapter {
            messages: Arc::new(Mutex::new(Vec::new())),
            reactions: Arc::new(Mutex::new(Vec::new())),
            typing: typing.clone(),
            typing_stops: typing_stops.clone(),
            platform: "weixin",
        });

        let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user1");
        let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
        gw.register_adapter("weixin", adapter).await;
        gw.set_message_handler(Arc::new(|_messages| {
            Box::pin(async { Ok("done".to_string()) })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "weixin".into(),
            chat_id: "wx-user-1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some("msg-wx".into()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());
        assert!(!typing.lock().unwrap().is_empty());
        assert_eq!(typing_stops.lock().unwrap().as_slice(), &["wx-user-1"]);
        assert!(
            typing.lock().unwrap().iter().all(|id| id == "wx-user-1"),
            "typing refresh should target the same chat_id"
        );
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
                role_ids: vec![],
                ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
    async fn gateway_feishu_streaming_sends_final_text_without_anchor_edit() {
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
        gw.register_adapter("feishu", adapter).await;

        gw.set_streaming_handler(Arc::new(|_messages, on_chunk| {
            Box::pin(async move {
                on_chunk("你".to_string());
                on_chunk("好".to_string());
                Ok("你好".to_string())
            })
        }))
        .await;

        let incoming = IncomingMessage {
            platform: "feishu".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: "hello".into(),
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        let msgs = sent.lock().unwrap();
        let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
        assert_eq!(ordered, vec!["你好".to_string()]);
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
            role_ids: vec![],
            ..Default::default()
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
    async fn gateway_native_streaming_never_send_message_when_stream_succeeds() {
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

        let full = "好的，我来查一下。\n\n这是完整的长回答。";
        let full_for_handler = full.to_string();
        gw.set_streaming_handler(Arc::new(move |_messages, on_chunk| {
            let full = full_for_handler.clone();
            Box::pin(async move {
                on_chunk("好的，我来查一下。".to_string());
                Ok(full)
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&incoming).await.is_ok());

        assert!(
            sent.lock().unwrap().is_empty(),
            "successful native stream must not trigger send_message"
        );

        unsafe { std::env::remove_var("HERMES_WECOM_STREAM_FLUSH_INTERVAL_MS") };
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
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
            role_ids: vec![],
            ..Default::default()
        };
        assert!(gw.route_message(&reset).await.is_ok());

        let events = hook_seen.lock().unwrap();
        let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
        assert!(names.contains(&"session:end".to_string()));
        assert!(names.contains(&"session:reset".to_string()));
    }

    #[test]
    fn turn_outbound_tracker_records_and_counts_by_platform_chat() {
        let gw = Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            DmManager::with_pair_behavior(),
            GatewayConfig::default(),
        );
        gw.begin_turn_outbound_tracking("weixin:chat1", "weixin", "chat1");
        let cargo_toml = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        gw.record_turn_outbound_file("weixin", "chat1", cargo_toml);
        assert_eq!(gw.turn_outbound_file_count("weixin:chat1"), 1);
        assert_eq!(gw.turn_outbound_file_count("weixin:other"), 0);
        gw.clear_turn_outbound_tracking("weixin:chat1");
        assert_eq!(gw.turn_outbound_file_count("weixin:chat1"), 0);
    }

    #[test]
    fn turn_outbound_tracker_skips_unrelated_platform_chat() {
        let gw = Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            DmManager::with_pair_behavior(),
            GatewayConfig::default(),
        );
        gw.begin_turn_outbound_tracking("weixin:chat1", "weixin", "chat1");
        let cargo_toml = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        gw.record_turn_outbound_file("telegram", "999", cargo_toml);
        assert_eq!(gw.turn_outbound_file_count("weixin:chat1"), 0);
        gw.clear_turn_outbound_tracking("weixin:chat1");
    }
}
