use base64::Engine as _;
use futures::{FutureExt, SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, watch, Mutex as AsyncMutex, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{client_async_tls_with_config, MaybeTlsStream, WebSocketStream};

const DISCORD_GATEWAY_V10_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
const DISCORD_GATEWAY_BACKOFF_STEPS_MS: &[u64] = &[100, 250, 500, 1_000, 2_000];
const DISCORD_INBOUND_RETRY_MS: u64 = 50;
const DISCORD_THREAD_CONTEXT_CAPACITY: usize = 1_024;
const DISCORD_THREAD_RESOLVER_CAPACITY: usize = 64;
const DISCORD_IDENTIFY_MIN_INTERVAL_MS: u64 = 5_000;
const DISCORD_IDENTIFY_BUDGET_WINDOW_SECS: u64 = 24 * 60 * 60;
const DISCORD_GATEWAY_IO_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS);
const DISCORD_HEARTBEAT_READY_FRAME_DRAIN_CAP: usize = 64;
const DISCORD_ORDERED_INGRESS_SHUTDOWN_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);

const DISCORD_INVALID_SESSION_MIN_DELAY_MS: u64 = 1_000;
const DISCORD_INVALID_SESSION_MAX_DELAY_MS: u64 = 5_000;

type DiscordInvalidSessionDelay = Arc<dyn Fn() -> std::time::Duration + Send + Sync>;

fn discord_invalid_session_delay_from_unit(unit: f64) -> std::time::Duration {
    let span_ms = DISCORD_INVALID_SESSION_MAX_DELAY_MS - DISCORD_INVALID_SESSION_MIN_DELAY_MS;
    let offset_ms = (unit.clamp(0.0, 1.0) * span_ms as f64).floor() as u64;
    std::time::Duration::from_millis(DISCORD_INVALID_SESSION_MIN_DELAY_MS + offset_ms)
}

fn discord_random_invalid_session_delay() -> std::time::Duration {
    discord_invalid_session_delay_from_unit(rand::random())
}

trait DiscordTransportIo: AsyncRead + AsyncWrite {}
impl<T: AsyncRead + AsyncWrite + ?Sized> DiscordTransportIo for T {}

type DiscordTransport = Box<dyn DiscordTransportIo + Send + Unpin>;
type DiscordWebSocket = WebSocketStream<MaybeTlsStream<DiscordTransport>>;

/// Payload builder shared by the REST adapter helpers and the owned Gateway
/// runtime. The builder is deliberately independent of the adapter so the
/// socket task never retains an adapter or Gateway reference.
#[derive(Debug, Clone)]
struct DiscordGatewayPayloadBuilder {
    token: String,
    intents: u64,
}

impl DiscordGatewayPayloadBuilder {
    fn from_config(config: &DiscordConfig) -> Self {
        Self {
            token: config.token.clone(),
            intents: config.gateway_intents(),
        }
    }

    fn identify(&self) -> GatewayPayload {
        let data = IdentifyData {
            token: self.token.clone(),
            intents: self.intents,
            properties: IdentifyProperties {
                os: std::env::consts::OS.to_string(),
                browser: "hermes".to_string(),
                device: "hermes".to_string(),
            },
        };
        GatewayPayload {
            op: opcodes::IDENTIFY,
            d: Some(
                serde_json::to_value(data).expect("Discord IDENTIFY payload is always serializable"),
            ),
            s: None,
            t: None,
        }
    }

    fn resume(&self, session_id: &str, sequence: u64) -> GatewayPayload {
        let data = ResumeData {
            token: self.token.clone(),
            session_id: session_id.to_string(),
            seq: sequence,
        };
        GatewayPayload {
            op: opcodes::RESUME,
            d: Some(
                serde_json::to_value(data).expect("Discord RESUME payload is always serializable"),
            ),
            s: None,
            t: None,
        }
    }

    fn heartbeat(sequence: Option<u64>) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HEARTBEAT,
            // Discord requires an explicit null when there is no sequence yet.
            d: Some(
                sequence
                    .map(|value| serde_json::Value::Number(value.into()))
                    .unwrap_or(serde_json::Value::Null),
            ),
            s: None,
            t: None,
        }
    }
}

/// A Discord dispatch that passed the adapter's local ingress policy.
///
/// The message remains platform-native until the CLI boundary so it retains
/// guild, DM, and thread context. `sender_is_bot` is derived from the
/// authenticated Discord Gateway dispatch rather than guessed from a username.
#[derive(Debug)]
pub struct DiscordInboundMessage {
    message: IncomingDiscordMessage,
    sender_is_bot: bool,
    /// The READY-authenticated bot identity used for mention admission. This
    /// must survive a full CLI queue and shutdown replay.
    client_user_id: Option<String>,
}

impl DiscordInboundMessage {
    /// Convert this verified Discord event into the gateway-neutral message.
    pub fn to_gateway_message(&self) -> Option<crate::gateway::IncomingMessage> {
        self.message.to_gateway_message()
    }

    /// Whether Discord identified the sender as a bot or webhook.
    pub fn sender_is_bot(&self) -> bool {
        self.sender_is_bot
    }

    /// Build sender provenance from authenticated Discord Gateway fields.
    ///
    /// Member role IDs are never derived from message content or display names.
    pub fn sender_provenance(&self) -> crate::gateway::IncomingSender {
        let sender = if self.sender_is_bot {
            crate::gateway::IncomingSender::bot()
        } else {
            crate::gateway::IncomingSender::verified_discord_member(
                self.message.member_role_ids.clone(),
                self.message.guild_id.clone(),
            )
        };
        sender.with_discord_message_reply(self.message.message_type == 19)
    }

    /// Access the complete Discord message context before neutral conversion.
    pub fn message(&self) -> &IncomingDiscordMessage {
        &self.message
    }

    /// Build a durable record only after ordered thread resolution and local
    /// mention/channel policy admitted this inbound message to the CLI.
    pub fn journal_entry(&self) -> DiscordJournalEntry {
        DiscordJournalEntry::new_post_policy(self.message.clone(), self.client_user_id.clone())
    }

    /// Rebuild trusted ingress only from a post-policy record.
    ///
    /// Raw journal records are intentionally not routable at the CLI boundary:
    /// they must re-enter the ordered Discord resolver after authentication.
    pub fn from_post_policy_journal_entry(
        entry: DiscordJournalEntry,
    ) -> Result<Self, GatewayError> {
        if !entry.is_post_policy() {
            return Err(GatewayError::Platform(
                "Discord pre-policy journal entry cannot bypass ordered ingress resolution"
                    .to_string(),
            ));
        }
        let sender_is_bot = entry.message.is_bot;
        Ok(Self {
            message: entry.message,
            sender_is_bot,
            client_user_id: entry.client_user_id,
        })
    }
}

/// The point at which a durable Discord record became safe to replay.
///
/// Old spool records default to [`PrePolicy`], which is deliberately
/// conservative: they must re-enter Discord's authenticated ordered resolver
/// rather than being routed directly by the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscordIngressJournalPhase {
    PrePolicy,
    PostPolicy,
}

impl Default for DiscordIngressJournalPhase {
    fn default() -> Self {
        Self::PrePolicy
    }
}

/// Raw, attributable Discord ingress persisted before thread lookup or local
/// routing policy. Safe pre-policy records may be re-resolved after READY.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DiscordJournalEntry {
    message: IncomingDiscordMessage,
    client_user_id: Option<String>,
    #[serde(default)]
    phase: DiscordIngressJournalPhase,
}

impl DiscordJournalEntry {
    /// Construct a raw record that must re-enter ordered resolver/policy.
    pub fn new(message: IncomingDiscordMessage, client_user_id: Option<String>) -> Self {
        Self {
            message,
            client_user_id,
            phase: DiscordIngressJournalPhase::PrePolicy,
        }
    }

    fn new_post_policy(message: IncomingDiscordMessage, client_user_id: Option<String>) -> Self {
        Self {
            message,
            client_user_id,
            phase: DiscordIngressJournalPhase::PostPolicy,
        }
    }

    pub fn message(&self) -> &IncomingDiscordMessage {
        &self.message
    }

    pub fn client_user_id(&self) -> Option<&str> {
        self.client_user_id.as_deref()
    }

    pub fn phase(&self) -> DiscordIngressJournalPhase {
        self.phase
    }

    pub fn is_pre_policy(&self) -> bool {
        self.phase == DiscordIngressJournalPhase::PrePolicy
    }

    pub fn is_post_policy(&self) -> bool {
        self.phase == DiscordIngressJournalPhase::PostPolicy
    }
}

 

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordIngressReservationOutcome {
    Recorded,
    /// The CLI spool still holds the raw journal record; this worker owns the
    /// next resolution attempt after an in-process restart.
    ExistingUnresolved,
    /// A route task or terminal quarantine already owns this message ID.
    ExistingRouteOwned,
}

#[derive(Debug)]
pub struct DiscordIngressReservation {
    entry: DiscordJournalEntry,
    durability_ack:
        Option<tokio::sync::oneshot::Sender<DiscordIngressReservationOutcome>>,
}

impl DiscordIngressReservation {
    pub fn entry(&self) -> &DiscordJournalEntry {
        &self.entry
    }

    pub fn acknowledge_durable(&mut self, outcome: DiscordIngressReservationOutcome) {
        if let Some(ack) = self.durability_ack.take() {
            let _ = ack.send(outcome);
        }
    }
}

#[derive(Debug)]
pub enum DiscordIngressJournalEvent {
    Reserve(DiscordIngressReservation),
    Remove { message_id: String },
}


#[derive(Debug, Clone)]
struct DiscordUnresolvedIngress {
    message: IncomingDiscordMessage,
    client_user_id: Option<String>,
}

impl From<DiscordJournalEntry> for DiscordUnresolvedIngress {
    fn from(entry: DiscordJournalEntry) -> Self {
        Self {
            message: entry.message,
            client_user_id: entry.client_user_id,
        }
    }
}

