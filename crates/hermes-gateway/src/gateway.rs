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
use futures::FutureExt;
use parking_lot::Mutex as ParkingMutex;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use hermes_config::{
    load_user_config_file, normalize_service_tier, save_config_yaml, DisplayConfig,
    QuickCommandConfig,
};
use hermes_core::errors::GatewayError;
use hermes_core::{
    MemoryExportArtifactRef, MemoryExportEvidenceDigest, MemoryProviderGovernanceError,
    MemorySessionId, MemorySubject, MemorySubjectError,
};
use hermes_core::subprocess::CommandNoWindowExt;
use hermes_core::traits::{
    NativeMediaFailureClass, ParseMode, PlatformAdapter, SendMessageOptions,
};
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
use crate::platforms::helpers::{extract_inline_images, extract_media_markers};
use crate::session::{ConversationScope, Session, SessionManager};
use crate::session_control::{
    interrupt_control, ActiveSessionControl, BusyAdmission, BusyInputMode, BusyMessageDecision,
    BusySessionCoordinator, GatewayRouteOutcome, ProcessingOutcome, QueueAdmission,
    QueuedIncoming, QueuedTurnKind, ResetAdmission, TurnCompletion, TurnLease,
};
use crate::stream::{StreamConfig, StreamManager};

const DEFAULT_MESSAGE_DEDUP_CAPACITY: usize = 4096;
const MAX_INGRESS_WORKERS: usize = 1024;
const INGRESS_BACKPRESSURE_RETRY_AFTER: Duration = Duration::from_millis(50);
const MAX_RETAINED_SESSIONS: usize = 4096;
const MAX_MEMORY_EXPORT_DOWNLOAD_GRANTS: usize = 64;
const MEMORY_EXPORT_DOWNLOAD_GRANT_TTL: Duration = Duration::from_secs(10 * 60);
const SESSION_COMMIT_TIMEOUT: Duration = Duration::from_secs(30);
const NONCRITICAL_ADAPTER_TIMEOUT: Duration = Duration::from_secs(5);
const ADAPTER_FILE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_TURN_TIMEOUT_SECS: u64 = 30 * 60;

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

    /// Enable defensive runtime admission and retention limits.
    ///
    /// Disabled by default so existing workloads retain their prior capacity.
    #[serde(default)]
    pub resource_limits_enabled: bool,

    /// Optional runtime cap for concurrently admitted ingress workers.
    ///
    /// When unset, the production defensive limit remains
    /// [`MAX_INGRESS_WORKERS`]. Values below one are clamped to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ingress_worker_limit: Option<usize>,

    /// Hard deadline for one agent turn, including tool execution.
    #[serde(default = "default_turn_timeout_secs")]
    pub turn_timeout_secs: u64,

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
            resource_limits_enabled: false,
            ingress_worker_limit: None,
            turn_timeout_secs: DEFAULT_TURN_TIMEOUT_SECS,
            kanban_dispatch_in_gateway: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_turn_timeout_secs() -> u64 {
    DEFAULT_TURN_TIMEOUT_SECS
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

/// Sender provenance provided by a platform adapter at ingress.
///
/// Unknown provenance is deliberately distinct from a verified human. Only a
/// verified bot can use an explicit Discord bot-policy bypass. Discord member
/// role metadata is retained only when it came from an authenticated gateway
/// event and is consumed only by Discord's existing role policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IncomingSender {
    kind: IncomingSenderKind,
    verified_discord_member: Option<VerifiedDiscordMember>,
    discord_message_is_reply: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VerifiedDiscordMember {
    role_ids: BTreeSet<String>,
    guild_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum IncomingSenderKind {
    #[default]
    Unknown,
    Human,
    Bot,
}

impl IncomingSender {
    /// Construct provenance for a verified human platform identity.
    pub fn human() -> Self {
        Self {
            kind: IncomingSenderKind::Human,
            verified_discord_member: None,
            discord_message_is_reply: false,
        }
    }

    /// Construct a verified human Discord member from Gateway MESSAGE_CREATE
    /// member data. Callers must not source these role IDs from user text.
    pub fn verified_discord_member(
        role_ids: impl IntoIterator<Item = String>,
        guild_id: Option<String>,
    ) -> Self {
        Self {
            kind: IncomingSenderKind::Human,
            verified_discord_member: Some(VerifiedDiscordMember {
                role_ids: role_ids
                    .into_iter()
                    .map(|role_id| role_id.trim().to_string())
                    .filter(|role_id| !role_id.is_empty())
                    .collect(),
                guild_id: guild_id
                    .map(|guild_id| guild_id.trim().to_string())
                    .filter(|guild_id| !guild_id.is_empty()),
            }),
            discord_message_is_reply: false,
        }
    }

    /// Construct provenance for a verified bot or webhook platform identity.
    pub fn bot() -> Self {
        Self {
            kind: IncomingSenderKind::Bot,
            verified_discord_member: None,
            discord_message_is_reply: false,
        }
    }

    /// Construct provenance when an adapter cannot verify sender type.
    pub fn unknown() -> Self {
        Self::default()
    }

    /// Convert platform-supplied bot metadata without treating an absent field
    /// as a human assertion.
    pub fn from_platform_bot_flag(is_bot: Option<bool>) -> Self {
        match is_bot {
            Some(true) => Self::bot(),
            Some(false) => Self::human(),
            None => Self::unknown(),
        }
    }

    pub fn is_bot(&self) -> bool {
        matches!(self.kind, IncomingSenderKind::Bot)
    }

    pub(crate) fn with_discord_message_reply(mut self, is_reply: bool) -> Self {
        self.discord_message_is_reply = is_reply;
        self
    }

    pub fn discord_message_is_reply(&self) -> bool {
        self.discord_message_is_reply
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

/// Identity-safe enrichment returned after authorization and dedup ownership,
/// but before session scope and busy-queue admission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AcceptedIngressPreparation {
    pub thread_id: Option<String>,
}

/// Classification for a side-effecting accepted-ingress preparation.
///
/// The preparer runs after authorization and dedup ownership but before any
/// session, model, or output work. Only [`Retryable`] proves the provider
/// operation did not cross a remote side-effect boundary.
#[derive(Debug)]
pub enum AcceptedIngressPreparationError {
    Retryable {
        error: GatewayError,
        retry_after: Option<std::time::Duration>,
    },
    Rejected(GatewayError),
    Ambiguous(GatewayError),
}

impl AcceptedIngressPreparationError {
    pub fn retryable(error: GatewayError) -> Self {
        let retry_after = match &error {
            GatewayError::RateLimited { retry_after } => *retry_after,
            _ => None,
        };
        Self::Retryable { error, retry_after }
    }
}

impl From<GatewayError> for AcceptedIngressPreparationError {
    fn from(error: GatewayError) -> Self {
        Self::Ambiguous(error)
    }
}

pub type AcceptedIngressPreparationResult =
    Result<AcceptedIngressPreparation, AcceptedIngressPreparationError>;

/// A callback must classify preparation failures at the remote-side-effect
/// boundary. Raw gateway errors become ambiguous rather than replayable.

pub type AcceptedIngressPreparer = Arc<
    dyn for<'a> Fn(
            &'a IncomingMessage,
            &'a IncomingSender,
        ) -> std::pin::Pin<
            Box<dyn Future<Output = AcceptedIngressPreparationResult> + Send + 'a>,
        > + Send
        + Sync,
>;

/// Structured runtime context passed to V2 handlers.
#[derive(Debug, Clone, Default)]
pub struct GatewayRuntimeContext {
    /// Gateway-internal routing key; never used as a memory subject id.
    pub session_key: String,
    /// Authenticated platform namespace from accepted ingress.
    pub platform: String,
    /// Authenticated tenant namespace. A missing value is rejected before a
    /// context-aware agent can construct memory providers or models.
    pub tenant_id: Option<String>,
    /// An explicit authenticated team namespace when an adapter provides one.
    /// Group chat IDs are not silently promoted into team sharing.
    pub team_id: Option<String>,
    /// Logical gateway session UUID from SessionManager, not `session_key`.
    pub session_id: Option<String>,
    /// Whether accepted ingress was a direct message.
    pub is_dm: bool,
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

impl GatewayRuntimeContext {
    /// Derive memory identity only from accepted gateway ingress/session state.
    ///
    /// A group remains principal-private even if a true team id is available;
    /// sharing requires an explicit `MemoryVisibility::Team` construction
    /// outside this default gateway path.
    pub fn memory_subject(&self) -> Result<MemorySubject, MemorySubjectError> {
        if self.platform.trim().is_empty() {
            return Err(MemorySubjectError::MissingAuthenticatedField { field: "platform" });
        }
        if self.chat_id.trim().is_empty() {
            return Err(MemorySubjectError::MissingAuthenticatedField { field: "chat id" });
        }
        if let Some(thread_id) = self.thread_id.as_deref() {
            // A missing thread denotes the root conversation. A supplied
            // thread is authenticated ingress identity and must obey the same
            // closed identifier rules as the session that scopes it.
            MemorySessionId::new(thread_id)?;
        }
        MemorySubject::from_authenticated_gateway(
            self.tenant_id.as_deref(),
            (!self.user_id.trim().is_empty()).then_some(self.user_id.as_str()),
            self.team_id.as_deref(),
            self.session_id.as_deref(),
            self.is_dm,
        )
    }
}

/// Context-aware callback type for processing messages through the agent loop.
pub type MessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
        ) -> Pin<Box<dyn Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Callback type for streaming message processing.
/// Takes session messages and a chunk callback, returns the final response.
pub type StreamingMessageHandler = Arc<
    dyn Fn(
            Vec<Message>,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Context-aware callback type for streaming message processing.
pub type StreamingMessageHandlerWithContext = Arc<
    dyn Fn(
            Vec<Message>,
            GatewayRuntimeContext,
            Arc<dyn Fn(String) + Send + Sync>,
        ) -> Pin<Box<dyn Future<Output = Result<String, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Content-free terminal state for one provider in a governed memory erase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayMemoryForgetProviderState {
    VerifiedAbsent,
    Incomplete {
        error: Option<MemoryProviderGovernanceError>,
        readback_absent: bool,
    },
}

/// Content-free provider receipt rendered by the authenticated gateway route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMemoryForgetProviderStatus {
    pub provider: String,
    pub state: GatewayMemoryForgetProviderState,
}

/// Safe recovery advice when a governed erase has residual or unavailable
/// provider state. Only `erased: true` may be rendered as erasure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayMemoryForgetRemediation {
    Retry,
    Reconfigure,
}

/// Result of a subject-bound, authenticated memory-erasure request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMemoryForgetResponse {
    pub erased: bool,
    pub providers: Vec<GatewayMemoryForgetProviderStatus>,
    pub remediation: Option<GatewayMemoryForgetRemediation>,
}

