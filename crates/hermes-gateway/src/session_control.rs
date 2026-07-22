//! Gateway session-control primitives.
//!
//! These helpers model the Python gateway's active-session guard without
//! coupling platform adapters to a concrete Rust agent implementation.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fmt;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::watch;

use crate::commands::should_bypass_active_session;

use crate::gateway::{IncomingMessage, IncomingSender};

const MAX_ACTIVE_SESSION_LANES: usize = 64;
const MAX_PENDING_PER_SESSION: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionSource {
    pub platform: String,
    pub chat_id: String,
    pub chat_type: String,
    pub user_id: Option<String>,
    pub thread_id: Option<String>,
    pub scope_id: Option<String>,
    pub guild_id: Option<String>,
}

impl SessionSource {
    pub fn new(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        chat_type: impl Into<String>,
    ) -> Self {
        Self {
            platform: platform.into(),
            chat_id: chat_id.into(),
            chat_type: chat_type.into(),
            user_id: None,
            thread_id: None,
            scope_id: None,
            guild_id: None,
        }
    }

    pub fn with_user(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    pub fn with_thread(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    pub fn with_scope(mut self, scope_id: impl Into<String>) -> Self {
        let scope_id = scope_id.into();
        self.guild_id = Some(scope_id.clone());
        self.scope_id = Some(scope_id);
        self
    }

    pub fn with_guild_id(self, guild_id: impl Into<String>) -> Self {
        self.with_scope(guild_id)
    }

    pub fn scope_id(&self) -> Option<&str> {
        self.scope_id
            .as_deref()
            .or(self.guild_id.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }
}

pub fn build_session_key(source: &SessionSource) -> String {
    let mut parts = vec![source.platform.trim(), source.chat_id.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if let Some(scope_id) = source.scope_id() {
        parts.insert(1, scope_id.to_string());
    }
    if let Some(thread_id) = source
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(thread_id.to_string());
    }
    parts.join(":")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageType {
    Text,
    Image,
    File,
    Voice,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessageEvent {
    pub text: String,
    pub message_type: MessageType,
    pub source: SessionSource,
    pub message_id: Option<String>,
    pub metadata: BTreeMap<String, Value>,
}

impl MessageEvent {
    pub fn text(text: impl Into<String>, source: SessionSource) -> Self {
        Self {
            text: text.into(),
            message_type: MessageType::Text,
            source,
            message_id: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn is_command(&self) -> bool {
        self.text.trim_start().starts_with('/')
    }

    pub fn get_command(&self) -> Option<String> {
        command_name_from_text(&self.text)
    }

    pub fn get_command_args(&self) -> String {
        let trimmed = self.text.trim_start();
        if !trimmed.starts_with('/') {
            return self.text.clone();
        }
        trimmed
            .split_once(char::is_whitespace)
            .map(|(_, args)| args.trim().to_string())
            .unwrap_or_default()
    }
}

pub fn command_name_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let token = trimmed.strip_prefix('/')?.split_whitespace().next()?;
    let command = token.split('@').next().unwrap_or(token).trim();
    if command.is_empty() {
        Some(String::new())
    } else {
        Some(command.to_ascii_lowercase().replace('-', "_"))
    }
}

pub fn event_bypasses_active_session(event: &MessageEvent) -> bool {
    event
        .get_command()
        .as_deref()
        .is_some_and(|command| should_bypass_active_session(Some(command)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessingOutcome {
    Success,
    Failure,
    Cancelled,
}

/// Authoritative completion state for one accepted ingress route.
///
/// `Retryable` means no handler/model/tool or external delivery began and the
/// inbound journal may safely return to pending. `Backpressured` is an
/// admission-only refusal: no route side effect occurred, and the live
/// consumer must retain FIFO work and retry after its bounded delay.
/// `Uncertain` means an external or handler side effect may have occurred and
/// must be quarantined.
#[derive(Debug)]
pub enum GatewayRouteOutcome {
    Committed,
    /// A full inbound route failed before handler/model/tool execution or any
    /// external delivery/notification. `retry_after` preserves an exact
    /// provider pacing hint when one exists; spool owners must persist it as
    /// an absolute deadline rather than reducing it to local backoff.
    Retryable {
        error: hermes_core::errors::GatewayError,
        retry_after: Option<Duration>,
    },
    /// Admission was refused before a worker or handler was created.
    Backpressured { retry_after: Duration },
    /// A provider accepted a prefix and supplied an exact safe continuation.
    Partial(hermes_core::errors::PartialDelivery),
    /// An external or handler side effect may have occurred and must be
    /// quarantined instead of replayed.
    Uncertain(hermes_core::errors::GatewayError),
}

impl GatewayRouteOutcome {
    pub fn into_result(self) -> Result<(), hermes_core::errors::GatewayError> {
        match self {
            Self::Committed => Ok(()),
            Self::Retryable { error, .. } | Self::Uncertain(error) => Err(error),
            Self::Backpressured { retry_after } => Err(
                hermes_core::errors::GatewayError::Platform(format!(
                    "gateway ingress capacity reached; retry after {} ms",
                    retry_after.as_millis()
                )),
            ),
            Self::Partial(partial) => Err(hermes_core::errors::GatewayError::PartialDelivery(partial)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusyInputMode {
    Queue,
    Interrupt,
    Steer,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentActivitySummary {
    pub api_call_count: Option<u32>,
    pub max_iterations: Option<u32>,
    pub current_tool: Option<String>,
    pub elapsed: Option<Duration>,
    pub seconds_since_activity: Option<f64>,
}

pub trait ActiveSessionControl: Send + Sync {
    fn interrupt(&self, _message: &str) {}
    fn steer(&self, _message: &str) -> bool {
        false
    }
    fn activity_summary(&self) -> Option<AgentActivitySummary> {
        None
    }
}

pub(crate) fn interrupt_control(control: &dyn ActiveSessionControl, message: &str) -> bool {
    std::panic::catch_unwind(AssertUnwindSafe(|| control.interrupt(message))).is_ok()
}

fn steer_control(control: &dyn ActiveSessionControl, message: &str) -> bool {
    std::panic::catch_unwind(AssertUnwindSafe(|| control.steer(message))).unwrap_or(false)
}

fn control_activity_summary(control: &dyn ActiveSessionControl) -> Option<AgentActivitySummary> {
    std::panic::catch_unwind(AssertUnwindSafe(|| control.activity_summary()))
        .ok()
        .flatten()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusyMessageDecision {
    pub handled: bool,
    pub queued: bool,
    pub interrupted: bool,
    pub steered: bool,
    pub ack: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IngressValidation {
    DeduplicatedAtIngress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueuedTurnKind {
    Message,
    Retry,
}

/// A gateway-internal queue item constructed only after ingress validation.
///
/// The validation marker is intentionally private: platform payloads can never
/// claim that they have already passed the gateway's ingress checks.
#[derive(Debug)]
pub(crate) struct QueuedIncoming {
    incoming: IncomingMessage,
    /// Verified sender provenance retained across queue drain.
    #[cfg_attr(not(test), allow(dead_code))]
    sender: IncomingSender,
    _ingress_validation: IngressValidation,
    agent_text_override: Option<String>,
    dedup_key: Option<String>,
    turn_kind: QueuedTurnKind,
    completion_tx: Option<
        tokio::sync::oneshot::Sender<
            Result<GatewayRouteOutcome, hermes_core::errors::GatewayError>,
        >,
    >,
}

impl QueuedIncoming {
    pub(crate) fn from_validated_ingress(
        incoming: IncomingMessage,
        sender: IncomingSender,
    ) -> Self {
        Self {
            incoming,
            sender,
            _ingress_validation: IngressValidation::DeduplicatedAtIngress,
            agent_text_override: None,
            dedup_key: None,
            turn_kind: QueuedTurnKind::Message,
            completion_tx: None,
        }
    }

    pub(crate) fn with_agent_text_override(mut self, text: impl Into<String>) -> Self {
        self.agent_text_override = Some(text.into());
        self
    }

    pub(crate) fn with_dedup_key(mut self, key: Option<String>) -> Self {
        self.dedup_key = key;
        self
    }

    pub(crate) fn with_turn_kind(mut self, turn_kind: QueuedTurnKind) -> Self {
        self.turn_kind = turn_kind;
        self
    }

    pub(crate) fn with_completion_sender(
        mut self,
        completion_tx: tokio::sync::oneshot::Sender<
            Result<GatewayRouteOutcome, hermes_core::errors::GatewayError>,
        >,
    ) -> Self {
        self.completion_tx = Some(completion_tx);
        self
    }

    pub(crate) fn clear_completion_sender(&mut self) {
        self.completion_tx.take();
    }

    pub(crate) fn take_completion_sender(
        &mut self,
    ) -> Option<
        tokio::sync::oneshot::Sender<
            Result<GatewayRouteOutcome, hermes_core::errors::GatewayError>,
        >,
    > {
        self.completion_tx.take()
    }

    pub(crate) fn incoming(&self) -> &IncomingMessage {
        &self.incoming
    }

    #[cfg(test)]
    pub(crate) fn sender(&self) -> IncomingSender {
        self.sender.clone()
    }

    pub(crate) fn agent_text_override(&self) -> Option<&str> {
        self.agent_text_override.as_deref()
    }

    pub(crate) fn dedup_key(&self) -> Option<&str> {
        self.dedup_key.as_deref()
    }

    pub(crate) fn turn_kind(&self) -> QueuedTurnKind {
        self.turn_kind
    }
}

/// Cancellation and completion state shared by all clones of one turn lease.
struct TurnLifecycle {
    cancelled: watch::Sender<bool>,
    finished: watch::Sender<bool>,
}

impl TurnLifecycle {
    fn new() -> Self {
        let (cancelled, _) = watch::channel(false);
        let (finished, _) = watch::channel(false);
        Self {
            cancelled,
            finished,
        }
    }

    fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    fn finish(&self) {
        self.finished.send_replace(true);
    }

    async fn wait_cancelled(&self) {
        let mut cancelled = self.cancelled.subscribe();
        while !*cancelled.borrow_and_update() {
            if cancelled.changed().await.is_err() {
                break;
            }
        }
    }

    async fn wait_finished(&self) {
        let mut finished = self.finished.subscribe();
        while !*finished.borrow_and_update() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }
}

pub(crate) struct TurnExecutionGuard(Arc<TurnLifecycle>);

impl Drop for TurnExecutionGuard {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Ownership for one active gateway turn.
///
/// A lease expires when its session is reset or a successor turn claims the
/// lane, so late work cannot complete or deliver into a newer session state.
#[derive(Clone)]
pub(crate) struct TurnLease {
    session_key: String,
    turn_id: u64,
    lifecycle: Arc<TurnLifecycle>,
}

impl fmt::Debug for TurnLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnLease")
            .field("session_key", &self.session_key)
            .field("turn_id", &self.turn_id)
            .finish_non_exhaustive()
    }
}

impl PartialEq for TurnLease {
    fn eq(&self, other: &Self) -> bool {
        self.session_key == other.session_key && self.turn_id == other.turn_id
    }
}

impl Eq for TurnLease {}

impl TurnLease {
    pub(crate) fn session_key(&self) -> &str {
        &self.session_key
    }

    pub(crate) fn cancel(&self) {
        self.lifecycle.cancel();
    }

    pub(crate) async fn cancelled(&self) {
        self.lifecycle.wait_cancelled().await;
    }

    pub(crate) async fn wait_finished(&self) {
        self.lifecycle.wait_finished().await;
    }

    pub(crate) fn execution_guard(&self) -> TurnExecutionGuard {
        TurnExecutionGuard(self.lifecycle.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueAdmission {
    Queued,
    Full,
    Inactive,
}

pub(crate) enum BusyAdmission {
    Claimed {
        lease: TurnLease,
        entry: QueuedIncoming,
    },
    Queued(BusyMessageDecision),
    AtCapacity,
}

pub(crate) enum TurnCompletion {
    Stale,
    Completed {
        next: Option<(TurnLease, QueuedIncoming)>,
    },
}

impl TurnCompletion {
    #[cfg(test)]
    fn into_next(self) -> Option<(TurnLease, QueuedIncoming)> {
        match self {
            Self::Stale => None,
            Self::Completed { next } => next,
        }
    }

    #[cfg(test)]
    fn is_stale(&self) -> bool {
        matches!(self, Self::Stale)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SessionBarrier {
    Reset,
    ResetDrainingAfterFailure,
    IdleExpiry,
}

pub(crate) enum ResetAdmission {
    Started {
        lease: TurnLease,
        revoked_lease: Option<TurnLease>,
        released_dedup_keys: Vec<String>,
    },
    AlreadyInProgress {
        draining_after_failure: bool,
    },
    TransitionInProgress,
    AtCapacity,
}

#[derive(Clone)]
struct ActiveSession {
    lease: TurnLease,
    control: Option<Arc<dyn ActiveSessionControl>>,
    dedup_key: Option<String>,
    barrier: Option<SessionBarrier>,
    started_at: Instant,
}

pub struct BusySessionCoordinator {
    active: HashMap<String, ActiveSession>,
    pending: HashMap<String, VecDeque<QueuedIncoming>>,
    next_turn_id: u64,
    busy_ack_ts: HashMap<String, Instant>,
    ack_cooldown: Duration,
    resource_limits_enabled: bool,
}

impl Default for BusySessionCoordinator {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl BusySessionCoordinator {
    pub fn new(ack_cooldown: Duration) -> Self {
        Self::with_resource_limits(ack_cooldown, false)
    }

    pub fn with_resource_limits(ack_cooldown: Duration, resource_limits_enabled: bool) -> Self {
        Self {
            active: HashMap::new(),
            pending: HashMap::new(),
            next_turn_id: 0,
            busy_ack_ts: HashMap::new(),
            ack_cooldown,
            resource_limits_enabled,
        }
    }

    fn next_lease(&mut self, session_key: &str) -> TurnLease {
        self.next_turn_id = self.next_turn_id.wrapping_add(1);
        TurnLease {
            session_key: session_key.to_string(),
            turn_id: self.next_turn_id,
            lifecycle: Arc::new(TurnLifecycle::new()),
        }
    }

    fn has_lane_capacity(&self, session_key: &str) -> bool {
        !self.resource_limits_enabled
            || self.active.contains_key(session_key)
            || self.active.len() < MAX_ACTIVE_SESSION_LANES
    }

    pub(crate) fn mark_active_at(
        &mut self,
        session_key: impl Into<String>,
        control: Option<Arc<dyn ActiveSessionControl>>,
        now: Instant,
    ) -> TurnLease {
        self.mark_active_at_with_dedup(session_key.into(), control, now, None)
    }

    fn mark_active_at_with_dedup(
        &mut self,
        session_key: String,
        control: Option<Arc<dyn ActiveSessionControl>>,
        now: Instant,
        dedup_key: Option<String>,
    ) -> TurnLease {
        let lease = self.next_lease(&session_key);
        self.active.insert(
            session_key,
            ActiveSession {
                lease: lease.clone(),
                control,
                dedup_key,
                barrier: None,
                started_at: now,
            },
        );
        lease
    }

    pub(crate) fn mark_active(
        &mut self,
        session_key: impl Into<String>,
        control: Option<Arc<dyn ActiveSessionControl>>,
    ) -> TurnLease {
        self.mark_active_at(session_key, control, Instant::now())
    }

    /// Atomically claim an idle session lane or append to the current lane.
    ///
    /// The returned lease is the sole authority allowed to complete the active
    /// turn. Since the active marker is installed while holding this lock, two
    /// first messages cannot both invoke a handler.
    pub(crate) fn claim_idle_or_enqueue(
        &mut self,
        session_key: &str,
        entry: QueuedIncoming,
        mode: BusyInputMode,
        now: Instant,
    ) -> BusyAdmission {
        if !self.has_lane_capacity(session_key) {
            return BusyAdmission::AtCapacity;
        }
        let Some(active) = self.active.get(session_key).cloned() else {
            let dedup_key = entry.dedup_key().map(str::to_string);
            return BusyAdmission::Claimed {
                lease: self.mark_active_at_with_dedup(
                    session_key.to_string(),
                    None,
                    now,
                    dedup_key,
                ),
                entry,
            };
        };

        let text = entry.incoming().text.clone();
        let decision = match mode {
            BusyInputMode::Queue => {
                if !self.queue_entry(session_key, entry) {
                    return BusyAdmission::AtCapacity;
                }
                BusyMessageDecision {
                    handled: true,
                    queued: true,
                    interrupted: false,
                    steered: false,
                    ack: self.should_ack_at(session_key, now).then(|| {
                        "Queued for the next turn. I will respond once the current task finishes."
                            .to_string()
                    }),
                }
            }
            BusyInputMode::Interrupt => {
                if !self.queue_entry(session_key, entry) {
                    return BusyAdmission::AtCapacity;
                }
                let interrupted = active
                    .control
                    .as_deref()
                    .is_some_and(|control| interrupt_control(control, &text));
                BusyMessageDecision {
                    handled: true,
                    queued: true,
                    interrupted,
                    steered: false,
                    ack: self.should_ack_at(session_key, now).then(|| {
                        if interrupted {
                            interrupt_ack(active.started_at, active.control.as_deref(), now)
                        } else {
                            "Queued for the next turn. I will respond once the current task finishes."
                                .to_string()
                        }
                    }),
                }
            }
            BusyInputMode::Steer => {
                let steered = active
                    .control
                    .as_deref()
                    .is_some_and(|control| steer_control(control, &text));
                if steered {
                    BusyMessageDecision {
                        handled: true,
                        queued: false,
                        interrupted: false,
                        steered: true,
                        ack: self.should_ack_at(session_key, now).then(|| {
                            "Steered the running task with your latest message.".to_string()
                        }),
                    }
                } else {
                    if !self.queue_entry(session_key, entry) {
                        return BusyAdmission::AtCapacity;
                    }
                    BusyMessageDecision {
                        handled: true,
                        queued: true,
                        interrupted: false,
                        steered: false,
                        ack: self.should_ack_at(session_key, now).then(|| {
                            "Queued for the next turn. I will respond once the current task finishes."
                                .to_string()
                        }),
                    }
                }
            }
        };
        BusyAdmission::Queued(decision)
    }

    /// Complete an owned turn and atomically claim its FIFO successor.
    pub(crate) fn complete_turn(
        &mut self,
        lease: &TurnLease,
        _outcome: ProcessingOutcome,
    ) -> TurnCompletion {
        let is_owner = self
            .active
            .get(lease.session_key())
            .is_some_and(|active| active.lease == *lease);
        if !is_owner {
            return TurnCompletion::Stale;
        }

        self.active.remove(lease.session_key());
        self.busy_ack_ts.remove(lease.session_key());
        let next = self.pop_pending(lease.session_key()).map(|entry| {
            let dedup_key = entry.dedup_key().map(str::to_string);
            let next = self.mark_active_at_with_dedup(
                lease.session_key().to_string(),
                None,
                Instant::now(),
                dedup_key,
            );
            (next, entry)
        });
        TurnCompletion::Completed { next }
    }

    pub(crate) fn mark_reset_draining_after_failure(&mut self, lease: &TurnLease) -> bool {
        let Some(active) = self.active.get_mut(lease.session_key()) else {
            return false;
        };
        if active.lease != *lease || active.barrier != Some(SessionBarrier::Reset) {
            return false;
        }
        active.barrier = Some(SessionBarrier::ResetDrainingAfterFailure);
        true
    }

    /// Revoke the old lease, preserve its queued ingress, and install an
    /// active reset barrier. New and existing queued ingress stay behind the
    /// barrier until the revoked worker terminates and reset work completes.
    pub(crate) fn begin_reset_session(&mut self, session_key: &str) -> ResetAdmission {
        if !self.has_lane_capacity(session_key) {
            return ResetAdmission::AtCapacity;
        }
        if let Some(barrier) = self
            .active
            .get(session_key)
            .and_then(|active| active.barrier)
        {
            return match barrier {
                SessionBarrier::Reset => ResetAdmission::AlreadyInProgress {
                    draining_after_failure: false,
                },
                SessionBarrier::ResetDrainingAfterFailure => ResetAdmission::AlreadyInProgress {
                    draining_after_failure: true,
                },
                SessionBarrier::IdleExpiry => ResetAdmission::TransitionInProgress,
            };
        }

        let mut released_dedup_keys = Vec::new();
        let revoked_lease = self.active.remove(session_key).map(|active| {
            let revoked_lease = active.lease.clone();
            revoked_lease.cancel();
            if let Some(key) = active.dedup_key {
                released_dedup_keys.push(key);
            }
            if let Some(control) = active.control {
                let _ = interrupt_control(
                    control.as_ref(),
                    "Session reset requested; cancel the active gateway turn.",
                );
            }
            revoked_lease
        });
        self.busy_ack_ts.remove(session_key);
        let lease = self.next_lease(session_key);
        self.active.insert(
            session_key.to_string(),
            ActiveSession {
                lease: lease.clone(),
                control: None,
                dedup_key: None,
                barrier: Some(SessionBarrier::Reset),
                started_at: Instant::now(),
            },
        );
        ResetAdmission::Started {
            lease,
            revoked_lease,
            released_dedup_keys,
        }
    }

    /// Install a boundary for idle expiry only when the lane is fully idle.
    pub(crate) fn begin_idle_expiry(&mut self, session_key: &str) -> Option<TurnLease> {
        if self.active.contains_key(session_key)
            || self
                .pending
                .get(session_key)
                .is_some_and(|pending| !pending.is_empty())
        {
            return None;
        }
        if !self.has_lane_capacity(session_key) {
            return None;
        }
        let lease = self.next_lease(session_key);
        self.active.insert(
            session_key.to_string(),
            ActiveSession {
                lease: lease.clone(),
                control: None,
                dedup_key: None,
                barrier: Some(SessionBarrier::IdleExpiry),
                started_at: Instant::now(),
            },
        );
        Some(lease)
    }

    pub fn is_active(&self, session_key: &str) -> bool {
        self.active.contains_key(session_key)
    }

    pub(crate) fn is_current(&self, lease: &TurnLease) -> bool {
        self.active
            .get(lease.session_key())
            .is_some_and(|active| active.lease == *lease)
    }

    pub(crate) fn attach_control(
        &mut self,
        lease: &TurnLease,
        control: Arc<dyn ActiveSessionControl>,
    ) -> bool {
        let Some(active) = self.active.get_mut(lease.session_key()) else {
            return false;
        };
        if active.lease != *lease {
            return false;
        }
        active.control = Some(control);
        true
    }

    pub fn interrupt_active(&mut self, session_key: &str, message: &str) -> bool {
        let Some(active) = self.active.get(session_key) else {
            return false;
        };
        let Some(control) = active.control.as_ref() else {
            return false;
        };
        interrupt_control(control.as_ref(), message)
    }

    pub(crate) fn interrupt_if_current(&mut self, lease: &TurnLease, message: &str) -> bool {
        if !self.is_current(lease) {
            return false;
        }
        self.interrupt_active(lease.session_key(), message)
    }

    pub(crate) fn enqueue_if_active(
        &mut self,
        session_key: &str,
        entry: QueuedIncoming,
    ) -> QueueAdmission {
        if !self.active.contains_key(session_key) {
            return QueueAdmission::Inactive;
        }
        if self.queue_entry(session_key, entry) {
            QueueAdmission::Queued
        } else {
            QueueAdmission::Full
        }
    }

    pub fn pending_len(&self, session_key: &str) -> usize {
        self.pending
            .get(session_key)
            .map(VecDeque::len)
            .unwrap_or(0)
    }

    fn pop_pending(&mut self, session_key: &str) -> Option<QueuedIncoming> {
        let queue = self.pending.get_mut(session_key)?;
        let entry = queue.pop_front();
        if queue.is_empty() {
            self.pending.remove(session_key);
        }
        entry
    }

    fn should_ack_at(&mut self, session_key: &str, now: Instant) -> bool {
        let should = self
            .busy_ack_ts
            .get(session_key)
            .map(|last| now.duration_since(*last) >= self.ack_cooldown)
            .unwrap_or(true);
        if should {
            self.busy_ack_ts.insert(session_key.to_string(), now);
        }
        should
    }

    fn queue_entry(&mut self, session_key: &str, entry: QueuedIncoming) -> bool {
        let queue = self.pending.entry(session_key.to_string()).or_default();
        if self.resource_limits_enabled && queue.len() >= MAX_PENDING_PER_SESSION {
            return false;
        }
        queue.push_back(entry);
        true
    }
    /// Snapshot active controls before publishing the shutdown signal.
    pub(crate) fn active_controls_for_shutdown(&self) -> Vec<Arc<dyn ActiveSessionControl>> {
        self.active
            .values()
            .filter_map(|active| active.control.clone())
            .collect()
    }

    /// Cancel every lane and discard queued ingress after runtime workers stop.
    pub(crate) fn abort_all_for_shutdown(&mut self) -> Vec<String> {
        let mut released_dedup_keys = Vec::new();
        for (_, active) in self.active.drain() {
            if let Some(key) = active.dedup_key {
                released_dedup_keys.push(key);
            }
        }
        for (_, pending) in self.pending.drain() {
            released_dedup_keys.extend(
                pending
                    .into_iter()
                    .filter_map(|entry| entry.dedup_key().map(str::to_string)),
            );
        }
        self.busy_ack_ts.clear();
        released_dedup_keys
    }
}

fn interrupt_ack(
    started_at: Instant,
    control: Option<&dyn ActiveSessionControl>,
    now: Instant,
) -> String {
    let mut text = "Interrupting current task with your latest message.".to_string();
    let elapsed = now.duration_since(started_at);
    if elapsed >= Duration::from_secs(60) {
        text.push_str(&format!(" Running for {} min.", elapsed.as_secs() / 60));
    }
    if let Some(summary) = control.and_then(control_activity_summary) {
        if let (Some(done), Some(max)) = (summary.api_call_count, summary.max_iterations) {
            text.push_str(&format!(" Progress: {done}/{max}."));
        }
        if let Some(tool) = summary.current_tool.filter(|s| !s.trim().is_empty()) {
            text.push_str(&format!(" Current tool: {tool}."));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(thread_id: &str) -> SessionSource {
        SessionSource::new("telegram", "-1001", "group").with_thread(thread_id)
    }

    fn event(text: &str, thread_id: &str) -> MessageEvent {
        MessageEvent::text(text, source(thread_id))
    }

    #[test]
    fn session_keys_preserve_distinct_topics() {
        assert_eq!(build_session_key(&source("10")), "telegram:-1001:10");
        assert_ne!(
            build_session_key(&source("10")),
            build_session_key(&source("11"))
        );
    }

    #[test]
    fn session_source_scope_id_is_canonical_and_guild_id_is_alias() {
        let scoped = SessionSource::new("relay", "C1", "channel").with_scope("T1");
        assert_eq!(scoped.scope_id.as_deref(), Some("T1"));
        assert_eq!(scoped.guild_id.as_deref(), Some("T1"));
        assert_eq!(scoped.scope_id(), Some("T1"));
        assert_eq!(build_session_key(&scoped), "relay:T1:C1");

        let legacy = SessionSource::new("relay", "C1", "channel").with_guild_id("G1");
        assert_eq!(legacy.scope_id.as_deref(), Some("G1"));
        assert_eq!(legacy.guild_id.as_deref(), Some("G1"));
        assert_eq!(build_session_key(&legacy), "relay:G1:C1");
    }

    #[test]
    fn command_parsing_strips_bot_suffix_and_lowercases() {
        let event = event("/RESET@TigerNanoBot now", "10");
        assert!(event.is_command());
        assert_eq!(event.get_command().as_deref(), Some("reset"));
        assert_eq!(event.get_command_args(), "now");
        assert_eq!(
            command_name_from_text("/reload-mcp").as_deref(),
            Some("reload_mcp")
        );
        assert_eq!(
            command_name_from_text("/path/to/file.py").as_deref(),
            Some("path/to/file.py")
        );
    }

    #[test]
    fn bypass_only_recognized_commands() {
        assert!(event_bypasses_active_session(&event("/stop", "10")));
        assert!(event_bypasses_active_session(&event("/model claude", "10")));
        assert!(event_bypasses_active_session(&event("/reload-mcp", "10")));
        assert!(!event_bypasses_active_session(&event("/foobar", "10")));
        assert!(!event_bypasses_active_session(&event(
            "/path/to/file.py",
            "10"
        )));
    }

    #[derive(Default)]
    struct MockControl {
        interrupts: std::sync::atomic::AtomicUsize,
        steer_accepts: bool,
    }

    impl ActiveSessionControl for MockControl {
        fn interrupt(&self, _message: &str) {
            self.interrupts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }

        fn steer(&self, _message: &str) -> bool {
            self.steer_accepts
        }

        fn activity_summary(&self) -> Option<AgentActivitySummary> {
            Some(AgentActivitySummary {
                api_call_count: Some(21),
                max_iterations: Some(60),
                current_tool: Some("terminal".to_string()),
                elapsed: None,
                seconds_since_activity: Some(0.5),
            })
        }
    }

    struct PanickingControl;

    impl ActiveSessionControl for PanickingControl {
        fn interrupt(&self, _message: &str) {
            panic!("deterministic control interrupt panic");
        }

        fn steer(&self, _message: &str) -> bool {
            panic!("deterministic control steer panic");
        }

        fn activity_summary(&self) -> Option<AgentActivitySummary> {
            panic!("deterministic control summary panic");
        }
    }

    fn queued_entry(text: &str, sender: IncomingSender) -> QueuedIncoming {
        QueuedIncoming::from_validated_ingress(
            IncomingMessage {
                platform: "telegram".to_string(),
                chat_id: "-1001".to_string(),
                user_id: "sender".to_string(),
                text: text.to_string(),
                message_id: None,
                thread_id: None,
                is_dm: false,
            },
            sender,
        )
    }

    #[test]
    fn claim_or_enqueue_is_atomic_and_preserves_fifo() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();

        let BusyAdmission::Claimed {
            lease: first_lease, ..
        } = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("first", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        )
        else {
            panic!("first ingress must claim the idle lane");
        };
        let BusyAdmission::Queued(first_queue) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("second", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("second ingress must queue behind the active turn");
        };
        let BusyAdmission::Queued(second_queue) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("third", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("third ingress must queue behind the active turn");
        };

        assert!(first_queue.queued);
        assert!(second_queue.queued);
        assert_eq!(coordinator.pending_len(&key), 2);

        let (second_lease, second) = coordinator
            .complete_turn(&first_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("first completion must atomically claim its successor");
        assert_eq!(second.incoming().text, "second");
        assert!(coordinator.is_current(&second_lease));
        assert_eq!(coordinator.pending_len(&key), 1);

        let (third_lease, third) = coordinator
            .complete_turn(&second_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("second completion must atomically claim its successor");
        assert_eq!(third.incoming().text, "third");
        assert!(coordinator.is_current(&third_lease));
        assert_eq!(coordinator.pending_len(&key), 0);

        assert!(coordinator
            .complete_turn(&third_lease, ProcessingOutcome::Success)
            .into_next()
            .is_none());
        assert!(!coordinator.is_active(&key));
    }

    #[test]
    fn fifo_drain_preserves_more_than_thirty_two_entries() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let BusyAdmission::Claimed {
            mut lease,
            entry: first,
        } = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("item-0", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        )
        else {
            panic!("first ingress must claim the idle lane");
        };
        let mut observed = vec![first.incoming().text.clone()];

        for index in 1..40 {
            let entry = queued_entry(&format!("item-{index}"), IncomingSender::human());
            assert!(matches!(
                coordinator.claim_idle_or_enqueue(&key, entry, BusyInputMode::Queue, now,),
                BusyAdmission::Queued(_)
            ));
        }

        while let Some((next_lease, entry)) = coordinator
            .complete_turn(&lease, ProcessingOutcome::Success)
            .into_next()
        {
            observed.push(entry.incoming().text.clone());
            lease = next_lease;
        }

        let expected: Vec<String> = (0..40).map(|index| format!("item-{index}")).collect();
        assert_eq!(observed, expected);
        assert!(!coordinator.is_active(&key));
        assert_eq!(coordinator.pending_len(&key), 0);
    }

    #[test]
    fn queued_envelopes_retain_verified_sender_identity() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let BusyAdmission::Claimed {
            lease: first_lease, ..
        } = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("first", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        )
        else {
            panic!("first ingress must claim the lane");
        };
        let BusyAdmission::Queued(_) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("bot", IncomingSender::bot()),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("bot ingress must queue");
        };
        let BusyAdmission::Queued(_) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("human", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("human ingress must queue");
        };

        let (bot_lease, bot) = coordinator
            .complete_turn(&first_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("bot item must be first in FIFO order");
        assert_eq!(bot.sender(), IncomingSender::bot());
        let (_, human) = coordinator
            .complete_turn(&bot_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("human item must remain after bot item");
        assert_eq!(human.sender(), IncomingSender::human());
    }

    #[test]
    fn interrupt_mode_keeps_the_entry_and_reports_progress() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let control = Arc::new(MockControl::default());
        let _lease =
            coordinator.mark_active_at(&key, Some(control.clone()), now - Duration::from_secs(600));

        let BusyAdmission::Queued(decision) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("Are you working?", IncomingSender::human()),
            BusyInputMode::Interrupt,
            now,
        ) else {
            panic!("interrupt must queue behind the active turn");
        };

        assert!(decision.interrupted);
        assert!(decision.queued);
        assert_eq!(
            control
                .interrupts
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let ack = decision
            .ack
            .expect("first busy message must be acknowledged");
        assert!(ack.contains("21/60"));
        assert!(ack.contains("terminal"));
        assert!(ack.contains("10 min"));
    }

    #[test]
    fn reset_revokes_leases_and_preserves_queued_work() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = build_session_key(&source("10"));
        let now = Instant::now();
        let BusyAdmission::Claimed {
            lease: first_lease, ..
        } = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("first", IncomingSender::human())
                .with_dedup_key(Some("active-key".to_string())),
            BusyInputMode::Queue,
            now,
        )
        else {
            panic!("first ingress must claim the lane");
        };
        let control = Arc::new(MockControl::default());
        assert!(coordinator.attach_control(&first_lease, control.clone()));
        let BusyAdmission::Queued(_) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("queued before reset", IncomingSender::human())
                .with_dedup_key(Some("pending-key".to_string())),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("follow-up must queue");
        };

        let ResetAdmission::Started {
            lease: reset_lease,
            revoked_lease,
            released_dedup_keys,
        } = coordinator.begin_reset_session(&key)
        else {
            panic!("first reset must install a barrier");
        };
        assert_eq!(revoked_lease.as_ref(), Some(&first_lease));
        assert_eq!(released_dedup_keys, ["active-key".to_string()]);

        assert!(!coordinator.is_current(&first_lease));
        assert!(coordinator.is_current(&reset_lease));
        assert_eq!(coordinator.pending_len(&key), 1);
        assert_eq!(
            control
                .interrupts
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(coordinator
            .complete_turn(&first_lease, ProcessingOutcome::Cancelled)
            .is_stale());
        assert!(matches!(
            coordinator.begin_reset_session(&key),
            ResetAdmission::AlreadyInProgress { .. }
        ));

        let BusyAdmission::Queued(decision) = coordinator.claim_idle_or_enqueue(
            &key,
            queued_entry("after reset", IncomingSender::human()),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("post-reset ingress must queue behind the barrier");
        };
        assert!(decision.queued);
        let (post_reset_lease, post_reset_entry) = coordinator
            .complete_turn(&reset_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("completing the reset barrier must promote queued ingress");
        assert_eq!(post_reset_entry.incoming().text, "queued before reset");
        assert_ne!(first_lease.turn_id, post_reset_lease.turn_id);
        let post_reset_control = Arc::new(MockControl::default());
        assert!(coordinator.attach_control(&post_reset_lease, post_reset_control.clone()));
        assert!(!coordinator.interrupt_if_current(
            &first_lease,
            "stale timeout must not interrupt promoted work",
        ));
        assert_eq!(
            post_reset_control
                .interrupts
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert!(coordinator.interrupt_if_current(
            &post_reset_lease,
            "current timeout should interrupt promoted work",
        ));
        assert_eq!(
            post_reset_control
                .interrupts
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        let (_, after_reset_entry) = coordinator
            .complete_turn(&post_reset_lease, ProcessingOutcome::Success)
            .into_next()
            .expect("preserved ingress must keep FIFO order ahead of new ingress");
        assert_eq!(after_reset_entry.incoming().text, "after reset");
    }

    #[test]
    fn panicking_control_cannot_wedge_reset_or_busy_admission() {
        let mut coordinator = BusySessionCoordinator::default();
        let key = "panic-control";
        let now = Instant::now();
        let BusyAdmission::Claimed { lease, .. } = coordinator.claim_idle_or_enqueue(
            key,
            queued_entry("first", IncomingSender::human())
                .with_dedup_key(Some("active".to_string())),
            BusyInputMode::Queue,
            now,
        ) else {
            panic!("first turn must claim the lane");
        };
        assert!(coordinator.attach_control(&lease, Arc::new(PanickingControl)));
        assert!(!coordinator.interrupt_active(key, "interrupt"));
        let BusyAdmission::Queued(decision) = coordinator.claim_idle_or_enqueue(
            key,
            queued_entry("steer", IncomingSender::human())
                .with_dedup_key(Some("pending".to_string())),
            BusyInputMode::Steer,
            now,
        ) else {
            panic!("failed steering must queue");
        };
        assert!(decision.queued);
        assert!(!decision.steered);
        assert!(interrupt_ack(now, Some(&PanickingControl), now).contains("Interrupting"));

        let ResetAdmission::Started {
            lease: reset_lease,
            revoked_lease,
            released_dedup_keys,
        } = coordinator.begin_reset_session(key)
        else {
            panic!("reset must survive control panic");
        };
        assert_eq!(revoked_lease.as_ref(), Some(&lease));
        assert_eq!(released_dedup_keys, ["active".to_string()]);
        assert!(coordinator.is_current(&reset_lease));
        assert_eq!(coordinator.pending_len(key), 1);
    }

    #[tokio::test]
    async fn turn_lifecycle_remembers_cancel_and_finish_before_waiters_subscribe() {
        let mut coordinator = BusySessionCoordinator::default();
        let lease = coordinator.mark_active("lifecycle", None);

        lease.cancel();
        tokio::time::timeout(Duration::from_millis(50), lease.cancelled())
            .await
            .expect("late cancellation waiter must observe the latched state");

        drop(lease.execution_guard());
        tokio::time::timeout(Duration::from_millis(50), lease.wait_finished())
            .await
            .expect("late completion waiter must observe the latched state");
    }

    #[test]
    fn reset_during_idle_expiry_remains_retryable() {
        let mut coordinator = BusySessionCoordinator::default();
        let expiry_lease = coordinator
            .begin_idle_expiry("transition")
            .expect("idle expiry should install its barrier");
        assert!(matches!(
            coordinator.begin_reset_session("transition"),
            ResetAdmission::TransitionInProgress
        ));
        assert!(matches!(
            coordinator.complete_turn(&expiry_lease, ProcessingOutcome::Success),
            TurnCompletion::Completed { next: None }
        ));
        assert!(matches!(
            coordinator.begin_reset_session("transition"),
            ResetAdmission::Started { .. }
        ));
    }

    #[test]
    fn active_lane_and_pending_queue_limits_are_opt_in() {
        let now = Instant::now();
        let mut unlimited = BusySessionCoordinator::default();
        for index in 0..=MAX_ACTIVE_SESSION_LANES {
            assert!(matches!(
                unlimited.claim_idle_or_enqueue(
                    &format!("unlimited-{index}"),
                    queued_entry("first", IncomingSender::human()),
                    BusyInputMode::Queue,
                    now,
                ),
                BusyAdmission::Claimed { .. }
            ));
        }
        for index in 0..=MAX_PENDING_PER_SESSION {
            assert!(matches!(
                unlimited.claim_idle_or_enqueue(
                    "unlimited-0",
                    queued_entry(&format!("queued-{index}"), IncomingSender::human()),
                    BusyInputMode::Queue,
                    now,
                ),
                BusyAdmission::Queued(_)
            ));
        }

        let mut limited =
            BusySessionCoordinator::with_resource_limits(Duration::from_secs(30), true);
        for index in 0..MAX_ACTIVE_SESSION_LANES {
            assert!(matches!(
                limited.claim_idle_or_enqueue(
                    &format!("limited-{index}"),
                    queued_entry("first", IncomingSender::human()),
                    BusyInputMode::Queue,
                    now,
                ),
                BusyAdmission::Claimed { .. }
            ));
        }
        assert!(matches!(
            limited.claim_idle_or_enqueue(
                "one-too-many",
                queued_entry("first", IncomingSender::human()),
                BusyInputMode::Queue,
                now,
            ),
            BusyAdmission::AtCapacity
        ));
        for index in 0..MAX_PENDING_PER_SESSION {
            assert!(matches!(
                limited.claim_idle_or_enqueue(
                    "limited-0",
                    queued_entry(&format!("queued-{index}"), IncomingSender::human()),
                    BusyInputMode::Queue,
                    now,
                ),
                BusyAdmission::Queued(_)
            ));
        }
        assert!(matches!(
            limited.enqueue_if_active(
                "limited-0",
                queued_entry("queue overflow", IncomingSender::human()),
            ),
            QueueAdmission::Full
        ));
    }

    #[test]
    fn reset_and_expiry_barriers_respect_opted_in_lane_capacity() {
        let now = Instant::now();
        let mut coordinator =
            BusySessionCoordinator::with_resource_limits(Duration::from_secs(30), true);
        for index in 0..MAX_ACTIVE_SESSION_LANES {
            assert!(matches!(
                coordinator.claim_idle_or_enqueue(
                    &format!("lane-{index}"),
                    queued_entry("first", IncomingSender::human()),
                    BusyInputMode::Queue,
                    now,
                ),
                BusyAdmission::Claimed { .. }
            ));
        }

        assert!(matches!(
            coordinator.begin_reset_session("new-reset"),
            ResetAdmission::AtCapacity
        ));
        assert!(coordinator.begin_idle_expiry("new-expiry").is_none());
        assert!(matches!(
            coordinator.begin_reset_session("lane-0"),
            ResetAdmission::Started { .. }
        ));
    }
}
