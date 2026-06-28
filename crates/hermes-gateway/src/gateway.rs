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

#[derive(Debug, Clone)]
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

impl Default for SessionRuntimeState {
    fn default() -> Self {
        Self {
            model: None,
            provider: None,
            profile: None,
            branch: None,
            personality: None,
            home: None,
            service_tier: None,
            tool_progress: None,
            budget: None,
            verbose: false,
            yolo: false,
            reasoning: false,
            pending_system_notes: Vec::new(),
        }
    }
}

fn gateway_profiles_dir() -> PathBuf {
    hermes_config::hermes_home().join("profiles")
}

fn load_gateway_profile_aliases(profiles_dir: &Path) -> BTreeMap<String, String> {
    let path = profiles_dir.join("aliases.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return BTreeMap::new();
    };
    serde_json::from_str::<BTreeMap<String, String>>(&raw).unwrap_or_default()
}

fn resolve_gateway_profile_name(
    requested: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<String, String> {
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err("profile name cannot be empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(format!(
            "invalid profile name '{}': path separators are not allowed",
            trimmed
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(format!(
            "invalid profile name '{}': use letters, numbers, '-', '_' or '.'",
            trimmed
        ));
    }
    Ok(aliases
        .get(trimmed)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
        .unwrap_or(trimmed)
        .to_string())
}

fn resolve_gateway_profile_path(profiles_dir: &Path, name: &str) -> Option<PathBuf> {
    let yaml = profiles_dir.join(format!("{name}.yaml"));
    if yaml.exists() {
        return Some(yaml);
    }
    let yml = profiles_dir.join(format!("{name}.yml"));
    yml.exists().then_some(yml)
}

fn yaml_string(map: &serde_yaml::Mapping, key: &str) -> Option<String> {
    map.get(&serde_yaml::Value::String(key.to_string()))
        .and_then(serde_yaml::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn load_gateway_profile_overlay(requested: &str) -> Result<GatewayProfileOverlay, String> {
    let profiles_dir = gateway_profiles_dir();
    let aliases = load_gateway_profile_aliases(&profiles_dir);
    let name = resolve_gateway_profile_name(requested, &aliases)?;
    let path = resolve_gateway_profile_path(&profiles_dir, &name).ok_or_else(|| {
        format!(
            "profile '{}' not found under {}",
            name,
            profiles_dir.display()
        )
    })?;
    let raw =
        std::fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|err| format!("parse {}: {err}", path.display()))?;
    let Some(map) = value.as_mapping() else {
        return Err(format!("profile '{}' must be a YAML mapping", name));
    };

    let model = yaml_string(map, "model");
    let provider = yaml_string(map, "provider").or_else(|| {
        model
            .as_deref()
            .and_then(|value| value.split_once(':').map(|(provider, _)| provider.trim()))
            .filter(|provider| !provider.is_empty())
            .map(str::to_string)
    });
    let personality = yaml_string(map, "personality");
    let home = yaml_string(map, "home_dir").or_else(|| yaml_string(map, "home"));

    Ok(GatewayProfileOverlay {
        name,
        path,
        model,
        provider,
        personality,
        home,
    })
}

fn apply_gateway_profile_overlay(state: &mut SessionRuntimeState, overlay: &GatewayProfileOverlay) {
    state.profile = Some(overlay.name.clone());
    if let Some(model) = &overlay.model {
        state.model = Some(model.clone());
    }
    if let Some(provider) = &overlay.provider {
        state.provider = Some(provider.clone());
    }
    if let Some(personality) = &overlay.personality {
        state.personality = Some(personality.clone());
    }
    if let Some(home) = &overlay.home {
        state.home = Some(home.clone());
    }
}

fn render_profile_overlay_reply(requested: &str, overlay: &GatewayProfileOverlay) -> String {
    let mut applied = Vec::new();
    if let Some(model) = &overlay.model {
        applied.push(format!("model={model}"));
    }
    if let Some(provider) = &overlay.provider {
        applied.push(format!("provider={provider}"));
    }
    if let Some(personality) = &overlay.personality {
        applied.push(format!("personality={personality}"));
    }
    if let Some(home) = &overlay.home {
        applied.push(format!("home={home}"));
    }
    let applied = if applied.is_empty() {
        "metadata only".to_string()
    } else {
        applied.join(", ")
    };
    format!(
        "👤 Profile switched to: {} (requested '{}'; {}; {})",
        overlay.name,
        requested.trim(),
        applied,
        overlay.path.display()
    )
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
            if allowed == "*" {
                return true;
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

    fn channel_matches_any(channel_id: &str, set: &HashSet<String>) -> bool {
        let candidate = channel_id.trim();
        if candidate.is_empty() {
            return false;
        }
        set.iter().any(|entry| {
            let allowed = entry.trim();
            allowed == "*" || allowed.eq_ignore_ascii_case(candidate)
        })
    }

    fn is_channel_allowed(&self, channel_id: &str) -> bool {
        self.allowed_channels.is_empty()
            || Self::channel_matches_any(channel_id, &self.allowed_channels)
    }

    fn is_channel_ignored(&self, channel_id: &str) -> bool {
        Self::channel_matches_any(channel_id, &self.ignored_channels)
    }

    pub fn is_group_chat_authorized(&self, channel_id: &str) -> bool {
        Self::channel_matches_any(channel_id, &self.authorized_group_chats)
    }

    fn allows_sender_without_user_allowlist(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> bool {
        incoming.platform.eq_ignore_ascii_case("discord")
            && sender.is_bot
            && self.bot_sender_bypasses_allowlist
    }
}

#[derive(Debug, Clone, Copy)]
struct ReactionLifecyclePlan {
    start: &'static str,
    success: &'static str,
    error: &'static str,
}

impl Gateway {
    /// Create a new `Gateway` with the given session manager and config.
    pub fn new(
        session_manager: Arc<SessionManager>,
        dm_manager: DmManager,
        config: GatewayConfig,
    ) -> Self {
        let stream_manager = Arc::new(StreamManager::new(config.streaming.clone()));
        let default_model = config.model.clone();

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
            default_model: RwLock::new(default_model),
            tool_progress_modes: RwLock::new(BTreeMap::new()),
            usage_stats: RwLock::new(HashMap::new()),
            background_tasks: Arc::new(BackgroundTaskManager::new(8)),
            mcp_reload_generation: RwLock::new(0),
            hook_registry: RwLock::new(None),
            platform_access_policies: RwLock::new(HashMap::new()),
            message_deduplicator: RwLock::new(MessageDeduplicator::default()),
            busy_sessions: Arc::new(RwLock::new(BusySessionCoordinator::default())),
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

    /// Effective model for a composed platform/chat/user session, including
    /// per-session overrides and process-wide `/model --global` state.
    pub async fn effective_model_for_session(
        &self,
        platform: &str,
        chat_id: &str,
        user_id: &str,
    ) -> Option<String> {
        let key = self
            .session_manager
            .compose_session_key(platform, chat_id, user_id);
        self.effective_session_model(&key).await
    }

    async fn clear_session_boundary_security_state(&self, session_key: &str) {
        if session_key.is_empty() {
            return;
        }
        let mut states = self.runtime_state.write().await;
        if let Some(state) = states.get_mut(session_key) {
            state.yolo = false;
        }
        hermes_tools::approval::clear_session(session_key);
    }

    fn reaction_lifecycle_plan(
        incoming: &IncomingMessage,
        access_policy: Option<&PlatformAccessPolicy>,
    ) -> Option<ReactionLifecyclePlan> {
        if incoming.text.trim_start().starts_with('/') {
            return None;
        }
        incoming
            .message_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        if matches!(
            access_policy.and_then(|policy| policy.reactions_enabled),
            Some(false)
        ) {
            return None;
        }
        if !(incoming.is_dm || incoming.text.contains("<@")) {
            return None;
        }

        if incoming.platform.eq_ignore_ascii_case("slack") {
            return Some(ReactionLifecyclePlan {
                start: "eyes",
                success: "white_check_mark",
                error: "x",
            });
        }
        if incoming.platform.eq_ignore_ascii_case("discord") {
            return Some(ReactionLifecyclePlan {
                start: "👀",
                success: "✅",
                error: "❌",
            });
        }
        if incoming.platform.eq_ignore_ascii_case("telegram")
            && matches!(
                access_policy.and_then(|policy| policy.reactions_enabled),
                Some(true)
            )
        {
            return Some(ReactionLifecyclePlan {
                start: "👀",
                success: "👍",
                error: "👎",
            });
        }
        None
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

    fn session_lifecycle_context(
        session_key: &str,
        session: &Session,
        reason: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "platform": session.platform,
            "chat_id": session.chat_id,
            "user_id": session.user_id,
            "session_key": session_key,
            "session_id": session.id,
            "reason": reason,
        })
    }

    async fn emit_session_finalize(&self, session_key: &str, session: &Session, reason: &str) {
        self.emit_hook_event(
            "on_session_finalize",
            Self::session_lifecycle_context(session_key, session, reason),
        )
        .await;
    }

    async fn emit_session_reset_lifecycle(
        &self,
        session_key: &str,
        session: &Session,
        reason: &str,
    ) {
        self.emit_hook_event(
            "on_session_reset",
            Self::session_lifecycle_context(session_key, session, reason),
        )
        .await;
    }

    async fn finalize_active_sessions(&self, reason: &str) -> usize {
        let sessions = self.session_manager.all_sessions().await;
        for (session_key, session) in &sessions {
            self.emit_session_finalize(session_key, session, reason)
                .await;
        }
        sessions.len()
    }

    fn busy_input_mode(&self) -> BusyInputMode {
        match self.config.display.normalized_busy_input_mode() {
            "queue" => BusyInputMode::Queue,
            "steer" => BusyInputMode::Steer,
            _ => BusyInputMode::Interrupt,
        }
    }

    fn incoming_to_busy_event(incoming: &IncomingMessage, text: impl Into<String>) -> MessageEvent {
        let mut source = SessionSource::new(
            &incoming.platform,
            &incoming.chat_id,
            if incoming.is_dm { "dm" } else { "group" },
        )
        .with_user(&incoming.user_id);
        if let Some(thread_id) = incoming
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            source = source.with_thread(thread_id);
        }
        let mut event = MessageEvent::text(text, source);
        event.message_id = incoming.message_id.clone();
        event.message_type = MessageType::Text;
        event
    }

    fn busy_event_to_incoming(event: MessageEvent) -> IncomingMessage {
        IncomingMessage {
            platform: event.source.platform,
            chat_id: event.source.chat_id,
            user_id: event
                .source
                .user_id
                .unwrap_or_else(|| "unknown".to_string()),
            text: event.text,
            message_id: event.message_id,
            thread_id: event.source.thread_id,
            is_dm: event.source.chat_type.eq_ignore_ascii_case("dm"),
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
        let finalized = self.finalize_active_sessions("shutdown").await;
        if finalized > 0 {
            info!(
                finalized,
                "Finalized active gateway sessions before shutdown"
            );
        }
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
        self.route_message_from_sender(incoming, IncomingSender::human())
            .await
    }

    /// Route an incoming message with platform-provided sender metadata.
    pub async fn route_message_from_sender(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<(), GatewayError> {
        let mut current = incoming.clone();
        for drain_depth in 0..32 {
            match self
                .route_message_once_from_sender(&current, sender)
                .await?
            {
                Some(next) => {
                    current = next;
                }
                None => return Ok(()),
            }
            if drain_depth == 31 {
                warn!(
                    platform = current.platform,
                    chat_id = current.chat_id,
                    "busy-session drain depth reached safety cap"
                );
            }
        }
        Ok(())
    }

    async fn route_message_once_from_sender(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<Option<IncomingMessage>, GatewayError> {
        let access_policy = self.platform_access_policy(&incoming.platform).await;
        let is_slash_command = incoming.text.trim_start().starts_with('/');
        if let Some(policy) = access_policy.as_ref() {
            let bypasses_user_allowlist =
                policy.allows_sender_without_user_allowlist(incoming, sender);
            if !incoming.is_dm {
                if policy.is_channel_ignored(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel is ignored by platform policy"
                    );
                    return Ok(None);
                }
                if !policy.is_channel_allowed(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel not in platform allowlist"
                    );
                    return Ok(None);
                }
                match policy.group_mode {
                    GroupAccessMode::Disabled => {
                        debug!(
                            platform = incoming.platform,
                            user_id = incoming.user_id,
                            "Group traffic denied by platform policy"
                        );
                        return Ok(None);
                    }
                    GroupAccessMode::Allowlist => {
                        if !bypasses_user_allowlist
                            && !policy.is_user_allowed(&incoming.user_id)
                            && !policy.is_group_chat_authorized(&incoming.chat_id)
                        {
                            debug!(
                                platform = incoming.platform,
                                user_id = incoming.user_id,
                                "Group message denied: user not in allowlist"
                            );
                            return Ok(None);
                        }
                    }
                    GroupAccessMode::Open => {}
                }
            }
            if is_slash_command
                && policy.slash_requires_allowlist
                && policy.has_allowlist()
                && !bypasses_user_allowlist
                && !policy.is_user_allowed(&incoming.user_id)
            {
                debug!(
                    platform = incoming.platform,
                    user_id = incoming.user_id,
                    "Slash command denied: user not in platform allowlist"
                );
                return Ok(None);
            }
        }

        // 1. Check DM authorization if this is a direct message
        if incoming.is_dm {
            let dm_manager = self.dm_manager.read().await;
            let decision = dm_manager
                .handle_dm(&incoming.user_id, &incoming.platform)
                .await;

            match decision {
                DmDecision::Allow => {
                    // Proceed
                }
                DmDecision::Pair { message } => {
                    // Send pairing message and return
                    if let Some(msg) = message {
                        self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                            .await?;
                    }
                    return Ok(None);
                }
                DmDecision::Deny => {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied for unauthorized user"
                    );
                    return Ok(None);
                }
            }
        }

        if self.should_suppress_duplicate(incoming).await {
            debug!(
                platform = incoming.platform,
                chat_id = incoming.chat_id,
                message_id = incoming.message_id.as_deref().unwrap_or_default(),
                "Duplicate platform message redelivery suppressed"
            );
            return Ok(None);
        }

        // 2. Get or create session
        let session_key = self.session_manager.compose_session_key(
            &incoming.platform,
            &incoming.chat_id,
            &incoming.user_id,
        );
        let existing_session = self.session_manager.get_session(&session_key).await;
        let session = self
            .session_manager
            .get_or_create_session(&incoming.platform, &incoming.chat_id, &incoming.user_id)
            .await;
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

        let mut agent_text_override: Option<String> = None;

        if !is_slash_command {
            let decision = {
                let mut busy = self.busy_sessions.write().await;
                busy.handle_busy_message(
                    &session_key,
                    Self::incoming_to_busy_event(incoming, incoming.text.clone()),
                    self.busy_input_mode(),
                )
            };
            if decision.handled {
                if self.config.display.busy_ack_enabled() {
                    if let Some(ack) = decision.ack {
                        self.send_message_threaded(
                            &incoming.platform,
                            &incoming.chat_id,
                            &ack,
                            None,
                            Self::reply_thread_id(incoming),
                        )
                        .await?;
                    }
                }
                return Ok(None);
            }
        }

        // Slash commands are executed directly by the gateway command runtime.
        // Installed skill commands are the exception: after built-ins and quick
        // commands decline them, they are converted into a normal agent turn
        // containing the resolved SKILL.md content.
        if is_slash_command {
            match self.execute_slash_command(incoming, &session_key).await? {
                SlashCommandOutcome::Handled => return Ok(None),
                SlashCommandOutcome::ForwardToAgent { message } => {
                    agent_text_override = Some(message);
                }
            }
        }

        let reaction_plan = Self::reaction_lifecycle_plan(incoming, access_policy.as_ref());
        let reaction_adapter = if reaction_plan.is_some() {
            self.get_adapter(&incoming.platform).await
        } else {
            None
        };
        if let (Some(adapter), Some(message_id), Some(plan)) = (
            &reaction_adapter,
            incoming.message_id.as_deref(),
            reaction_plan,
        ) {
            if let Err(err) = adapter
                .add_reaction(&incoming.chat_id, message_id, plan.start)
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

        let agent_text = agent_text_override.as_deref().unwrap_or(&incoming.text);
        let enriched_text =
            self.enrich_message_with_transcription(&self.enrich_message_with_vision(agent_text));
        self.maybe_apply_smart_model_routing(&session_key, &enriched_text)
            .await;

        // 3. Add the user message to the session
        self.session_manager
            .add_message(&session_key, Message::user(enriched_text))
            .await;
        self.bump_input_usage(&session_key, agent_text.chars().count())
            .await;

        // 4. Get all session messages for the agent loop
        let messages = self.session_manager.get_messages(&session_key).await;

        // 5. Process through agent loop (streaming or non-streaming)
        {
            let mut busy = self.busy_sessions.write().await;
            busy.mark_active(&session_key, None);
        }
        let processing_result = if self.config.streaming_enabled {
            self.route_streaming(incoming, messages, &session_key).await
        } else {
            self.route_non_streaming(incoming, messages, &session_key)
                .await
        };

        if let (Some(adapter), Some(message_id), Some(plan)) = (
            &reaction_adapter,
            incoming.message_id.as_deref(),
            reaction_plan,
        ) {
            if let Err(err) = adapter
                .remove_reaction(&incoming.chat_id, message_id, plan.start)
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
                plan.success
            } else {
                plan.error
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

        let pending = {
            let mut busy = self.busy_sessions.write().await;
            busy.finish(
                &session_key,
                if processing_result.is_ok() {
                    ProcessingOutcome::Success
                } else {
                    ProcessingOutcome::Failure
                },
            )
            .map(Self::busy_event_to_incoming)
        };
        processing_result?;
        Ok(pending)
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
        let args = Self::normalize_slash_command_arg_dashes(parts.next().unwrap_or_default())
            .trim()
            .to_string();
        (cmd, args)
    }

    fn normalize_slash_command_text(input: &str) -> String {
        let (cmd, args) = Self::split_slash_command(input);
        if args.is_empty() {
            cmd
        } else {
            format!("{cmd} {args}")
        }
    }

    fn normalize_slash_command_arg_dashes(args: &str) -> String {
        let mut normalized = args.to_string();
        for dash in ['\u{2012}', '\u{2013}', '\u{2014}', '\u{2015}', '\u{2212}'] {
            normalized = normalized.replace(&format!("{dash}{dash}"), "--");
        }
        for dash in ['\u{2012}', '\u{2013}', '\u{2015}', '\u{2212}'] {
            normalized = normalized.replace(dash, "-");
        }
        normalized.replace('\u{2014}', "--")
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
                    | GatewayCommandResult::ShowVersion(text)
                    | GatewayCommandResult::CompressContext(text)
                    | GatewayCommandResult::StopAgent(text) => Some(text),
                    GatewayCommandResult::QueuePrompt { prompt } => Some(format!(
                        "🧵 Queued follow-up for the active session: {prompt}"
                    )),
                    GatewayCommandResult::SteerPrompt { prompt } => {
                        Some(format!("🧭 Steering instruction accepted: {prompt}"))
                    }
                    GatewayCommandResult::SwitchModel { request } => {
                        Some(format!("🔀 Model switch alias parsed: {}", request.model))
                    }
                    GatewayCommandResult::SwitchFast { reply, .. }
                    | GatewayCommandResult::SwitchPersonality { reply, .. }
                    | GatewayCommandResult::SetTitle { reply, .. }
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
                // Inbox-style gateway platforms stay quiet unless explicitly raised.
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
            let mut modes = self.tool_progress_modes.write().await;
            let current = modes
                .get(&platform)
                .cloned()
                .unwrap_or_else(|| default_mode.clone());
            let next = Self::next_tool_progress_mode(&current).to_string();
            modes.insert(platform.clone(), next.clone());
            next
        };

        let mut states = self.runtime_state.write().await;
        let state = states.entry(session_key.to_string()).or_default();
        state.tool_progress = Some(next.clone());
        state.verbose = next == "verbose";
        drop(states);

        Ok(format!(
            "📝 Tool progress for {platform}: {}",
            next.to_ascii_uppercase()
        ))
    }

    async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<SlashCommandOutcome, GatewayError> {
        let command_text = Self::normalize_slash_command_text(&incoming.text);
        if let Some(reply) = self.resolve_quick_command(&command_text).await? {
            self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                .await?;
            return Ok(SlashCommandOutcome::Handled);
        }

        let result = handle_command(&command_text);
        if matches!(result, GatewayCommandResult::Unknown(_)) {
            match self.resolve_skill_slash_command(&command_text) {
                Ok(Some(message)) => {
                    if let Some(command_name) = Self::extract_command_name(&command_text) {
                        self.emit_hook_event(
                            &format!("command:{}", command_name),
                            serde_json::json!({
                                "platform": incoming.platform,
                                "chat_id": incoming.chat_id,
                                "user_id": incoming.user_id,
                                "session_id": session_key,
                                "command": command_name,
                                "kind": "skill"
                            }),
                        )
                        .await;
                    }
                    return Ok(SlashCommandOutcome::ForwardToAgent { message });
                }
                Ok(None) => {}
                Err(err) => {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        &format!("Skill command blocked: {err}"),
                        None,
                    )
                    .await?;
                    return Ok(SlashCommandOutcome::Handled);
                }
            }
        }
        if !matches!(result, GatewayCommandResult::Unknown(_)) {
            if let Some(command_name) = Self::extract_command_name(&command_text) {
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
        if let GatewayCommandResult::ForwardPrompt { prompt } = result {
            return Ok(SlashCommandOutcome::ForwardToAgent { message: prompt });
        }
        let handled = self
            .apply_command_result(incoming, session_key, result)
            .await?;
        Ok(if handled {
            SlashCommandOutcome::Handled
        } else {
            SlashCommandOutcome::ForwardToAgent {
                message: command_text,
            }
        })
    }

    fn resolve_skill_slash_command(&self, input: &str) -> Result<Option<String>, String> {
        let (cmd, args) = Self::split_slash_command(input);
        let config = SkillCommandResolverConfig::default();
        resolve_installed_skill_slash_command(&cmd, &args, &config)
            .map(|maybe| maybe.map(|invocation| invocation.message))
    }

    async fn apply_reload_skills_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<(), GatewayError> {
        let config = SkillCommandResolverConfig::default();
        let snapshot = installed_skill_slash_command_snapshot(&config);
        {
            let mut states = self.runtime_state.write().await;
            states
                .entry(session_key.to_string())
                .or_default()
                .pending_system_notes
                .push(build_skill_reload_system_note(&snapshot));
        }
        self.send_message(
            &incoming.platform,
            &incoming.chat_id,
            &render_skill_slash_command_snapshot(&snapshot),
            None,
        )
        .await
    }

    fn estimate_gateway_messages_tokens(messages: &[Message]) -> u64 {
        let values = messages
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect::<Vec<_>>();
        estimate_messages_tokens_rough(&values)
    }

    async fn effective_session_model(&self, session_key: &str) -> Option<String> {
        let state_model = self
            .runtime_state
            .read()
            .await
            .get(session_key)
            .and_then(|state| state.model.clone());
        if state_model.is_some() {
            state_model
        } else {
            self.default_model.read().await.clone()
        }
    }

    async fn build_model_switch_preflight_warning(
        &self,
        session_key: &str,
        new_model: &str,
    ) -> Option<String> {
        if !self.config.model_switch_preflight_warning {
            return None;
        }

        let messages = self.session_manager.get_messages(session_key).await;
        let estimate = Self::estimate_gateway_messages_tokens(&messages);
        let current_model = self.effective_session_model(session_key).await;
        format_model_switch_preflight_warning(current_model.as_deref(), new_model, estimate)
            .map(|warning| format!("⚠️ {warning}"))
    }

    fn persist_gateway_default_model_to_config(
        &self,
        model: &str,
    ) -> Result<Option<PathBuf>, String> {
        let Some(path) = self
            .config
            .model_switch_config_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        else {
            return Ok(None);
        };
        let path = PathBuf::from(path);
        let mut disk = load_user_config_file(&path)
            .map_err(|err| format!("load {}: {err}", path.display()))?;
        disk.model = Some(model.to_string());
        save_config_yaml(&path, &disk).map_err(|err| format!("save {}: {err}", path.display()))?;
        Ok(Some(path))
    }

    async fn apply_model_switch_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        request: ModelSwitchRequest,
    ) -> Result<(), GatewayError> {
        let warning = self
            .build_model_switch_preflight_warning(session_key, &request.model)
            .await;
        let persist = match request.scope {
            ModelSwitchScope::Session => false,
            ModelSwitchScope::Global => true,
            ModelSwitchScope::Default => self.config.model_switch_persist_by_default,
        };

        {
            let mut states = self.runtime_state.write().await;
            let state = states.entry(session_key.to_string()).or_default();
            state.model = Some(request.model.clone());
            if let Some(provider) = request.provider.clone() {
                state.provider = Some(provider);
            } else if request.model.contains(':') {
                state.provider = None;
            }
        }

        let mut lines = vec![format!("🔀 Model switched to: {}", request.model)];
        if let Some(provider) = request.provider.as_deref() {
            lines.push(format!("Provider: {provider}"));
        }

        if persist {
            *self.default_model.write().await = Some(request.model.clone());
            match self.persist_gateway_default_model_to_config(&request.model) {
                Ok(Some(path)) => lines.push(format!("Saved to {}.", path.display())),
                Ok(None) => lines.push(
                    "Saved as the gateway default for this process; no config path was configured."
                        .to_string(),
                ),
                Err(err) => {
                    warn!(error = %err, "Failed to persist gateway model switch");
                    lines.push(format!(
                        "⚠️ Config save failed: {err}. The switch remains active for this gateway process."
                    ));
                }
            }
        } else {
            lines.push("Session only. Use `/model <name> --global` to persist.".to_string());
        }

        if request.force_refresh {
            lines.push(
                "Refresh flag accepted; this Rust gateway has no separate in-process model catalog cache to clear."
                    .to_string(),
            );
        }
        if let Some(warning) = warning {
            lines.push(warning);
        }

        self.send_message(
            &incoming.platform,
            &incoming.chat_id,
            &lines.join("\n"),
            None,
        )
        .await?;
        Ok(())
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
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ResetSession(reply) => {
                let current_session = self.session_manager.get_session(session_key).await;
                self.emit_hook_event(
                    "session:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": current_session.as_ref().map(|s| s.id.clone())
                    }),
                )
                .await;
                if let Some(old_session) = current_session.as_ref() {
                    self.emit_session_finalize(session_key, old_session, "reset")
                        .await;
                }
                let reset_snapshot = self
                    .session_manager
                    .reset_session_with_snapshots(session_key)
                    .await;
                self.clear_session_boundary_security_state(session_key)
                    .await;
                let reset_session = reset_snapshot
                    .as_ref()
                    .map(|(_, new_session)| new_session)
                    .or(current_session.as_ref());
                self.emit_hook_event(
                    "session:reset",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": reset_session.map(|s| s.id.clone())
                    }),
                )
                .await;
                if let Some(new_session) = reset_session {
                    self.emit_session_reset_lifecycle(session_key, new_session, "reset")
                        .await;
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { request } => {
                self.apply_model_switch_command(incoming, session_key, request)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
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
                {
                    let mut busy = self.busy_sessions.write().await;
                    let _ = busy.interrupt_active(
                        session_key,
                        "User requested /stop for the active gateway task.",
                    );
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::QueuePrompt { prompt } => {
                let active = {
                    let mut busy = self.busy_sessions.write().await;
                    let active = busy.is_active(session_key);
                    if active {
                        busy.queue_message(
                            session_key,
                            Self::incoming_to_busy_event(incoming, prompt.clone()),
                        );
                    }
                    active
                };
                let reply = if active {
                    format!("🧵 Queued follow-up for the active session: {prompt}")
                } else {
                    "No active gateway turn is running. Send the prompt normally to start it."
                        .to_string()
                };
                self.send_message_threaded(
                    &incoming.platform,
                    &incoming.chat_id,
                    &reply,
                    None,
                    Self::reply_thread_id(incoming),
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::SteerPrompt { prompt } => {
                let decision = {
                    let mut busy = self.busy_sessions.write().await;
                    busy.handle_busy_message(
                        session_key,
                        Self::incoming_to_busy_event(incoming, prompt.clone()),
                        BusyInputMode::Steer,
                    )
                };
                let reply = if decision.steered {
                    format!("🧭 Steered the running task: {prompt}")
                } else if decision.queued {
                    format!("🧵 No live steering hook was ready; queued follow-up: {prompt}")
                } else {
                    "No active gateway turn is running. Use /steer while a task is in flight."
                        .to_string()
                };
                self.send_message_threaded(
                    &incoming.platform,
                    &incoming.chat_id,
                    &reply,
                    None,
                    Self::reply_thread_id(incoming),
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::ForwardPrompt { .. } => Ok(false),
            GatewayCommandResult::ShowUsage(_) => {
                let text = self.build_usage_text(session_key).await;
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowInsights(text) => {
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleVerbose(_) => {
                let reply = self.apply_verbose_command(incoming, session_key).await?;
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ToggleYolo(_) => {
                let mut states = self.runtime_state.write().await;
                let state = states.entry(session_key.to_string()).or_default();
                state.yolo = !state.yolo;
                if state.yolo {
                    hermes_tools::approval::enable_session_yolo(session_key);
                } else {
                    hermes_tools::approval::disable_session_yolo(session_key);
                }
                let reply = format!("🤠 YOLO mode: {}", if state.yolo { "ON" } else { "OFF" });
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowTitle => {
                let reply = match self.session_manager.get_title(session_key).await {
                    Some(title) => format!("🏷 Current session title: {}", title),
                    None => "🏷 No explicit title set for this gateway session.".to_string(),
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SetTitle { title, reply } => {
                let stored = self.session_manager.set_title(session_key, &title).await;
                let response = match &stored {
                    Some(stored_title) if stored_title.as_str() == title => reply,
                    Some(stored_title) => format!("🏷 Session title set to: {}", stored_title),
                    None => "🏷 Session title cleared.".to_string(),
                };
                self.emit_hook_event(
                    "session:title",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "title": stored
                    }),
                )
                .await;
                self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowStatus(_) => {
                let text = self.build_status_text(session_key).await;
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ShowVersion(text) => {
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
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
            GatewayCommandResult::ReloadSkills => {
                self.apply_reload_skills_command(incoming, session_key)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProvider { provider, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProfile { profile, reply } => {
                let response = match load_gateway_profile_overlay(&profile) {
                    Ok(overlay) => {
                        let mut states = self.runtime_state.write().await;
                        let state = states.entry(session_key.to_string()).or_default();
                        apply_gateway_profile_overlay(state, &overlay);
                        render_profile_overlay_reply(&profile, &overlay)
                    }
                    Err(err) => {
                        let mut states = self.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().profile = Some(profile);
                        format!("{reply}\n⚠️ Profile file not applied: {err}")
                    }
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &response, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchFast {
                service_tier,
                reply,
            } => {
                let mut states = self.runtime_state.write().await;
                states
                    .entry(session_key.to_string())
                    .or_default()
                    .service_tier = service_tier.clone();
                drop(states);
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
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
                        let title = s.title.as_deref().unwrap_or("(untitled)");
                        out.push_str(&format!(
                            "• `{}` — {} messages, title `{}`, platform `{}` (id `{}`)\n",
                            key,
                            s.messages.len(),
                            title,
                            s.platform,
                            s.id
                        ));
                    }
                    out.push_str("\nUse `/sessions <key or id>` to switch.");
                    out
                };
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
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
                        .replace_messages_and_title(
                            session_key,
                            target.messages.clone(),
                            target.title.clone(),
                        )
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
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }
}

include!("gateway/routing_send.rs");

#[cfg(test)]
mod tests;