/// Authenticated gateway callback for subject-bound memory erasure.
pub type GatewayMemoryForgetHandler = Arc<
    dyn Fn(
            GatewayRuntimeContext,
        ) -> Pin<Box<dyn Future<Output = Result<GatewayMemoryForgetResponse, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Content-free recovery advice for a governed memory export that did not
/// produce a committed owner-only artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayMemoryExportRemediation {
    Retry,
    Reconfigure,
}

/// Internal metadata for a manager-committed memory export artifact.
///
/// The artifact reference is never rendered to chat. It is retained only long
/// enough to bind a gateway-local, single-use download reference to the
/// authenticated subject that requested it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMemoryExportArtifact {
    pub artifact_ref: MemoryExportArtifactRef,
    pub manifest_digest: MemoryExportEvidenceDigest,
    pub record_count: u64,
    pub page_count: u64,
    pub provider_count: u32,
}

/// A content-free, single-use download reference issued by the gateway after
/// an all-provider memory export commits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMemoryExportDownloadMetadata {
    pub download_ref: String,
    pub manifest_digest: MemoryExportEvidenceDigest,
    pub record_count: u64,
    pub page_count: u64,
    pub provider_count: u32,
}

/// Result of a subject-bound, authenticated memory-export request.
///
/// `artifact` must be `Some` only for an all-provider committed export. The
/// gateway then turns it into a subject-bound, single-use download reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMemoryExportResponse {
    pub artifact: Option<GatewayMemoryExportArtifact>,
    pub remediation: Option<GatewayMemoryExportRemediation>,
}