impl From<&DiscordUnresolvedIngress> for DiscordJournalEntry {
    fn from(ingress: &DiscordUnresolvedIngress) -> Self {
        Self::new(ingress.message.clone(), ingress.client_user_id.clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DiscordSequenceAck {
    generation: u64,
    sequence: u64,
}

#[derive(Debug)]
enum DiscordOrderedIngress {
    Context {
        ack: Option<DiscordSequenceAck>,
        event_name: String,
        data: serde_json::Value,
    },
    Message {
        ack: Option<DiscordSequenceAck>,
        ingress: DiscordUnresolvedIngress,
        journaled: bool,
    },
    Commit(DiscordSequenceAck),
}

#[derive(Debug)]
enum DiscordThreadLookupError {
    Retry {
        retry_after: Option<std::time::Duration>,
        message: String,
    },
    /// The payload and authoritative channel record are irreconcilable, so
    /// replaying this ingress cannot make it safe to route.
    Rejected {
        message: String,
    },
    /// Credentials, permission, or another non-structural client failure must
    /// retain the durable record for an operator-driven recovery.
    PreserveAndStop {
        message: String,
    },
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscordCachedChannelContext {
    Root {
        guild_id: String,
    },
    Thread {
        guild_id: String,
        parent_channel_id: String,
    },
}

impl DiscordCachedChannelContext {
    fn guild_id(&self) -> &str {
        match self {
            Self::Root { guild_id } | Self::Thread { guild_id, .. } => guild_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiscordCachedChannelLookup {
    Missing,
    Context(DiscordCachedChannelContext),
    GuildMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordThreadResolution {
    Resolved,
    NeedsLookup,
    Rejected,
}

/// Bounded authoritative channel classification for Discord guild ingress.
#[derive(Debug, Default)]
struct DiscordThreadContextCache {
    channels: std::collections::HashMap<String, DiscordCachedChannelContext>,
    recency: VecDeque<String>,
}

impl DiscordThreadContextCache {
    fn record_thread(&mut self, guild_id: &str, thread_id: &str, parent_channel_id: &str) {
        let guild_id = guild_id.trim();
        let thread_id = thread_id.trim();
        let parent_channel_id = parent_channel_id.trim();
        if guild_id.is_empty() || thread_id.is_empty() || parent_channel_id.is_empty() {
            return;
        }
        self.record(
            thread_id,
            DiscordCachedChannelContext::Thread {
                guild_id: guild_id.to_string(),
                parent_channel_id: parent_channel_id.to_string(),
            },
        );
    }

    fn record_root(&mut self, guild_id: &str, channel_id: &str) {
        let guild_id = guild_id.trim();
        let channel_id = channel_id.trim();
        if !guild_id.is_empty() && !channel_id.is_empty() {
            self.record(
                channel_id,
                DiscordCachedChannelContext::Root {
                    guild_id: guild_id.to_string(),
                },
            );
        }
    }

    fn record(&mut self, channel_id: &str, context: DiscordCachedChannelContext) {
        self.channels.insert(channel_id.to_string(), context);
        self.recency.retain(|known| known != channel_id);
        self.recency.push_back(channel_id.to_string());
        while self.recency.len() > DISCORD_THREAD_CONTEXT_CAPACITY {
            if let Some(expired) = self.recency.pop_front() {
                self.channels.remove(&expired);
            }
        }
    }

    fn remove(&mut self, guild_id: &str, channel_id: &str) {
        let guild_id = guild_id.trim();
        let channel_id = channel_id.trim();
        if self
            .channels
            .get(channel_id)
            .is_some_and(|context| context.guild_id() == guild_id)
        {
            self.channels.remove(channel_id);
            self.recency.retain(|known| known != channel_id);
        }
    }

    fn context_for(&self, guild_id: &str, channel_id: &str) -> DiscordCachedChannelLookup {
        let guild_id = guild_id.trim();
        match self.channels.get(channel_id.trim()) {
            Some(context) if context.guild_id() == guild_id => {
                DiscordCachedChannelLookup::Context(context.clone())
            }
            Some(_) => DiscordCachedChannelLookup::GuildMismatch,
            None => DiscordCachedChannelLookup::Missing,
        }
    }

    /// Resolve an event which omitted its guild only from a cached channel
    /// record. The cache is keyed by Discord's globally unique channel ID, so
    /// this cannot merge authority from multiple guilds.
    fn context_for_channel(&self, channel_id: &str) -> Option<DiscordCachedChannelContext> {
        self.channels.get(channel_id.trim()).cloned()
    }
}

/// Discord's `GET /gateway/bot` session-start budget for this process.
///
/// The driver owns one shard, so the documented five-second per-bucket spacing
/// is always enforced even when this optional server budget is unavailable.
#[derive(Debug, Clone, Copy)]
struct DiscordGatewaySessionStartLimit {
    total: u64,
    remaining: u64,
    reset_after_ms: u64,
}

#[derive(Debug, Default)]
struct DiscordIdentifyPacer {
    next_identify_at: Option<tokio::time::Instant>,
    total: Option<u64>,
    remaining: Option<u64>,
    reset_at: Option<tokio::time::Instant>,
}

impl DiscordIdentifyPacer {
    fn configure(&mut self, limit: DiscordGatewaySessionStartLimit) {
        let now = tokio::time::Instant::now();
        let reset_after =
            std::time::Duration::from_millis(limit.reset_after_ms.max(1));
        self.total = Some(limit.total);
        self.remaining = Some(limit.remaining);
        self.reset_at = Some(now + reset_after);
    }

    fn refresh_budget_window(&mut self, now: tokio::time::Instant) {
        let Some(reset_at) = self.reset_at else {
            return;
        };
        if reset_at > now {
            return;
        }

        self.remaining = self.total;
        // `reset_after` is the countdown to the end of the current window,
        // not the duration of every future window. Once it elapses, replenish
        // once and conservatively re-arm Discord's documented daily budget.
        self.reset_at = Some(
            now + std::time::Duration::from_secs(DISCORD_IDENTIFY_BUDGET_WINDOW_SECS),
        );
    }

    fn next_deadline(&mut self) -> tokio::time::Instant {
        let now = tokio::time::Instant::now();
        self.refresh_budget_window(now);
        let budget_deadline = self
            .remaining
            .filter(|remaining| *remaining == 0)
            .and_then(|_| self.reset_at);
        self.next_identify_at
            .into_iter()
            .chain(budget_deadline)
            .max()
            .unwrap_or(now)
            .max(now)
    }

    /// Consume one IDENTIFY only when it can be sent now. Delayed windows are
    /// consumed after the wait rather than reserving an extra slot early.
    fn reserve_if_ready(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        self.refresh_budget_window(now);
        if self
            .remaining
            .is_some_and(|remaining| remaining == 0)
            || self
                .next_identify_at
                .is_some_and(|deadline| deadline > now)
        {
            return false;
        }

        self.next_identify_at = Some(
            now + std::time::Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS),
        );
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining = remaining.saturating_sub(1);
        }
        true
    }
}

/// Owns exactly one Discord Gateway v10 socket lifecycle.
///
/// The runtime coordinates configuration and the worker task; the worker owns
/// socket/session state and an outbound mpsc sender. The CLI owns the receiver
/// and invokes `Gateway`, which prevents a Gateway <-> adapter reference cycle.
struct DiscordGatewayRuntime {
    payloads: DiscordGatewayPayloadBuilder,
    gateway_url: String,
    proxy: AdapterProxyConfig,
    rest_client: Client,
    api_base_url: String,
    global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
    require_mention: bool,
    channel_controls: DiscordChannelControls,
    bot_policy: DiscordBotMessagePolicy,
    thread_participation: Arc<Mutex<DiscordThreadParticipationTracker>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<DiscordInboundMessage>>>>,
    journal_tx: Arc<RwLock<Option<mpsc::Sender<DiscordIngressJournalEvent>>>>,
    recovered_ingress: Arc<Mutex<std::collections::VecDeque<(DiscordJournalEntry, bool)>>>,
    /// A MESSAGE_CREATE that could not enter the bounded ordered stage. It
    /// stays behind every already-queued item until that stage drains.
    overflow_ingress: Arc<Mutex<std::collections::VecDeque<DiscordOrderedIngress>>>,
    thread_contexts: Arc<Mutex<DiscordThreadContextCache>>,
    identify_pacer: Arc<AsyncMutex<DiscordIdentifyPacer>>,
    /// Cancels socket intake and its ordered ingress stage. This is separate
    /// from outbound REST cancellation so accepted CLI-owned routes can drain.
    stop_tx: watch::Sender<bool>,
    /// Cancels outbound REST and liveness only during final adapter shutdown.
    outbound_stop_tx: watch::Sender<bool>,
    invalid_session_delay: DiscordInvalidSessionDelay,
    worker_alive: Arc<AtomicBool>,
    fatal: Arc<AtomicBool>,
    task: AsyncMutex<Option<tokio::task::JoinHandle<()>>>,
    /// Supervised ordered-ingress child. It stays runtime-owned so abort can
    /// await the recovery guard before CLI snapshots retained ingress.
    ingress_task: Arc<AsyncMutex<Option<tokio::task::JoinHandle<()>>>>,
}

/// Socket worker state deliberately excludes the runtime task handle.
///
/// A spawned worker therefore cannot retain its own owner through the stored
/// `JoinHandle`; only explicit inbound/cancellation primitives are shared.
#[derive(Clone)]
struct DiscordGatewayWorker {
    payloads: DiscordGatewayPayloadBuilder,
    gateway_url: String,
    proxy: AdapterProxyConfig,
    rest_client: Client,
    api_base_url: String,
    global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
    require_mention: bool,
    channel_controls: DiscordChannelControls,
    bot_policy: DiscordBotMessagePolicy,
    thread_participation: Arc<Mutex<DiscordThreadParticipationTracker>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<DiscordInboundMessage>>>>,
    journal_tx: Arc<RwLock<Option<mpsc::Sender<DiscordIngressJournalEvent>>>>,
    recovered_ingress: Arc<Mutex<std::collections::VecDeque<(DiscordJournalEntry, bool)>>>,
    overflow_ingress: Arc<Mutex<std::collections::VecDeque<DiscordOrderedIngress>>>,
    stop: watch::Receiver<bool>,
    invalid_session_delay: DiscordInvalidSessionDelay,
    thread_contexts: Arc<Mutex<DiscordThreadContextCache>>,
    identify_pacer: Arc<AsyncMutex<DiscordIdentifyPacer>>,
    worker_alive: Arc<AtomicBool>,
    fatal: Arc<AtomicBool>,
    /// Present only on the socket parent. The ordered child receives a clone
    /// without this slot, preventing a child-handle ownership cycle.
    ingress_task: Option<Arc<AsyncMutex<Option<tokio::task::JoinHandle<()>>>>>,
}

struct DiscordGatewayWorkerAliveGuard(Arc<AtomicBool>);

impl Drop for DiscordGatewayWorkerAliveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The socket parent retains the ordered-ingress task in a runtime-owned slot.
/// Holding the slot lock while joining is cancellation-safe: an abort of the
/// parent leaves the handle available for `DiscordGatewayRuntime::abort` to
/// abort and join before recovery is inspected.
impl DiscordGatewayWorker {
    fn for_ordered_ingress_stage(&self) -> Self {
        let mut worker = self.clone();
        worker.ingress_task = None;
        worker
    }

    async fn join_ordered_ingress_task(&self) -> Result<(), tokio::task::JoinError> {
        let Some(slot) = self.ingress_task.as_ref() else {
            return Ok(());
        };
        let mut slot = slot.lock().await;
        let result = match slot.as_mut() {
            Some(handle) => handle.await,
            None => return Ok(()),
        };
        slot.take();
        result
    }
}

/// Owns every MESSAGE_CREATE that has left the bounded ordered receiver but
/// has not yet been handed to the durable CLI receiver. Dropping this guard
/// (including through `JoinHandle::abort`) puts the current item and receiver
/// backlog back into shared recovery.
struct DiscordOrderedIngressRecoveryGuard {
    worker: DiscordGatewayWorker,
    receiver: mpsc::Receiver<DiscordOrderedIngress>,
    current: Option<(DiscordJournalEntry, bool)>,
}

impl DiscordOrderedIngressRecoveryGuard {
    fn new(
        worker: DiscordGatewayWorker,
        receiver: mpsc::Receiver<DiscordOrderedIngress>,
    ) -> Self {
        Self {
            worker,
            receiver,
            current: None,
        }
    }

    async fn recv(&mut self) -> Option<DiscordOrderedIngress> {
        self.receiver.recv().await
    }

    fn retain_current(&mut self, ingress: &DiscordUnresolvedIngress, journaled: bool) {
        self.current = Some((ingress.into(), journaled));
    }

    fn mark_current_journaled(&mut self) {
        if let Some((_, journaled)) = self.current.as_mut() {
            *journaled = true;
        }
    }

    fn discard_current(&mut self) {
        self.current = None;
    }
}

impl Drop for DiscordOrderedIngressRecoveryGuard {
    fn drop(&mut self) {
        let mut recovered = std::collections::VecDeque::new();
        if let Some(current) = self.current.take() {
            recovered.push_back(current);
        }
        while let Ok(ordered) = self.receiver.try_recv() {
            if let DiscordOrderedIngress::Message {
                ingress,
                journaled,
                ..
            } = ordered
            {
                recovered.push_back(((&ingress).into(), journaled));
            }
        }
        self.worker.requeue_recovered_ingress(recovered);
    }
}

/// Keeps the socket worker's FIFO overflow local only while it is actively
/// being handed back to the ordered stage. On cancellation it returns local
/// work to the shared overflow queue; runtime appends that queue only after
/// the ordered child has joined and recovered its earlier receiver backlog.
struct DiscordOrderedOverflowGuard {
    worker: DiscordGatewayWorker,
    pending: std::collections::VecDeque<DiscordOrderedIngress>,
}

impl DiscordOrderedOverflowGuard {
    fn new(worker: DiscordGatewayWorker) -> Self {
        Self {
            worker,
            pending: std::collections::VecDeque::new(),
        }
    }

    fn take_shared(&mut self) {
        self.pending.extend(self.worker.take_ordered_overflow());
    }

    fn pop_front(&mut self) -> Option<DiscordOrderedIngress> {
        self.pending.pop_front()
    }

    fn push_front(&mut self, ingress: DiscordOrderedIngress) {
        self.pending.push_front(ingress);
    }
    fn flush_to_recovery(&mut self) {
        self.take_shared();
        self.worker
            .requeue_ordered_overflow(std::mem::take(&mut self.pending));
    }
}

impl Drop for DiscordOrderedOverflowGuard {
    fn drop(&mut self) {
        self.take_shared();
        self.worker
            .restore_ordered_overflow(std::mem::take(&mut self.pending));
    }
}

/// Keeps shutdown handoff entries owned by shared recovery storage until the
/// caller has either durably transferred or intentionally discarded them.
///
/// `flush_recovered_ingress_for_shutdown` awaits a bounded journal reservation.
/// Its future may itself be cancelled by the CLI's outer shutdown deadline, so
/// entries must remain reachable even while that await is pending.
struct DiscordRecoveredIngressGuard {
    recovered_ingress: Arc<Mutex<std::collections::VecDeque<(DiscordJournalEntry, bool)>>>,
    retained: std::collections::VecDeque<(DiscordJournalEntry, bool)>,
    remaining: std::collections::VecDeque<(DiscordJournalEntry, bool)>,
}

impl DiscordRecoveredIngressGuard {
    fn take(
        recovered_ingress: Arc<
            Mutex<std::collections::VecDeque<(DiscordJournalEntry, bool)>>,
        >,
    ) -> Self {
        let remaining = std::mem::take(
            &mut *recovered_ingress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Self {
            recovered_ingress,
            retained: std::collections::VecDeque::new(),
            remaining,
        }
    }

    fn front(&self) -> Option<&(DiscordJournalEntry, bool)> {
        self.remaining.front()
    }

    fn retain_front(&mut self, journaled: bool) {
        let (entry, _) = self
            .remaining
            .pop_front()
            .expect("Discord recovered ingress guard lost its current entry");
        self.retained.push_back((entry, journaled));
    }

    fn discard_front(&mut self) {
        self.remaining
            .pop_front()
            .expect("Discord recovered ingress guard lost its current entry");
    }
}

impl Drop for DiscordRecoveredIngressGuard {
    fn drop(&mut self) {
        self.retained.append(&mut self.remaining);
        if self.retained.is_empty() {
            return;
        }

        // Entries taken before a concurrent worker requeue were older, so put
        // them back ahead of newly recovered work while preserving their FIFO
        // order.
        let mut recovered = self
            .recovered_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some(entry) = self.retained.pop_back() {
            recovered.push_front(entry);
        }
    }
}

impl DiscordGatewayRuntime {
    fn new(
        config: &DiscordConfig,
        thread_participation: Arc<Mutex<DiscordThreadParticipationTracker>>,
        rest_client: Client,
        api_base_url: String,
    ) -> Self {
        Self::new_with_global_rate_limit(
            config,
            thread_participation,
            rest_client,
            api_base_url,
            Arc::new(DiscordGlobalRateLimitBarrier::default()),
        )
    }

    fn new_with_global_rate_limit(
        config: &DiscordConfig,
        thread_participation: Arc<Mutex<DiscordThreadParticipationTracker>>,
        rest_client: Client,
        api_base_url: String,
        global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
    ) -> Self {
        let (stop_tx, _) = watch::channel(false);
        let (outbound_stop_tx, _) = watch::channel(false);
        Self {
            payloads: DiscordGatewayPayloadBuilder::from_config(config),
            gateway_url: config
                .gateway_url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .unwrap_or(DISCORD_GATEWAY_V10_URL)
                .to_string(),
            proxy: config.proxy.clone(),
            rest_client,
            api_base_url,
            global_rate_limit,
            require_mention: config.require_mention,
            channel_controls: config.channel_controls.clone(),
            bot_policy: config.bot_policy,
            thread_participation,
            inbound_tx: Arc::new(RwLock::new(None)),
            journal_tx: Arc::new(RwLock::new(None)),
            recovered_ingress: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            overflow_ingress: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            stop_tx,
            outbound_stop_tx,
            invalid_session_delay: Arc::new(discord_random_invalid_session_delay),
            thread_contexts: Arc::new(Mutex::new(DiscordThreadContextCache::default())),
            identify_pacer: Arc::new(AsyncMutex::new(DiscordIdentifyPacer::default())),
            worker_alive: Arc::new(AtomicBool::new(false)),
            fatal: Arc::new(AtomicBool::new(false)),
            task: AsyncMutex::new(None),
            ingress_task: Arc::new(AsyncMutex::new(None)),
        }
    }


    async fn set_durable_inbound_sender(
        &self,
        tx: mpsc::Sender<DiscordInboundMessage>,
        journal_tx: mpsc::Sender<DiscordIngressJournalEvent>,
    ) {
        *self.journal_tx.write().await = Some(journal_tx);
        *self.inbound_tx.write().await = Some(tx);
    }


    /// Return raw, durable ingress to the authenticated ordered resolver.
    ///
    /// Post-policy records are owned by the CLI route spool and must never
    /// re-enter this pre-policy queue, where they could be evaluated twice.
    fn queue_recovered_pre_policy_ingress(
        &self,
        entries: Vec<DiscordJournalEntry>,
    ) -> Result<(), GatewayError> {
        if let Some(entry) = entries.iter().find(|entry| !entry.is_pre_policy()) {
            return Err(GatewayError::Platform(format!(
                "Discord post-policy journal entry {} cannot enter pre-policy recovery",
                entry.message().message_id
            )));
        }
        self.worker()
            .requeue_recovered_ingress(entries.into_iter().map(|entry| (entry, true)));
        Ok(())
    }

    /// Snapshot retained socket ingress for a CLI-owned durable quarantine.
    /// The snapshot is non-destructive until the CLI confirms its fsynced
    /// handoff by message ID.
    fn snapshot_recovered_ingress_for_cli(&self) -> Vec<DiscordJournalEntry> {
        self.worker().snapshot_recovered_ingress_for_cli()
    }

    /// Release only recovered ingress that the CLI durably quarantined.
    fn acknowledge_recovered_ingress_for_cli(&self, message_ids: &[String]) {
        self.worker()
            .acknowledge_recovered_ingress_for_cli(message_ids);
    }

    /// Before the CLI-owned journal sender is closed, turn every remaining
    /// live ingress item into a durable reservation. A single outer deadline
    /// bounds the whole drain while each item gets the remaining budget.
    ///
    /// The guard deliberately keeps the current entry in shared recovery until
    /// its reservation outcome is known. This makes cancellation at either the
    /// bounded channel send or the durability acknowledgement lossless.
    async fn flush_recovered_ingress_for_shutdown(&self) -> Result<(), GatewayError> {
        let worker = self.worker();
        let deadline =
            tokio::time::Instant::now() + DISCORD_ORDERED_INGRESS_SHUTDOWN_TIMEOUT;
        let mut recovered =
            DiscordRecoveredIngressGuard::take(Arc::clone(&self.recovered_ingress));
        let mut handoff_failed = false;

        while let Some((entry, journaled)) = recovered.front().cloned() {
            if journaled {
                recovered.retain_front(true);
                continue;
            }
            match worker.reserve_ingress_for_shutdown(&entry, deadline).await {
                Some(DiscordIngressReservationOutcome::Recorded)
                | Some(DiscordIngressReservationOutcome::ExistingUnresolved) => {
                    recovered.retain_front(true);
                }
                Some(DiscordIngressReservationOutcome::ExistingRouteOwned) => {
                    recovered.discard_front();
                }
                None => {
                    handoff_failed = true;
                    error!(
                        message_id = %entry.message().message_id,
                        "Discord shutdown could not durably hand off live ingress"
                    );
                    recovered.retain_front(false);
                }
            }
        }

        // Requeue before reporting failure; the caller may choose a later
        // durable recovery path, but it must never inherit an empty queue.
        drop(recovered);
        if handoff_failed {
            Err(GatewayError::Platform(
                "Discord shutdown refused to close inbound senders because live ingress was not durably handed off"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    }


    async fn close_inbound_sender(&self) {
        self.journal_tx.write().await.take();
        self.inbound_tx.write().await.take();
    }

    /// Abort and join the ordered-ingress child while its supervisor slot
    /// remains locked. If the socket parent was itself cancelled while
    /// awaiting this child, the handle stays in this slot rather than
    /// detaching, so this fence observes the recovery guard's Drop.
    async fn abort_and_join_ordered_ingress_task(&self) {
        let result = {
            let mut ingress_task = self.ingress_task.lock().await;
            let result = match ingress_task.as_mut() {
                Some(handle) => {
                    handle.abort();
                    Some(handle.await)
                }
                None => None,
            };
            ingress_task.take();
            result
        };
        if let Some(Err(error)) = result {
            if !error.is_cancelled() {
                debug!(error = %error, "Discord ordered ingress task ended while aborting");
            }
        }
        // The socket parent's overflow guard restores cancellation-time
        // overflow to this queue. The child has now run its recovery guard,
        // so append the later overflow only after the receiver backlog.
        let worker = self.worker();
        let overflow = worker.take_ordered_overflow();
        worker.requeue_ordered_overflow(overflow);
    }

    async fn configure_identify_session_start_limit(
        &self,
        limit: DiscordGatewaySessionStartLimit,
    ) {
        self.identify_pacer.lock().await.configure(limit);
    }

    fn is_healthy(&self) -> bool {
        self.worker_alive.load(Ordering::SeqCst) && !self.fatal.load(Ordering::SeqCst)
    }

    /// Terminal Discord close codes are a configuration/credential boundary,
    /// not a transient transport failure. Recreate the adapter after changing
    /// the operator-supplied configuration to clear this latch.
    fn is_reconnectable(&self) -> bool {
        !self.fatal.load(Ordering::SeqCst)
    }

    fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.outbound_stop_tx.subscribe()
    }
    /// Reopen outbound REST only while the adapter lifecycle lock is starting
    /// a new instance, before bootstrap requests such as `/gateway/bot`.
    fn reset_outbound_stop(&self) {
        self.outbound_stop_tx.send_replace(false);
    }

    fn worker(&self) -> DiscordGatewayWorker {
        DiscordGatewayWorker {
            payloads: self.payloads.clone(),
            gateway_url: self.gateway_url.clone(),
            proxy: self.proxy.clone(),
            rest_client: self.rest_client.clone(),
            api_base_url: self.api_base_url.clone(),
            global_rate_limit: Arc::clone(&self.global_rate_limit),
            require_mention: self.require_mention,
            channel_controls: self.channel_controls.clone(),
            bot_policy: self.bot_policy,
            thread_participation: Arc::clone(&self.thread_participation),
            inbound_tx: Arc::clone(&self.inbound_tx),
            journal_tx: Arc::clone(&self.journal_tx),
            recovered_ingress: Arc::clone(&self.recovered_ingress),
            overflow_ingress: Arc::clone(&self.overflow_ingress),
            stop: self.stop_tx.subscribe(),
            invalid_session_delay: Arc::clone(&self.invalid_session_delay),
            thread_contexts: Arc::clone(&self.thread_contexts),
            identify_pacer: Arc::clone(&self.identify_pacer),
            worker_alive: Arc::clone(&self.worker_alive),
            fatal: Arc::clone(&self.fatal),
            ingress_task: Some(Arc::clone(&self.ingress_task)),
        }
    }

    #[cfg(test)]
    async fn start(&self) -> Result<(), GatewayError> {
        self.start_with_adapter_state(None).await
    }

    /// Start only if the adapter is still live after any asynchronous startup
    /// preparation. The running check is made while serialized with `stop`,
    /// preventing a stale start from resurrecting a stopped socket.
    async fn start_while_adapter_running(
        &self,
        adapter: &BasePlatformAdapter,
    ) -> Result<(), GatewayError> {
        self.start_with_adapter_state(Some(adapter)).await
    }

    async fn start_with_adapter_state(
        &self,
        adapter: Option<&BasePlatformAdapter>,
    ) -> Result<(), GatewayError> {
        let mut task = self.task.lock().await;
        if adapter.is_some_and(|adapter| !adapter.is_running()) {
            return Ok(());
        }
        if self.fatal.load(Ordering::SeqCst) {
            return Err(GatewayError::Platform(
                "Discord Gateway stopped after a fatal close; rebuild the adapter after correcting credentials, intents, or shard configuration"
                    .to_string(),
            ));
        }
        // Fail synchronously before claiming a socket owner. Connection setup
        // normalizes this URL again because a resumed endpoint can differ.
        discord_gateway_v10_url(&self.gateway_url)?;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            debug!("Discord Gateway runtime is already running");
            return Ok(());
        }
        let inbound_ready = self
            .inbound_tx
            .read()
            .await
            .as_ref()
            .is_some_and(|sender| !sender.is_closed());
        let journal_ready = self
            .journal_tx
            .read()
            .await
            .as_ref()
            .is_some_and(|sender| !sender.is_closed());
        if !journal_ready {
            self.worker_alive.store(false, Ordering::SeqCst);
            return Err(GatewayError::Platform(
                "Discord Gateway durable ingress journal receiver is unavailable".to_string(),
            ));
        }
        if !inbound_ready {
            self.worker_alive.store(false, Ordering::SeqCst);
            return Err(GatewayError::Platform(
                "Discord Gateway inbound receiver is unavailable".to_string(),
            ));
        }

        // A completed worker is no longer a live socket owner. Dropping its
        // handle here makes a later start a clean, single-owner restart.
        task.take();
        self.abort_and_join_ordered_ingress_task().await;
        self.outbound_stop_tx.send_replace(false);
        self.stop_tx.send_replace(false);
        let worker = self.worker();
        self.worker_alive.store(true, Ordering::SeqCst);
        let handle = tokio::spawn(async move {
            worker.run().await;
        });
        *task = Some(handle);
        Ok(())

    }

    /// Quiesce socket intake and ordered ingress, while retaining outbound
    /// REST capability for the CLI-owned route drain.
    async fn stop_intake(&self) -> Result<(), GatewayError> {
        let result = {
            let mut task = self.task.lock().await;
            self.stop_tx.send_replace(true);
            let result = match task.as_mut() {
                Some(handle) => Some(handle.await),
                None => None,
            };
            task.take();
            result
        };
        if let Some(Err(error)) = result {
            debug!(error = %error, "Discord Gateway runtime task ended while stopping intake");
        }
        // A normally stopped parent has already joined its child. This fence
        // covers cancellation/panic paths before durable recovery is drained.
        self.abort_and_join_ordered_ingress_task().await;
        let flush = self.flush_recovered_ingress_for_shutdown().await;
        self.worker_alive.store(false, Ordering::SeqCst);
        flush
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        self.outbound_stop_tx.send_replace(true);
        self.stop_intake().await
    }

    /// Cancellation-safe emergency stop for a quiesce deadline. Both the
    /// socket parent and ordered-ingress child remain in supervised slots
    /// until they have joined, so retained ingress is visible immediately.
    async fn abort(&self) {
        self.stop_tx.send_replace(true);
        self.outbound_stop_tx.send_replace(true);
        {
            let mut task = self.task.lock().await;
            if let Some(handle) = task.as_mut() {
                handle.abort();
                if let Err(error) = handle.await {
                    if !error.is_cancelled() {
                        debug!(error = %error, "Discord Gateway runtime task ended while aborting");
                    }
                }
            }
            task.take();
        }
        self.abort_and_join_ordered_ingress_task().await;
        self.worker_alive.store(false, Ordering::SeqCst);
    }

    /// Synchronous finalizer used when ownership disappears without an
    /// explicit graceful shutdown. It cannot await, but it aborts both
    /// supervised tasks; neither task holds a sender back to this runtime.
    fn cancel_and_abort_on_drop(&self) {
        self.stop_tx.send_replace(true);
        self.outbound_stop_tx.send_replace(true);
        if let Ok(task) = self.task.try_lock() {
            if let Some(handle) = task.as_ref() {
                handle.abort();
            }
        }
        if let Ok(ingress_task) = self.ingress_task.try_lock() {
            if let Some(handle) = ingress_task.as_ref() {
                handle.abort();
            }
        }
        self.worker_alive.store(false, Ordering::SeqCst);
    }
}

impl Drop for DiscordGatewayRuntime {
    fn drop(&mut self) {
        self.cancel_and_abort_on_drop();
    }
}

impl DiscordGatewayWorker {
    #[cfg(test)]
    fn with_invalid_session_delay(
        mut self,
        delay: impl Fn() -> std::time::Duration + Send + Sync + 'static,
    ) -> Self {
        self.invalid_session_delay = Arc::new(delay);
        self
    }
}

impl DiscordGatewayWorker {
    fn take_recovered_ingress(&self) -> Vec<(DiscordJournalEntry, bool)> {
        self.recovered_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    /// Snapshot retained socket ingress for the CLI-owned durable quarantine.
    ///
    /// This is intentionally non-destructive: the CLI acknowledges message
    /// IDs only after its scoped spool write has completed and fsynced.
    fn snapshot_recovered_ingress_for_cli(&self) -> Vec<DiscordJournalEntry> {
        let mut seen = BTreeSet::new();
        let mut entries = Vec::new();
        {
            let recovered = self
                .recovered_ingress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (entry, _) in recovered.iter() {
                if seen.insert(entry.message().message_id.clone()) {
                    entries.push(entry.clone());
                }
            }
        }
        let overflow = self
            .overflow_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for ordered in overflow.iter() {
            if let DiscordOrderedIngress::Message { ingress, .. } = ordered {
                if seen.insert(ingress.message.message_id.clone()) {
                    entries.push(ingress.into());
                }
            }
        }
        entries
    }

    /// Release only items that the CLI has durably quarantined.
    fn acknowledge_recovered_ingress_for_cli(&self, message_ids: &[String]) {
        let acknowledged: BTreeSet<&str> = message_ids
            .iter()
            .map(String::as_str)
            .map(str::trim)
            .filter(|message_id| !message_id.is_empty())
            .collect();
        if acknowledged.is_empty() {
            return;
        }
        self.recovered_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(entry, _)| {
                !acknowledged.contains(entry.message().message_id.as_str())
            });
        self.overflow_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|ordered| {
                !matches!(
                    ordered,
                    DiscordOrderedIngress::Message { ingress, .. }
                        if acknowledged.contains(ingress.message.message_id.as_str())
                )
            });
    }

    fn retain_ordered_overflow(&self, ingress: DiscordOrderedIngress) {
        self.overflow_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(ingress);
    }

    fn take_ordered_overflow(&self) -> std::collections::VecDeque<DiscordOrderedIngress> {
        std::mem::take(
            &mut *self
                .overflow_ingress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Return overflow to its shared FIFO without promoting it ahead of
    /// ordered-stage entries that may still be recovering.
    fn restore_ordered_overflow(
        &self,
        mut overflow: std::collections::VecDeque<DiscordOrderedIngress>,
    ) {
        if overflow.is_empty() {
            return;
        }
        let mut shared = self
            .overflow_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while let Some(ordered) = overflow.pop_back() {
            shared.push_front(ordered);
        }
    }

    /// The ordered stage requeues its own receiver first. Append held overflow
    /// only after that join so a shutdown journal preserves Discord FIFO.
    fn requeue_ordered_overflow(
        &self,
        overflow: impl IntoIterator<Item = DiscordOrderedIngress>,
    ) {
        self.requeue_recovered_ingress(overflow.into_iter().filter_map(|ordered| {
            let DiscordOrderedIngress::Message {
                ingress,
                journaled,
                ..
            } = ordered
            else {
                return None;
            };
            Some(((&ingress).into(), journaled))
        }));
    }

    /// Return retained ingress with its durability state. A live message that
    /// has not yet been fsynced must reserve again after restart.
    fn requeue_recovered_ingress(
        &self,
        entries: impl IntoIterator<Item = (DiscordJournalEntry, bool)>,
    ) {
        let mut recovered = self
            .recovered_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (entry, journaled) in entries {
            let message_id = entry.message().message_id.as_str();
            if !recovered
                .iter()
                .any(|(queued, _)| queued.message().message_id == message_id)
            {
                recovered.push_back((entry, journaled));
            }
        }
    }

    fn has_recovered_ingress(&self) -> bool {
        !self
            .recovered_ingress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }

    /// Inject retained ingress without ever awaiting a bounded ordered channel
    /// from the socket task. A full stage leaves the head in recovery and is
    /// retried by the active socket loop, so heartbeat and close frames remain
    /// responsive.
    fn drain_recovered_ingress(
        &self,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
        client_user_id: &str,
    ) -> bool {
        let mut recovered =
            DiscordRecoveredIngressGuard::take(Arc::clone(&self.recovered_ingress));
        while let Some((entry, journaled)) = recovered.front().cloned() {
            debug_assert!(
                entry.is_pre_policy(),
                "post-policy records must remain owned by the CLI route spool"
            );
            let mut ingress = DiscordUnresolvedIngress::from(entry);
            // A fresh READY identity is authoritative. Recovered raw ingress
            // must not retain a prior process/session's bot identity.
            ingress.client_user_id = Some(client_user_id.to_string());
            match ordered_ingress_tx.try_send(DiscordOrderedIngress::Message {
                ack: None,
                ingress,
                journaled,
            }) {
                Ok(()) => recovered.discard_front(),
                Err(mpsc::error::TrySendError::Full(_)) => return true,
                Err(mpsc::error::TrySendError::Closed(_)) => return false,
            }
        }
        true
    }

    async fn run(&self) {
        let _alive_guard = DiscordGatewayWorkerAliveGuard(Arc::clone(&self.worker_alive));
        let mut stop = self.stop.clone();
        let mut session = GatewaySession::new();
        let mut backoff_index = 0usize;
        let mut pending_initial_ack = None;
        let (ordered_ingress_tx, ordered_ingress_rx) =
            mpsc::channel(DISCORD_THREAD_RESOLVER_CAPACITY);
        let (sequence_ack_tx, mut sequence_ack_rx) = mpsc::unbounded_channel();
        let (ingress_done_tx, mut ingress_done) = watch::channel(false);
        let mut pending_overflow =
            DiscordOrderedOverflowGuard::new(self.for_ordered_ingress_stage());
        // Keep the child handle in a runtime-owned slot before its first
        // cancellation point. The child itself intentionally does not retain
        // the slot, so runtime abort can join its recovery guard.
        let mut ingress_task_slot = self
            .ingress_task
            .as_ref()
            .expect("socket parent must own the ordered ingress task slot")
            .lock()
            .await;
        debug_assert!(ingress_task_slot.is_none());
        let ingress_worker = self.for_ordered_ingress_stage();
        *ingress_task_slot = Some(tokio::spawn(async move {
            ingress_worker
                .run_ordered_ingress_stage(ordered_ingress_rx, sequence_ack_tx)
                .await;
            ingress_done_tx.send_replace(true);
        }));
        drop(ingress_task_slot);
        // Recovery is intentionally delayed until a READY identity (or a
        // previously authenticated RESUME session) authoritatively scopes
        // raw durable ingress. Never route persisted raw payloads directly.

        'worker: while !*stop.borrow() && !*ingress_done.borrow() {
            if let Some(client_user_id) = session.client_user_id.clone() {
                if !self.drain_recovered_ingress(&ordered_ingress_tx, client_user_id.as_str()) {
                    break;
                }
            }
            let outcome = self
                .connect_and_listen(
                    &mut session,
                    &ordered_ingress_tx,
                    &mut sequence_ack_rx,
                    &mut pending_initial_ack,
                    &mut ingress_done,
                    &mut stop,
                )
                .await;
            let reconnect_delay = match outcome {
                Ok(DiscordGatewayConnectionOutcome::Stopped) => break,
                Ok(DiscordGatewayConnectionOutcome::Unhealthy) => {
                    error!("Discord Gateway worker stopped because its inbound receiver is unavailable");
                    break;
                }
                Ok(DiscordGatewayConnectionOutcome::Fatal) => {
                    self.fatal.store(true, Ordering::SeqCst);
                    error!("Discord Gateway worker stopped after a fatal gateway close code");
                    break;
                }
                Ok(DiscordGatewayConnectionOutcome::Reconnect) => {
                    debug!("Discord Gateway reconnect requested");
                    None
                }
                Ok(DiscordGatewayConnectionOutcome::ReconnectAfter(delay)) => {
                    debug!(delay_ms = delay.as_millis(), "Discord Gateway delaying fresh IDENTIFY after INVALID_SESSION");
                    Some(delay)
                }
                Ok(DiscordGatewayConnectionOutcome::Backpressure) => {
                    pending_overflow.take_shared();
                    warn!("Discord ordered ingress stage is saturated; awaiting FIFO handoff before reconnect");
                    None
                }
                Err(error) => {
                    warn!(error = %error, "Discord Gateway connection ended; reconnecting");
                    None
                }
            };

            while let Some(ordered) = pending_overflow.pop_front() {
                let reservation = tokio::select! {
                    result = ordered_ingress_tx.reserve() => Some(result),
                    changed = stop.changed() => {
                        let _ = changed;
                        None
                    }
                    changed = ingress_done.changed() => {
                        let _ = changed;
                        None
                    }
                };
                match reservation {
                    Some(Ok(permit)) => permit.send(ordered),
                    Some(Err(_)) | None => {
                        pending_overflow.push_front(ordered);
                        break 'worker;
                    }
                }
            }

            if *stop.borrow() || *ingress_done.borrow() {
                break;
            }

            let delay = reconnect_delay.unwrap_or_else(|| {
                let delay_ms = DISCORD_GATEWAY_BACKOFF_STEPS_MS
                    [backoff_index.min(DISCORD_GATEWAY_BACKOFF_STEPS_MS.len().saturating_sub(1))];
                backoff_index = (backoff_index + 1)
                    .min(DISCORD_GATEWAY_BACKOFF_STEPS_MS.len().saturating_sub(1));
                std::time::Duration::from_millis(delay_ms)
            });
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        break;
                    }
                }
            }
        }

        pending_overflow.take_shared();
        drop(ordered_ingress_tx);
        if let Err(error) = self.join_ordered_ingress_task().await {
            warn!(error = %error, "Discord ordered ingress stage ended unexpectedly");
        }
        pending_overflow.flush_to_recovery();

        debug!("Discord Gateway runtime stopped");
    }

    fn next_heartbeat_deadline(
        session: &GatewaySession,
    ) -> Option<tokio::time::Instant> {
        session
            .heartbeat_interval_ms
            .filter(|interval| *interval > 0)
            .map(|interval| {
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(interval)
            })
    }

    fn initial_heartbeat_deadline(
        session: &GatewaySession,
        jitter: f64,
    ) -> Option<tokio::time::Instant> {
        session
            .heartbeat_interval_ms
            .filter(|interval| *interval > 0)
            .map(|interval| {
                tokio::time::Instant::now()
                    + std::time::Duration::from_millis(interval)
                        .mul_f64(jitter.clamp(0.0, 1.0))
            })
    }

    fn heartbeat_deadline_after_payload(
        current: Option<tokio::time::Instant>,
        session: &GatewaySession,
        opcode: u8,
    ) -> Option<tokio::time::Instant> {
        if opcode == opcodes::HELLO {
            Self::initial_heartbeat_deadline(session, rand::random::<f64>())
        } else {
            current
        }
    }

    async fn open_socket(&self, gateway_url: &str) -> Result<DiscordWebSocket, GatewayError> {
        let url = url::Url::parse(gateway_url).map_err(|error| {
            GatewayError::ConnectionFailed(format!("Invalid Discord Gateway URL: {error}"))
        })?;
        let transport = self.open_transport(&url).await?;
        client_async_tls_with_config(gateway_url, transport, None, None)
            .await
            .map(|(socket, _)| socket)
            .map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord Gateway WebSocket handshake failed: {error}"
                ))
            })
    }

    async fn open_transport(
        &self,
        gateway_url: &url::Url,
    ) -> Result<DiscordTransport, GatewayError> {
        let target_host = discord_url_host(gateway_url).ok_or_else(|| {
            GatewayError::ConnectionFailed("Discord Gateway URL has no host".to_string())
        })?;
        let target_port = gateway_url.port_or_known_default().ok_or_else(|| {
            GatewayError::ConnectionFailed("Discord Gateway URL has no port".to_string())
        })?;

        if let Some(socks_proxy) = self.proxy.socks_proxy.as_deref() {
            let proxy = Self::parse_proxy_url(socks_proxy, &["socks5", "socks5h"])?;
            let proxy_host = discord_url_host(&proxy).ok_or_else(|| {
                GatewayError::ConnectionFailed("Discord SOCKS proxy URL has no host".to_string())
            })?;
            let proxy_port = proxy.port_or_known_default().unwrap_or(1080);
            let username = urlencoding::decode(proxy.username()).map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord SOCKS proxy username is invalid: {error}"
                ))
            })?;
            let password = proxy
                .password()
                .map(urlencoding::decode)
                .transpose()
                .map_err(|error| {
                    GatewayError::ConnectionFailed(format!(
                        "Discord SOCKS proxy password is invalid: {error}"
                    ))
                })?
                .unwrap_or_default();
            let stream = if proxy.scheme() == "socks5" {
                let deadline = tokio::time::Instant::now() + DISCORD_GATEWAY_IO_TIMEOUT;
                let targets = tokio::time::timeout_at(
                    deadline,
                    tokio::net::lookup_host((target_host.as_str(), target_port)),
                )
                .await
                .map_err(|_| {
                    GatewayError::ConnectionFailed(
                        "Discord Gateway local DNS resolution exceeded its deadline".to_string(),
                    )
                })?
                .map_err(|error| {
                    GatewayError::ConnectionFailed(format!(
                        "Discord Gateway local DNS resolution failed: {error}"
                    ))
                })?
                .collect::<Vec<_>>();
                if targets.is_empty() {
                    return Err(GatewayError::ConnectionFailed(
                        "Discord Gateway local DNS resolution returned no addresses".to_string(),
                    ));
                }
                Self::connect_socks5_targets(
                    &proxy_host,
                    proxy_port,
                    targets,
                    username.as_ref(),
                    password.as_ref(),
                    deadline,
                )
                .await?
            } else if username.is_empty() {
                tokio_socks::tcp::Socks5Stream::connect(
                    (proxy_host.as_str(), proxy_port),
                    (target_host.as_str(), target_port),
                )
                .await
                .map_err(|error| {
                    GatewayError::ConnectionFailed(format!(
                        "Discord Gateway SOCKS connection failed: {error}"
                    ))
                })?
            } else {
                tokio_socks::tcp::Socks5Stream::connect_with_password(
                    (proxy_host.as_str(), proxy_port),
                    (target_host.as_str(), target_port),
                    username.as_ref(),
                    password.as_ref(),
                )
                .await
                .map_err(|error| {
                    GatewayError::ConnectionFailed(format!(
                        "Discord Gateway SOCKS connection failed: {error}"
                    ))
                })?
            };
            return Ok(Box::new(stream.into_inner()));
        }

        if let Some(http_proxy) = self.proxy.http_proxy.as_deref() {
            let proxy = Self::parse_proxy_url(http_proxy, &["http"])?;
            return Self::open_http_connect_tunnel(&proxy, &target_host, target_port).await;
        }

        let stream = tokio::net::TcpStream::connect((target_host.as_str(), target_port))
            .await
            .map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord Gateway TCP connection failed: {error}"
                ))
            })?;
        Ok(Box::new(stream))
    }

    /// Attempt every locally resolved target through a SOCKS5 proxy under one
    /// end-to-end deadline. A failed address is not evidence that another A/AAAA
    /// record is unreachable.
    async fn connect_socks5_targets(
        proxy_host: &str,
        proxy_port: u16,
        targets: Vec<std::net::SocketAddr>,
        username: &str,
        password: &str,
        deadline: tokio::time::Instant,
    ) -> Result<tokio_socks::tcp::Socks5Stream<tokio::net::TcpStream>, GatewayError> {
        let mut last_error = None;
        let target_count = targets.len();
        for (index, target) in targets.into_iter().enumerate() {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                break;
            }
            // Reserve a fair share of the remaining outer budget for every
            // untried address. Otherwise an unreachable first AAAA/A record
            // can consume the entire deadline and starve a reachable A record.
            let remaining_targets = (target_count - index).max(1) as u32;
            let attempt_deadline =
                now + deadline.saturating_duration_since(now) / remaining_targets;
            let result = tokio::time::timeout_at(attempt_deadline, async {
                if username.is_empty() {
                    tokio_socks::tcp::Socks5Stream::connect((proxy_host, proxy_port), target)
                        .await
                } else {
                    tokio_socks::tcp::Socks5Stream::connect_with_password(
                        (proxy_host, proxy_port),
                        target,
                        username,
                        password,
                    )
                    .await
                }
            })
            .await;
            match result {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) => last_error = Some(error.to_string()),
                Err(_) => {
                    last_error = Some(format!(
                        "SOCKS CONNECT attempt for {target} exceeded its reserved deadline"
                    ));
                }
            }
        }
        let detail = last_error
            .unwrap_or_else(|| "no SOCKS CONNECT attempt could be made".to_string());
        Err(GatewayError::ConnectionFailed(format!(
            "Discord Gateway SOCKS connection failed: {detail}"
        )))
    }

    fn parse_proxy_url(
        raw: &str,
        allowed_schemes: &[&str],
    ) -> Result<url::Url, GatewayError> {
        let proxy = url::Url::parse(raw.trim()).map_err(|error| {
            GatewayError::ConnectionFailed(format!("Invalid Discord proxy URL: {error}"))
        })?;
        if !allowed_schemes.contains(&proxy.scheme()) {
            return Err(GatewayError::ConnectionFailed(format!(
                "Unsupported Discord proxy scheme `{}`",
                proxy.scheme()
            )));
        }
        Ok(proxy)
    }

    async fn open_http_connect_tunnel(
        proxy: &url::Url,
        target_host: &str,
        target_port: u16,
    ) -> Result<DiscordTransport, GatewayError> {
        let proxy_host = discord_url_host(proxy).ok_or_else(|| {
            GatewayError::ConnectionFailed("Discord HTTP proxy URL has no host".to_string())
        })?;
        let proxy_port = proxy.port_or_known_default().unwrap_or(8080);
        let mut stream = tokio::net::TcpStream::connect((proxy_host.as_str(), proxy_port))
            .await
            .map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord HTTP proxy connection failed: {error}"
                ))
            })?;
        let authority = discord_connect_authority(target_host, target_port);
        let mut request = format!(
            "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\nProxy-Connection: Keep-Alive\r\n"
        );
        if !proxy.username().is_empty() {
            let username = urlencoding::decode(proxy.username()).map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord HTTP proxy username is invalid: {error}"
                ))
            })?;
            let password = proxy
                .password()
                .map(urlencoding::decode)
                .transpose()
                .map_err(|error| {
                    GatewayError::ConnectionFailed(format!(
                        "Discord HTTP proxy password is invalid: {error}"
                    ))
                })?
                .unwrap_or_default();
            let credentials = base64::engine::general_purpose::STANDARD
                .encode(format!("{username}:{password}"));
            request.push_str(&format!("Proxy-Authorization: Basic {credentials}\r\n"));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord HTTP proxy CONNECT write failed: {error}"
                ))
            })?;

        const MAX_CONNECT_RESPONSE_BYTES: usize = 16 * 1024;
        let mut response = Vec::with_capacity(1024);
        let mut chunk = [0_u8; 1024];
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            if response.len() >= MAX_CONNECT_RESPONSE_BYTES {
                return Err(GatewayError::ConnectionFailed(
                    "Discord HTTP proxy CONNECT response exceeded 16 KiB".to_string(),
                ));
            }
            let read = stream.read(&mut chunk).await.map_err(|error| {
                GatewayError::ConnectionFailed(format!(
                    "Discord HTTP proxy CONNECT read failed: {error}"
                ))
            })?;
            if read == 0 {
                return Err(GatewayError::ConnectionFailed(
                    "Discord HTTP proxy closed during CONNECT".to_string(),
                ));
            }
            response.extend_from_slice(&chunk[..read]);
        }
        let status_line = response
            .split(|byte| *byte == b'\n')
            .next()
            .and_then(|line| std::str::from_utf8(line).ok())
            .map(str::trim)
            .unwrap_or_default();
        let connected = status_line
            .split_ascii_whitespace()
            .nth(1)
            .is_some_and(|status| status == "200");
        if !connected {
            return Err(GatewayError::ConnectionFailed(format!(
                "Discord HTTP proxy CONNECT failed: {status_line}"
            )));
        }
        Ok(Box::new(stream))
    }

    fn commit_sequence_ack(
        session: &mut GatewaySession,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        ack: DiscordSequenceAck,
    ) {
        session.commit_sequence_for_generation(ack.generation, ack.sequence);
        if pending_initial_ack.as_ref() == Some(&ack) {
            *pending_initial_ack = None;
        }
    }

    fn track_initial_durability(
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        is_ready: bool,
        ack: Option<DiscordSequenceAck>,
        outcome: DiscordInboundDispatchOutcome,
    ) {
        if is_ready {
            *pending_initial_ack =
                (outcome == DiscordInboundDispatchOutcome::Queued)
                    .then_some(ack)
                    .flatten();
        }
    }

    fn drain_sequence_acks(
        session: &mut GatewaySession,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        sequence_ack_rx: &mut mpsc::UnboundedReceiver<DiscordSequenceAck>,
    ) {
        if pending_initial_ack
            .as_ref()
            .is_some_and(|ack| ack.generation != session.generation())
        {
            *pending_initial_ack = None;
        }
        while let Ok(ack) = sequence_ack_rx.try_recv() {
            Self::commit_sequence_ack(session, pending_initial_ack, ack);
        }
    }

    async fn await_authentication_preparation(
        &self,
        session: &mut GatewaySession,
        sequence_ack_rx: &mut mpsc::UnboundedReceiver<DiscordSequenceAck>,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        ingress_done: &mut watch::Receiver<bool>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<DiscordAuthenticationPreparation, GatewayError> {
        if *ingress_done.borrow() {
            return Ok(DiscordAuthenticationPreparation::Unhealthy);
        }
        Self::drain_sequence_acks(session, pending_initial_ack, sequence_ack_rx);
        loop {
            if session.can_resume() || pending_initial_ack.is_none() {
                return Ok(DiscordAuthenticationPreparation::Ready);
            }
            // A successfully enqueued READY owns the only ACK that can make
            // this lineage resumable. Do not open another socket until that
            // durability decision completes.
            tokio::select! {
                biased;
                ack = sequence_ack_rx.recv() => {
                    let Some(ack) = ack else {
                        return Ok(DiscordAuthenticationPreparation::Unhealthy);
                    };
                    Self::commit_sequence_ack(session, pending_initial_ack, ack);
                    Self::drain_sequence_acks(session, pending_initial_ack, sequence_ack_rx);
                }
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return Ok(DiscordAuthenticationPreparation::Stopped);
                    }
                }
                changed = ingress_done.changed() => {
                    if changed.is_err() || *ingress_done.borrow() {
                        return Ok(DiscordAuthenticationPreparation::Unhealthy);
                    }
                }
            }
        }
    }

    async fn connect_and_listen(
        &self,
        session: &mut GatewaySession,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
        sequence_ack_rx: &mut mpsc::UnboundedReceiver<DiscordSequenceAck>,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        ingress_done: &mut watch::Receiver<bool>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<DiscordGatewayConnectionOutcome, GatewayError> {
        match self
            .await_authentication_preparation(
                session,
                sequence_ack_rx,
                pending_initial_ack,
                ingress_done,
                stop,
            )
            .await?
        {
            DiscordAuthenticationPreparation::Ready => {}
            DiscordAuthenticationPreparation::Stopped => {
                return Ok(DiscordGatewayConnectionOutcome::Stopped);
            }
            DiscordAuthenticationPreparation::Unhealthy => {
                return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
            }
        };
        if !session.can_resume() {
            match self.await_identify_eligibility(stop, ingress_done).await {
                DiscordAuthenticationPreparation::Ready => {}
                DiscordAuthenticationPreparation::Stopped => {
                    return Ok(DiscordGatewayConnectionOutcome::Stopped);
                }
                DiscordAuthenticationPreparation::Unhealthy => {
                    return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
                }
            }
        }
        let using_resume_endpoint = session.can_resume()
            && session
                .resume_gateway_url
                .as_deref()
                .map(str::trim)
                .is_some_and(|url| !url.is_empty());
        let url = match self.connection_url(session) {
            Ok(url) => url,
            Err(error) if using_resume_endpoint => {
                session.resume_gateway_url = None;
                warn!(
                    error = %error,
                    "Discord resume endpoint is invalid; retaining the session and using the primary Gateway"
                );
                self.connection_url(session)?
            }
            Err(error) => return Err(error),
        };
        debug!(
            resume = session.can_resume(),
            resume_endpoint = using_resume_endpoint,
            "connecting Discord Gateway"
        );
        let socket_result = tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Ok(DiscordGatewayConnectionOutcome::Stopped);
                }
                return Ok(DiscordGatewayConnectionOutcome::Reconnect);
            }
            changed = ingress_done.changed() => {
                if changed.is_err() || *ingress_done.borrow() {
                    return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
                }
                return Ok(DiscordGatewayConnectionOutcome::Reconnect);
            }
            result = tokio::time::timeout(DISCORD_GATEWAY_IO_TIMEOUT, self.open_socket(&url)) => {
                result.unwrap_or_else(|_| Err(GatewayError::ConnectionFailed(
                    "Discord Gateway connection timed out".to_string(),
                )))
            }
        };
        let mut socket = match socket_result {
            Ok(socket) => socket,
            Err(error) => {
                if using_resume_endpoint {
                    session.resume_gateway_url = None;
                    warn!(
                        error = %error,
                        "Discord resume endpoint failed; retaining the session and retrying through the primary Gateway"
                    );
                }
                return Err(error);
            }
        };
        // The ordered stage may commit READY while the TCP/TLS/WebSocket
        // handshake is in flight. RESUME must win before HELLO is handled.
        Self::drain_sequence_acks(session, pending_initial_ack, sequence_ack_rx);
        session.begin_connection();
        let hello_deadline = tokio::time::Instant::now() + DISCORD_GATEWAY_IO_TIMEOUT;
        let mut received_hello = false;
        let mut next_heartbeat_at = None;
        let mut heartbeat_ack_deadline = None;
        let mut hello_deadline_probed = false;
        let mut heartbeat_deadline_probed = false;
        let mut recovery_retry =
            tokio::time::interval(std::time::Duration::from_millis(DISCORD_INBOUND_RETRY_MS));
        recovery_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            Self::drain_sequence_acks(session, pending_initial_ack, sequence_ack_rx);
            if *stop.borrow() {
                return Ok(DiscordGatewayConnectionOutcome::Stopped);
            }
            if let Some(client_user_id) = session.client_user_id.as_deref() {
                if !self.drain_recovered_ingress(ordered_ingress_tx, client_user_id) {
                    return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
                }
            }

            let heartbeat_deadline = next_heartbeat_at;
            let ack_deadline = heartbeat_ack_deadline;
            let now = tokio::time::Instant::now();
            if received_hello || now < hello_deadline {
                hello_deadline_probed = false;
            }
            let heartbeat_due = heartbeat_deadline.is_some_and(|deadline| now >= deadline)
                || ack_deadline.is_some_and(|deadline| now >= deadline);
            if !heartbeat_due {
                heartbeat_deadline_probed = false;
            }
            let probe_ready_frame = (!received_hello
                && now >= hello_deadline
                && !hello_deadline_probed)
                || (heartbeat_due && !heartbeat_deadline_probed);
            if probe_ready_frame {
                heartbeat_deadline_probed |= heartbeat_due;
                let mut drained = 0usize;
                while drained < DISCORD_HEARTBEAT_READY_FRAME_DRAIN_CAP {
                    let Some(frame) = socket.next().now_or_never() else {
                        break;
                    };
                    drained += 1;
                    let heartbeat_was_pending = session.heartbeat_pending;
                    if let Some(outcome) = self
                        .handle_socket_frame(
                            frame,
                            &mut socket,
                            session,
                            ordered_ingress_tx,
                            pending_initial_ack,
                            &mut received_hello,
                            &mut heartbeat_ack_deadline,
                            &mut next_heartbeat_at,
                            stop,
                        )
                        .await?
                    {
                        return Ok(outcome);
                    }
                    if heartbeat_was_pending && !session.heartbeat_pending {
                        break;
                    }
                }
                if drained == DISCORD_HEARTBEAT_READY_FRAME_DRAIN_CAP {
                    // A perpetually ready dispatch stream must not starve a
                    // due HELLO/heartbeat timer. ACKs had one bounded chance
                    // to win; the timer action now wins deterministically.
                    let now = tokio::time::Instant::now();
                    if !received_hello && now >= hello_deadline {
                        return Err(GatewayError::ConnectionFailed(
                            "Discord Gateway did not send HELLO before the deadline".to_string(),
                        ));
                    }
                    let heartbeat_due = next_heartbeat_at
                        .is_some_and(|deadline| now >= deadline)
                        || heartbeat_ack_deadline
                            .is_some_and(|deadline| now >= deadline);
                    if heartbeat_due && received_hello {
                        if session.heartbeat_pending {
                            warn!("Discord Gateway heartbeat was not acknowledged before its ACK deadline; reconnecting");
                            return Ok(DiscordGatewayConnectionOutcome::Reconnect);
                        }
                        if !self.send_heartbeat(&mut socket, session).await? {
                            return Ok(DiscordGatewayConnectionOutcome::Reconnect);
                        }
                        heartbeat_ack_deadline = Self::next_heartbeat_deadline(session);
                        next_heartbeat_at = Self::next_heartbeat_deadline(session);
                    }
                    heartbeat_deadline_probed = false;
                    tokio::task::yield_now().await;
                }
                if drained > 0 {
                    // HELLO and heartbeat ACK handling can alter the timer
                    // state. Rebuild every deadline snapshot before waiting.
                    continue;
                }
            }

            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return Ok(DiscordGatewayConnectionOutcome::Stopped);
                    }
                }
                changed = ingress_done.changed() => {
                    if changed.is_err() || *ingress_done.borrow() {
                        return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
                    }
                }
                ack = sequence_ack_rx.recv() => {
                    let Some(ack) = ack else {
                        return Ok(DiscordGatewayConnectionOutcome::Unhealthy);
                    };
                    Self::commit_sequence_ack(session, pending_initial_ack, ack);
                }
                _ = recovery_retry.tick() => {
                    // The loop top retries only the retained recovery head via
                    // try_send; this wakeup never blocks socket reads.
                }
                _ = async {
                    if received_hello {
                        std::future::pending::<()>().await;
                    } else {
                        tokio::time::sleep_until(hello_deadline).await;
                    }
                } => {
                    return Err(GatewayError::ConnectionFailed(
                        "Discord Gateway did not send HELLO before the deadline".to_string(),
                    ));
                }
                _ = async {
                    if let Some(deadline) = ack_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    if session.heartbeat_pending {
                        warn!("Discord Gateway heartbeat was not acknowledged before its ACK deadline; reconnecting");
                        return Ok(DiscordGatewayConnectionOutcome::Reconnect);
                    }
                    heartbeat_ack_deadline = None;
                    heartbeat_deadline_probed = false;
                }
                _ = async {
                    if let Some(deadline) = heartbeat_deadline {
                        tokio::time::sleep_until(deadline).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {
                    // `heartbeat_tick` turns an unacknowledged prior beat
                    // into a reconnect. Skipping it while pending lets a
                    // stream of OP 1 requests postpone liveness forever.
                    if !self.send_heartbeat(&mut socket, session).await? {
                        return Ok(DiscordGatewayConnectionOutcome::Reconnect);
                    }
                    heartbeat_ack_deadline = Self::next_heartbeat_deadline(session);
                    next_heartbeat_at = Self::next_heartbeat_deadline(session);
                    heartbeat_deadline_probed = false;
                }
                frame = socket.next() => {
                    if let Some(outcome) = self
                        .handle_socket_frame(
                            frame,
                            &mut socket,
                            session,
                            ordered_ingress_tx,
                            pending_initial_ack,
                            &mut received_hello,
                            &mut heartbeat_ack_deadline,
                            &mut next_heartbeat_at,
                            stop,
                        )
                        .await?
                    {
                        return Ok(outcome);
                    }
                }
            }
        }
    }

    async fn handle_socket_frame(
        &self,
        frame: Option<Result<Message, tokio_tungstenite::tungstenite::Error>>,
        socket: &mut DiscordWebSocket,
        session: &mut GatewaySession,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        received_hello: &mut bool,
        heartbeat_ack_deadline: &mut Option<tokio::time::Instant>,
        next_heartbeat_at: &mut Option<tokio::time::Instant>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<Option<DiscordGatewayConnectionOutcome>, GatewayError> {
        match frame {
            Some(Ok(Message::Text(text))) => {
                let payload = match serde_json::from_str::<GatewayPayload>(text.as_str()) {
                    Ok(payload) => payload,
                    Err(error) => {
                        warn!(error = %error, "discarding malformed Discord Gateway payload");
                        return Ok(None);
                    }
                };
                let opcode = payload.op;
                let heartbeat_pending_before = session.heartbeat_pending;
                let action = self
                    .process_gateway_payload(
                        socket,
                        session,
                        ordered_ingress_tx,
                        pending_initial_ack,
                        payload,
                        stop,
                    )
                    .await?;
                // Do not consume the HELLO deadline or an IDENTIFY slot for a
                // malformed HELLO. GatewaySession returns Reconnect before it
                // can emit authentication actions when the heartbeat interval
                // is absent, non-numeric, or zero.
                if opcode == opcodes::HELLO
                    && action.is_none()
                    && session.heartbeat_interval_ms.is_some()
                {
                    *received_hello = true;
                }
                *next_heartbeat_at = Self::heartbeat_deadline_after_payload(
                    *next_heartbeat_at,
                    session,
                    opcode,
                );
                if opcode == opcodes::HEARTBEAT
                    && !heartbeat_pending_before
                    && session.heartbeat_pending
                {
                    // OP 1 creates a new pending heartbeat only if no prior
                    // ACK was outstanding. A repeated request may still be
                    // answered, but it must not move the original ACK
                    // deadline.
                    *heartbeat_ack_deadline = Self::next_heartbeat_deadline(session);
                } else if opcode == opcodes::HEARTBEAT_ACK {
                    *heartbeat_ack_deadline = None;
                }
                Ok(action)
            }
            Some(Ok(Message::Ping(payload))) => {
                self.send_socket_message(socket, Message::Pong(payload), "pong")
                    .await?;
                Ok(None)
            }
            Some(Ok(Message::Close(frame))) => {
                let code = frame.as_ref().map(|close| u16::from(close.code));
                let outcome = discord_gateway_close_outcome(session, code);
                match outcome {
                    DiscordGatewayConnectionOutcome::Fatal => {
                        error!(close_code = ?code, "Discord Gateway rejected this connection permanently");
                    }
                    DiscordGatewayConnectionOutcome::Reconnect => {
                        debug!(close_code = ?code, "Discord Gateway closed socket; reconnecting");
                    }
                    _ => {}
                }
                Ok(Some(outcome))
            }
            None => Ok(Some(DiscordGatewayConnectionOutcome::Reconnect)),
            Some(Ok(_)) => Ok(None),
            Some(Err(error)) => Err(GatewayError::ConnectionFailed(format!(
                "Discord Gateway socket error: {error}"
            ))),
        }
    }

    /// Wait for a fresh IDENTIFY to become eligible before opening a socket,
    /// without consuming the slot. This keeps a budget-exhausted process from
    /// holding a live socket whose heartbeats and close frames cannot be read.
    async fn await_identify_eligibility(
        &self,
        stop: &mut watch::Receiver<bool>,
        ingress_done: &mut watch::Receiver<bool>,
    ) -> DiscordAuthenticationPreparation {
        loop {
            if *stop.borrow() {
                return DiscordAuthenticationPreparation::Stopped;
            }
            if *ingress_done.borrow() {
                return DiscordAuthenticationPreparation::Unhealthy;
            }
            let deadline = self.identify_pacer.lock().await.next_deadline();
            if deadline <= tokio::time::Instant::now() {
                return DiscordAuthenticationPreparation::Ready;
            }
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        return DiscordAuthenticationPreparation::Stopped;
                    }
                }
                changed = ingress_done.changed() => {
                    if changed.is_err() || *ingress_done.borrow() {
                        return DiscordAuthenticationPreparation::Unhealthy;
                    }
                }
            }
        }
    }

    /// Atomically consume an already-eligible slot at the IDENTIFY write.
    /// Eligibility was awaited before socket creation, so a false result means
    /// state changed while this connection was opening; reconnect rather than
    /// waiting inside the socket loop.
    async fn reserve_identify_slot(&self) -> bool {
        self.identify_pacer.lock().await.reserve_if_ready()
    }

    fn invalid_session_reconnect_outcome(
        &self,
        _resumable: bool,
    ) -> DiscordGatewayConnectionOutcome {
        // Discord requires a random 1–5 second delay before *every*
        // INVALID_SESSION reconnect. A resumable session retains its state;
        // a non-resumable one is reset by GatewaySession, but neither may
        // immediately IDENTIFY/RESUME.
        DiscordGatewayConnectionOutcome::ReconnectAfter((self.invalid_session_delay)())
    }

    async fn process_gateway_payload(
        &self,
        socket: &mut DiscordWebSocket,
        session: &mut GatewaySession,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
        pending_initial_ack: &mut Option<DiscordSequenceAck>,
        payload: GatewayPayload,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<Option<DiscordGatewayConnectionOutcome>, GatewayError> {
        for action in session.handle_gateway_event(&payload) {
            match action {
                GatewayAction::SendIdentify => {
                    if !self.reserve_identify_slot().await {
                        return Ok(Some(DiscordGatewayConnectionOutcome::Reconnect));
                    }
                    *pending_initial_ack = None;
                    session.begin_identify();
                    self.send_payload(socket, self.payloads.identify()).await?;
                }
                GatewayAction::SendHeartbeatNow => {
                    self.send_requested_heartbeat(socket, session).await?;
                }
                GatewayAction::SendResume => {
                    if let (Some(session_id), Some(sequence)) = (
                        session.session_id.as_deref(),
                        session.resume_sequence(),
                    ) {
                        self.send_payload(socket, self.payloads.resume(session_id, sequence))
                            .await?;
                    } else {
                        // A partial READY must never produce a malformed RESUME
                        // or bypass fresh-session IDENTIFY pacing.
                        *pending_initial_ack = None;
                        session.reset();
                        return Ok(Some(DiscordGatewayConnectionOutcome::Reconnect));
                    }
                }
                GatewayAction::Reconnect => {
                    return Ok(Some(DiscordGatewayConnectionOutcome::Reconnect));
                }
                GatewayAction::InvalidSession(resumable) => {
                    if !resumable {
                        *pending_initial_ack = None;
                    }
                    return Ok(Some(self.invalid_session_reconnect_outcome(resumable)));
                }
                GatewayAction::Dispatch(event_name, data) => {
                    let is_ready = event_name == "READY";
                    let ack = payload.s.map(|sequence| DiscordSequenceAck {
                        generation: session.generation(),
                        sequence,
                    });
                    let outcome = if event_name == "INTERACTION_CREATE" {
                        // This adapter has no registered interaction response
                        // consumer. Log and explicitly drop the unsupported
                        // event, then commit its sequence below so RESUME
                        // cannot replay it forever and block FIFO ingress.
                        warn!(
                            "Discord INTERACTION_CREATE received without a response consumer; dropping unsupported dispatch"
                        );
                        DiscordInboundDispatchOutcome::Ignored
                    } else if Self::is_thread_context_event(&event_name) {
                        self.enqueue_thread_context_event(
                            event_name,
                            data,
                            ack,
                            ordered_ingress_tx,
                        )
                    } else {
                        self.dispatch_message_create(
                            &event_name,
                            &data,
                            session,
                            ack,
                            ordered_ingress_tx,
                        )
                        .await
                    };
                    let outcome = if outcome == DiscordInboundDispatchOutcome::Ignored {
                        match ack {
                            Some(ack) => {
                                self.enqueue_sequence_commit(ack, ordered_ingress_tx)
                            }
                            None => DiscordInboundDispatchOutcome::Queued,
                        }
                    } else {
                        outcome
                    };
                    Self::track_initial_durability(
                        pending_initial_ack,
                        is_ready,
                        ack,
                        outcome,
                    );
                    if is_ready && session.identified {
                        if let Some(client_user_id) = session.client_user_id.clone() {
                            if !self
                                .drain_recovered_ingress(ordered_ingress_tx, client_user_id.as_str())
                            {
                                return Ok(Some(DiscordGatewayConnectionOutcome::Unhealthy));
                            }
                        }
                    }
                    match outcome {
                        DiscordInboundDispatchOutcome::Backpressure => {
                            warn!(
                                resume_sequence = ?session.resume_sequence(),
                                "Discord ordered ingress stage is full; reconnecting for replay"
                            );
                            return Ok(Some(DiscordGatewayConnectionOutcome::Backpressure));
                        }
                        DiscordInboundDispatchOutcome::Unhealthy => {
                            return Ok(Some(DiscordGatewayConnectionOutcome::Unhealthy));
                        }
                        DiscordInboundDispatchOutcome::Ignored
                        | DiscordInboundDispatchOutcome::Queued => {}
                    }
                }
            }
        }

        Ok(None)
    }

    async fn send_heartbeat(
        &self,
        socket: &mut DiscordWebSocket,
        session: &mut GatewaySession,
    ) -> Result<bool, GatewayError> {
        match session.heartbeat_tick() {
            HeartbeatTick::Send => {
                self.send_payload(
                    socket,
                    DiscordGatewayPayloadBuilder::heartbeat(session.sequence),
                )
                .await?;
                session.heartbeat_sent();
                Ok(true)
            }
            HeartbeatTick::Reconnect => {
                warn!("Discord Gateway heartbeat was not acknowledged; reconnecting");
                Ok(false)
            }
        }
    }

    async fn send_requested_heartbeat(
        &self,
        socket: &mut DiscordWebSocket,
        session: &mut GatewaySession,
    ) -> Result<(), GatewayError> {
        self.send_payload(socket, DiscordGatewayPayloadBuilder::heartbeat(session.sequence))
            .await?;
        session.heartbeat_sent();
        Ok(())
    }

    fn is_thread_context_event(event_name: &str) -> bool {
        matches!(
            event_name,
            "THREAD_CREATE"
                | "THREAD_UPDATE"
                | "CHANNEL_CREATE"
                | "CHANNEL_UPDATE"
                | "THREAD_LIST_SYNC"
                | "GUILD_CREATE"
                | "THREAD_DELETE"
                | "CHANNEL_DELETE"
        )
    }

    fn enqueue_thread_context_event(
        &self,
        event_name: String,
        data: serde_json::Value,
        ack: Option<DiscordSequenceAck>,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
    ) -> DiscordInboundDispatchOutcome {
        match ordered_ingress_tx.try_send(DiscordOrderedIngress::Context {
            ack,
            event_name,
            data,
        }) {
            Ok(()) => DiscordInboundDispatchOutcome::Queued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    "Discord ordered ingress stage is full; leaving channel context event for resumable replay"
                );
                DiscordInboundDispatchOutcome::Backpressure
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                error!("Discord ordered ingress stage closed before channel context event");
                DiscordInboundDispatchOutcome::Unhealthy
            }
        }
    }

    fn enqueue_sequence_commit(
        &self,
        ack: DiscordSequenceAck,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
    ) -> DiscordInboundDispatchOutcome {
        match ordered_ingress_tx.try_send(DiscordOrderedIngress::Commit(ack)) {
            Ok(()) => DiscordInboundDispatchOutcome::Queued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!(
                    "Discord ordered ingress stage is full; leaving dispatch for resumable replay"
                );
                DiscordInboundDispatchOutcome::Backpressure
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                error!("Discord ordered ingress stage closed before sequence commit");
                DiscordInboundDispatchOutcome::Unhealthy
            }
        }
    }

    fn observe_thread_context(&self, event_name: &str, data: &serde_json::Value) {
        fn string_id(value: Option<&serde_json::Value>) -> Option<&str> {
            value
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
        }

        fn record_channel(
            contexts: &mut DiscordThreadContextCache,
            channel: &serde_json::Value,
            outer_guild_id: Option<&str>,
        ) {
            let Some(channel_id) = string_id(channel.get("id")) else {
                return;
            };
            let channel_guild_id = string_id(channel.get("guild_id"));
            let guild_id = match (outer_guild_id, channel_guild_id) {
                (Some(outer), Some(channel)) if outer != channel => return,
                (Some(outer), _) => outer,
                (None, Some(channel)) => channel,
                (None, None) => return,
            };
            let Some(channel_type) = channel
                .get("type")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u8::try_from(value).ok())
            else {
                return;
            };
            if matches!(channel_type, 10 | 11 | 12) {
                if let Some(parent_channel_id) = string_id(channel.get("parent_id")) {
                    contexts.record_thread(guild_id, channel_id, parent_channel_id);
                }
            } else {
                contexts.record_root(guild_id, channel_id);
            }
        }

        let mut contexts = self
            .thread_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match event_name {
            "THREAD_CREATE" | "THREAD_UPDATE" | "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
                record_channel(&mut contexts, data, string_id(data.get("guild_id")));
            }
            "THREAD_LIST_SYNC" => {
                let guild_id = string_id(data.get("guild_id"));
                if let Some(threads) = data.get("threads").and_then(serde_json::Value::as_array) {
                    for thread in threads {
                        record_channel(&mut contexts, thread, guild_id);
                    }
                }
            }
            "GUILD_CREATE" => {
                let guild_id = string_id(data.get("id"));
                for key in ["channels", "threads"] {
                    if let Some(channels) =
                        data.get(key).and_then(serde_json::Value::as_array)
                    {
                        for channel in channels {
                            record_channel(&mut contexts, channel, guild_id);
                        }
                    }
                }
            }
            "THREAD_DELETE" | "CHANNEL_DELETE" => {
                if let (Some(guild_id), Some(channel_id)) = (
                    string_id(data.get("guild_id")),
                    string_id(data.get("id")),
                ) {
                    contexts.remove(guild_id, channel_id);
                }
            }
            _ => {}
        }
    }

    /// Resolve non-DM ingress only from trusted event, cache, or REST state.
    fn resolve_thread_parent(
        &self,
        message: &mut IncomingDiscordMessage,
    ) -> DiscordThreadResolution {
        if message.is_dm {
            if matches!(message.channel_type, Some(10 | 11 | 12))
                || message.thread_id.is_some()
                || message.parent_channel_id.is_some()
            {
                return DiscordThreadResolution::Rejected;
            }
            return DiscordThreadResolution::Resolved;
        }

        let declared_guild_id = message
            .guild_id
            .as_deref()
            .map(str::trim)
            .filter(|guild_id| !guild_id.is_empty())
            .map(str::to_string);
        let cached = {
            let contexts = self
                .thread_contexts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match declared_guild_id.as_deref() {
                Some(guild_id) => contexts.context_for(guild_id, &message.channel_id),
                None => contexts
                    .context_for_channel(&message.channel_id)
                    .map(DiscordCachedChannelLookup::Context)
                    .unwrap_or(DiscordCachedChannelLookup::Missing),
            }
        };
        let Some(guild_id) = declared_guild_id else {
            return match cached {
                // The cache proves this is guild traffic, but cannot invent
                // the event's missing guild authority.
                DiscordCachedChannelLookup::Context(_) => DiscordThreadResolution::Rejected,
                // Ask REST so transient lookup failure retains/rejournals the
                // ordered ingress instead of being silently accepted as a DM.
                DiscordCachedChannelLookup::Missing => DiscordThreadResolution::NeedsLookup,
                DiscordCachedChannelLookup::GuildMismatch => DiscordThreadResolution::Rejected,
            };
        };
        let payload_parent = message
            .parent_channel_id
            .as_deref()
            .map(str::trim)
            .filter(|parent_channel_id| !parent_channel_id.is_empty())
            .map(str::to_string);

        match cached {
            DiscordCachedChannelLookup::Context(DiscordCachedChannelContext::Thread {
                parent_channel_id,
                ..
            }) => {
                if payload_parent
                    .as_deref()
                    .is_some_and(|parent| parent != parent_channel_id)
                {
                    return DiscordThreadResolution::Rejected;
                }
                message.thread_id = Some(message.channel_id.clone());
                message.parent_channel_id = Some(parent_channel_id);
                DiscordThreadResolution::Resolved
            }
            DiscordCachedChannelLookup::Context(DiscordCachedChannelContext::Root { .. }) => {
                if message.thread_id.is_some() || payload_parent.is_some() {
                    DiscordThreadResolution::Rejected
                } else {
                    DiscordThreadResolution::Resolved
                }
            }
            DiscordCachedChannelLookup::Missing => {
                if let Some(parent_channel_id) = payload_parent {
                    // A guild-bearing event can authoritatively seed a missed
                    // cache entry. A guild-less event reached REST above.
                    self.thread_contexts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .record_thread(&guild_id, &message.channel_id, &parent_channel_id);
                    message.thread_id = Some(message.channel_id.clone());
                    message.parent_channel_id = Some(parent_channel_id);
                    DiscordThreadResolution::Resolved
                } else {
                    DiscordThreadResolution::NeedsLookup
                }
            }
            DiscordCachedChannelLookup::GuildMismatch => DiscordThreadResolution::Rejected,
        }
    }

    async fn send_payload(
        &self,
        socket: &mut DiscordWebSocket,
        payload: GatewayPayload,
    ) -> Result<(), GatewayError> {
        let encoded = serde_json::to_string(&payload).map_err(|error| {
            GatewayError::ConnectionFailed(format!(
                "Discord Gateway payload serialization failed: {error}"
            ))
        })?;
        self.send_socket_message(socket, Message::Text(encoded.into()), "payload")
            .await
    }

    async fn send_socket_message(
        &self,
        socket: &mut DiscordWebSocket,
        message: Message,
        operation: &'static str,
    ) -> Result<(), GatewayError> {
        let mut stop = self.stop.clone();
        if *stop.borrow() {
            return Err(GatewayError::ConnectionFailed(
                "Discord Gateway stopped before socket write".to_string(),
            ));
        }

        tokio::select! {
            result = tokio::time::timeout(DISCORD_GATEWAY_IO_TIMEOUT, socket.send(message)) => {
                match result {
                    Ok(result) => result.map_err(|error| GatewayError::ConnectionFailed(format!(
                        "Discord Gateway {operation} failed: {error}"
                    ))),
                    Err(_) => Err(GatewayError::ConnectionFailed(format!(
                        "Discord Gateway {operation} timed out"
                    ))),
                }
            }
            changed = stop.changed() => {
                let reason = if changed.is_err() || *stop.borrow() {
                    "stopped"
                } else {
                    "cancellation state changed"
                };
                Err(GatewayError::ConnectionFailed(format!(
                    "Discord Gateway {operation} cancelled because runtime {reason}"
                )))
            }
        }
    }

    /// Enqueue every MESSAGE_CREATE into one ordered, bounded ingress stage.
    ///
    /// Cache hits and misses share the same FIFO so an asynchronous
    /// authoritative channel lookup cannot reorder later Discord messages.
    async fn dispatch_message_create(
        &self,
        event_name: &str,
        data: &serde_json::Value,
        session: &GatewaySession,
        ack: Option<DiscordSequenceAck>,
        ordered_ingress_tx: &mpsc::Sender<DiscordOrderedIngress>,
    ) -> DiscordInboundDispatchOutcome {
        let Some(DispatchEvent::MessageCreate(message)) =
            DiscordAdapter::parse_dispatch(event_name, data)
        else {
            return DiscordInboundDispatchOutcome::Ignored;
        };
        if message.message_id.trim().is_empty() {
            // A journal reservation is keyed by the message ID. Never admit
            // an unusable key into the ordered FIFO: a full journal cannot
            // acknowledge it, which would otherwise strand every later
            // sequence behind this malformed dispatch.
            warn!("Discord MESSAGE_CREATE dropped with blank message ID");
            return DiscordInboundDispatchOutcome::Ignored;
        }
        let Some(client_user_id) = session
            .client_user_id
            .as_deref()
            .map(str::trim)
            .filter(|client_user_id| !client_user_id.is_empty())
        else {
            warn!(
                message_id = %message.message_id,
                "Discord MESSAGE_CREATE received before authoritative READY identity; refusing ingress"
            );
            return DiscordInboundDispatchOutcome::Ignored;
        };
        if !DiscordAdapter::should_accept_message(
            &message,
            Some(client_user_id),
            self.bot_policy,
        ) {
            debug!(message_id = %message.message_id, "Discord message dropped by sender policy");
            return DiscordInboundDispatchOutcome::Ignored;
        }
        if message.to_gateway_message().is_none() {
            warn!(message_id = %message.message_id, "Discord message is missing an attributable author ID");
            return DiscordInboundDispatchOutcome::Ignored;
        }
        let message_id = message.message_id.clone();
        let queued = DiscordUnresolvedIngress {
            message,
            client_user_id: session.client_user_id.clone(),
        };
        if self.has_recovered_ingress() {
            // Never let a live dispatch consume a slot ahead of a retained
            // durable record. Keep it behind recovery and reconnect so Discord
            // can replay the uncommitted sequence if needed.
            self.requeue_recovered_ingress([((&queued).into(), false)]);
            warn!(
                message_id = %message_id,
                "Discord recovered ingress is pending; retaining live message behind recovery"
            );
            return DiscordInboundDispatchOutcome::Backpressure;
        }
        match ordered_ingress_tx.try_send(DiscordOrderedIngress::Message {
            ack,
            ingress: queued,
            journaled: false,
        }) {
            Ok(()) => DiscordInboundDispatchOutcome::Queued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(
                ordered @ DiscordOrderedIngress::Message { .. },
            )) => {
                // Retain this message behind the already-full FIFO. The parent
                // will await its ordered send after this socket yields, and on
                // shutdown appends it only after the stage has handed off all
                // earlier items.
                self.retain_ordered_overflow(ordered);
                warn!(
                    message_id = %message_id,
                    "Discord ordered ingress stage is full; retaining overflow behind queued ingress"
                );
                DiscordInboundDispatchOutcome::Backpressure
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                error!("Discord ordered ingress returned an unexpected full item");
                DiscordInboundDispatchOutcome::Unhealthy
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(
                ordered @ DiscordOrderedIngress::Message { .. },
            )) => {
                // Stop can close the ordered consumer while a socket frame is
                // already ready in the producer select. Preserve the returned
                // value; its sequence is still uncommitted and it must enter
                // the same shutdown recovery FIFO as a full queue.
                self.retain_ordered_overflow(ordered);
                warn!(
                    message_id = %message_id,
                    "Discord ordered ingress stage closed during dispatch; retaining ingress for recovery"
                );
                DiscordInboundDispatchOutcome::Unhealthy
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                error!("Discord Gateway ordered ingress stage closed with a non-message item");
                DiscordInboundDispatchOutcome::Unhealthy
            }
        }
    }

    async fn run_ordered_ingress_stage(
        &self,
        receiver: mpsc::Receiver<DiscordOrderedIngress>,
        sequence_ack_tx: mpsc::UnboundedSender<DiscordSequenceAck>,
    ) {
        let mut stop = self.stop.clone();
        let mut recovery =
            DiscordOrderedIngressRecoveryGuard::new(self.for_ordered_ingress_stage(), receiver);
        'ingress: loop {
            if *stop.borrow() {
                break;
            }
            let Some(ordered) = (tokio::select! {
                biased;
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        None
                    } else {
                        continue;
                    }
                }
                ordered = recovery.recv() => ordered,
            }) else {
                break;
            };
            let (ack, mut queued, journaled) = match ordered {
                DiscordOrderedIngress::Commit(ack) => {
                    let _ = sequence_ack_tx.send(ack);
                    continue;
                }
                DiscordOrderedIngress::Context {
                    ack,
                    event_name,
                    data,
                } => {
                    self.observe_thread_context(&event_name, &data);
                    if let Some(ack) = ack {
                        let _ = sequence_ack_tx.send(ack);
                    }
                    continue;
                }
                DiscordOrderedIngress::Message {
                    ack,
                    ingress,
                    journaled,
                } => (ack, ingress, journaled),
            };
            // Set this before the first await. On abort, the guard requeues
            // the exact pre-resolution journal entry and durability state.
            recovery.retain_current(&queued, journaled);

            if !journaled {
                match self.reserve_ingress(&queued).await {
                    Some(DiscordIngressReservationOutcome::Recorded)
                    | Some(DiscordIngressReservationOutcome::ExistingUnresolved) => {
                        recovery.mark_current_journaled();
                    }
                    Some(DiscordIngressReservationOutcome::ExistingRouteOwned) => {
                        recovery.discard_current();
                        if let Some(ack) = ack {
                            let _ = sequence_ack_tx.send(ack);
                        }
                        continue;
                    }
                    None => break,
                }
            }
            if let Some(ack) = ack {
                let _ = sequence_ack_tx.send(ack);
            }

            let mut retry_index = 0usize;
            loop {
                if *stop.borrow() {
                    break 'ingress;
                }
                match self.resolve_thread_parent(&mut queued.message) {
                    DiscordThreadResolution::Resolved => break,
                    DiscordThreadResolution::Rejected => {
                        warn!(
                            message_id = %queued.message.message_id,
                            channel_id = %queued.message.channel_id,
                            "Discord guild ingress rejected because its cached context lacks the authoritative guild"
                        );
                        self.remove_journaled_ingress(&queued.message.message_id).await;
                        recovery.discard_current();
                        continue 'ingress;
                    }
                    DiscordThreadResolution::NeedsLookup => {
                        match self.resolve_thread_parent_from_rest(&mut queued.message).await {
                            Ok(()) => break,
                            Err(DiscordThreadLookupError::Retry {
                                retry_after,
                                message,
                            }) => {
                                let fallback_ms = DISCORD_GATEWAY_BACKOFF_STEPS_MS[retry_index.min(
                                    DISCORD_GATEWAY_BACKOFF_STEPS_MS.len().saturating_sub(1),
                                )];
                                retry_index = (retry_index + 1).min(
                                    DISCORD_GATEWAY_BACKOFF_STEPS_MS.len().saturating_sub(1),
                                );
                                let delay = retry_after.unwrap_or_else(|| {
                                    std::time::Duration::from_millis(fallback_ms)
                                });
                                warn!(
                                    message_id = %queued.message.message_id,
                                    channel_id = %queued.message.channel_id,
                                    error = %message,
                                    delay_ms = delay.as_millis(),
                                    "Discord channel lookup failed transiently; retaining ordered ingress"
                                );
                                tokio::select! {
                                    _ = tokio::time::sleep(delay) => {}
                                    changed = stop.changed() => {
                                        if changed.is_err() || *stop.borrow() {
                                            break 'ingress;
                                        }
                                    }
                                }
                            }
                            Err(DiscordThreadLookupError::Rejected { message }) => {
                                warn!(
                                    message_id = %queued.message.message_id,
                                    channel_id = %queued.message.channel_id,
                                    error = %message,
                                    "Discord guild ingress rejected because authoritative channel context is invalid"
                                );
                                self.remove_journaled_ingress(&queued.message.message_id).await;
                                recovery.discard_current();
                                continue 'ingress;
                            }
                            Err(DiscordThreadLookupError::PreserveAndStop { message }) => {
                                error!(
                                    message_id = %queued.message.message_id,
                                    channel_id = %queued.message.channel_id,
                                    error = %message,
                                    "Discord channel lookup cannot safely continue; retaining journaled ingress"
                                );
                                break 'ingress;
                            }
                            Err(DiscordThreadLookupError::Stopped) => break 'ingress,
                        }
                    }
                }
            }

            let message_id = queued.message.message_id.clone();
            let Some(mut inbound) = self.prepare_routable_inbound(
                queued.message,
                queued.client_user_id.as_deref(),
            ) else {
                self.remove_journaled_ingress(&message_id).await;
                recovery.discard_current();
                continue;
            };
            loop {
                if *stop.borrow() {
                    break 'ingress;
                }
                match self.try_enqueue_inbound(inbound).await {
                    DiscordInboundQueueOutcome::Queued => {
                        recovery.discard_current();
                        continue 'ingress;
                    }
                    DiscordInboundQueueOutcome::Full(returned) => {
                        inbound = returned;
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_millis(
                                DISCORD_INBOUND_RETRY_MS,
                            )) => {}
                            changed = stop.changed() => {
                                if changed.is_err() || *stop.borrow() {
                                    break 'ingress;
                                }
                            }
                        }
                    }
                    DiscordInboundQueueOutcome::Unhealthy => break 'ingress,
                }
            }
        }
    }

    async fn reserve_ingress_for_shutdown(
        &self,
        entry: &DiscordJournalEntry,
        deadline: tokio::time::Instant,
    ) -> Option<DiscordIngressReservationOutcome> {
        let sender = self.journal_tx.read().await.clone()?;
        let (durability_ack, ack_rx) = tokio::sync::oneshot::channel();
        let reservation = DiscordIngressReservation {
            entry: entry.clone(),
            durability_ack: Some(durability_ack),
        };
        tokio::time::timeout_at(deadline, async {
            sender
                .send(DiscordIngressJournalEvent::Reserve(reservation))
                .await
                .ok()?;
            ack_rx.await.ok()
        })
        .await
        .ok()
        .flatten()
    }

    async fn reserve_ingress(
        &self,
        ingress: &DiscordUnresolvedIngress,
    ) -> Option<DiscordIngressReservationOutcome> {
        let sender = self.journal_tx.read().await.clone()?;
        let (durability_ack, ack_rx) = tokio::sync::oneshot::channel();
        let reservation = DiscordIngressReservation {
            entry: ingress.into(),
            durability_ack: Some(durability_ack),
        };
        let mut stop = self.stop.clone();
        if *stop.borrow() {
            return None;
        }
        tokio::select! {
            result = sender.send(DiscordIngressJournalEvent::Reserve(reservation)) => {
                result.ok()?;
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return None;
                }
            }
        }
        tokio::select! {
            result = ack_rx => result.ok(),
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    None
                } else {
                    None
                }
            }
        }
    }

    async fn remove_journaled_ingress(&self, message_id: &str) {
        let Some(sender) = self.journal_tx.read().await.clone() else {
            return;
        };
        let mut stop = self.stop.clone();
        tokio::select! {
            _ = sender.send(DiscordIngressJournalEvent::Remove {
                message_id: message_id.to_string(),
            }) => {}
            _ = stop.changed() => {}
        }
    }

    async fn resolve_thread_parent_from_rest(
        &self,
        message: &mut IncomingDiscordMessage,
    ) -> Result<(), DiscordThreadLookupError> {
        self.resolve_thread_parent_from_rest_until(
            message,
            tokio::time::Instant::now() + DISCORD_GATEWAY_IO_TIMEOUT,
        )
        .await
    }

    async fn resolve_thread_parent_from_rest_until(
        &self,
        message: &mut IncomingDiscordMessage,
        deadline: tokio::time::Instant,
    ) -> Result<(), DiscordThreadLookupError> {
        let declared_guild_id = message
            .guild_id
            .as_deref()
            .map(str::trim)
            .filter(|guild_id| !guild_id.is_empty())
            .map(str::to_string);
        let mut stop = self.stop.clone();
        if *stop.borrow() {
            return Err(DiscordThreadLookupError::Stopped);
        }
        if let Err(error) = self
            .global_rate_limit
            .wait_until_open(deadline, &mut stop, "Discord channel lookup")
            .await
        {
            if *stop.borrow() {
                return Err(DiscordThreadLookupError::Stopped);
            }
            let retry_after = match &error {
                GatewayError::RateLimited { retry_after } => *retry_after,
                _ => None,
            };
            return Err(DiscordThreadLookupError::Retry {
                retry_after,
                message: format!("Discord channel lookup deferred by global rate limit: {error}"),
            });
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DiscordThreadLookupError::PreserveAndStop {
                message: "Discord channel lookup exceeded its deadline before request".to_string(),
            });
        }
        let request = self
            .rest_client
            .get(format!(
                "{}/channels/{}",
                self.api_base_url,
                urlencoding::encode(&message.channel_id)
            ))
            .header(
                "Authorization",
                format!("Bot {}", self.payloads.token),
            )
            .timeout(remaining)
            .send();
        let response = tokio::select! {
            result = tokio::time::timeout(remaining, request) => {
                match result {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => return Err(DiscordThreadLookupError::Retry {
                        retry_after: None,
                        message: format!("Discord channel lookup request failed: {error}"),
                    }),
                    Err(_) => return Err(DiscordThreadLookupError::PreserveAndStop {
                        message: "Discord channel lookup request exceeded its deadline".to_string(),
                    }),
                }
            }
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    return Err(DiscordThreadLookupError::Stopped);
                }
                return Err(DiscordThreadLookupError::Stopped);
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let retry_after_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<f64>().ok())
                .and_then(discord_retry_after_duration);
            let header_global = discord_rate_limit_is_global_header(response.headers());
            let header_delay =
                retry_after_header.unwrap_or(std::time::Duration::from_secs(1));
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS && header_global {
                // Publish header-proven global admission before consuming an
                // untrusted body so concurrent routes stop immediately.
                self.global_rate_limit.extend(header_delay);
            }
            let body = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                tokio::select! {
                    body = discord_response_text_limited(response) => body,
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(DiscordThreadLookupError::Retry {
                            retry_after: Some(header_delay),
                            message: "Discord channel lookup 429 body exceeded its deadline".to_string(),
                        });
                    }
                    _ = stop.changed() => return Err(DiscordThreadLookupError::Stopped),
                }
            } else {
                tokio::select! {
                    body = discord_response_text_limited(response) => body,
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(DiscordThreadLookupError::Retry {
                            retry_after: None,
                            message: "Discord channel lookup error body exceeded its deadline".to_string(),
                        });
                    }
                    _ = stop.changed() => return Err(DiscordThreadLookupError::Stopped),
                }
            };
            let message = format!("Discord channel lookup failed ({status}): {body}");
            if status == reqwest::StatusCode::NOT_FOUND
                || status == reqwest::StatusCode::GONE
                || status == reqwest::StatusCode::FORBIDDEN
            {
                return Err(DiscordThreadLookupError::Rejected { message });
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                let retry_after = if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let (retry_after, body_global) =
                        discord_rate_limit_delay(&body, retry_after_header);
                    if header_global || body_global {
                        self.global_rate_limit.extend(retry_after);
                    }
                    Some(retry_after)
                } else {
                    None
                };
                return Err(DiscordThreadLookupError::Retry {
                    retry_after,
                    message,
                });
            }
            return Err(DiscordThreadLookupError::PreserveAndStop { message });
        }
        let channel = tokio::select! {
            result = discord_response_json_limited::<serde_json::Value>(response) => {
                result.map_err(|error| DiscordThreadLookupError::PreserveAndStop {
                    message: format!("Discord channel lookup response was incomplete: {error}"),
                })?
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(DiscordThreadLookupError::PreserveAndStop {
                    message: "Discord channel lookup response body exceeded its deadline".to_string(),
                });
            }
            _ = stop.changed() => return Err(DiscordThreadLookupError::Stopped),
        };
        let channel_id = channel
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|channel_id| !channel_id.is_empty())
            .ok_or_else(|| DiscordThreadLookupError::PreserveAndStop {
                message: "Discord channel lookup omitted its channel ID".to_string(),
            })?;
        let response_guild_id = channel
            .get("guild_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|response_guild_id| !response_guild_id.is_empty())
            .ok_or_else(|| DiscordThreadLookupError::PreserveAndStop {
                message: "Discord channel lookup omitted its authoritative guild".to_string(),
            })?;
        let channel_type = channel
            .get("type")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u8::try_from(value).ok())
            .ok_or_else(|| DiscordThreadLookupError::PreserveAndStop {
                message: "Discord channel lookup omitted its channel type".to_string(),
            })?;
        if channel_id != message.channel_id {
            return Err(DiscordThreadLookupError::Rejected {
                message: "Discord channel lookup returned a different channel ID".to_string(),
            });
        }
        let Some(guild_id) = declared_guild_id else {
            return Err(DiscordThreadLookupError::Rejected {
                message: "Discord guild ingress omitted its authoritative guild".to_string(),
            });
        };
        if response_guild_id != guild_id {
            return Err(DiscordThreadLookupError::Rejected {
                message: "Discord channel lookup returned a different guild".to_string(),
            });
        }
        message.guild_id = Some(guild_id);
        let mut contexts = self
            .thread_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(channel_type, 10 | 11 | 12) {
            let parent_channel_id = channel
                .get("parent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| DiscordThreadLookupError::PreserveAndStop {
                    message: "Discord thread channel lookup omitted its parent".to_string(),
                })?;
            contexts.record_thread(response_guild_id, &message.channel_id, parent_channel_id);
            message.thread_id = Some(message.channel_id.clone());
            message.parent_channel_id = Some(parent_channel_id.to_string());
        } else {
            if message.thread_id.is_some() {
                return Err(DiscordThreadLookupError::Rejected {
                    message:
                        "Discord payload identified a thread but REST identified a root channel"
                            .to_string(),
                });
            }
            contexts.record_root(response_guild_id, &message.channel_id);
        }
        Ok(())
    }

    fn prepare_routable_inbound(
        &self,
        message: IncomingDiscordMessage,
        client_user_id: Option<&str>,
    ) -> Option<DiscordInboundMessage> {
        let context = message.channel_context();
        if self.channel_controls.is_ignored(&context) {
            debug!(channel_id = %context.channel_id, "Discord message dropped by channel policy");
            return None;
        }
        let bot_participated_in_thread = message.thread_id.as_deref().is_some_and(|thread_id| {
            self.thread_participation
                .lock()
                .map(|tracker| tracker.contains(thread_id))
                .unwrap_or(false)
        });
        let bot_mentioned = client_user_id.is_some_and(|user_id| message.mentions_user(user_id));
        if !discord_allows_message_without_mention(
            self.require_mention,
            &self.channel_controls,
            &context,
            bot_participated_in_thread,
            bot_mentioned,
        ) {
            debug!(message_id = %message.message_id, "Discord message dropped because it does not mention the bot");
            return None;
        }
        Some(DiscordInboundMessage {
            sender_is_bot: message.is_bot,
            client_user_id: client_user_id.map(str::to_string),
            message,
        })
    }

    /// Attempts one nonblocking handoff to the CLI-owned bounded queue.
    async fn try_enqueue_inbound(
        &self,
        inbound: DiscordInboundMessage,
    ) -> DiscordInboundQueueOutcome {
        let message_id = inbound.message.message_id.clone();
        let sender = self.inbound_tx.read().await.clone();
        let Some(sender) = sender else {
            error!(
                message_id = %message_id,
                "Discord Gateway has no inbound receiver; terminating worker"
            );
            return DiscordInboundQueueOutcome::Unhealthy;
        };

        match sender.try_send(inbound) {
            Ok(()) => DiscordInboundQueueOutcome::Queued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(inbound)) => {
                DiscordInboundQueueOutcome::Full(inbound)
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                error!(
                    message_id = %message_id,
                    "Discord Gateway inbound receiver is closed; terminating worker"
                );
                DiscordInboundQueueOutcome::Unhealthy
            }
        }
    }


    fn connection_url(&self, session: &GatewaySession) -> Result<String, GatewayError> {
        let base = session
            .can_resume()
            .then_some(session.resume_gateway_url.as_deref())
            .flatten()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(self.gateway_url.as_str());
        discord_gateway_v10_url(base)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordAuthenticationPreparation {
    Ready,
    Stopped,
    Unhealthy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordGatewayConnectionOutcome {
    /// Discord rejected credentials, shard setup, API version, or intents.
    /// Retrying cannot fix these conditions.
    Fatal,
    Reconnect,
    /// Discord requires a randomized 1–5 second wait after every
    /// `INVALID_SESSION`, whether the retained session is resumable or not.
    ReconnectAfter(std::time::Duration),
    /// The ordered ingress queue is saturated. Reconnect from the prior
    /// sequence so Discord can replay the event that was not retained.
    Backpressure,
    /// The CLI receiver is absent or closed; do not reconnect indefinitely.
    Unhealthy,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordInboundDispatchOutcome {
    Ignored,
    Queued,
    Backpressure,
    Unhealthy,
}

enum DiscordInboundQueueOutcome {
    Queued,
    Full(DiscordInboundMessage),
    Unhealthy,
}


/// Apply Discord's documented gateway close-code semantics to the owned
/// session. Invalid sequence/session timeout must discard resume state before
/// the next HELLO; authentication/sharding/intent failures are terminal.
fn discord_gateway_close_outcome(
    session: &mut GatewaySession,
    close_code: Option<u16>,
) -> DiscordGatewayConnectionOutcome {
    match close_code {
        Some(4007 | 4009) => {
            session.reset();
            DiscordGatewayConnectionOutcome::Reconnect
        }
        Some(4004 | 4010..=4014) => DiscordGatewayConnectionOutcome::Fatal,
        _ => DiscordGatewayConnectionOutcome::Reconnect,
    }
}

fn discord_url_host(url: &url::Url) -> Option<String> {
    url.host().map(|host| match host {
        url::Host::Domain(domain) => domain.to_string(),
        url::Host::Ipv4(address) => address.to_string(),
        url::Host::Ipv6(address) => address.to_string(),
    })
}

fn discord_connect_authority(host: &str, port: u16) -> String {
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn discord_effective_proxy(proxy: &AdapterProxyConfig) -> AdapterProxyConfig {
    if let Some(socks_proxy) = proxy
        .socks_proxy
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return AdapterProxyConfig {
            http_proxy: None,
            socks_proxy: Some(socks_proxy.to_string()),
        };
    }
    AdapterProxyConfig {
        http_proxy: proxy
            .http_proxy
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        socks_proxy: None,
    }
}

fn discord_retry_after_duration(seconds: f64) -> Option<std::time::Duration> {
    if !seconds.is_finite() || seconds < 0.0 {
        return None;
    }
    std::time::Duration::try_from_secs_f64(seconds).ok()
}

fn validate_discord_proxy(proxy: &AdapterProxyConfig) -> Result<(), GatewayError> {
    let configured = proxy
        .socks_proxy
        .as_deref()
        .map(|raw| (raw, &["socks5", "socks5h"][..], "SOCKS"))
        .or_else(|| {
            proxy
                .http_proxy
                .as_deref()
                .map(|raw| (raw, &["http"][..], "HTTP"))
        });
    let Some((raw, schemes, label)) = configured else {
        return Ok(());
    };
    let parsed = url::Url::parse(raw).map_err(|_| {
        GatewayError::ConnectionFailed(format!("Invalid Discord {label} proxy URL"))
    })?;
    if !schemes.contains(&parsed.scheme())
        || parsed.host_str().is_none()
        || !(parsed.path().is_empty() || parsed.path() == "/")
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(GatewayError::ConnectionFailed(format!(
            "Unsupported Discord {label} proxy URL"
        )));
    }
    let has_username = !parsed.username().is_empty();
    let has_password = parsed.password().is_some();
    if has_username != has_password
        || urlencoding::decode(parsed.username()).is_err()
        || parsed
            .password()
            .is_some_and(|password| urlencoding::decode(password).is_err())
    {
        return Err(GatewayError::ConnectionFailed(format!(
            "Invalid Discord {label} proxy credentials"
        )));
    }
    Ok(())
}

fn discord_gateway_v10_url(base: &str) -> Result<String, GatewayError> {
    let mut url = url::Url::parse(base).map_err(|error| {
        GatewayError::ConnectionFailed(format!("Invalid Discord Gateway URL: {error}"))
    })?;
    if !matches!(url.scheme(), "ws" | "wss") {
        return Err(GatewayError::ConnectionFailed(
            "Discord Gateway URL must use ws:// or wss://".to_string(),
        ));
    }
    if url.fragment().is_some() {
        return Err(GatewayError::ConnectionFailed(
            "Discord Gateway URL must not include a fragment".to_string(),
        ));
    }

    if !url.username().is_empty() || url.password().is_some() {
        return Err(GatewayError::ConnectionFailed(
            "Discord Gateway URL must not embed credentials".to_string(),
        ));
    }
    let semantic_query = url
        .query_pairs()
        .filter_map(|(key, value)| {
            if key.eq_ignore_ascii_case("compress") {
                return Some(Err(GatewayError::ConnectionFailed(
                    "Discord Gateway compression is unsupported; remove the compress query parameter"
                        .to_string(),
                )));
            }
            if key.eq_ignore_ascii_case("v") || key.eq_ignore_ascii_case("encoding") {
                return None;
            }
            Some(Ok((key.into_owned(), value.into_owned())))
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Preserve private routing/tenant parameters, but own exactly the
    // protocol knobs this text-frame implementation can safely support.
    url.set_query(None);
    {
        let mut query = url.query_pairs_mut();
        for (key, value) in semantic_query {
            query.append_pair(&key, &value);
        }
        query.append_pair("v", "10");
        query.append_pair("encoding", "json");
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod gateway_driver_tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};
    use tokio_tungstenite::accept_async;

    type MockDiscordWebSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

    const IDENTIFY_PACING_TEST_TIMEOUT: Duration = Duration::from_secs(7);


    fn driver_test_config(
        gateway_url: String,
        bot_policy: DiscordBotMessagePolicy,
    ) -> DiscordConfig {
        DiscordConfig {
            token: "test-token".to_string(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            api_base_url: None,
            gateway_url: Some(gateway_url),
            liveness_interval_seconds: 0.0,
            liveness_failure_threshold: 1,
            require_mention: false,
            intents: 1 << 9,
            bot_policy,
            reply_to_mode: "first".to_string(),
            channel_controls: DiscordChannelControls::default(),
            channel_skill_bindings: Vec::new(),
        }
    }

    struct TestRuntime {
        runtime: Arc<DiscordGatewayRuntime>,
        _state_dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestRuntime {
        type Target = Arc<DiscordGatewayRuntime>;

        fn deref(&self) -> &Self::Target {
            &self.runtime
        }
    }

    fn test_runtime(
        gateway_url: String,
        bot_policy: DiscordBotMessagePolicy,
    ) -> TestRuntime {
        test_runtime_from_config(driver_test_config(gateway_url, bot_policy))
    }

    fn test_runtime_from_config(config: DiscordConfig) -> TestRuntime {
        test_runtime_from_config_and_api(
            config,
            Client::new(),
            "http://127.0.0.1:9".to_string(),
        )
    }

    fn test_runtime_from_config_and_api(
        config: DiscordConfig,
        rest_client: Client,
        api_base_url: String,
    ) -> TestRuntime {
        let state_dir = tempfile::tempdir().expect("isolated Discord test state");
        let tracker = DiscordThreadParticipationTracker::from_path(
            state_dir.path().join("discord_threads.json"),
            DiscordThreadParticipationTracker::DEFAULT_MAX_TRACKED,
        );
        let runtime = Arc::new(DiscordGatewayRuntime::new(
            &config,
            Arc::new(Mutex::new(tracker)),
            rest_client,
            api_base_url,
        ));
        runtime
            .thread_contexts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_root("guild-1", "channel-1");
        TestRuntime {
            runtime,
            _state_dir: state_dir,
        }
    }

    async fn attach_test_durable_inbound(
        runtime: &DiscordGatewayRuntime,
        inbound_tx: mpsc::Sender<DiscordInboundMessage>,
    ) -> tokio::task::JoinHandle<()> {
        let (journal_tx, mut journal_rx) = mpsc::channel(16);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        tokio::spawn(async move {
            while let Some(event) = journal_rx.recv().await {
                if let DiscordIngressJournalEvent::Reserve(mut reservation) = event {
                    reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
                }
            }
        })
    }

    fn ordered_test_message(message_id: &str) -> IncomingDiscordMessage {
        IncomingDiscordMessage {
            channel_id: "channel-1".to_string(),
            channel_type: Some(0),
            guild_id: Some("guild-1".to_string()),
            parent_channel_id: None,
            thread_id: None,
            is_dm: false,
            message_id: message_id.to_string(),
            user_id: Some("user-1".to_string()),
            username: Some("Ada".to_string()),
            content: format!("payload-{message_id}"),
            is_bot: false,
            member_role_ids: vec!["role-1".to_string()],
            message_type: 0,
            mention_user_ids: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
            reply_to_attachments: Vec::new(),
            attachments: Vec::new(),
        }
    }

    #[tokio::test]
    async fn message_create_before_ready_identity_is_not_enqueued() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let session = GatewaySession::new();
        let (ordered_tx, mut ordered_rx) = mpsc::channel(1);
        let outcome = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "pre-ready-message",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "hello",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(outcome, DiscordInboundDispatchOutcome::Ignored);
        assert!(
            timeout(Duration::from_millis(50), ordered_rx.recv())
                .await
                .is_err(),
            "pre-READY MESSAGE_CREATE must not reach ordered ingress"
        );
    }

    #[tokio::test]
    async fn blank_message_id_is_ignored_without_blocking_later_ordered_ingress() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (journal_ids_tx, journal_ids_rx) = tokio::sync::oneshot::channel();
        let journal_task = tokio::spawn(async move {
            let mut ids = Vec::new();
            while let Some(event) = journal_rx.recv().await {
                if let DiscordIngressJournalEvent::Reserve(mut reservation) = event {
                    ids.push(reservation.entry().message.message_id.clone());
                    reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
                }
            }
            let _ = journal_ids_tx.send(ids);
        });
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        let (sequence_ack_tx, _sequence_ack_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_ack_tx)
                .await;
        });
        let mut session = GatewaySession::new();
        session.client_user_id = Some("bot-self".to_string());

        let blank = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": " \t ",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "ignored",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(blank, DiscordInboundDispatchOutcome::Ignored);

        let valid = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "valid-after-blank",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "process me",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                None,
                &ordered_tx,
            )
            .await;
        assert_eq!(valid, DiscordInboundDispatchOutcome::Queued);
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("valid ingress must not be blocked by blank predecessor")
            .expect("inbound receiver stays live");
        assert_eq!(inbound.message().message_id, "valid-after-blank");

        runtime.stop_tx.send_replace(true);
        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage exits cleanly")
            .expect("ordered stage task succeeds");
        drop(worker);
        drop(runtime);
        let journal_ids = timeout(Duration::from_secs(1), journal_ids_rx)
            .await
            .expect("journal sender closes on clean shutdown")
            .expect("journal task reports reserved IDs");
        assert_eq!(
            journal_ids,
            vec!["valid-after-blank"],
            "blank IDs must never enter the durable journal"
        );
        journal_task.await.expect("journal task succeeds");
    }

    #[tokio::test]
    async fn unsupported_interaction_commits_then_next_message_progresses_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(90, &url)).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::DISPATCH,
                    d: Some(serde_json::json!({
                        "id": "unsupported-interaction",
                        "type": 2,
                    })),
                    s: Some(91),
                    t: Some("INTERACTION_CREATE".to_string()),
                },
            )
            .await;
            send_payload(
                &mut socket,
                message_create_with_sequence("after-interaction", "human", false, None, 92),
            )
            .await;
            assert!(
                timeout(Duration::from_millis(250), socket.next()).await.is_err(),
                "unsupported interaction must not reconnect or replay before FIFO progresses"
            );
        });

        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("next MESSAGE_CREATE progresses")
            .expect("inbound receiver remains live");
        assert_eq!(inbound.message().message_id, "after-interaction");
        timeout(Duration::from_secs(1), server)
            .await
            .expect("mock gateway confirms no reconnect")
            .expect("mock gateway task succeeds");
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn ready_message_racing_a_closed_ordered_stage_is_retained_for_shutdown_recovery() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::All,
        );
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        session.client_user_id = Some("bot-self".to_string());
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        drop(ordered_rx);
        runtime.stop_tx.send_replace(true);

        let outcome = worker
            .dispatch_message_create(
                "MESSAGE_CREATE",
                &serde_json::json!({
                    "id": "ready-race-message",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "hello",
                    "author": { "id": "human", "username": "Ada", "bot": false }
                }),
                &session,
                Some(DiscordSequenceAck {
                    generation: 1,
                    sequence: 7,
                }),
                &ordered_tx,
            )
            .await;
        assert_eq!(outcome, DiscordInboundDispatchOutcome::Unhealthy);
        let retained = worker.take_ordered_overflow();
        assert_eq!(retained.len(), 1);
        let Some(DiscordOrderedIngress::Message {
            ack,
            ingress,
            journaled,
        }) = retained.front()
        else {
            panic!("closed ordered send must retain its returned message");
        };
        assert_eq!(ack.as_ref().map(|ack| ack.sequence), Some(7));
        assert_eq!(ingress.message.message_id, "ready-race-message");
        assert!(!journaled);
    }

    #[tokio::test]
    async fn durable_ingress_ack_gates_sequence_and_suppresses_replayed_reservations() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        let (sequence_tx, mut sequence_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });

        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: Some(DiscordSequenceAck {
                    generation: 7,
                    sequence: 41,
                }),
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-41"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        let mut reservation = match timeout(Duration::from_secs(1), journal_rx.recv())
            .await
            .expect("durability reservation")
            .expect("journal channel open")
        {
            DiscordIngressJournalEvent::Reserve(reservation) => reservation,
            DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
        };
        assert_eq!(reservation.entry().message().message_id, "message-41");
        assert!(matches!(
            inbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            sequence_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
        assert_eq!(
            timeout(Duration::from_secs(1), sequence_rx.recv())
                .await
                .expect("sequence commit after durability ACK"),
            Some(DiscordSequenceAck {
                generation: 7,
                sequence: 41,
            })
        );
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("routable ingress after durability ACK")
            .expect("inbound channel open");
        assert_eq!(inbound.message().message_id, "message-41");

        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: Some(DiscordSequenceAck {
                    generation: 7,
                    sequence: 42,
                }),
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-41"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        let mut replay = match timeout(Duration::from_secs(1), journal_rx.recv())
            .await
            .expect("replay durability reservation")
            .expect("journal channel open")
        {
            DiscordIngressJournalEvent::Reserve(reservation) => reservation,
            DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
        };
        replay.acknowledge_durable(DiscordIngressReservationOutcome::ExistingRouteOwned);
        assert_eq!(
            timeout(Duration::from_secs(1), sequence_rx.recv())
                .await
                .expect("replayed sequence commit"),
            Some(DiscordSequenceAck {
                generation: 7,
                sequence: 42,
            })
        );
        tokio::task::yield_now().await;
        assert!(matches!(
            inbound_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        runtime
            .queue_recovered_pre_policy_ingress(vec![DiscordJournalEntry::new(
                ordered_test_message("message-recovered"),
                Some("bot-self".to_string()),
            )])
            .expect("pre-policy recovery is accepted");
        let recovered = worker.take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (recovered, journaled) = recovered.into_iter().next().unwrap();
        assert!(journaled);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .unwrap();
        let inbound = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("recovered ingress bypasses re-reservation")
            .expect("inbound channel open");
        assert_eq!(inbound.message().message_id, "message-recovered");
        assert!(matches!(
            journal_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage stops")
            .expect("ordered stage joins");
    }

    #[tokio::test]
    async fn stop_durably_handoffs_unjournaled_ordered_ingress_before_restart() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(4);
        let (journal_tx, mut journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (ordered_tx, ordered_rx) = mpsc::channel(4);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-unfsynced"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .unwrap();
        runtime.stop_tx.send_replace(true);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let draining = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });
        timeout(Duration::from_secs(1), draining)
            .await
            .expect("stopped ordered stage drains into shared recovery")
            .expect("ordered stage joins");
        let flush = runtime.flush_recovered_ingress_for_shutdown();
        let journal = async {
            let mut reservation = match timeout(Duration::from_secs(1), journal_rx.recv())
                .await
                .expect("shutdown durability reservation")
                .expect("journal channel open")
            {
                DiscordIngressJournalEvent::Reserve(reservation) => reservation,
                DiscordIngressJournalEvent::Remove { .. } => panic!("unexpected journal removal"),
            };
            assert_eq!(reservation.entry().message().message_id, "message-unfsynced");
            reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
        };
        let (flush, ()) = tokio::join!(flush, journal);
        flush.expect("shutdown durability handoff");

        let recovered = runtime.worker().take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (recovered, journaled) = recovered.into_iter().next().unwrap();
        assert!(journaled, "shutdown handoff must commit only after journal ACK");

        runtime.stop_tx.send_replace(false);
        let (replay_tx, replay_rx) = mpsc::channel(4);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let replay_worker = runtime.worker();
        let stage = tokio::spawn(async move {
            replay_worker
                .run_ordered_ingress_stage(replay_rx, sequence_tx)
                .await;
        });
        replay_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), inbound_rx.recv())
                .await
                .expect("durably handed-off ingress is routed after restart")
                .expect("inbound channel open")
                .message()
                .message_id,
            "message-unfsynced"
        );
        assert!(matches!(
            journal_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        drop(replay_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("replay stage stops")
            .expect("replay stage joins");
    }

    #[tokio::test]
    async fn cancelled_shutdown_handoff_keeps_full_journal_entries_recoverable_in_fifo_order() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        journal_tx
            .send(DiscordIngressJournalEvent::Remove {
                message_id: "already-full".to_string(),
            })
            .await
            .expect("fill journal channel");
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        worker.requeue_recovered_ingress(vec![
            (
                DiscordJournalEntry::new(
                    ordered_test_message("message-cancel-first"),
                    Some("bot-self".to_string()),
                ),
                false,
            ),
            (
                DiscordJournalEntry::new(
                    ordered_test_message("message-cancel-second"),
                    Some("bot-self".to_string()),
                ),
                false,
            ),
        ]);

        assert!(
            timeout(
                Duration::from_millis(50),
                runtime.flush_recovered_ingress_for_shutdown()
            )
            .await
            .is_err(),
            "a full journal must leave shutdown handoff blocked until cancellation"
        );

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["message-cancel-first", "message-cancel-second"],
            "cancellation must retain every unjournaled entry in FIFO order"
        );
        assert!(
            recovered.iter().all(|(_, journaled)| !journaled),
            "cancelled reservations must not claim durable ownership"
        );
    }

    #[tokio::test]
    async fn aborting_blocked_ordered_stage_recovers_current_and_receiver_backlog() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        journal_tx
            .send(DiscordIngressJournalEvent::Remove {
                message_id: "already-full".to_string(),
            })
            .await
            .expect("fill journal channel");
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        for message_id in ["message-stage-first", "message-stage-second"] {
            ordered_tx
                .send(DiscordOrderedIngress::Message {
                    ack: None,
                    ingress: DiscordUnresolvedIngress {
                        message: ordered_test_message(message_id),
                        client_user_id: Some("bot-self".to_string()),
                    },
                    journaled: false,
                })
                .await
                .expect("queue ordered ingress");
        }
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage_worker = worker.clone();
        let mut stage = tokio::spawn(async move {
            stage_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });
        assert!(
            timeout(Duration::from_millis(50), &mut stage).await.is_err(),
            "the first reservation must block on the full journal"
        );
        stage.abort();
        assert!(stage.await.expect_err("aborted stage must cancel").is_cancelled());

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["message-stage-first", "message-stage-second"],
            "abort must retain the in-flight entry and ordered receiver backlog"
        );
        assert!(recovered.iter().all(|(_, journaled)| !journaled));
    }

    #[tokio::test]
    async fn forbidden_thread_lookup_rejects_only_its_head_and_advances_later_ingress() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind forbidden lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept lookup request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read lookup request");
            socket
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .expect("write forbidden lookup response");
        });

        let config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });

        let mut forbidden = ordered_test_message("forbidden-head");
        forbidden.channel_id = "thread-without-view-channel".to_string();
        forbidden.channel_type = Some(11);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: forbidden,
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue forbidden head");
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("after-forbidden"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue later valid message");

        let routed = timeout(Duration::from_secs(1), inbound_rx.recv())
            .await
            .expect("later ingress progresses without reconnecting")
            .expect("inbound channel remains live");
        assert_eq!(routed.message().message_id, "after-forbidden");
        assert!(
            runtime
                .snapshot_recovered_ingress_for_cli()
                .iter()
                .all(|entry| entry.message().message_id != "forbidden-head"),
            "conclusive 403 must remove the durable rejected head"
        );

        drop(ordered_tx);
        timeout(Duration::from_secs(1), stage)
            .await
            .expect("ordered stage exits after its sender closes")
            .expect("ordered stage succeeds");
        server.await.expect("lookup fixture succeeds");
    }

    #[tokio::test]
    async fn stalled_channel_get_body_preserves_fifo_for_recovery() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stalled lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stalled, _) = listener.accept().await.expect("accept stalled lookup");
            let mut request = [0_u8; 4096];
            let _ = stalled.read(&mut request).await.expect("read stalled lookup");
            stalled
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 512\r\nConnection: close\r\n\r\n",
                )
                .await
                .expect("write incomplete lookup headers");

            // The lookup deadline must abandon this incomplete successful
            // response instead of retrying around its durable FIFO head.
            let _ = timeout(Duration::from_secs(7), stalled.read(&mut request)).await;
        });

        let config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (ordered_tx, ordered_rx) = mpsc::channel(2);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let stage = tokio::spawn(async move {
            worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        });

        let mut stalled = ordered_test_message("stalled-head");
        stalled.channel_id = "thread-stalled".to_string();
        stalled.channel_type = Some(11);
        for message in [stalled, ordered_test_message("after-stalled")] {
            ordered_tx
                .send(DiscordOrderedIngress::Message {
                    ack: None,
                    ingress: DiscordUnresolvedIngress {
                        message,
                        client_user_id: Some("bot-self".to_string()),
                    },
                    journaled: false,
                })
                .await
                .expect("queue ordered ingress");
        }
        drop(ordered_tx);

        timeout(Duration::from_secs(7), stage)
            .await
            .expect("incomplete successful lookup stops the ordered stage")
            .expect("ordered stage succeeds");
        assert!(
            matches!(
                inbound_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "the stalled head must not route and later ingress must not advance around it"
        );
        assert_eq!(
            runtime
                .snapshot_recovered_ingress_for_cli()
                .iter()
                .map(|entry| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["stalled-head", "after-stalled"],
            "the incomplete 2xx head and its FIFO backlog must remain recoverable"
        );
        server.await.expect("lookup fixture succeeds");
    }

    #[tokio::test]
    async fn runtime_abort_joins_ordered_child_before_immediate_recovery_snapshot() {
        use tokio::io::AsyncReadExt;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind blocked lookup fixture");
        let api_base_url = format!("http://{}", listener.local_addr().unwrap());
        let (lookup_started_tx, lookup_started_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("lookup request");
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await.expect("read lookup request");
            let _ = lookup_started_tx.send(());
            std::future::pending::<()>().await;
        });
        let gateway_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind authenticated gateway fixture");
        let gateway_url = format!("ws://{}", gateway_listener.local_addr().unwrap());
        let gateway_resume_url = gateway_url.clone();
        let gateway_server = tokio::spawn(async move {
            let mut socket = accept(&gateway_listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(1, &gateway_resume_url)).await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime abort closes the authenticated gateway socket");
        });


        let config = driver_test_config(gateway_url, DiscordBotMessagePolicy::None);
        let runtime =
            test_runtime_from_config_and_api(config, Client::new(), api_base_url);
        let worker = runtime.worker();
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let mut current = ordered_test_message("abort-current");
        current.channel_id = "thread-blocked".to_string();
        current.channel_type = Some(11);
        let mut backlog = current.clone();
        backlog.message_id = "abort-backlog".to_string();
        worker.requeue_recovered_ingress([
            (
                DiscordJournalEntry::new(current, Some("bot-self".to_string())),
                true,
            ),
            (
                DiscordJournalEntry::new(backlog, Some("bot-self".to_string())),
                true,
            ),
        ]);

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(1), lookup_started_rx)
            .await
            .expect("first ordered ingress reaches blocked REST lookup")
            .expect("lookup fixture signals request");

        runtime.abort().await;
        let snapshot = runtime.snapshot_recovered_ingress_for_cli();
        assert_eq!(
            snapshot
                .iter()
                .map(|entry| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            ["abort-current", "abort-backlog"],
            "awaited runtime abort must join the child before recovery is snapshotted"
        );

        gateway_server
            .await
            .expect("authenticated gateway fixture stops after runtime abort");

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn saturated_stop_restart_retains_authenticated_mention_identity() {
        let mut config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.require_mention = true;
        let runtime = test_runtime_from_config(config);
        let worker = runtime.worker();
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        inbound_tx
            .send(test_inbound_message("already-queued"))
            .await
            .expect("fill CLI-owned inbound queue");
        let (journal_tx, _journal_rx) = mpsc::channel(4);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let mut mentioned = ordered_test_message("mentioned-before-stop");
        mentioned.mention_user_ids = vec!["bot-self".to_string()];
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: mentioned,
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: true,
            })
            .await
            .expect("queue journal-owned mentioned ingress");
        let draining_worker = worker.clone();
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let draining = tokio::spawn(async move {
            draining_worker
                .run_ordered_ingress_stage(ordered_rx, sequence_tx)
                .await;
        });

        // Give the resolver one bounded enqueue attempt against the full queue,
        // then interrupt its retry. The requeue originates from
        // `DiscordInboundMessage::journal_entry`.
        tokio::time::sleep(Duration::from_millis(DISCORD_INBOUND_RETRY_MS * 2)).await;
        runtime.stop_tx.send_replace(true);
        drop(ordered_tx);
        timeout(Duration::from_secs(1), draining)
            .await
            .expect("full-queue stop interrupts ordered ingress")
            .expect("ordered ingress joins");

        let recovered = worker.take_recovered_ingress();
        assert_eq!(recovered.len(), 1);
        let (entry, journaled) = recovered.into_iter().next().expect("recovered entry");
        assert!(journaled);
        assert_eq!(entry.client_user_id(), Some("bot-self"));

        runtime.stop_tx.send_replace(false);
        assert_eq!(
            inbound_rx
                .recv()
                .await
                .expect("preexisting CLI ingress remains queued")
                .message()
                .message_id,
            "already-queued"
        );
        runtime
            .queue_recovered_pre_policy_ingress(vec![entry])
            .expect("preserved pre-policy entry requeues");
        let (recovered, journaled) = worker
            .take_recovered_ingress()
            .into_iter()
            .next()
            .expect("restart receives the preserved record");
        assert!(journaled);
        let (replay_tx, replay_rx) = mpsc::channel(1);
        replay_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: recovered.into(),
                journaled,
            })
            .await
            .expect("replay preserved journal entry");
        drop(replay_tx);
        let replay_worker = runtime.worker();
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        let replay = tokio::spawn(async move {
            replay_worker.run_ordered_ingress_stage(replay_rx, sequence_tx).await;
        });
        assert_eq!(
            timeout(Duration::from_secs(1), inbound_rx.recv())
                .await
                .expect("replayed mentioned message is routed")
                .expect("inbound sender remains open")
                .message()
                .message_id,
            "mentioned-before-stop"
        );
        timeout(Duration::from_secs(1), replay)
            .await
            .expect("replay stage stops")
            .expect("replay stage joins");
    }

    #[tokio::test]
    async fn ordered_overflow_is_appended_after_queued_ingress_on_shutdown() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let (ordered_tx, ordered_rx) = mpsc::channel(1);
        ordered_tx
            .send(DiscordOrderedIngress::Message {
                ack: None,
                ingress: DiscordUnresolvedIngress {
                    message: ordered_test_message("message-a"),
                    client_user_id: Some("bot-self".to_string()),
                },
                journaled: false,
            })
            .await
            .expect("queue first ingress");
        worker.retain_ordered_overflow(DiscordOrderedIngress::Message {
            ack: None,
            ingress: DiscordUnresolvedIngress {
                message: ordered_test_message("message-b"),
                client_user_id: Some("bot-self".to_string()),
            },
            journaled: false,
        });

        runtime.stop_tx.send_replace(true);
        let (sequence_tx, _sequence_rx) = mpsc::unbounded_channel();
        worker.run_ordered_ingress_stage(ordered_rx, sequence_tx).await;
        worker.requeue_ordered_overflow(worker.take_ordered_overflow());

        let recovered = worker.take_recovered_ingress();
        assert_eq!(
            recovered
                .iter()
                .map(|(entry, _)| entry.message().message_id.as_str())
                .collect::<Vec<_>>(),
            vec!["message-a", "message-b"],
            "a full-stage overflow must retain every earlier queued ingress first"
        );
        assert!(
            recovered.iter().all(|(_, journaled)| !journaled),
            "neither item may claim durable ownership before the ordered handoff"
        );
    }

    #[test]
    fn ipv6_gateway_host_is_normalized_and_bracketed_exactly_once_for_connect() {
        let url = url::Url::parse("wss://[::1]:443").unwrap();
        let host = discord_url_host(&url).expect("IPv6 host");
        assert_eq!(host, "::1");
        assert_eq!(discord_connect_authority(&host, 443), "[::1]:443");
    }

    #[test]
    fn gateway_url_preserves_semantic_queries_and_rejects_compression() {
        let normalized = discord_gateway_v10_url(
            "wss://gateway.example/private?tenant=acme&routing=blue&v=9&encoding=etf",
        )
        .expect("private semantic gateway URL is supported");
        let parsed = url::Url::parse(&normalized).unwrap();
        assert_eq!(
            parsed.query_pairs().collect::<Vec<_>>(),
            vec![
                ("tenant".into(), "acme".into()),
                ("routing".into(), "blue".into()),
                ("v".into(), "10".into()),
                ("encoding".into(), "json".into()),
            ]
        );
        assert!(
            discord_gateway_v10_url("wss://gateway.discord.gg/?compress=zlib-stream").is_err(),
            "the text-frame driver must fail closed instead of advertising compression"
        );
        assert!(discord_gateway_v10_url("wss://user:secret@gateway.discord.gg").is_err());
        assert!(discord_gateway_v10_url("wss://gateway.discord.gg/#fragment").is_err());
    }

    #[tokio::test]
    async fn compressed_gateway_config_fails_before_opening_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = driver_test_config(
            format!(
                "ws://{}/?tenant=acme&compress=zlib-stream",
                listener.local_addr().unwrap()
            ),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config(config);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let error = runtime
            .start()
            .await
            .expect_err("unsupported compression must fail before worker spawn");
        assert!(error.to_string().contains("compression is unsupported"));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "rejected compression must not connect and loop on binary frames"
        );
    }

    #[tokio::test]
    async fn fragmented_gateway_config_fails_before_opening_a_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = driver_test_config(
            format!("ws://{}/?tenant=acme#not-for-websocket", listener.local_addr().unwrap()),
            DiscordBotMessagePolicy::None,
        );
        let runtime = test_runtime_from_config(config);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let error = runtime
            .start()
            .await
            .expect_err("fragmented gateway config must fail before worker spawn");
        assert!(error.to_string().contains("must not include a fragment"));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(
            timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err(),
            "a rejected fragment must not cause repeated websocket handshakes"
        );
    }

    #[tokio::test]
    async fn runtime_start_requires_live_inbound_and_durable_journal_receivers() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );

        let error = runtime
            .start()
            .await
            .expect_err("a memory-only runtime must not start");
        assert!(error
            .to_string()
            .contains("durable ingress journal receiver is unavailable"));
        assert!(runtime.task.lock().await.is_none());
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));

        let (journal_tx, _journal_rx) = mpsc::channel(1);
        *runtime.journal_tx.write().await = Some(journal_tx);
        let error = runtime
            .start()
            .await
            .expect_err("a journal without an inbound receiver must not start");
        assert!(error
            .to_string()
            .contains("Gateway inbound receiver is unavailable"));
        assert!(runtime.task.lock().await.is_none());
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_runtime_aborts_its_socket_worker() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            let _ = accepted_tx.send(());
            let _ = socket.next().await;
        });

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(1), accepted_rx)
            .await
            .expect("worker opens its socket")
            .expect("server signals accepted socket");
        let abort = runtime
            .task
            .lock()
            .await
            .as_ref()
            .expect("runtime owns worker task")
            .abort_handle();

        drop(runtime);
        timeout(Duration::from_secs(1), async {
            while !abort.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping runtime aborts the socket worker");
        timeout(Duration::from_secs(1), server)
            .await
            .expect("server observes worker socket close")
            .expect("server joins");
    }

    #[tokio::test]
    async fn runtime_start_rejects_invalid_gateway_override_before_spawning_worker() {
        let runtime = test_runtime(
            "https://gateway.discord.test/not-a-websocket".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let (journal_tx, _journal_rx) = mpsc::channel(1);
        runtime
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let error = runtime.start().await.expect_err("invalid URL must fail start");
        assert!(matches!(error, GatewayError::ConnectionFailed(_)));
        assert!(!runtime.worker_alive.load(Ordering::SeqCst));
        assert!(runtime.task.lock().await.is_none());
    }

    #[test]
    fn stale_resume_url_is_ignored_without_resumable_session_state() {
        let runtime = test_runtime(
            "ws://primary.example.test".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        session.resume_gateway_url = Some("ws://stale.example.test".to_string());
        session.session_id = Some("session-1".to_string());

        let primary = url::Url::parse(&worker.connection_url(&session).unwrap()).unwrap();
        assert_eq!(primary.host_str(), Some("primary.example.test"));

        session.commit_sequence(Some(41));
        let resumable = url::Url::parse(&worker.connection_url(&session).unwrap()).unwrap();
        assert_eq!(resumable.host_str(), Some("stale.example.test"));
    }

    #[tokio::test]
    async fn http_connect_proxy_tunnels_gateway_bytes_with_basic_auth() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let mut config = driver_test_config(
            "ws://gateway.example.test:8080".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.proxy.http_proxy =
            Some(format!("http://agent:secret@{proxy_address}"));
        let runtime = test_runtime_from_config(config);

        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                assert!(request.len() <= 16 * 1024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(
                "CONNECT gateway.example.test:8080 HTTP/1.1\r\n"
            ));
            let credentials = base64::engine::general_purpose::STANDARD
                .encode("agent:secret");
            assert!(request.contains(&format!(
                "\r\nProxy-Authorization: Basic {credentials}\r\n"
            )));
            stream
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            let mut ping = [0_u8; 4];
            stream.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let url = url::Url::parse("ws://gateway.example.test:8080").unwrap();
        let mut transport = runtime
            .worker()
            .open_transport(&url)
            .await
            .expect("HTTP CONNECT tunnel");
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn socks5h_proxy_tunnels_gateway_bytes_with_username_and_password() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let mut config = driver_test_config(
            "ws://gateway.example.test:443".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.proxy.socks_proxy =
            Some(format!("socks5h://agent:p%40ss@{proxy_address}"));
        let runtime = test_runtime_from_config(config);

        let proxy = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut greeting = [0_u8; 2];
            stream.read_exact(&mut greeting).await.unwrap();
            assert_eq!(greeting[0], 5);
            let mut methods = vec![0_u8; usize::from(greeting[1])];
            stream.read_exact(&mut methods).await.unwrap();
            assert!(methods.contains(&2));
            stream.write_all(&[5, 2]).await.unwrap();

            let mut auth = [0_u8; 2];
            stream.read_exact(&mut auth).await.unwrap();
            assert_eq!(auth[0], 1);
            let mut username = vec![0_u8; usize::from(auth[1])];
            stream.read_exact(&mut username).await.unwrap();
            let mut password_len = [0_u8; 1];
            stream.read_exact(&mut password_len).await.unwrap();
            let mut password = vec![0_u8; usize::from(password_len[0])];
            stream.read_exact(&mut password).await.unwrap();
            assert_eq!(&username, b"agent");
            assert_eq!(&password, b"p@ss");
            stream.write_all(&[1, 0]).await.unwrap();

            let mut request = [0_u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(request, [5, 1, 0, 3]);
            let mut domain_len = [0_u8; 1];
            stream.read_exact(&mut domain_len).await.unwrap();
            let mut domain = vec![0_u8; usize::from(domain_len[0])];
            stream.read_exact(&mut domain).await.unwrap();
            let mut port = [0_u8; 2];
            stream.read_exact(&mut port).await.unwrap();
            assert_eq!(&domain, b"gateway.example.test");
            assert_eq!(u16::from_be_bytes(port), 443);
            stream
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();

            let mut ping = [0_u8; 4];
            stream.read_exact(&mut ping).await.unwrap();
            assert_eq!(&ping, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });

        let url = url::Url::parse("ws://gateway.example.test:443").unwrap();
        let mut transport = runtime
            .worker()
            .open_transport(&url)
            .await
            .expect("SOCKS5h tunnel");
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[tokio::test]
    async fn locally_resolved_socks5_retries_each_target_under_one_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = listener.local_addr().unwrap();
        let first = "127.0.0.1:41001".parse::<std::net::SocketAddr>().unwrap();
        let second = "127.0.0.2:41002".parse::<std::net::SocketAddr>().unwrap();
        let proxy = tokio::spawn(async move {
            for (expected, reply_code) in [(first, 5_u8), (second, 0_u8)] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut greeting = [0_u8; 2];
                stream.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1]);
                let mut method = [0_u8; 1];
                stream.read_exact(&mut method).await.unwrap();
                assert_eq!(method, [0]);
                stream.write_all(&[5, 0]).await.unwrap();

                let mut request = [0_u8; 10];
                stream.read_exact(&mut request).await.unwrap();
                assert_eq!(request[..4], [5, 1, 0, 1]);
                assert_eq!(
                    std::net::SocketAddr::from((
                        std::net::Ipv4Addr::new(request[4], request[5], request[6], request[7]),
                        u16::from_be_bytes([request[8], request[9]]),
                    )),
                    expected
                );
                if reply_code != 0 {
                    // Deliberately blackhole the first CONNECT response. The
                    // client must reserve time for the second DNS address
                    // instead of letting this attempt consume the outer limit.
                    let mut eof = [0_u8; 1];
                    let read = timeout(Duration::from_millis(750), stream.read(&mut eof))
                        .await
                        .expect("first SOCKS attempt must be cancelled at its slice");
                    assert_eq!(read.unwrap(), 0);
                    continue;
                }
                stream
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let mut ping = [0_u8; 4];
                stream.read_exact(&mut ping).await.unwrap();
                assert_eq!(&ping, b"ping");
                stream.write_all(b"pong").await.unwrap();
            }
        });

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        let mut transport = DiscordGatewayWorker::connect_socks5_targets(
            &proxy_address.ip().to_string(),
            proxy_address.port(),
            vec![first, second],
            "",
            "",
            deadline,
        )
        .await
        .expect("the second local DNS target must be attempted");
        assert!(
            tokio::time::Instant::now() < deadline,
            "the successful fallback must retain the shared outer deadline"
        );
        transport.write_all(b"ping").await.unwrap();
        let mut pong = [0_u8; 4];
        transport.read_exact(&mut pong).await.unwrap();
        assert_eq!(&pong, b"pong");
        proxy.await.unwrap();
    }

    #[test]
    fn private_gateway_realms_scope_tenant_credentials_without_leaking_transport_knobs() {
        let mut tenant_a = driver_test_config(
            "wss://alice:secret@gateway.example/private?tenant=A&v=9&encoding=etf"
                .to_string(),
            DiscordBotMessagePolicy::None,
        );
        let mut tenant_b = tenant_a.clone();
        tenant_b.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?encoding=json&tenant=B&v=10"
                .to_string(),
        );
        let mut other_credentials = tenant_a.clone();
        other_credentials.gateway_url = Some(
            "wss://bob:secret@gateway.example/private?tenant=A&v=10&encoding=json"
                .to_string(),
        );
        let mut transport_equivalent = tenant_a.clone();
        transport_equivalent.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?encoding=json&v=10&tenant=A"
                .to_string(),
        );
        let mut rejected_compression = tenant_a.clone();
        rejected_compression.gateway_url = Some(
            "wss://alice:secret@gateway.example/private?tenant=A&compress=zlib-stream"
                .to_string(),
        );

        let tenant_a_scope = DiscordAdapter::state_scope_for_config(&tenant_a);
        assert_ne!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&tenant_b),
            "semantic tenant query must isolate persisted state"
        );
        assert_ne!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&other_credentials),
            "private userinfo must isolate persisted state"
        );
        assert_eq!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&transport_equivalent),
            "gateway v/encoding and query order are transport-only"
        );
        assert_eq!(
            tenant_a_scope,
            DiscordAdapter::state_scope_for_config(&rejected_compression),
            "rejected compress remains a transport-only knob for realm identity"
        );
    }

    #[test]
    fn identify_pacer_spaces_fresh_identifies_for_one_shard() {
        let mut pacer = DiscordIdentifyPacer::default();
        let first = pacer.next_deadline();
        assert!(pacer.reserve_if_ready());
        let second = pacer.next_deadline();
        assert!(
            second.duration_since(first)
                >= Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS)
        );
    }

    #[test]
    fn identify_pacer_rearms_exhausted_daily_budget() {
        let mut pacer = DiscordIdentifyPacer::default();
        pacer.configure(DiscordGatewaySessionStartLimit {
            total: 2,
            remaining: 0,
            reset_after_ms: 60_000,
        });
        let reset_at = pacer.reset_at.expect("configured reset");
        let blocked = pacer.next_deadline();
        assert_eq!(blocked, reset_at);
        assert!(!pacer.reserve_if_ready());

        pacer.reset_at = Some(tokio::time::Instant::now() - Duration::from_millis(1));
        let refreshed = pacer.next_deadline();
        assert_eq!(pacer.remaining, Some(2));
        assert!(
            pacer
                .reset_at
                .expect("rearmed reset")
                .duration_since(tokio::time::Instant::now())
                >= Duration::from_secs(DISCORD_IDENTIFY_BUDGET_WINDOW_SECS)
                    - Duration::from_millis(1)
        );
        assert!(refreshed <= tokio::time::Instant::now());
        assert!(pacer.reserve_if_ready());
        assert_eq!(pacer.remaining, Some(1));
    }

    #[tokio::test(start_paused = true)]
    async fn requested_heartbeat_preserves_scheduled_window() {
        let mut session = GatewaySession::new();
        session.heartbeat_interval_ms = Some(1_000);
        let original = DiscordGatewayWorker::next_heartbeat_deadline(&session)
            .expect("scheduled heartbeat");

        tokio::time::advance(Duration::from_millis(999)).await;
        session.heartbeat_sent();
        let unchanged = DiscordGatewayWorker::heartbeat_deadline_after_payload(
            Some(original),
            &session,
            opcodes::HEARTBEAT,
        )
        .expect("periodic heartbeat schedule remains active");

        assert_eq!(unchanged, original);
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(tokio::time::Instant::now(), unchanged);
        assert_eq!(session.heartbeat_tick(), HeartbeatTick::Reconnect);
    }

    #[tokio::test(start_paused = true)]
    async fn initial_heartbeat_deadline_uses_injected_jitter() {
        let mut session = GatewaySession::new();
        session.heartbeat_interval_ms = Some(1_000);

        let now = tokio::time::Instant::now();

        let quarter = DiscordGatewayWorker::initial_heartbeat_deadline(&session, 0.25)
            .expect("jittered initial heartbeat");
        assert_eq!(quarter.duration_since(now), Duration::from_millis(250));

        let clamped = DiscordGatewayWorker::initial_heartbeat_deadline(&session, 2.0)
            .expect("clamped jittered initial heartbeat");
        assert_eq!(clamped.duration_since(now), Duration::from_secs(1));
    }

    #[test]
    fn invalid_session_reconnect_paces_both_resumable_modes() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime
            .worker()
            .with_invalid_session_delay(|| Duration::from_millis(1_234));

        assert_eq!(
            worker.invalid_session_reconnect_outcome(true),
            DiscordGatewayConnectionOutcome::ReconnectAfter(Duration::from_millis(1_234))
        );
        assert_eq!(
            worker.invalid_session_reconnect_outcome(false),
            DiscordGatewayConnectionOutcome::ReconnectAfter(Duration::from_millis(1_234))
        );
    }

    #[tokio::test]
    async fn exhausted_identify_wait_observes_an_already_stopped_runtime() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        runtime
            .configure_identify_session_start_limit(DiscordGatewaySessionStartLimit {
                total: 0,
                remaining: 0,
                reset_after_ms: 60_000,
            })
            .await;
        runtime.stop_tx.send_replace(true);

        let mut stop = runtime.stop_tx.subscribe();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let result = timeout(
            Duration::from_millis(100),
            runtime
                .worker()
                .await_identify_eligibility(&mut stop, &mut ingress_done),
        )
        .await
        .expect("already-stopped paced wait returns promptly");
        assert!(matches!(
            result,
            DiscordAuthenticationPreparation::Stopped
        ));
    }

    async fn accept(listener: &TcpListener) -> MockDiscordWebSocket {
        let (stream, _) = listener.accept().await.expect("mock gateway accepts TCP");
        accept_async(stream).await.expect("mock gateway accepts WebSocket")
    }

    async fn send_payload(socket: &mut MockDiscordWebSocket, payload: GatewayPayload) {
        socket
            .send(Message::Text(
                serde_json::to_string(&payload)
                    .expect("mock payload serializes")
                    .into(),
            ))
            .await
            .expect("mock gateway sends payload");
    }

    async fn receive_opcode(
        socket: &mut MockDiscordWebSocket,
        opcode: u8,
    ) -> GatewayPayload {
        receive_opcode_with_timeout(socket, opcode, Duration::from_secs(1)).await
    }

    async fn receive_opcode_with_timeout(
        socket: &mut MockDiscordWebSocket,
        opcode: u8,
        wait_for_payload: Duration,
    ) -> GatewayPayload {
        for _ in 0..4 {
            let frame = timeout(wait_for_payload, socket.next())
                .await
                .expect("client emits a gateway payload")
                .expect("client socket remains open")
                .expect("client gateway frame succeeds");
            if let Message::Text(text) = frame {
                let payload = serde_json::from_str::<GatewayPayload>(text.as_str())
                    .expect("client payload is JSON");
                if payload.op == opcode {
                    return payload;
                }
            }
        }
        panic!("client did not emit opcode {opcode}");
    }

    fn hello(interval_ms: u64) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": interval_ms })),
            s: None,
            t: None,
        }
    }

    fn ready(sequence: u64, resume_gateway_url: &str) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "session_id": "session-1",
                "resume_gateway_url": resume_gateway_url,
                "user": { "id": "self-id", "username": "Hermes" }
            })),
            s: Some(sequence),
            t: Some("READY".to_string()),
        }
    }

    #[tokio::test]
    async fn durability_ack_arriving_during_connection_preparation_forces_resume() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        let actions = session.handle_gateway_event(&ready(42, "ws://resume.example"));
        assert!(matches!(
            actions.as_slice(),
            [GatewayAction::Dispatch(name, _)] if name == "READY"
        ));
        assert!(!session.can_resume());

        let ack = DiscordSequenceAck {
            generation: session.generation(),
            sequence: 42,
        };
        let mut pending_initial_ack = None;
        DiscordGatewayWorker::track_initial_durability(
            &mut pending_initial_ack,
            true,
            Some(ack),
            DiscordInboundDispatchOutcome::Queued,
        );
        let (ack_tx, mut ack_rx) = mpsc::unbounded_channel();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let (_stop_tx, mut stop) = watch::channel(false);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            ack_tx.send(ack).unwrap();
        });

        let preparation = timeout(
            Duration::from_secs(1),
            worker.await_authentication_preparation(
                &mut session,
                &mut ack_rx,
                &mut pending_initial_ack,
                &mut ingress_done,
                &mut stop,
            ),
        )
        .await
        .expect("durability ACK must interrupt connection preparation")
        .unwrap();
        assert_eq!(preparation, DiscordAuthenticationPreparation::Ready);
        assert!(pending_initial_ack.is_none());
        assert!(session.can_resume());
        assert_eq!(
            session.handle_gateway_event(&hello(41_250)),
            vec![GatewayAction::SendResume]
        );
    }

    #[tokio::test]
    async fn ready_backpressure_does_not_wait_for_an_unenqueued_ack() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        runtime.stop_tx.send_replace(false);
        let worker = runtime.worker();
        let mut session = GatewaySession::new();
        let _ = session.handle_gateway_event(&ready(42, "ws://resume.example"));
        let ack = DiscordSequenceAck {
            generation: session.generation(),
            sequence: 42,
        };
        let mut pending_initial_ack = Some(ack);
        DiscordGatewayWorker::track_initial_durability(
            &mut pending_initial_ack,
            true,
            Some(ack),
            DiscordInboundDispatchOutcome::Backpressure,
        );
        assert!(pending_initial_ack.is_none());

        let (_ack_tx, mut ack_rx) = mpsc::unbounded_channel();
        let (_ingress_done_tx, mut ingress_done) = watch::channel(false);
        let (_stop_tx, mut stop) = watch::channel(false);
        let preparation = timeout(
            Duration::from_secs(1),
            worker.await_authentication_preparation(
                &mut session,
                &mut ack_rx,
                &mut pending_initial_ack,
                &mut ingress_done,
                &mut stop,
            ),
        )
        .await
        .expect("backpressured READY must reconnect for fresh IDENTIFY")
        .unwrap();
        assert_eq!(preparation, DiscordAuthenticationPreparation::Ready);
    }

    fn message_create(
        id: &str,
        author_id: &str,
        is_bot: bool,
        guild_id: Option<&str>,
    ) -> GatewayPayload {
        let mut data = serde_json::json!({
            "id": id,
            "channel_id": "channel-1",
            "channel_type": if guild_id.is_some() { 0 } else { 1 },
            "content": "hello",
            "author": { "id": author_id, "username": "sender", "bot": is_bot }
        });
        if let Some(guild_id) = guild_id {
            data["guild_id"] = serde_json::Value::String(guild_id.to_string());
        }
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(data),
            s: Some(3),
            t: Some("MESSAGE_CREATE".to_string()),
        }
    }

    fn thread_create(thread_id: &str, parent_channel_id: &str) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "id": thread_id,
                "guild_id": "guild-1",
                "parent_id": parent_channel_id,
                "type": 11,
            })),
            s: Some(3),
            t: Some("THREAD_CREATE".to_string()),
        }
    }

    fn message_create_with_sequence(
        id: &str,
        author_id: &str,
        is_bot: bool,
        guild_id: Option<&str>,
        sequence: u64,
    ) -> GatewayPayload {
        let mut payload = message_create(id, author_id, is_bot, guild_id);
        payload.s = Some(sequence);
        payload
    }

    fn test_inbound_message(id: &str) -> DiscordInboundMessage {
        let data = message_create(id, "filler-author", false, None)
            .d
            .expect("test message has payload data");
        let message =
            DiscordAdapter::parse_message_create(&data).expect("test message parses as inbound");
        DiscordInboundMessage {
            sender_is_bot: message.is_bot,
            client_user_id: Some("bot-self".to_string()),
            message,
        }
    }

    #[test]
    fn thread_context_cache_enriches_standard_messages_and_fails_closed_on_a_known_miss() {
        let runtime = test_runtime(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        let worker = runtime.worker();
        worker.observe_thread_context(
            "GUILD_CREATE",
            &serde_json::json!({
                "id": "guild-a",
                "threads": [{ "id": "thread-a", "parent_id": "parent-a", "type": 11 }]
            }),
        );
        worker.observe_thread_context(
            "THREAD_LIST_SYNC",
            &serde_json::json!({
                "guild_id": "guild-a",
                "threads": [{ "id": "thread-b", "parent_id": "parent-b", "type": 11 }]
            }),
        );

        let mut standard = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-a",
            "channel_id": "thread-a",
            "guild_id": "guild-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("standard message parses");
        assert!(standard.thread_id.is_none());
        assert_eq!(
            worker.resolve_thread_parent(&mut standard),
            DiscordThreadResolution::Resolved
        );
        assert_eq!(standard.parent_channel_id.as_deref(), Some("parent-a"));
        assert_eq!(standard.thread_id.as_deref(), Some("thread-a"));

        let mut cross_guild = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-cross-guild",
            "channel_id": "thread-a",
            "guild_id": "guild-b",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("cross-guild message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut cross_guild),
            DiscordThreadResolution::Rejected
        );

        let mut missing_guild = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-missing-guild",
            "channel_id": "thread-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("missing-guild message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut missing_guild),
            DiscordThreadResolution::Rejected
        );

        assert!(
            !missing_guild.is_dm,
            "missing guild/channel metadata must not default into the DM policy path"
        );

        let mut explicit_dm = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "explicit-dm",
            "channel_id": "dm-channel",
            "channel_type": 1,
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("explicit DM parses");
        assert!(explicit_dm.is_dm);
        assert_eq!(
            worker.resolve_thread_parent(&mut explicit_dm),
            DiscordThreadResolution::Resolved
        );
        let mut unresolvable = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "message-unknown",
            "channel_id": "unknown-thread",
            "channel_type": 11,
            "guild_id": "guild-a",
            "content": "hello",
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("known thread message parses");
        assert_eq!(
            worker.resolve_thread_parent(&mut unresolvable),
            DiscordThreadResolution::NeedsLookup
        );
    }

    #[tokio::test]
    async fn routed_thread_ingress_does_not_count_as_hermes_participation() {
        let mut config = driver_test_config(
            "ws://127.0.0.1:9".to_string(),
            DiscordBotMessagePolicy::None,
        );
        config.require_mention = true;
        let runtime = test_runtime_from_config(config);
        let worker = runtime.worker();
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let mut mentioned = DiscordAdapter::parse_message_create(&serde_json::json!({
            "id": "mentioned",
            "channel_id": "thread-unanswered",
            "guild_id": "guild-a",
            "content": "<@self-id> please answer",
            "mentions": [{ "id": "self-id", "username": "hermes", "bot": true }],
            "author": { "id": "human-a", "username": "sender", "bot": false }
        }))
        .expect("mentioned thread message parses");
        mentioned.thread_id = Some("thread-unanswered".to_string());
        mentioned.parent_channel_id = Some("parent-a".to_string());
        let inbound = worker
            .prepare_routable_inbound(mentioned.clone(), Some("self-id"))
            .expect("mention is eligible for routing");
        assert!(matches!(
            worker.try_enqueue_inbound(inbound).await,
            DiscordInboundQueueOutcome::Queued
        ));
        assert_eq!(
            rx.recv().await.expect("routed ingress").message().message_id,
            "mentioned"
        );
        assert!(!runtime
            .thread_participation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains("thread-unanswered"));

        let mut follow_up = mentioned;
        follow_up.message_id = "unmentioned-follow-up".to_string();
        follow_up.mention_user_ids.clear();
        assert!(
            worker
                .prepare_routable_inbound(follow_up, Some("self-id"))
                .is_none(),
            "routing ingress alone must not unlock unmentioned thread follow-ups"
        );
    }

    #[tokio::test]
    async fn mock_ws_identify_includes_direct_messages_and_routes_once() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(4);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            let identify = receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            assert_ne!(
                identify.d.unwrap()["intents"].as_u64().unwrap() & (1 << 12),
                0,
                "IDENTIFY must request DIRECT_MESSAGES"
            );
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(&mut socket, message_create("message-1", "human-1", false, None)).await;
            timeout(Duration::from_secs(1), async {
                tokio::select! {
                    _ = listener.accept() => panic!("runtime opened a duplicate Discord Gateway socket"),
                    _ = socket.next() => {}
                }
            })
            .await
            .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("routable message arrives")
            .expect("inbound sender remains live");
        assert!(!inbound.sender_is_bot());
        assert!(inbound.message().is_dm);
        assert_eq!(inbound.message().guild_id, None);
        assert_eq!(inbound.to_gateway_message().unwrap().thread_id, None);
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());

        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_filters_ready_self_authored_dispatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::All);
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(&mut socket, message_create("self-message", "self-id", true, Some("guild-1"))).await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        assert!(timeout(Duration::from_millis(100), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_preserves_verified_bot_provenance() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::All);
        let (tx, mut rx) = mpsc::channel(2);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            // Standard MESSAGE_CREATE carries neither parent_id nor
            // channel_type. THREAD_CREATE supplies the authoritative cache.
            send_payload(&mut socket, thread_create("channel-1", "parent-1")).await;
            send_payload(
                &mut socket,
                message_create("bot-message", "other-bot", true, Some("guild-1")),
            )
            .await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        let inbound = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("bot policy admits the dispatch")
            .expect("inbound sender remains live");
        assert!(inbound.sender_is_bot());
        assert_eq!(inbound.message().guild_id.as_deref(), Some("guild-1"));
        assert_eq!(
            inbound.message().parent_channel_id.as_deref(),
            Some("parent-1")
        );
        assert_eq!(inbound.message().thread_id.as_deref(), Some("channel-1"));
        let routed = inbound.to_gateway_message().unwrap();
        assert_eq!(routed.user_id, "other-bot");
        assert_eq!(routed.chat_id, "parent-1");
        assert_eq!(routed.thread_id.as_deref(), Some("channel-1"));
        assert!(
            !runtime
                .thread_participation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains("channel-1"),
            "accepted ingress does not prove Hermes replied in the thread"
        );
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_resumes_then_resets_after_invalid_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(7, &url)).await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::RECONNECT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            let resume = receive_opcode(&mut second, opcodes::RESUME).await;
            assert_eq!(resume.d.unwrap()["session_id"], "session-1");
            send_payload(
                &mut second,
                GatewayPayload {
                    op: opcodes::INVALID_SESSION,
                    d: Some(serde_json::Value::Bool(false)),
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut third = accept(&listener).await;
            send_payload(&mut third, hello(60_000)).await;
            let identify = receive_opcode_with_timeout(
                &mut third,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            assert!(identify.d.is_some());
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            loop {
                if server.is_finished() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mock gateway observes resume then identify");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn reconnect_identify_spacing_starts_at_the_observed_first_write() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            // Connection preparation must not consume the IDENTIFY bucket.
            tokio::time::sleep(Duration::from_millis(100)).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            let first_identify_at = tokio::time::Instant::now();
            drop(first);

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            assert!(
                tokio::time::Instant::now().duration_since(first_identify_at)
                    >= Duration::from_millis(DISCORD_IDENTIFY_MIN_INTERVAL_MS - 25),
                "the second server-observed IDENTIFY must honor the prior write timestamp"
            );
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, server)
            .await
            .expect("server observes paced second IDENTIFY")
            .expect("mock gateway task succeeds");
        runtime.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_falls_back_to_primary_when_resume_endpoint_is_unreachable() {
        let primary = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let primary_url = format!("ws://{}", primary.local_addr().unwrap());
        let unreachable = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let unreachable_url = format!("ws://{}", unreachable.local_addr().unwrap());
        drop(unreachable);
        let runtime = test_runtime(primary_url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&primary).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(7, &unreachable_url)).await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::RECONNECT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = timeout(Duration::from_secs(3), accept(&primary))
                .await
                .expect("primary Gateway receives fallback connection");
            send_payload(&mut second, hello(60_000)).await;
            let resume = receive_opcode(&mut second, opcodes::RESUME).await;
            assert_eq!(resume.d.unwrap()["session_id"], "session-1");
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            while !server.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resume falls back through the primary Gateway");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_saturated_inbound_queue_preserves_fifo_without_stalling_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(test_inbound_message("already-queued"))
            .expect("fixture fills bounded inbound queue");
        let _journal = attach_test_durable_inbound(&runtime, tx).await;
        let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, &url)).await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "retained-first",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "retained-second",
                    "human-1",
                    false,
                    Some("guild-1"),
                    4,
                ),
            )
            .await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            receive_opcode(&mut socket, opcodes::HEARTBEAT).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT_ACK,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            let _ = heartbeat_tx.send(());
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), heartbeat_rx)
            .await
            .expect("socket answers heartbeat while ingress is backpressured")
            .expect("mock gateway confirms heartbeat");
        let fixture = rx.recv().await.expect("fixture remains first");
        assert_eq!(fixture.message().message_id, "already-queued");
        let first = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("first retained dispatch reaches the drained queue")
            .expect("inbound receiver remains live");
        assert_eq!(first.message().message_id, "retained-first");
        let second = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("second retained dispatch follows the first")
            .expect("inbound receiver remains live");
        assert_eq!(second.message().message_id, "retained-second");
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_repeated_heartbeat_requests_cannot_extend_an_unacked_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(100)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, &url)).await;

            // Discord can request an immediate heartbeat repeatedly. Keep
            // asking without ACKing: a healthy client must reconnect at its
            // original pending-ACK/periodic deadline, not let OP 1 move it.
            for _ in 0..40 {
                let request = GatewayPayload {
                    op: opcodes::HEARTBEAT,
                    d: None,
                    s: None,
                    t: None,
                };
                let encoded = Message::Text(
                    serde_json::to_string(&request)
                        .expect("heartbeat request serializes")
                        .into(),
                );
                if first.send(encoded).await.is_err() {
                    break;
                }
                let response = timeout(Duration::from_millis(100), first.next()).await;
                let Ok(Some(Ok(Message::Text(text)))) = response else {
                    break;
                };
                let payload =
                    serde_json::from_str::<GatewayPayload>(text.as_str()).expect("heartbeat JSON");
                assert_eq!(payload.op, opcodes::HEARTBEAT);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            let mut second = timeout(Duration::from_secs(1), accept(&listener))
                .await
                .expect("unacknowledged heartbeat reconnects despite repeated OP 1");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode(&mut second, opcodes::RESUME).await;
        });

        runtime.start().await.expect("runtime starts");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("heartbeat test server completes")
            .expect("heartbeat test server joins");
        runtime.stop().await.expect("runtime stops");
        journal.abort();
    }

    #[tokio::test]
    async fn mock_ws_retained_dispatch_survives_nonresumable_invalid_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(test_inbound_message("already-queued"))
            .expect("fixture fills bounded inbound queue");
        let _journal = attach_test_durable_inbound(&runtime, tx).await;
        let (identified_tx, identified_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(2, &url)).await;
            send_payload(
                &mut first,
                message_create_with_sequence(
                    "retained-after-reset",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            send_payload(
                &mut first,
                GatewayPayload {
                    op: opcodes::INVALID_SESSION,
                    d: Some(serde_json::Value::Bool(false)),
                    s: None,
                    t: None,
                },
            )
            .await;

            let mut second = accept(&listener).await;
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            let _ = identified_tx.send(());
            timeout(Duration::from_secs(1), second.next())
                .await
                .expect("runtime stops mock socket")
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, identified_rx)
            .await
            .expect("nonresumable reset reconnects with IDENTIFY")
            .expect("mock gateway observes IDENTIFY");
        let first = rx.recv().await.expect("fixture remains queued");
        assert_eq!(first.message().message_id, "already-queued");
        let retained = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("local pending dispatch survives invalid session reset")
            .expect("inbound receiver remains live");
        assert_eq!(retained.message().message_id, "retained-after-reset");
        assert!(timeout(Duration::from_millis(75), rx.recv()).await.is_err());
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_ready_resume_endpoint_falls_back_to_configured_gateway_with_identify() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url.clone(), DiscordBotMessagePolicy::None);
        let (inbound_tx, mut inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(60_000)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, "http://unsupported-resume.example")).await;
            timeout(Duration::from_secs(1), first.next())
                .await
                .expect("invalid READY closes the first socket");

            let mut second = timeout(IDENTIFY_PACING_TEST_TIMEOUT, accept(&listener))
                .await
                .expect("configured gateway receives fallback connection");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode_with_timeout(
                &mut second,
                opcodes::IDENTIFY,
                IDENTIFY_PACING_TEST_TIMEOUT,
            )
            .await;
            send_payload(&mut second, ready(2, &url)).await;
            send_payload(
                &mut second,
                message_create_with_sequence(
                    "after-malformed-ready",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
        });

        runtime.start().await.unwrap();
        let routed = timeout(IDENTIFY_PACING_TEST_TIMEOUT, inbound_rx.recv())
            .await
            .expect("fresh identify reconnect accepts later ingress")
            .expect("inbound receiver remains live");
        assert_eq!(routed.message().message_id, "after-malformed-ready");
        runtime.stop().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_closed_inbound_receiver_stops_without_reconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (tx, rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, tx).await;

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            send_payload(
                &mut socket,
                message_create_with_sequence(
                    "closed-receiver",
                    "human-1",
                    false,
                    Some("guild-1"),
                    3,
                ),
            )
            .await;
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("unhealthy worker closes its socket");
            assert!(
                timeout(Duration::from_millis(250), listener.accept())
                    .await
                    .is_err(),
                "closed inbound receiver must stop the worker rather than reconnect"
            );
        });

        runtime.start().await.unwrap();
        drop(rx);
        server.await.unwrap();
        runtime.stop().await;
    }

    #[tokio::test]
    async fn mock_ws_reconnects_at_next_interval_after_unacknowledged_heartbeat() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let resume_url = url.clone();
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello(100)).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(1, &resume_url)).await;
            receive_opcode(&mut first, opcodes::HEARTBEAT).await;

            let mut second = timeout(Duration::from_secs(2), accept(&listener))
                .await
                .expect("next fixed interval reconnects the unacknowledged socket");
            send_payload(&mut second, hello(60_000)).await;
            receive_opcode(&mut second, opcodes::RESUME).await;
        });

        runtime.start().await.unwrap();
        timeout(IDENTIFY_PACING_TEST_TIMEOUT, async {
            while !server.is_finished() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("unacknowledged heartbeat reconnects at the next interval");
        runtime.stop().await;
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_stop_cancels_live_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (identified_tx, identified_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(60_000)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            let _ = identified_tx.send(());
            timeout(Duration::from_secs(1), socket.next())
                .await
                .expect("stop closes the live socket")
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), identified_rx)
            .await
            .expect("runtime identifies before shutdown")
            .expect("mock gateway observes IDENTIFY");
        timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("stop cancels the runtime");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mock_ws_stop_during_heartbeat_loop_cancels_immediately() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://{}", listener.local_addr().unwrap());
        let runtime = test_runtime(url, DiscordBotMessagePolicy::None);
        let (inbound_tx, _inbound_rx) = mpsc::channel(1);
        let _journal = attach_test_durable_inbound(&runtime, inbound_tx).await;
        let (stopped_tx, stopped_rx) = tokio::sync::oneshot::channel::<()>();
        let (heartbeat_tx, heartbeat_rx) = tokio::sync::oneshot::channel::<()>();

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello(100)).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(2, "ws://127.0.0.1/unused")).await;
            receive_opcode(&mut socket, opcodes::HEARTBEAT).await;
            send_payload(
                &mut socket,
                GatewayPayload {
                    op: opcodes::HEARTBEAT_ACK,
                    d: None,
                    s: None,
                    t: None,
                },
            )
            .await;
            let _ = heartbeat_tx.send(());
            stopped_rx.await.expect("runtime stop completes");
            let terminal = timeout(Duration::from_millis(200), socket.next())
                .await
                .expect("stopped runtime closes socket");
            match terminal {
                None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
                Some(Ok(frame)) => panic!("stopped runtime emitted another frame: {frame:?}"),
            }
        });

        runtime.start().await.unwrap();
        timeout(Duration::from_secs(1), heartbeat_rx)
            .await
            .expect("heartbeat loop starts")
            .expect("server observes heartbeat");
        timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("stop cancels the runtime");
        let _ = stopped_tx.send(());
        server.await.unwrap();
    }
}
