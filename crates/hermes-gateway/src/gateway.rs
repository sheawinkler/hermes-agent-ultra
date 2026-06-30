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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use hermes_config::{
    load_user_config_file, normalize_service_tier, save_config_yaml, DisplayConfig,
    QuickCommandConfig,
};
use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter, SendMessageOptions};
use hermes_core::types::{Message, MessageRole};
use hermes_intelligence::{
    build_model_switch_preflight_warning as format_model_switch_preflight_warning,
    estimate_messages_tokens_rough,
};
use hermes_tools::skill_commands::{
    build_skill_reload_system_note, installed_skill_slash_command_snapshot,
    render_skill_slash_command_snapshot, resolve_installed_skill_slash_command,
    SkillCommandResolverConfig,
};

use crate::background::{BackgroundTaskManager, TaskStatus};
use crate::commands::{handle_command, GatewayCommandResult, ModelSwitchRequest, ModelSwitchScope};
use crate::delivery::prepare_platform_message_for_adapter;
use crate::dm::{DmDecision, DmManager};
use crate::hooks::{HookEvent, HookRegistry};
use crate::media::validate_media_delivery_path;
use crate::platforms::helpers::{extract_inline_images, extract_media_markers};
use crate::session::{Session, SessionManager};
use crate::session_control::{
    ActiveSessionControl, BusyInputMode, BusySessionCoordinator, MessageEvent, MessageType,
    ProcessingOutcome, SessionSource,
};
use crate::stream::{StreamConfig, StreamManager};

const DEFAULT_MESSAGE_DEDUP_CAPACITY: usize = 4096;

// ---------------------------------------------------------------------------
// GatewayConfig
// ---------------------------------------------------------------------------

/// Configuration for the Gateway orchestrator.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayConfig {
    /// Default model used when a gateway session has no explicit `/model` override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Persist plain `/model <name>` switches by default unless `--session` is used.
    #[serde(default = "default_true")]
    pub model_switch_persist_by_default: bool,

    /// Optional config.yaml path for durable gateway `/model` writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_switch_config_path: Option<String>,

    /// Warn when the current transcript is likely to preflight-compress after a switch.
    #[serde(default = "default_true")]
    pub model_switch_preflight_warning: bool,

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

    /// Display command/runtime settings.
    #[serde(default)]
    pub display: DisplayConfig,

    /// Default provider service tier for gateway agent turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    /// User-defined slash commands that bypass the agent loop.
    #[serde(default)]
    pub quick_commands: BTreeMap<String, QuickCommandConfig>,

    /// Whether this gateway process owns Kanban dispatch/notifier duties.
    #[serde(default = "default_true")]
    pub kanban_dispatch_in_gateway: bool,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            model: None,
            model_switch_persist_by_default: true,
            model_switch_config_path: None,
            model_switch_preflight_warning: true,
            ssrf_protection: true,
            media_cache_dir: None,
            media_cache_max_bytes: 0,
            streaming_enabled: false,
            streaming: StreamConfig::default(),
            display: DisplayConfig::default(),
            service_tier: None,
            quick_commands: BTreeMap::new(),
            kanban_dispatch_in_gateway: true,
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
    /// Platform-specific message ID (for reply threading).
    pub message_id: Option<String>,
    /// Platform-native thread/topic root for replies and progress updates.
    pub thread_id: Option<String>,
    /// Whether this is a DM (direct message) or group message.
    pub is_dm: bool,
}

/// Sender metadata carried by platform adapters when routing a message.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IncomingSender {
    /// True when the source user is a bot/webhook account.
    pub is_bot: bool,
}

impl IncomingSender {
    pub fn human() -> Self {
        Self { is_bot: false }
    }

    pub fn bot() -> Self {
        Self { is_bot: true }
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
    pub platform: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub user_id: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub profile: Option<String>,
    pub branch: Option<String>,
    pub personality: Option<String>,
    pub home: Option<String>,
    pub service_tier: Option<String>,
    pub tool_progress: Option<String>,
    pub verbose: bool,
    pub yolo: bool,
    pub reasoning: bool,
    pub mcp_reload_generation: u64,
    /// Registration handle for attaching live interrupt/steer controls to a busy session.
    pub busy_control: Option<BusyControlRegistration>,
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

#[derive(Clone)]
pub struct BusyControlRegistration {
    session_key: String,
    coordinator: Arc<RwLock<BusySessionCoordinator>>,
}

impl std::fmt::Debug for BusyControlRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusyControlRegistration")
            .field("session_key", &self.session_key)
            .finish_non_exhaustive()
    }
}