/// Authenticated gateway callback for a transactionally exported, bound
/// memory subject.
pub type GatewayMemoryExportHandler = Arc<
    dyn Fn(
            GatewayRuntimeContext,
        ) -> Pin<Box<dyn Future<Output = Result<GatewayMemoryExportResponse, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

/// Authenticated gateway callback that opens the manager-verified descriptor
/// associated with a redeemed, single-use export grant.
pub type GatewayMemoryExportOpenHandler = Arc<
    dyn Fn(
            GatewayRuntimeContext,
            GatewayMemoryExportArtifact,
        ) -> Pin<Box<dyn Future<Output = Result<std::fs::File, GatewayError>> + Send>,
        > + Send
        + Sync,
>;

#[derive(Clone)]
struct GatewayMemoryExportDownloadGrant {
    subject: MemorySubject,
    artifact: GatewayMemoryExportArtifact,
    issued_at: Instant,
}

#[derive(Clone)]
pub struct BusyControlRegistration {
    lease: TurnLease,
    coordinator: Arc<RwLock<BusySessionCoordinator>>,
}

impl std::fmt::Debug for BusyControlRegistration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BusyControlRegistration")
            .field("session_key", &self.lease.session_key())
            .finish_non_exhaustive()
    }
}

impl BusyControlRegistration {
    fn new(lease: TurnLease, coordinator: Arc<RwLock<BusySessionCoordinator>>) -> Self {
        Self { lease, coordinator }
    }