impl BusyControlRegistration {
    fn new(
        session_key: impl Into<String>,
        coordinator: Arc<RwLock<BusySessionCoordinator>>,
    ) -> Self {
        Self {
            session_key: session_key.into(),
            coordinator,
        }
    }

    pub async fn attach(&self, control: Arc<dyn ActiveSessionControl>) -> bool {
        self.coordinator
            .write()
            .await
            .attach_control(&self.session_key, control)
    }
}

#[derive(Debug, Clone, Default)]
struct UsageStats {
    user_messages: u64,
    assistant_messages: u64,
    input_chars: u64,
    output_chars: u64,
    last_updated_at: Option<DateTime<Utc>>,
}

enum SlashCommandOutcome {
    Handled,
    ForwardToAgent { message: String },
}

#[derive(Debug, Clone, Default)]
struct CompressionOutcome {
    removed_messages: usize,
    summary_warning: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct SessionRuntimeState {
    model: Option<String>,
    provider: Option<String>,
    profile: Option<String>,
    branch: Option<String>,
    personality: Option<String>,
    home: Option<String>,
    service_tier: Option<String>,
    tool_progress: Option<String>,
    /// Optional usage budget (same units as `/budget` input; gateway displays as-is).
    budget: Option<f64>,
    verbose: bool,
    yolo: bool,
    reasoning: bool,
    pending_system_notes: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GatewayProfileOverlay {
    name: String,
    path: PathBuf,
    model: Option<String>,
    provider: Option<String>,
    personality: Option<String>,
    home: Option<String>,
}

#[derive(Debug)]
struct MessageDeduplicator {
    seen: HashSet<String>,
    order: VecDeque<String>,
    capacity: usize,
}

impl Default for MessageDeduplicator {
    fn default() -> Self {
        Self {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: DEFAULT_MESSAGE_DEDUP_CAPACITY,
        }
    }
}

impl MessageDeduplicator {
    fn seen_or_record(&mut self, key: String) -> bool {
        if self.seen.contains(&key) {
            return true;
        }

        while self.seen.len() >= self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.seen.remove(&oldest);
        }

        self.seen.insert(key.clone());
        self.order.push_back(key);
        false
    }
}

include!("gateway/profile_overlay.rs");

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
    /// Process-wide gateway default model updated by persistent `/model` switches.
    default_model: RwLock<Option<String>>,
    tool_progress_modes: RwLock<BTreeMap<String, String>>,
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
    /// Bounded duplicate guard for platform redeliveries/restarts.
    message_deduplicator: RwLock<MessageDeduplicator>,
    /// Active gateway sessions plus queued/steered busy follow-ups.
    busy_sessions: Arc<RwLock<BusySessionCoordinator>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum GroupAccessMode {
    #[default]
    Open,
    Allowlist,
    Disabled,
}

#[derive(Debug, Clone, Default)]
pub struct PlatformAccessPolicy {
    pub allowed_users: HashSet<String>,
    pub admin_users: HashSet<String>,
    pub allowed_channels: HashSet<String>,
    pub authorized_group_chats: HashSet<String>,
    pub ignored_channels: HashSet<String>,
    pub group_mode: GroupAccessMode,
    pub slash_requires_allowlist: bool,
    pub bot_sender_bypasses_allowlist: bool,
    pub reactions_enabled: Option<bool>,
}

include!("gateway/platform_access_policy.rs");

#[derive(Debug, Clone, Copy)]
struct ReactionLifecyclePlan {
    start: &'static str,
    success: &'static str,
    error: &'static str,
}

include!("gateway/lifecycle_methods.rs");
include!("gateway/routing_methods.rs");
include!("gateway/slash_command_methods.rs");

include!("gateway/routing_send.rs");

#[cfg(test)]
mod tests;