    pub async fn attach(&self, control: Arc<dyn ActiveSessionControl>) -> bool {
        self.coordinator
            .write()
            .await
            .attach_control(&self.lease, control)
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

#[derive(Clone)]
struct TurnMutationBaseline {
    session: Option<Session>,
    usage: Option<UsageStats>,
    runtime: Option<SessionRuntimeState>,
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

#[derive(Debug, Clone, Copy)]
enum DedupResolution {
    Completed,
    Retry,
}

enum DedupClaimOutcome {
    Owner,
    Completed,
    InFlight(tokio::sync::oneshot::Receiver<DedupResolution>),
}

#[derive(Debug)]
struct MessageDeduplicator {
    claimed: HashMap<String, Vec<tokio::sync::oneshot::Sender<DedupResolution>>>,
    completed: HashSet<String>,
    completed_order: VecDeque<String>,
    capacity: usize,
}

impl Default for MessageDeduplicator {
    fn default() -> Self {
        Self {
            claimed: HashMap::new(),
            completed: HashSet::new(),
            completed_order: VecDeque::new(),
            capacity: DEFAULT_MESSAGE_DEDUP_CAPACITY,
        }
    }
}

impl MessageDeduplicator {
    fn claim(&mut self, key: String) -> DedupClaimOutcome {
        if self.completed.contains(&key) {
            return DedupClaimOutcome::Completed;
        }
        if let Some(waiters) = self.claimed.get_mut(&key) {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            waiters.push(sender);
            return DedupClaimOutcome::InFlight(receiver);
        }
        self.claimed.insert(key, Vec::new());
        DedupClaimOutcome::Owner
    }

    fn complete(&mut self, key: &str) {
        let Some(waiters) = self.claimed.remove(key) else {
            return;
        };
        while self.completed.len() >= self.capacity {
            let Some(oldest) = self.completed_order.pop_front() else {
                break;
            };
            self.completed.remove(&oldest);
        }
        if self.completed.insert(key.to_string()) {
            self.completed_order.push_back(key.to_string());
        }
        for waiter in waiters {
            let _ = waiter.send(DedupResolution::Completed);
        }
    }

    fn forget(&mut self, key: &str) {
        let Some(waiters) = self.claimed.remove(key) else {
            return;
        };
        for waiter in waiters {
            let _ = waiter.send(DedupResolution::Retry);
        }
    }
}

struct DedupClaimGuard {
    deduplicator: Arc<ParkingMutex<MessageDeduplicator>>,
    key: Option<String>,
    complete_on_drop: Option<Arc<AtomicBool>>,
}

impl DedupClaimGuard {
    fn new(deduplicator: Arc<ParkingMutex<MessageDeduplicator>>, key: String) -> Self {
        Self {
            deduplicator,
            key: Some(key),
            complete_on_drop: None,
        }
    }
    fn complete_on_drop_when(&mut self, boundary: Arc<AtomicBool>) {
        self.complete_on_drop = Some(boundary);
    }

    fn disarm(&mut self) {
        self.key = None;
    }
}

impl Drop for DedupClaimGuard {
    fn drop(&mut self) {
        let Some(key) = self.key.as_deref() else {
            return;
        };
        let mut deduplicator = self.deduplicator.lock();
        if self
            .complete_on_drop
            .as_ref()
            .is_some_and(|boundary| boundary.load(Ordering::Acquire))
        {
            deduplicator.complete(key);
        } else {
            deduplicator.forget(key);
        }
    }
}

enum DedupIngressClaim {
    Untracked,
    Completed,
    Owner { key: String, guard: DedupClaimGuard },
}
/// Internal completion record for one route caller.
///
/// Durable ingress consumers only need the externally durable disposition,
/// while direct callers must still observe a handler failure even after a
/// successfully delivered error notice commits the inbound message.
#[derive(Debug)]
pub(crate) enum RouteCompletion {
    Outcome(GatewayRouteOutcome),
    CommittedFailure(GatewayError),
}

impl RouteCompletion {
    pub(crate) fn into_outcome(self) -> GatewayRouteOutcome {
        match self {
            Self::Outcome(outcome) => outcome,
            Self::CommittedFailure(_) => GatewayRouteOutcome::Committed,
        }
    }

    pub(crate) fn into_result(self) -> Result<(), GatewayError> {
        match self {
            Self::Outcome(outcome) => outcome.into_result(),
            Self::CommittedFailure(error) => Err(error),
        }
    }
}

impl From<GatewayRouteOutcome> for RouteCompletion {
    fn from(outcome: GatewayRouteOutcome) -> Self {
        Self::Outcome(outcome)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ErrorNotificationOutcome {
    Accepted,
    Rejected,
    Uncertain,
}

include!("gateway/profile_overlay.rs");

// ---------------------------------------------------------------------------
// Gateway
type SessionTurnLockRegistry = HashMap<Arc<str>, std::sync::Weak<tokio::sync::Mutex<()>>>;

struct SessionTurnLock {
    key: Arc<str>,
    lock: Option<Arc<tokio::sync::Mutex<()>>>,
    registry: Arc<ParkingMutex<SessionTurnLockRegistry>>,
}

impl Clone for SessionTurnLock {
    fn clone(&self) -> Self {
        Self {
            key: self.key.clone(),
            lock: Some(self.lock.as_ref().expect("session lock missing").clone()),
            registry: self.registry.clone(),
        }
    }
}

impl SessionTurnLock {
    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock
            .as_ref()
            .expect("session lock missing")
            .lock()
            .await
    }
}

impl Drop for SessionTurnLock {
    fn drop(&mut self) {
        let mut registry = self.registry.lock();
        let lock = self.lock.take().expect("session lock missing");
        let weak = Arc::downgrade(&lock);
        let is_current = registry
            .get(self.key.as_ref())
            .is_some_and(|registered| registered.ptr_eq(&weak));
        drop(lock);
        if is_current && weak.strong_count() == 0 {
            registry.remove(self.key.as_ref());
        }
    }
}

struct OwnedSessionMutationGuard {
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
    lock: Option<SessionTurnLock>,
}

impl Drop for OwnedSessionMutationGuard {
    fn drop(&mut self) {
        drop(self.guard.take());
        drop(self.lock.take());
    }
}

#[derive(Default)]
struct StreamChunkMailboxState {
    pending: String,
    pending_event: bool,
    closed: bool,
}

#[derive(Default)]
struct StreamChunkMailbox {
    state: ParkingMutex<StreamChunkMailboxState>,
    notify: tokio::sync::Notify,
}

impl StreamChunkMailbox {
    fn push(&self, chunk: String) {
        let mut state = self.state.lock();
        if state.closed {
            return;
        }
        state.pending.push_str(&chunk);
        state.pending_event = true;
        drop(state);
        self.notify.notify_one();
    }

    fn close(&self) {
        let mut state = self.state.lock();
        state.closed = true;
        drop(state);
        self.notify.notify_one();
    }

    async fn recv(&self) -> Option<String> {
        loop {
            let notified = self.notify.notified();
            {
                let mut state = self.state.lock();
                if state.pending_event {
                    state.pending_event = false;
                    return Some(std::mem::take(&mut state.pending));
                }
                if state.closed {
                    return None;
                }
            }
            notified.await;
        }
    }
}

struct StreamChunkMailboxCloser(Arc<StreamChunkMailbox>);

impl Drop for StreamChunkMailboxCloser {
    fn drop(&mut self) {
        self.0.close();
    }
}
struct IngressWorkerGuard {
    active: Arc<AtomicUsize>,
}

impl Drop for IngressWorkerGuard {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

// ---------------------------------------------------------------------------

/// Central orchestrator for all platform adapters.
///
/// The `Gateway` owns a collection of named `PlatformAdapter` instances,
/// a `SessionManager`, a `DmManager`, and a `StreamManager`. It provides
/// a unified interface to start/stop adapters and route messages.
#[derive(Clone)]
pub struct Gateway {
    adapters: Arc<RwLock<HashMap<String, Arc<dyn PlatformAdapter>>>>,
    session_manager: Arc<SessionManager>,
    dm_manager: Arc<RwLock<DmManager>>,
    stream_manager: Arc<StreamManager>,
    config: Arc<GatewayConfig>,
    /// Optional message handler for processing messages through the agent loop.
    message_handler: Arc<RwLock<Option<MessageHandler>>>,
    /// Optional context-aware message handler for processing incoming messages.
    message_handler_with_context: Arc<RwLock<Option<MessageHandlerWithContext>>>,
    /// Optional streaming message handler.
    streaming_handler: Arc<RwLock<Option<StreamingMessageHandler>>>,
    /// Authenticated subject-bound governed memory-erasure handler.
    memory_forget_handler: Arc<RwLock<Option<GatewayMemoryForgetHandler>>>,
    /// Authenticated subject-bound governed memory-export handler.
    memory_export_handler: Arc<RwLock<Option<GatewayMemoryExportHandler>>>,
    /// Opens a redeemed export only through the subject-bound manager API.
    memory_export_open_handler: Arc<RwLock<Option<GatewayMemoryExportOpenHandler>>>,
    /// Single-use export download grants, each bound to the exact subject and
    /// committed artifact metadata that produced it.
    memory_export_download_grants: Arc<RwLock<HashMap<String, GatewayMemoryExportDownloadGrant>>>,
    /// Optional context-aware streaming message handler.
    streaming_handler_with_context: Arc<RwLock<Option<StreamingMessageHandlerWithContext>>>,
    /// Platform-specific preparation after authorization and dedup ownership.
    accepted_ingress_preparers: Arc<RwLock<HashMap<String, AcceptedIngressPreparer>>>,
    /// Runtime command state for each session.
    runtime_state: Arc<RwLock<HashMap<String, SessionRuntimeState>>>,
    /// Process-wide gateway default model updated by persistent `/model` switches.
    default_model: Arc<RwLock<Option<String>>>,
    tool_progress_modes: Arc<RwLock<BTreeMap<String, String>>>,
    /// Basic usage counters for each session.
    usage_stats: Arc<RwLock<HashMap<String, UsageStats>>>,
    /// Tracks async `/background` and `/btw` tasks.
    background_tasks: Arc<BackgroundTaskManager>,
    /// Supervised foreground turn workers, retained through cancellation-safe shutdown.
    turn_workers: Arc<StdMutex<HashMap<u64, tokio::task::JoinHandle<()>>>>,
    next_turn_worker_id: Arc<AtomicU64>,
    active_ingress_workers: Arc<AtomicUsize>,
    /// Sticky shutdown state and notification for every admitted worker.
    shutting_down: Arc<AtomicBool>,
    shutdown_tx: Arc<tokio::sync::watch::Sender<bool>>,
    /// MCP reload generation number.
    mcp_reload_generation: Arc<RwLock<u64>>,
    /// Optional hook registry for runtime event emission.
    hook_registry: Arc<RwLock<Option<Arc<HookRegistry>>>>,
    /// Per-platform allowlist policy for group and slash-command traffic.
    platform_access_policies: Arc<RwLock<HashMap<String, PlatformAccessPolicy>>>,
    /// One-shot warnings for platform policies that fail closed by default.
    fail_closed_default_warnings: Arc<RwLock<HashSet<String>>>,
    /// Bounded duplicate guard for platform redeliveries/restarts.
    message_deduplicator: Arc<ParkingMutex<MessageDeduplicator>>,
    /// Active gateway sessions plus queued/steered busy follow-ups.
    busy_sessions: Arc<RwLock<BusySessionCoordinator>>,
    /// Per-session mutation barriers with exact last-owner cleanup.
    session_turn_locks: Arc<ParkingMutex<SessionTurnLockRegistry>>,
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
    /// Discord role IDs accepted for verified guild members.
    pub allowed_roles: HashSet<String>,
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
