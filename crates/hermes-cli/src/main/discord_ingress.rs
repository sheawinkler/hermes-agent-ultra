#[cfg(feature = "gateway-discord")]
const DISCORD_ROUTE_SPOOL_SCHEMA_VERSION: u32 = 3;
#[cfg(feature = "gateway-discord")]
const DISCORD_ROUTE_DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(feature = "gateway-discord")]
const DISCORD_ROUTE_SPOOL_MAX_ENTRIES: usize = 256;
#[cfg(feature = "gateway-discord")]
const DISCORD_ROUTE_SPOOL_MAX_BYTES: usize = 8 * 1024 * 1024;
#[cfg(feature = "gateway-discord")]
const DISCORD_PARTIAL_DELIVERY_MAX_ATTEMPTS: u32 = 5;
#[cfg(feature = "gateway-discord")]
const DISCORD_PARTIAL_DELIVERY_MAX_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(60);
#[cfg(feature = "gateway-discord")]
const DISCORD_INGRESS_BACKPRESSURE_MAX_DELAY: std::time::Duration =
    std::time::Duration::from_secs(1);
#[cfg(feature = "gateway-discord")]
const DISCORD_INGRESS_BACKPRESSURE_MIN_DELAY: std::time::Duration =
    std::time::Duration::from_millis(1);
#[cfg(feature = "gateway-discord")]
const DISCORD_INGRESS_RETRYABLE_BASE_DELAY: std::time::Duration =
    std::time::Duration::from_millis(100);
#[cfg(feature = "gateway-discord")]
const DISCORD_RECOVERY_QUARANTINE_WRITE_ATTEMPTS: u8 = 3;
#[cfg(feature = "gateway-discord")]
const DISCORD_SPOOL_COMMIT_GATE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_millis(250);

struct DiscordIngressShutdown {
    #[cfg(feature = "gateway-discord")]
    adapter: Arc<DiscordAdapter>,
    #[cfg(feature = "gateway-discord")]
    task: Option<tokio::task::JoinHandle<()>>,
    #[cfg(feature = "gateway-discord")]
    intake_done: tokio::sync::watch::Receiver<bool>,
    #[cfg(feature = "gateway-discord")]
    spool_path: PathBuf,
    #[cfg(feature = "gateway-discord")]
    recovery: Arc<DiscordIngressRecovery>,
    #[cfg(feature = "gateway-discord")]
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
    /// A pre-established supervisor, retained by the gateway's sidecar task
    /// set before a drain can be cancelled.
    #[cfg(feature = "gateway-discord")]
    cleanup_tx: Option<tokio::sync::mpsc::UnboundedSender<DiscordIngressCleanupRequest>>,
    /// Resolves only after every cleanup sender has dropped and the
    /// supervisor has completed its final durable handoff.
    #[cfg(feature = "gateway-discord")]
    cleanup_supervisor_done: Option<tokio::sync::oneshot::Receiver<()>>,
}

/// Work handed to the independently owned cleanup supervisor. The sender takes
/// the ingress task synchronously, so cancellation cannot leave it owned by a
/// dropping drain future.
#[cfg(feature = "gateway-discord")]
struct DiscordIngressCleanupRequest {
    adapter: Arc<DiscordAdapter>,
    task: tokio::task::JoinHandle<()>,
    recovery: Arc<DiscordIngressRecovery>,
    spool_path: PathBuf,
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
    completion: Option<
        tokio::sync::oneshot::Sender<Result<(), hermes_core::errors::GatewayError>>,
    >,
}

/// Starts the cleanup supervisor before the ingress shutdown owner is exposed
/// to callers. Its JoinHandle is retained in the gateway sidecar set.
#[cfg(feature = "gateway-discord")]
fn start_discord_ingress_cleanup_supervisor() -> (
    tokio::sync::mpsc::UnboundedSender<DiscordIngressCleanupRequest>,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (cleanup_tx, mut cleanup_rx) = tokio::sync::mpsc::unbounded_channel();
    let (supervisor_done_tx, supervisor_done_rx) = tokio::sync::oneshot::channel();
    let supervisor = tokio::spawn(async move {
        while let Some(request) = cleanup_rx.recv().await {
            let DiscordIngressCleanupRequest {
                adapter,
                mut task,
                recovery,
                spool_path,
                spool_write_fence,
                completion,
            } = request;
            let result = abort_and_export_discord_ingress(
                &adapter,
                &mut task,
                &recovery,
                &spool_path,
                &spool_write_fence,
            )
            .await;
            if let Some(completion) = completion {
                if completion.send(result).is_err() {
                    tracing::debug!(
                        path = %spool_path.display(),
                        "Discord ingress cleanup completed after its awaiting drain was cancelled"
                    );
                }
            } else if let Err(error) = result {
                tracing::error!(
                    error = %error,
                    path = %spool_path.display(),
                    "cancelled Discord ingress cleanup failed"
                );
            }
        }
        let _ = supervisor_done_tx.send(());
    });
    (cleanup_tx, supervisor, supervisor_done_rx)
}

/// Keeps the ingress task owned until it has either completed normally or been
/// transferred to the pre-established cleanup supervisor.
#[cfg(feature = "gateway-discord")]
struct DiscordIngressCleanup {
    adapter: Arc<DiscordAdapter>,
    task: Option<tokio::task::JoinHandle<()>>,
    recovery: Arc<DiscordIngressRecovery>,
    spool_path: PathBuf,
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
    cleanup_tx: tokio::sync::mpsc::UnboundedSender<DiscordIngressCleanupRequest>,
}

#[cfg(feature = "gateway-discord")]
impl DiscordIngressCleanup {
    fn new(
        adapter: Arc<DiscordAdapter>,
        task: tokio::task::JoinHandle<()>,
        recovery: Arc<DiscordIngressRecovery>,
        spool_path: PathBuf,
        spool_write_fence: Arc<DiscordSpoolWriteFence>,
        cleanup_tx: tokio::sync::mpsc::UnboundedSender<DiscordIngressCleanupRequest>,
    ) -> Self {
        Self {
            adapter,
            task: Some(task),
            recovery,
            spool_path,
            spool_write_fence,
            cleanup_tx,
        }
    }

    fn task_mut(&mut self) -> &mut tokio::task::JoinHandle<()> {
        self.task
            .as_mut()
            .expect("ingress drain task remains owned until cleanup completes")
    }

    fn discard_completed_task(&mut self) {
        let _ = self.task.take();
    }

    fn handoff_cleanup(
        &mut self,
    ) -> Result<
        tokio::sync::oneshot::Receiver<Result<(), hermes_core::errors::GatewayError>>,
        hermes_core::errors::GatewayError,
    > {
        let task = self
            .task
            .take()
            .expect("ingress cleanup owns its task until terminal handoff");
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        let request = DiscordIngressCleanupRequest {
            adapter: Arc::clone(&self.adapter),
            task,
            recovery: Arc::clone(&self.recovery),
            spool_path: self.spool_path.clone(),
            spool_write_fence: Arc::clone(&self.spool_write_fence),
            completion: Some(completion_tx),
        };
        if let Err(error) = self.cleanup_tx.send(request) {
            let request = error.0;
            request.task.abort();
            return Err(hermes_core::errors::GatewayError::Platform(
                "Discord ingress cleanup supervisor is unavailable; ingress was aborted but durable recovery export could not be scheduled"
                    .to_string(),
            ));
        }
        Ok(completion_rx)
    }

    fn handoff_cleanup_without_wait(&mut self) {
        let Some(task) = self.task.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_err() {
            task.abort();
            return;
        }
        let request = DiscordIngressCleanupRequest {
            adapter: Arc::clone(&self.adapter),
            task,
            recovery: Arc::clone(&self.recovery),
            spool_path: self.spool_path.clone(),
            spool_write_fence: Arc::clone(&self.spool_write_fence),
            completion: None,
        };
        if let Err(error) = self.cleanup_tx.send(request) {
            let request = error.0;
            request.task.abort();
            tracing::error!(
                path = %request.spool_path.display(),
                "Discord ingress cleanup supervisor was unavailable; aborted ingress without durable export"
            );
        }
    }
}

#[cfg(feature = "gateway-discord")]
impl Drop for DiscordIngressCleanup {
    fn drop(&mut self) {
        self.handoff_cleanup_without_wait();
    }
}

#[cfg(feature = "gateway-discord")]
async fn finish_discord_ingress_cleanup(
    cleanup: &mut DiscordIngressCleanup,
) -> Result<(), hermes_core::errors::GatewayError> {
    let completion = cleanup.handoff_cleanup()?;
    completion.await.map_err(|_| {
        hermes_core::errors::GatewayError::Platform(
            "Discord ingress cleanup supervisor ended before reporting durable recovery export"
                .to_string(),
        )
    })?
}

#[cfg(feature = "gateway-discord")]
#[derive(Default)]
struct DiscordIngressRecovery {
    entries: Mutex<
        std::collections::VecDeque<hermes_gateway::platforms::discord::DiscordJournalEntry>,
    >,
}

#[cfg(feature = "gateway-discord")]
impl DiscordIngressRecovery {
    /// Retain a clone before an async spool write or unacknowledged reservation.
    /// The shutdown owner can durably export this buffer if the loop is aborted.
    fn retain(&self, entry: hermes_gateway::platforms::discord::DiscordJournalEntry) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !entries.iter().any(|queued| {
            queued.message().message_id == entry.message().message_id
        }) {
            entries.push_back(entry);
        }
    }

    fn release(&self, message_id: &str) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| entry.message().message_id != message_id);
    }

    fn snapshot(&self) -> Vec<hermes_gateway::platforms::discord::DiscordJournalEntry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    fn release_entries(
        &self,
        entries: &[hermes_gateway::platforms::discord::DiscordJournalEntry],
    ) {
        let message_ids = entries
            .iter()
            .map(|entry| entry.message().message_id.clone())
            .collect::<std::collections::HashSet<_>>();
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|entry| !message_ids.contains(&entry.message().message_id));
    }
}

/// Per-gateway spool write fence.
///
/// A forced shutdown closes this fence before it exports recovery. Writers
/// capture a generation before doing blocking I/O and must still hold the
/// current generation immediately before replacing the live spool. The local
/// commit gate serializes only that tiny replace critical section; it is never
/// shared across spool paths or held while a quarantine file is written.
#[cfg(feature = "gateway-discord")]
struct DiscordSpoolWriteFence {
    generation: std::sync::atomic::AtomicU64,
    closed: std::sync::atomic::AtomicBool,
    commit_gate: Mutex<()>,
    #[cfg(test)]
    before_replace_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

#[cfg(feature = "gateway-discord")]
impl Default for DiscordSpoolWriteFence {
    fn default() -> Self {
        Self {
            generation: std::sync::atomic::AtomicU64::new(0),
            closed: std::sync::atomic::AtomicBool::new(false),
            commit_gate: Mutex::new(()),
            #[cfg(test)]
            before_replace_hook: Mutex::new(None),
        }
    }
}

#[cfg(feature = "gateway-discord")]
impl DiscordSpoolWriteFence {
    fn capture(&self) -> Option<u64> {
        (!self.closed.load(Ordering::Acquire))
            .then(|| self.generation.load(Ordering::Acquire))
    }


    fn is_current(&self, generation: u64) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.generation.load(Ordering::Acquire) == generation
    }

    #[cfg(test)]
    fn set_before_replace_hook(&self, hook: Arc<dyn Fn() + Send + Sync>) {
        *self
            .before_replace_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(hook);
    }

    #[cfg(test)]
    fn run_before_replace_hook(&self) {
        let hook = self
            .before_replace_hook
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl DiscordIngressShutdown {
    async fn drain(self) -> Result<(), hermes_core::errors::GatewayError> {
        #[cfg(feature = "gateway-discord")]
        {
            return self
                .drain_with_timeout(DISCORD_ROUTE_DRAIN_TIMEOUT)
                .await;
        }
        #[cfg(not(feature = "gateway-discord"))]
        {
            let _ = self;
            Ok(())
        }
    }

    #[cfg(feature = "gateway-discord")]
    async fn finish_cleanup_supervisor(
        &mut self,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        let _ = self.cleanup_tx.take();
        self.cleanup_supervisor_done
            .take()
            .expect("Discord ingress shutdown owns the cleanup supervisor completion")
            .await
            .map_err(|_| {
                hermes_core::errors::GatewayError::Platform(
                    "Discord ingress cleanup supervisor ended before its final durable handoff"
                        .to_string(),
                )
            })
    }

    #[cfg(feature = "gateway-discord")]
    async fn drain_with_timeout(
        mut self,
        drain_timeout: std::time::Duration,
    ) -> Result<(), hermes_core::errors::GatewayError> {
        let mut cleanup = DiscordIngressCleanup::new(
            Arc::clone(&self.adapter),
            self.task
                .take()
                .expect("Discord ingress shutdown owns its task until drain"),
            Arc::clone(&self.recovery),
            self.spool_path.clone(),
            Arc::clone(&self.spool_write_fence),
            self.cleanup_tx
                .as_ref()
                .expect("Discord ingress shutdown owns its cleanup supervisor sender")
                .clone(),
        );
        let mut intake_done = self.intake_done.clone();
        let drain_result = {
            let shutdown = async {
                cleanup.adapter.quiesce_inbound().await?;
                if !*intake_done.borrow() && intake_done.wait_for(|done| *done).await.is_err() {
                    return Err(hermes_core::errors::GatewayError::Platform(
                        "Discord ingress owner ended before confirming durable intake".to_string(),
                    ));
                }
                cleanup.task_mut().await.map_err(|error| {
                    hermes_core::errors::GatewayError::Platform(format!(
                        "Discord ingress owner ended unexpectedly: {error}"
                    ))
                })
            };
            tokio::time::timeout(drain_timeout, shutdown).await
        };

        let outcome = match drain_result {
            Ok(Ok(())) => {
                cleanup.discard_completed_task();
                Ok(())
            }
            Ok(Err(error)) => match finish_discord_ingress_cleanup(&mut cleanup).await {
                Ok(()) => Err(error),
                Err(export_error) => Err(hermes_core::errors::GatewayError::Platform(
                    format!(
                        "Discord ingress drain failed: {error}; durable recovery export also failed: {export_error}"
                    ),
                )),
            },
            Err(_) => {
                tracing::warn!(
                    timeout_seconds = drain_timeout.as_secs_f64(),
                    path = %cleanup.spool_path.display(),
                    spool_state = "owner-aborted-before-drain-confirmation",
                    "Discord ingress drain reached its overall deadline; aborting the owner after durable recovery export"
                );
                match finish_discord_ingress_cleanup(&mut cleanup).await {
                    Ok(()) => Err(hermes_core::errors::GatewayError::Platform(
                        "Discord ingress drain timed out before durable confirmation; recovered ingress was quarantined"
                            .to_string(),
                    )),
                    Err(export_error) => Err(hermes_core::errors::GatewayError::Platform(
                        format!(
                            "Discord ingress drain timed out and durable recovery export failed: {export_error}"
                        ),
                    )),
                }
            }
        };
        drop(cleanup);
        self.finish_cleanup_supervisor().await?;
        outcome
    }
}

impl Drop for DiscordIngressShutdown {
    fn drop(&mut self) {
        #[cfg(feature = "gateway-discord")]
        if let Some(task) = self.task.take() {
            // Covers a drain future dropped before its first poll as well as
            // direct shutdown-owner disposal. The guard transfers ownership
            // synchronously to the supervisor retained by gateway sidecars.
            let Some(cleanup_tx) = self.cleanup_tx.as_ref().cloned() else {
                task.abort();
                return;
            };
            drop(DiscordIngressCleanup::new(
                Arc::clone(&self.adapter),
                task,
                Arc::clone(&self.recovery),
                self.spool_path.clone(),
                Arc::clone(&self.spool_write_fence),
                cleanup_tx,
            ));
        }
    }
}

#[cfg(feature = "gateway-discord")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DiscordRouteSpool {
    schema_version: u32,
    entries: Vec<DiscordRouteSpoolEntry>,
}

#[cfg(feature = "gateway-discord")]
impl Default for DiscordRouteSpool {
    fn default() -> Self {
        Self {
            schema_version: DISCORD_ROUTE_SPOOL_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

#[cfg(feature = "gateway-discord")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct DiscordRouteSpoolEntry {
    journal: hermes_gateway::platforms::discord::DiscordJournalEntry,
    state: DiscordRouteSpoolState,
    recorded_at_unix_ms: u128,
    last_error: Option<String>,
    /// A persisted cursor for a response whose visible prefix was already
    /// accepted by Discord. It is never applied to a fresh inbound route.
    #[serde(default)]
    partial_delivery: Option<hermes_core::errors::PartialDelivery>,
    #[serde(default)]
    partial_attempts: u32,
    #[serde(default)]
    partial_retry_after_unix_ms: Option<u128>,
    /// Absolute wall-clock deadline for a full inbound route that was proven
    /// retryable before any handler or external effect began.
    #[serde(default)]
    retry_not_before_unix_ms: Option<u128>,
}

#[cfg(feature = "gateway-discord")]
impl DiscordRouteSpoolEntry {
    fn with_state(
        journal: hermes_gateway::platforms::discord::DiscordJournalEntry,
        state: DiscordRouteSpoolState,
    ) -> Self {
        Self {
            journal,
            state,
            recorded_at_unix_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            last_error: None,
            partial_delivery: None,
            partial_attempts: 0,
            partial_retry_after_unix_ms: None,
            retry_not_before_unix_ms: None,
        }
    }

    /// Raw ingress is durable before thread lookup and local admission. It
    /// must re-enter the authenticated Discord resolver after restart.
    fn pre_policy(
        journal: hermes_gateway::platforms::discord::DiscordJournalEntry,
    ) -> Self {
        debug_assert!(journal.is_pre_policy());
        Self::with_state(journal, DiscordRouteSpoolState::PrePolicy)
    }

    /// The adapter has resolved thread context and admitted local policy, but
    /// the CLI has not begun route ownership yet.
    fn post_policy_pending(
        journal: hermes_gateway::platforms::discord::DiscordJournalEntry,
    ) -> Self {
        debug_assert!(journal.is_post_policy());
        Self::with_state(journal, DiscordRouteSpoolState::Pending)
    }

    /// A shutdown recovery record is safe to restore only before route
    /// ownership. Its phase determines which owner must resume it.
    fn pre_route_recovery(
        journal: hermes_gateway::platforms::discord::DiscordJournalEntry,
    ) -> Self {
        if journal.is_post_policy() {
            Self::post_policy_pending(journal)
        } else {
            Self::pre_policy(journal)
        }
    }

    fn message_id(&self) -> &str {
        &self.journal.message().message_id
    }
}

#[cfg(feature = "gateway-discord")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum DiscordRouteSpoolState {
    /// Raw durable ingress that must re-enter the adapter's authenticated,
    /// ordered resolver and local policy before it can reach the CLI router.
    PrePolicy,
    /// Legacy schema-v2 spelling. Startup immediately migrates it to
    /// `PrePolicy`; new spool writes never use it.
    Unresolved,
    /// Verified post-policy ingress durably recorded before route ownership.
    Pending,
    /// Verified post-policy ingress whose full route proved retryable before
    /// any handler, model, tool, or external effect began.
    RetryPending,
    Processing,
    PartialPending,
    PartialFailed,
    /// A crash occurred after route ownership began; external side effects may
    /// have happened, so this entry requires explicit operator resolution.
    Uncertain,
    Failed,
}

#[cfg(feature = "gateway-discord")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordSpoolAdmission {
    Recorded,
    Existing,
    AtCapacity,
}

#[cfg(feature = "gateway-discord")]
impl DiscordRouteSpool {
    fn load(path: &Path) -> Result<Self, AgentError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)
            .map_err(|error| AgentError::Io(format!("read {}: {error}", path.display())))?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let mut spool: Self = serde_json::from_str(&raw)
            .map_err(|error| AgentError::Config(format!("parse {}: {error}", path.display())))?;
        match spool.schema_version {
            2 => {
                // Schema v2 did not identify whether an unresolved record had
                // passed thread resolution and local policy. Treat every full
                // inbound record as raw, pre-policy work rather than guessing
                // that it is safe to route directly.
                for entry in &mut spool.entries {
                    if matches!(
                        entry.state,
                        DiscordRouteSpoolState::Unresolved | DiscordRouteSpoolState::Pending
                    ) {
                        entry.state = DiscordRouteSpoolState::PrePolicy;
                        entry.last_error = None;
                        entry.retry_not_before_unix_ms = None;
                    }
                }
                spool.schema_version = DISCORD_ROUTE_SPOOL_SCHEMA_VERSION;
            }
            DISCORD_ROUTE_SPOOL_SCHEMA_VERSION => {}
            schema_version => {
                return Err(AgentError::Config(format!(
                    "unsupported Discord route spool schema {} in {}",
                    schema_version,
                    path.display()
                )));
            }
        }

        for entry in &mut spool.entries {
            match entry.state {
                // A v3 spool may contain the legacy spelling only if an older
                // writer raced the upgrade. Its missing phase is deliberately
                // treated as pre-policy rather than routed directly.
                DiscordRouteSpoolState::Unresolved => {
                    if entry.journal.is_pre_policy() {
                        entry.state = DiscordRouteSpoolState::PrePolicy;
                        entry.last_error = None;
                        entry.retry_not_before_unix_ms = None;
                    } else {
                        entry.state = DiscordRouteSpoolState::Uncertain;
                        entry.last_error = Some(
                            "Discord spool legacy unresolved state conflicts with a post-policy journal; automatic replay is prohibited"
                                .to_string(),
                        );
                    }
                }
                DiscordRouteSpoolState::PrePolicy => {
                    if !entry.journal.is_pre_policy() {
                        entry.state = DiscordRouteSpoolState::Uncertain;
                        entry.last_error = Some(
                            "Discord spool pre-policy state contains a post-policy journal; automatic replay is prohibited"
                                .to_string(),
                        );
                    }
                }
                // A pre-route post-policy record is safe to retry after
                // restart. Only this explicit phase/state pair may seed the
                // CLI's direct route queue.
                DiscordRouteSpoolState::Pending => {
                    if entry.journal.is_post_policy() {
                        entry.state = DiscordRouteSpoolState::RetryPending;
                    } else {
                        entry.state = DiscordRouteSpoolState::PrePolicy;
                        entry.last_error = None;
                        entry.retry_not_before_unix_ms = None;
                    }
                }
                DiscordRouteSpoolState::RetryPending => {
                    if !entry.journal.is_post_policy() {
                        entry.state = DiscordRouteSpoolState::Uncertain;
                        entry.last_error = Some(
                            "Discord spool retry-pending state lacks post-policy admission proof; automatic replay is prohibited"
                                .to_string(),
                        );
                    }
                }
                // Once a route task owns an item, a crash may leave unknown
                // tool or provider side effects. The generic gateway dedup is
                // memory-only, so replay would be unsafe without a durable
                // transaction/idempotency key.
                DiscordRouteSpoolState::Processing => {
                    entry.state = DiscordRouteSpoolState::Uncertain;
                    entry.last_error = Some(
                        "Discord route ownership was interrupted by restart; side effects are uncertain and automatic replay is prohibited"
                            .to_string(),
                    );
                }
                DiscordRouteSpoolState::PartialPending
                | DiscordRouteSpoolState::PartialFailed
                | DiscordRouteSpoolState::Uncertain
                | DiscordRouteSpoolState::Failed => {}
            }
        }
        Ok(spool)
    }

    fn contains(&self, message_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.message_id() == message_id)
    }

    fn existing_reservation_outcome(
        &self,
        message_id: &str,
    ) -> hermes_gateway::platforms::discord::DiscordIngressReservationOutcome {
        match self
            .entries
            .iter()
            .find(|entry| entry.message_id() == message_id)
            .map(|entry| entry.state)
        {
            Some(DiscordRouteSpoolState::PrePolicy) => {
                hermes_gateway::platforms::discord::DiscordIngressReservationOutcome::ExistingUnresolved
            }
            _ => hermes_gateway::platforms::discord::DiscordIngressReservationOutcome::ExistingRouteOwned,
        }
    }

    /// Raw records are replayed only by Discord's authenticated, ordered
    /// resolver. They are never reconstructed into an inbound route here.
    fn recoverable_pre_policy_entries(
        &self,
    ) -> Vec<hermes_gateway::platforms::discord::DiscordJournalEntry> {
        self.entries
            .iter()
            .filter(|entry| {
                entry.state == DiscordRouteSpoolState::PrePolicy
                    && entry.journal.is_pre_policy()
            })
            .map(|entry| entry.journal.clone())
            .collect()
    }

    /// Recover only explicit post-policy retry-safe work in durable spool
    /// order. A raw record cannot bypass this boundary.
    fn recoverable_post_policy_inbound_entries(
        &self,
    ) -> Vec<(
        hermes_gateway::platforms::discord::DiscordInboundMessage,
        Option<u128>,
    )> {
        self.entries
            .iter()
            .filter(|entry| entry.state == DiscordRouteSpoolState::RetryPending)
            .filter_map(|entry| {
                match hermes_gateway::platforms::discord::DiscordInboundMessage::from_post_policy_journal_entry(
                    entry.journal.clone(),
                ) {
                    Ok(inbound) => Some((inbound, entry.retry_not_before_unix_ms)),
                    Err(error) => {
                        tracing::error!(
                            message_id = entry.message_id(),
                            error = %error,
                            "Discord retry-pending spool entry failed post-policy validation; refusing direct route"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    fn retry_not_before_unix_ms(&self, message_id: &str) -> Option<u128> {
        self.entries
            .iter()
            .find(|entry| entry.message_id() == message_id)
            .and_then(|entry| entry.retry_not_before_unix_ms)
    }

    fn retryable_partial_entries(
        &self,
    ) -> Vec<(
        String,
        hermes_core::errors::PartialDelivery,
        Option<u128>,
    )> {
        self.entries
            .iter()
            .filter(|entry| entry.state == DiscordRouteSpoolState::PartialPending)
            .filter_map(|entry| {
                entry
                    .partial_delivery
                    .as_ref()
                    .filter(|partial| partial.retryable && partial.repair_required.is_none())
                    .cloned()
                    .map(|partial| {
                        (
                            entry.message_id().to_string(),
                            partial,
                            entry.partial_retry_after_unix_ms,
                        )
                    })
            })
            .collect()
    }

    /// All retained states, including terminal failures, consume the same
    /// bounded durable spool budget. Reaching it is fail-closed admission
    /// backpressure; accepted dead letters are never evicted automatically.
    fn spool_bytes(&self) -> Option<usize> {
        serde_json::to_vec_pretty(self).ok().map(|raw| raw.len())
    }

    fn try_push(&mut self, entry: DiscordRouteSpoolEntry) -> DiscordSpoolAdmission {
        if self.entries.len() >= DISCORD_ROUTE_SPOOL_MAX_ENTRIES {
            return DiscordSpoolAdmission::AtCapacity;
        }
        self.entries.push(entry);
        if self
            .spool_bytes()
            .is_none_or(|bytes| bytes > DISCORD_ROUTE_SPOOL_MAX_BYTES)
        {
            self.entries.pop();
            return DiscordSpoolAdmission::AtCapacity;
        }
        DiscordSpoolAdmission::Recorded
    }

    fn record_journal(
        &mut self,
        journal: &hermes_gateway::platforms::discord::DiscordJournalEntry,
    ) -> DiscordSpoolAdmission {
        if !journal.is_pre_policy() {
            tracing::error!(
                message_id = journal.message().message_id,
                "refusing a post-policy journal at the raw Discord reservation boundary"
            );
            return DiscordSpoolAdmission::AtCapacity;
        }
        let message_id = journal.message().message_id.trim();
        if message_id.is_empty() {
            return DiscordSpoolAdmission::AtCapacity;
        }
        if self.contains(message_id) {
            return DiscordSpoolAdmission::Existing;
        }
        self.try_push(DiscordRouteSpoolEntry::pre_policy(journal.clone()))
    }

    fn record_routable(
        &mut self,
        inbound: &hermes_gateway::platforms::discord::DiscordInboundMessage,
    ) -> DiscordSpoolAdmission {
        let journal = inbound.journal_entry();
        if !journal.is_post_policy() {
            tracing::error!(
                message_id = inbound.message().message_id,
                "refusing an inbound Discord record without post-policy admission proof"
            );
            return DiscordSpoolAdmission::AtCapacity;
        }
        let message_id = inbound.message().message_id.trim();
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            if entry.state == DiscordRouteSpoolState::PrePolicy {
                entry.journal = journal;
                entry.state = DiscordRouteSpoolState::Pending;
                entry.last_error = None;
                entry.retry_not_before_unix_ms = None;
                return DiscordSpoolAdmission::Recorded;
            }
            return DiscordSpoolAdmission::Existing;
        }
        self.try_push(DiscordRouteSpoolEntry::post_policy_pending(journal))
    }

    fn set_processing(&mut self, message_id: &str) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            entry.state = DiscordRouteSpoolState::Processing;
            entry.last_error = None;
            entry.retry_not_before_unix_ms = None;
        }
    }

    /// `GatewayRouteOutcome::Retryable` proves no outbound effect committed,
    /// so this explicit post-policy record may resume its full inbound route.
    fn set_retry_pending(
        &mut self,
        message_id: &str,
        error: String,
        retry_not_before_unix_ms: Option<u128>,
    ) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            debug_assert!(entry.journal.is_post_policy());
            entry.state = DiscordRouteSpoolState::RetryPending;
            entry.last_error = Some(error);
            entry.partial_delivery = None;
            entry.partial_attempts = 0;
            entry.partial_retry_after_unix_ms = None;
            entry.retry_not_before_unix_ms = retry_not_before_unix_ms;
        }
    }

    /// Preserve terminal route failures as durable dead letters. They are
    /// excluded from active admission limits but never auto-deleted.
    fn set_failed(&mut self, message_id: &str, error: String) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            entry.state = DiscordRouteSpoolState::Failed;
            entry.last_error = Some(error);
            entry.retry_not_before_unix_ms = None;
        }
    }

    fn set_partial_delivery(
        &mut self,
        message_id: &str,
        partial: hermes_core::errors::PartialDelivery,
    ) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            let attempts = entry.partial_attempts.saturating_add(1);
            entry.partial_attempts = attempts;
            entry.last_error = Some(partial.failure.clone());
            entry.partial_delivery = Some(partial.clone());
            entry.retry_not_before_unix_ms = None;
            if !partial.retryable
                || partial.repair_required.is_some()
                || attempts >= DISCORD_PARTIAL_DELIVERY_MAX_ATTEMPTS
            {
                entry.state = DiscordRouteSpoolState::PartialFailed;
                entry.partial_retry_after_unix_ms = None;
                return;
            }
            let fallback = std::time::Duration::from_millis(
                1_000u64.saturating_mul(1u64 << attempts.saturating_sub(1).min(5)),
            )
            .min(DISCORD_PARTIAL_DELIVERY_MAX_BACKOFF);
            // A provider hint is authoritative. Keep the exact Duration in
            // `PartialDelivery` and round the persisted absolute deadline up
            // to the next millisecond so replay never occurs early.
            let retry_after = partial.retry_after.unwrap_or(fallback);
            let retry_after_ms = retry_after
                .as_nanos()
                .saturating_add(999_999)
                / 1_000_000;
            entry.state = DiscordRouteSpoolState::PartialPending;
            entry.partial_retry_after_unix_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .checked_add(retry_after_ms);
        }
    }


    fn set_uncertain(&mut self, message_id: &str, error: String) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == message_id)
        {
            entry.state = DiscordRouteSpoolState::Uncertain;
            entry.last_error = Some(error);
            entry.retry_not_before_unix_ms = None;
        }
    }

    fn remove_pre_policy(&mut self, message_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| {
            entry.message_id() != message_id
                || entry.state != DiscordRouteSpoolState::PrePolicy
        });
        self.entries.len() != before
    }

    fn remove_completed(&mut self, message_id: &str) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|entry| entry.message_id() != message_id);
        self.entries.len() != before
    }
}

#[cfg(feature = "gateway-discord")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiscordSpoolWriteOutcome {
    Written,
    Fenced,
}

#[cfg(feature = "gateway-discord")]
fn write_discord_route_spool(path: &Path, spool: &DiscordRouteSpool) -> Result<(), AgentError> {
    match write_discord_route_spool_inner(path, spool, None)? {
        DiscordSpoolWriteOutcome::Written => Ok(()),
        DiscordSpoolWriteOutcome::Fenced => unreachable!("unfenced spool write cannot be fenced"),
    }
}

#[cfg(feature = "gateway-discord")]
fn write_discord_route_spool_if_current(
    path: &Path,
    spool: &DiscordRouteSpool,
    fence: &DiscordSpoolWriteFence,
    generation: u64,
) -> Result<DiscordSpoolWriteOutcome, AgentError> {
    write_discord_route_spool_inner(path, spool, Some((fence, generation)))
}

#[cfg(feature = "gateway-discord")]
fn write_discord_route_spool_inner(
    path: &Path,
    spool: &DiscordRouteSpool,
    fence: Option<(&DiscordSpoolWriteFence, u64)>,
) -> Result<DiscordSpoolWriteOutcome, AgentError> {
    use std::io::Write as _;

    let parent = path.parent().ok_or_else(|| {
        AgentError::Io(format!(
            "Discord route spool path {} has no parent directory",
            path.display()
        ))
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|error| AgentError::Io(format!("mkdir {}: {error}", parent.display())))?;
    let mut raw = serde_json::to_vec_pretty(spool)
        .map_err(|error| AgentError::Config(format!("serialize Discord route spool: {error}")))?;
    raw.push(b'\n');

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("discord-route-spool.json");
    let temporary = parent.join(format!(
        ".{file_name}.tmp.{}.{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| -> Result<DiscordSpoolWriteOutcome, AgentError> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(|error| {
            AgentError::Io(format!("create {}: {error}", temporary.display()))
        })?;
        file.write_all(&raw).map_err(|error| {
            AgentError::Io(format!("write {}: {error}", temporary.display()))
        })?;
        file.sync_all().map_err(|error| {
            AgentError::Io(format!("fsync {}: {error}", temporary.display()))
        })?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| {
                    AgentError::Io(format!(
                        "set permissions on {}: {error}",
                        temporary.display()
                    ))
                })?;
        }
        let replace = || {
            hermes_config::atomic_replace_file(&temporary, path).map_err(|error| {
                AgentError::Io(format!(
                    "replace {} -> {}: {error}",
                    temporary.display(),
                    path.display()
                ))
            })
        };
        if let Some((fence, generation)) = fence {
            // The shutdown fence never waits on temp-file I/O. It only
            // serializes this check-and-replace boundary for this one spool.
            let _commit_gate = fence
                .commit_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !fence.is_current(generation) {
                return Ok(DiscordSpoolWriteOutcome::Fenced);
            }
            #[cfg(test)]
            fence.run_before_replace_hook();
            replace()?;
        } else {
            replace()?;
        }
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| {
                AgentError::Io(format!("fsync spool directory {}: {error}", parent.display()))
            })?;
        Ok(DiscordSpoolWriteOutcome::Written)
    })();
    let remove_temporary = result.is_err()
        || matches!(&result, Ok(DiscordSpoolWriteOutcome::Fenced));
    if remove_temporary {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(feature = "gateway-discord")]
async fn persist_discord_route_spool(
    path: &Path,
    spool: &DiscordRouteSpool,
    fence: &Arc<DiscordSpoolWriteFence>,
) -> bool {
    let path = path.to_path_buf();
    let spool = spool.clone();
    loop {
        let Some(generation) = fence.capture() else {
            return false;
        };
        let result = run_discord_spool_blocking({
            let path = path.clone();
            let spool = spool.clone();
            let fence = Arc::clone(fence);
            move || write_discord_route_spool_if_current(&path, &spool, &fence, generation)
        })
        .await;
        match result {
            Ok(DiscordSpoolWriteOutcome::Written) => return true,
            Ok(DiscordSpoolWriteOutcome::Fenced) => return false,
            Err(error) => {
                tracing::error!(
                    error = %error,
                    path = %path.display(),
                    "Discord ingress is paused because its private durable spool cannot be committed"
                );
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[cfg(feature = "gateway-discord")]
async fn run_discord_spool_blocking<T, F>(write: F) -> Result<T, AgentError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, AgentError> + Send + 'static,
{
    tokio::task::spawn_blocking(write)
        .await
        .map_err(|error| AgentError::Io(format!("Discord spool writer task failed: {error}")))?
}


#[cfg(feature = "gateway-discord")]
fn restore_recovered_ingress_pre_route(
    spool_path: &Path,
    entries: &[hermes_gateway::platforms::discord::DiscordJournalEntry],
) -> Result<(), AgentError> {
    let mut spool = DiscordRouteSpool::load(spool_path)?;
    for journal in entries {
        let replacement = DiscordRouteSpoolEntry::pre_route_recovery(journal.clone());
        if let Some(existing) = spool
            .entries
            .iter_mut()
            .find(|entry| entry.message_id() == journal.message().message_id)
        {
            // The shutdown recovery journal is retained only before route
            // execution starts. It therefore wins over a just-persisted
            // Processing marker for the same message.
            *existing = replacement;
        } else if spool.try_push(replacement) != DiscordSpoolAdmission::Recorded {
            return Err(AgentError::Gateway(format!(
                "Discord recovery cannot restore {} because the durable spool at {} is full",
                journal.message().message_id,
                spool_path.display()
            )));
        }
    }
    write_discord_route_spool(spool_path, &spool)
}

#[cfg(feature = "gateway-discord")]
async fn export_discord_ingress_recovery(
    adapter: &DiscordAdapter,
    recovery: &DiscordIngressRecovery,
    spool_path: &Path,
    restore_main_spool: bool,
) -> Result<(), hermes_core::errors::GatewayError> {
    let runtime_entries = adapter.snapshot_recovered_ingress_for_cli();
    let cli_entries = recovery.snapshot();
    let runtime_message_ids = runtime_entries
        .iter()
        .map(|entry| entry.message().message_id.clone())
        .collect::<Vec<_>>();
    let mut entries = runtime_entries;
    entries.extend(cli_entries.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    entries.retain(|entry| seen.insert(entry.message().message_id.clone()));
    if entries.is_empty() {
        return Ok(());
    }

    let parent = spool_path.parent().ok_or_else(|| {
        hermes_core::errors::GatewayError::Platform(format!(
            "Discord route spool path {} has no parent directory",
            spool_path.display()
        ))
    })?;
    let quarantine_path = parent.join("discord-route-spool-quarantine").join(format!(
        "shutdown-recovery-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let quarantine_spool = DiscordRouteSpool {
        schema_version: DISCORD_ROUTE_SPOOL_SCHEMA_VERSION,
        entries: entries
            .iter()
            .cloned()
            .map(DiscordRouteSpoolEntry::pre_route_recovery)
            .collect(),
    };
    let mut last_error = None;
    for attempt in 1..=DISCORD_RECOVERY_QUARANTINE_WRITE_ATTEMPTS {
        let write_result = run_discord_spool_blocking({
            let path = quarantine_path.clone();
            let spool = quarantine_spool.clone();
            move || write_discord_route_spool(&path, &spool)
        })
        .await;
        match write_result {
            Ok(()) => {
                if restore_main_spool {
                    let restore_result = run_discord_spool_blocking({
                        let path = spool_path.to_path_buf();
                        let entries = entries.clone();
                        move || restore_recovered_ingress_pre_route(&path, &entries)
                    })
                    .await;
                    if let Err(error) = restore_result {
                        return Err(hermes_core::errors::GatewayError::Platform(format!(
                            "Discord ingress recovery quarantine was durable at {}, but replay precedence could not be persisted to {}: {error}",
                            quarantine_path.display(),
                            spool_path.display()
                        )));
                    }
                }
                adapter.acknowledge_recovered_ingress_for_cli(&runtime_message_ids);
                recovery.release_entries(&cli_entries);
                tracing::warn!(
                    path = %quarantine_path.display(),
                    restored_to_spool = restore_main_spool,
                    "quarantined Discord ingress recovered during forced shutdown"
                );
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
        if attempt < DISCORD_RECOVERY_QUARANTINE_WRITE_ATTEMPTS {
            tracing::warn!(
                attempt,
                max_attempts = DISCORD_RECOVERY_QUARANTINE_WRITE_ATTEMPTS,
                path = %quarantine_path.display(),
                "Discord ingress recovery quarantine write failed; retaining recovery buffers for retry"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    Err(hermes_core::errors::GatewayError::Platform(format!(
        "failed to durably quarantine Discord ingress recovery after {} attempts at {}: {}",
        DISCORD_RECOVERY_QUARANTINE_WRITE_ATTEMPTS,
        quarantine_path.display(),
        last_error.unwrap_or_else(|| "unknown write failure".to_string())
    )))
}

#[cfg(feature = "gateway-discord")]
async fn close_discord_spool_write_fence(
    fence: &Arc<DiscordSpoolWriteFence>,
) -> Result<(), hermes_core::errors::GatewayError> {
    let fence = Arc::clone(fence);
    let close = run_discord_spool_blocking(move || {
        // Use the same gate as check-and-replace so shutdown cannot land
        // between a writer's generation check and its atomic replacement.
        let _commit_gate = fence
            .commit_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        fence.closed.store(true, Ordering::Release);
        fence.generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    });
    match tokio::time::timeout(DISCORD_SPOOL_COMMIT_GATE_TIMEOUT, close).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(hermes_core::errors::GatewayError::Platform(format!(
            "failed while closing the Discord spool write fence: {error}"
        ))),
        Err(_) => Err(hermes_core::errors::GatewayError::Platform(format!(
            "timed out after {}ms closing the Discord spool write fence; recovery was quarantined but automatic replay remains blocked",
            DISCORD_SPOOL_COMMIT_GATE_TIMEOUT.as_millis()
        ))),
    }
}

#[cfg(feature = "gateway-discord")]
async fn abort_and_export_discord_ingress(
    adapter: &DiscordAdapter,
    task: &mut tokio::task::JoinHandle<()>,
    recovery: &DiscordIngressRecovery,
    spool_path: &Path,
    spool_write_fence: &Arc<DiscordSpoolWriteFence>,
) -> Result<(), hermes_core::errors::GatewayError> {
    // Close the write generation with the same check-and-replace gate before
    // cancellation or export. A spawn_blocking writer may survive task
    // cancellation, but it cannot race a shutdown between its final epoch
    // check and the live-spool replacement.
    let boundary = close_discord_spool_write_fence(spool_write_fence).await;
    adapter.abort_inbound().await;
    task.abort();
    let _ = task.await;
    let export = export_discord_ingress_recovery(
        adapter,
        recovery,
        spool_path,
        boundary.is_ok(),
    )
    .await;
    match (boundary, export) {
        (Ok(()), result) => result,
        (Err(boundary_error), Ok(())) => Err(boundary_error),
        (Err(boundary_error), Err(export_error)) => {
            Err(hermes_core::errors::GatewayError::Platform(format!(
                "{boundary_error}; durable recovery export also failed: {export_error}"
            )))
        }
    }
}

#[cfg(feature = "gateway-discord")]
fn bounded_discord_route_error(error: &dyn std::fmt::Display) -> String {
    let mut text = hermes_config::redact_url_credentials_for_display(&error.to_string());
    const MAX_ERROR_BYTES: usize = 2_048;
    if text.len() > MAX_ERROR_BYTES {
        let boundary = text
            .char_indices()
            .take_while(|(index, _)| *index <= MAX_ERROR_BYTES)
            .map(|(index, _)| index)
            .last()
            .unwrap_or(0);
        text.truncate(boundary);
        text.push_str("…");
    }
    text
}

#[cfg(feature = "gateway-discord")]
fn bounded_discord_ingress_backpressure_delay(
    retry_after: std::time::Duration,
) -> std::time::Duration {
    retry_after
        .max(DISCORD_INGRESS_BACKPRESSURE_MIN_DELAY)
        .min(DISCORD_INGRESS_BACKPRESSURE_MAX_DELAY)
}

#[cfg(feature = "gateway-discord")]
fn bounded_discord_ingress_retryable_delay(attempt: u32) -> std::time::Duration {
    let multiplier = 1_u64 << attempt.saturating_sub(1).min(4);
    std::time::Duration::from_millis(
        (DISCORD_INGRESS_RETRYABLE_BASE_DELAY.as_millis() as u64).saturating_mul(multiplier),
    )
    .min(DISCORD_INGRESS_BACKPRESSURE_MAX_DELAY)
}

#[cfg(feature = "gateway-discord")]
fn discord_unix_ms_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(feature = "gateway-discord")]
fn discord_retry_not_before_unix_ms(now_unix_ms: u128, retry_after: std::time::Duration) -> u128 {
    // Round up so a fractional provider delay is never retried early after a
    // durable JSON round trip.
    let retry_after_ms = retry_after
        .as_nanos()
        .saturating_add(999_999)
        / 1_000_000;
    now_unix_ms.saturating_add(retry_after_ms)
}

#[cfg(feature = "gateway-discord")]
fn discord_retry_delay_until(
    retry_not_before_unix_ms: u128,
    now_unix_ms: u128,
) -> Option<std::time::Duration> {
    let remaining_ms = retry_not_before_unix_ms.checked_sub(now_unix_ms)?;
    if remaining_ms == 0 {
        return None;
    }
    Some(std::time::Duration::from_millis(
        u64::try_from(remaining_ms).unwrap_or(u64::MAX),
    ))
}

#[cfg(feature = "gateway-discord")]
fn merge_discord_partial_delivery(
    previous: &hermes_core::errors::PartialDelivery,
    mut next: hermes_core::errors::PartialDelivery,
) -> Result<hermes_core::errors::PartialDelivery, hermes_core::errors::GatewayError> {
    match (&previous.repair_required, &next.repair_required) {
        (Some(previous_repair), Some(next_repair)) if previous_repair != next_repair => {
            return Err(hermes_core::errors::GatewayError::Platform(
                "Discord partial delivery repair requirements conflict; refusing automatic suffix replay"
                    .to_string(),
            ));
        }
        (Some(previous_repair), _) => {
            next.repair_required = Some(previous_repair.clone());
        }
        (None, _) => {}
    }
    next.accepted_chunks = previous.accepted_chunks.saturating_add(next.accepted_chunks);
    let mut accepted_message_ids = previous.accepted_message_ids.clone();
    accepted_message_ids.extend(next.accepted_message_ids);
    next.accepted_message_ids = accepted_message_ids;
    Ok(next)
}

#[cfg(feature = "gateway-discord")]
async fn route_discord_inbound(
    gateway: &Gateway,
    inbound: &hermes_gateway::platforms::discord::DiscordInboundMessage,
) -> Result<hermes_gateway::GatewayRouteOutcome, hermes_core::errors::GatewayError> {
    let incoming = inbound.to_gateway_message().ok_or_else(|| {
        hermes_core::errors::GatewayError::Platform(
            "Discord inbound event lost its attributable author before routing".to_string(),
        )
    })?;
    gateway
        .route_message_from_sender_outcome(&incoming, inbound.sender_provenance())
        .await
}

#[cfg(feature = "gateway-discord")]
type DiscordRouteResult =
    Result<hermes_gateway::GatewayRouteOutcome, hermes_core::errors::GatewayError>;

#[cfg(feature = "gateway-discord")]
struct DiscordRouteTask(Option<tokio::task::JoinHandle<DiscordRouteResult>>);

#[cfg(feature = "gateway-discord")]
impl DiscordRouteTask {
    fn new(handle: tokio::task::JoinHandle<DiscordRouteResult>) -> Self {
        Self(Some(handle))
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<DiscordRouteResult> {
        self.0.as_mut().expect("Discord route task missing")
    }
}

#[cfg(feature = "gateway-discord")]
impl Drop for DiscordRouteTask {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(feature = "gateway-discord")]
fn discord_ingress_can_finish(
    inbound_closed: bool,
    journal_closed: bool,
    no_active_route: bool,
    pending_inbound_empty: bool,
    pending_partial_empty: bool,
    no_blocked_journal: bool,
    no_blocked_inbound: bool,
    retry_deadline_active: bool,
) -> bool {
    inbound_closed
        && journal_closed
        && no_active_route
        // Both queues are already durable. Once intake is closed, a provider
        // retry delay must not keep the owner alive beyond its drain bound.
        && (pending_inbound_empty || retry_deadline_active)
        && (pending_partial_empty || retry_deadline_active)
        && no_blocked_journal
        && no_blocked_inbound
}

#[cfg(feature = "gateway-discord")]
async fn run_discord_incoming_loop(
    gateway: Arc<Gateway>,
    rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordInboundMessage,
    >,
    journal_rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordIngressJournalEvent,
    >,
    spool_path: PathBuf,
    spool: DiscordRouteSpool,
    intake_done: tokio::sync::watch::Sender<bool>,
    recovery: Arc<DiscordIngressRecovery>,
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
) {
    run_discord_incoming_loop_with_router(
        gateway,
        rx,
        journal_rx,
        spool_path,
        spool,
        intake_done,
        recovery,
        spool_write_fence,
        |gateway, inbound| async move {
            route_discord_inbound(&gateway, inbound.as_ref()).await
        },
    )
    .await;
}

#[cfg(feature = "gateway-discord")]
async fn run_discord_incoming_loop_with_router<Route, RouteFuture>(
    gateway: Arc<Gateway>,
    rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordInboundMessage,
    >,
    journal_rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordIngressJournalEvent,
    >,
    spool_path: PathBuf,
    spool: DiscordRouteSpool,
    intake_done: tokio::sync::watch::Sender<bool>,
    recovery: Arc<DiscordIngressRecovery>,
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
    route: Route,
) where
    Route: Fn(
            Arc<Gateway>,
            Arc<hermes_gateway::platforms::discord::DiscordInboundMessage>,
        ) -> RouteFuture
        + Clone
        + Send
        + Sync
        + 'static,
    RouteFuture: Future<Output = DiscordRouteResult> + Send + 'static,
{
    run_discord_incoming_loop_with_router_and_clock(
        gateway,
        rx,
        journal_rx,
        spool_path,
        spool,
        intake_done,
        recovery,
        spool_write_fence,
        route,
        discord_unix_ms_now,
    )
    .await;
}

#[cfg(feature = "gateway-discord")]
async fn run_discord_incoming_loop_with_router_and_clock<Route, RouteFuture, Clock>(
    gateway: Arc<Gateway>,
    mut rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordInboundMessage,
    >,
    mut journal_rx: tokio::sync::mpsc::Receiver<
        hermes_gateway::platforms::discord::DiscordIngressJournalEvent,
    >,
    spool_path: PathBuf,
    mut spool: DiscordRouteSpool,
    intake_done: tokio::sync::watch::Sender<bool>,
    recovery: Arc<DiscordIngressRecovery>,
    spool_write_fence: Arc<DiscordSpoolWriteFence>,
    route: Route,
    unix_now_ms: Clock,
) where
    Route: Fn(
            Arc<Gateway>,
            Arc<hermes_gateway::platforms::discord::DiscordInboundMessage>,
        ) -> RouteFuture
        + Clone
        + Send
        + Sync
        + 'static,
    RouteFuture: Future<Output = DiscordRouteResult> + Send + 'static,
    Clock: Fn() -> u128 + Send + Sync,
{
    enum LoopEvent {
        Journal(
            Option<hermes_gateway::platforms::discord::DiscordIngressJournalEvent>,
        ),
        Inbound(Option<hermes_gateway::platforms::discord::DiscordInboundMessage>),
        Completed(Result<DiscordRouteResult, tokio::task::JoinError>),
        PartialRetryReady,
        InboundRetryReady,
    }

    enum ActiveRouteKind {
        Inbound(Arc<hermes_gateway::platforms::discord::DiscordInboundMessage>),
        Partial(hermes_core::errors::PartialDelivery),
    }

    let recoverable_inbound = spool.recoverable_post_policy_inbound_entries();
    let recoverable = recoverable_inbound.len();
    let pre_policy = spool
        .entries
        .iter()
        .filter(|entry| entry.state == DiscordRouteSpoolState::PrePolicy)
        .count();
    let retryable_partials = spool.retryable_partial_entries();
    let quarantined = spool
        .entries
        .len()
        .saturating_sub(pre_policy)
        .saturating_sub(recoverable)
        .saturating_sub(retryable_partials.len());
    if recoverable > 0 || pre_policy > 0 || !retryable_partials.is_empty() || quarantined > 0 {
        tracing::warn!(
            recoverable_post_policy = recoverable,
            pre_policy,
            retryable_partials = retryable_partials.len(),
            quarantined,
            path = %spool_path.display(),
            "Discord route spool loaded; raw ingress re-enters the adapter resolver and only explicit post-policy retries may route directly"
        );
    }

    let mut pending: std::collections::VecDeque<(
        String,
        Arc<hermes_gateway::platforms::discord::DiscordInboundMessage>,
        Option<u128>,
    )> = recoverable_inbound
        .into_iter()
        .map(|(inbound, retry_not_before_unix_ms)| {
            let inbound = Arc::new(inbound);
            let message_id = inbound.message().message_id.clone();
            (message_id, inbound, retry_not_before_unix_ms)
        })
        .collect();
    let mut pending_partials: std::collections::VecDeque<(
        String,
        hermes_core::errors::PartialDelivery,
        Option<u128>,
    )> = retryable_partials.into();
    let mut active: Option<(String, ActiveRouteKind, DiscordRouteTask)> = None;
    let mut blocked_journal: Option<
        hermes_gateway::platforms::discord::DiscordIngressReservation,
    > = None;
    let mut blocked_inbound: Option<
        Arc<hermes_gateway::platforms::discord::DiscordInboundMessage>,
    > = None;
    let mut inbound_closed = false;
    let mut journal_closed = false;
    let mut partial_retry_deadline: Option<tokio::time::Instant> = None;
    let mut inbound_retry_deadline: Option<tokio::time::Instant> = None;
    let mut inbound_retry_attempt = 0_u32;

    loop {
        if let Some(mut reservation) = blocked_journal.take() {
            let message_id = reservation.entry().message().message_id.clone();
            recovery.retain(reservation.entry().clone());
            match spool.record_journal(reservation.entry()) {
                DiscordSpoolAdmission::Recorded => {
                    if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                        return;
                    }
                    recovery.release(&message_id);
                    reservation.acknowledge_durable(
                        hermes_gateway::platforms::discord::DiscordIngressReservationOutcome::Recorded,
                    );
                }
                DiscordSpoolAdmission::Existing => {
                    let outcome = spool.existing_reservation_outcome(&message_id);
                    recovery.release(&message_id);
                    reservation.acknowledge_durable(outcome);
                }
                DiscordSpoolAdmission::AtCapacity => {
                    blocked_journal = Some(reservation);
                }
            }
        }

        if let Some(inbound) = blocked_inbound.take() {
            let message_id = inbound.message().message_id.clone();
            recovery.retain(inbound.journal_entry());
            match spool.record_routable(inbound.as_ref()) {
                DiscordSpoolAdmission::Recorded => {
                    if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                        return;
                    }
                    let retry_not_before_unix_ms =
                        spool.retry_not_before_unix_ms(&message_id);
                    recovery.release(&message_id);
                    pending.push_back((message_id, inbound, retry_not_before_unix_ms));
                }
                DiscordSpoolAdmission::Existing => {
                    recovery.release(&message_id);
                    tracing::warn!(
                        message_id,
                        path = %spool_path.display(),
                        "Discord redelivery matches a route-owned spool item; suppressing duplicate routing"
                    );
                }
                DiscordSpoolAdmission::AtCapacity => {
                    blocked_inbound = Some(inbound);
                }
            }
        }

        if active.is_none()
            && partial_retry_deadline.is_none()
            && inbound_retry_deadline.is_none()
        {
            if let Some((message_id, partial, retry_after_unix_ms)) =
                pending_partials.pop_front()
            {
                if let Some(retry_after_unix_ms) = retry_after_unix_ms {
                    if let Some(delay) =
                        discord_retry_delay_until(retry_after_unix_ms, unix_now_ms())
                    {
                        // Keep the durable PartialPending record queued while
                        // the deadline is selectable. Later inbound remains
                        // durable but must not overtake this suffix.
                        pending_partials.push_front((
                            message_id,
                            partial,
                            Some(retry_after_unix_ms),
                        ));
                        partial_retry_deadline = Some(tokio::time::Instant::now() + delay);
                    } else {
                        spool.set_processing(&message_id);
                        if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                            return;
                        }
                        let gateway = Arc::clone(&gateway);
                        let task_partial = partial.clone();
                        active = Some((
                            message_id,
                            ActiveRouteKind::Partial(partial),
                            DiscordRouteTask::new(tokio::spawn(async move {
                                match gateway.resume_partial_delivery(&task_partial).await {
                                    Ok(()) => Ok(hermes_gateway::GatewayRouteOutcome::Committed),
                                    Err(hermes_core::errors::GatewayError::PartialDelivery(next)) => {
                                        let next = if next == task_partial {
                                            // A bare zero-progress rate-limit returns
                                            // the exact stored cursor.
                                            next
                                        } else {
                                            // Preserve every resumed accepted chunk,
                                            // including providers that did not return
                                            // message IDs for that chunk.
                                            merge_discord_partial_delivery(&task_partial, next)?
                                        };
                                        Ok(hermes_gateway::GatewayRouteOutcome::Partial(next))
                                    }
                                    Err(error) => Err(error),
                                }
                            })),
                        ));
                    }
                } else {
                    spool.set_processing(&message_id);
                    if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                        return;
                    }
                    let gateway = Arc::clone(&gateway);
                    let task_partial = partial.clone();
                    active = Some((
                        message_id,
                        ActiveRouteKind::Partial(partial),
                        DiscordRouteTask::new(tokio::spawn(async move {
                            match gateway.resume_partial_delivery(&task_partial).await {
                                Ok(()) => Ok(hermes_gateway::GatewayRouteOutcome::Committed),
                                Err(hermes_core::errors::GatewayError::PartialDelivery(next)) => {
                                    let next = if next == task_partial {
                                        // A bare zero-progress rate-limit returns
                                        // the exact stored cursor.
                                        next
                                    } else {
                                        // Preserve every resumed accepted chunk,
                                        // including providers that did not return
                                        // message IDs for that chunk.
                                        merge_discord_partial_delivery(&task_partial, next)?
                                    };
                                    Ok(hermes_gateway::GatewayRouteOutcome::Partial(next))
                                }
                                Err(error) => Err(error),
                            }
                        })),
                    ));
                }
            }
        }

        if active.is_none()
            && partial_retry_deadline.is_none()
            && inbound_retry_deadline.is_none()
        {
            if let Some((message_id, inbound, retry_not_before_unix_ms)) = pending.pop_front() {
                if let Some(delay) = retry_not_before_unix_ms
                    .and_then(|deadline| discord_retry_delay_until(deadline, unix_now_ms()))
                {
                    // The unresolved entry already carries this absolute
                    // deadline durably. Keep the entire FIFO queue behind its
                    // head while cancellation remains selectable.
                    pending.push_front((message_id, inbound, retry_not_before_unix_ms));
                    inbound_retry_deadline = Some(tokio::time::Instant::now() + delay);
                } else {
                    recovery.retain(inbound.journal_entry());
                    spool.set_processing(&message_id);
                    if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence)
                        .await
                    {
                        return;
                    }
                    let gateway = Arc::clone(&gateway);
                    let inbound_for_route = Arc::clone(&inbound);
                    let route = route.clone();
                    let recovery_message_id = message_id.clone();
                    active = Some((
                        message_id,
                        ActiveRouteKind::Inbound(inbound),
                        DiscordRouteTask::new(tokio::spawn(async move {
                            route(gateway, inbound_for_route).await
                        })),
                    ));
                    recovery.release(&recovery_message_id);
                }
            }
        }

        if discord_ingress_can_finish(
            inbound_closed,
            journal_closed,
            active.is_none(),
            pending.is_empty(),
            pending_partials.is_empty(),
            blocked_journal.is_none(),
            blocked_inbound.is_none(),
            partial_retry_deadline.is_some() || inbound_retry_deadline.is_some(),
        ) {
            intake_done.send_replace(true);
            break;
        }

        let completed = async {
            if let Some((_, _, task)) = active.as_mut() {
                task.handle_mut().await
            } else {
                std::future::pending().await
            }
        };
        let retry_deadline = partial_retry_deadline;
        let retry_wait = async move {
            if let Some(deadline) = retry_deadline {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let inbound_retry_deadline_for_wait = inbound_retry_deadline;
        let inbound_retry_wait = async move {
            if let Some(deadline) = inbound_retry_deadline_for_wait {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        let event = tokio::select! {
            journal = journal_rx.recv(),
                if !journal_closed && blocked_journal.is_none() =>
            {
                LoopEvent::Journal(journal)
            }
            inbound = rx.recv(),
                if !inbound_closed && blocked_inbound.is_none() =>
            {
                LoopEvent::Inbound(inbound)
            }
            result = completed => LoopEvent::Completed(result),
            _ = retry_wait, if partial_retry_deadline.is_some() => LoopEvent::PartialRetryReady,
            _ = inbound_retry_wait, if inbound_retry_deadline.is_some() => LoopEvent::InboundRetryReady,
        };

        match event {
            LoopEvent::PartialRetryReady => {
                partial_retry_deadline = None;
            }
            LoopEvent::InboundRetryReady => {
                inbound_retry_deadline = None;
            }
            LoopEvent::Journal(Some(
                hermes_gateway::platforms::discord::DiscordIngressJournalEvent::Reserve(
                    mut reservation,
                ),
            )) => {
                let message_id = reservation.entry().message().message_id.clone();
                recovery.retain(reservation.entry().clone());
                match spool.record_journal(reservation.entry()) {
                    DiscordSpoolAdmission::Recorded => {
                        if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                            return;
                        }
                        recovery.release(&message_id);
                        reservation.acknowledge_durable(
                            hermes_gateway::platforms::discord::DiscordIngressReservationOutcome::Recorded,
                        );
                    }
                    DiscordSpoolAdmission::Existing => {
                        let outcome = spool.existing_reservation_outcome(&message_id);
                        recovery.release(&message_id);
                        reservation.acknowledge_durable(outcome);
                    }
                    DiscordSpoolAdmission::AtCapacity => {
                        tracing::error!(
                            message_id,
                            max_entries = DISCORD_ROUTE_SPOOL_MAX_ENTRIES,
                            max_bytes = DISCORD_ROUTE_SPOOL_MAX_BYTES,
                            path = %spool_path.display(),
                            "Discord durable ingress spool is at capacity; pausing admission before sequence acknowledgement"
                        );
                        blocked_journal = Some(reservation);
                    }
                }
            }
            LoopEvent::Journal(Some(
                hermes_gateway::platforms::discord::DiscordIngressJournalEvent::Remove {
                    message_id,
                },
            )) => {
                if spool.remove_pre_policy(&message_id) {
                    if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                        return;
                    }
                }
            }
            LoopEvent::Journal(None) => {
                journal_closed = true;
            }
            LoopEvent::Inbound(Some(inbound)) => {
                let inbound = Arc::new(inbound);
                let message_id = inbound.message().message_id.clone();
                recovery.retain(inbound.journal_entry());
                match spool.record_routable(inbound.as_ref()) {
                    DiscordSpoolAdmission::Recorded => {
                        if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                            return;
                        }
                        let retry_not_before_unix_ms =
                            spool.retry_not_before_unix_ms(&message_id);
                        recovery.release(&message_id);
                        pending.push_back((message_id, inbound, retry_not_before_unix_ms));
                    }
                    DiscordSpoolAdmission::Existing => {
                        recovery.release(&message_id);
                        tracing::warn!(
                            message_id,
                            path = %spool_path.display(),
                            "Discord redelivery matches a route-owned spool item; suppressing duplicate routing"
                        );
                    }
                    DiscordSpoolAdmission::AtCapacity => {
                        tracing::error!(
                            message_id,
                            max_entries = DISCORD_ROUTE_SPOOL_MAX_ENTRIES,
                            max_bytes = DISCORD_ROUTE_SPOOL_MAX_BYTES,
                            path = %spool_path.display(),
                            "Discord route spool is at capacity; pausing resolved ingress"
                        );
                        blocked_inbound = Some(inbound);
                    }
                }
            }
            LoopEvent::Inbound(None) => {
                inbound_closed = true;
            }
            LoopEvent::Completed(result) => {
                let (message_id, kind, _) =
                    active.take().expect("active Discord route task missing");
                let mut release_recovery_after_persist = None;
                match result {
                    Ok(Ok(hermes_gateway::GatewayRouteOutcome::Committed)) => {
                        inbound_retry_attempt = 0;
                        spool.remove_completed(&message_id);
                    }
                    Ok(Ok(hermes_gateway::GatewayRouteOutcome::Partial(partial))) => {
                        inbound_retry_attempt = 0;
                        let retryable = partial.retryable;
                        spool.set_partial_delivery(&message_id, partial.clone());
                        if retryable {
                            if let Some(entry) = spool
                                .entries
                                .iter()
                                .find(|entry| entry.message_id() == message_id)
                            {
                                if entry.state == DiscordRouteSpoolState::PartialPending {
                                    pending_partials.push_front((
                                        message_id.clone(),
                                        partial,
                                        entry.partial_retry_after_unix_ms,
                                    ));
                                }
                            }
                        }
                    }
                    Ok(Ok(hermes_gateway::GatewayRouteOutcome::Backpressured {
                        retry_after,
                    })) => {
                        inbound_retry_attempt = 0;
                        if let ActiveRouteKind::Inbound(inbound) = kind {
                            let delay = bounded_discord_ingress_backpressure_delay(retry_after);
                            let retry_not_before_unix_ms =
                                discord_retry_not_before_unix_ms(unix_now_ms(), delay);
                            recovery.retain(inbound.journal_entry());
                            tracing::debug!(
                                message_id,
                                retry_after_ms = retry_after.as_millis(),
                                delay_ms = delay.as_millis(),
                                path = %spool_path.display(),
                                "Discord route admission is full; preserving durable FIFO inbound for bounded retry"
                            );
                            spool.set_retry_pending(
                                &message_id,
                                format!(
                                    "gateway ingress capacity; retrying in {}ms",
                                    delay.as_millis()
                                ),
                                Some(retry_not_before_unix_ms),
                            );
                            // A typed admission refusal proves no handler, model,
                            // tool, or external effect began. Preserve the exact
                            // durable head entry and retry it in this process once
                            // capacity is available; later inbound cannot overtake it.
                            pending.push_front((
                                message_id.clone(),
                                inbound,
                                Some(retry_not_before_unix_ms),
                            ));
                            inbound_retry_deadline = Some(tokio::time::Instant::now() + delay);
                            release_recovery_after_persist = Some(message_id.clone());
                        } else {
                            tracing::error!(
                                message_id,
                                path = %spool_path.display(),
                                "Discord partial route unexpectedly reported an admission refusal; quarantining"
                            );
                            spool.set_uncertain(
                                &message_id,
                                "partial route unexpectedly reported gateway ingress backpressure"
                                    .to_string(),
                            );
                        }
                    }
                    Ok(Ok(hermes_gateway::GatewayRouteOutcome::Retryable {
                        error,
                        retry_after,
                    })) => {
                        let error = bounded_discord_route_error(&error);
                        if let ActiveRouteKind::Inbound(inbound) = kind {
                            inbound_retry_attempt = inbound_retry_attempt.saturating_add(1);
                            let delay = retry_after.unwrap_or_else(|| {
                                bounded_discord_ingress_retryable_delay(inbound_retry_attempt)
                            });
                            let retry_not_before_unix_ms =
                                discord_retry_not_before_unix_ms(unix_now_ms(), delay);
                            recovery.retain(inbound.journal_entry());
                            tracing::warn!(
                                message_id,
                                error,
                                attempt = inbound_retry_attempt,
                                delay_ms = delay.as_millis(),
                                path = %spool_path.display(),
                                "Discord route failed before an external effect; preserving durable FIFO inbound for retry"
                            );
                            spool.set_retry_pending(
                                &message_id,
                                format!("{error}; retrying in {}ms", delay.as_millis()),
                                Some(retry_not_before_unix_ms),
                            );
                            // The durable absolute deadline is written before
                            // sleeping so a restart retains the provider's
                            // full retry requirement and FIFO head ownership.
                            pending.push_front((
                                message_id.clone(),
                                inbound,
                                Some(retry_not_before_unix_ms),
                            ));
                            inbound_retry_deadline = Some(tokio::time::Instant::now() + delay);
                            release_recovery_after_persist = Some(message_id.clone());
                        } else {
                            inbound_retry_attempt = 0;
                            tracing::error!(
                                message_id,
                                error,
                                path = %spool_path.display(),
                                "Discord partial route unexpectedly reported a full-inbound retry; quarantining"
                            );
                            spool.set_uncertain(&message_id, error);
                        }
                    }
                    Ok(Ok(hermes_gateway::GatewayRouteOutcome::Uncertain(error)))
                    | Ok(Err(error)) => {
                        inbound_retry_attempt = 0;
                        let error = bounded_discord_route_error(&error);
                        tracing::error!(
                            message_id,
                            error,
                            path = %spool_path.display(),
                            "Discord route outcome is uncertain; automatic full-inbound replay is prohibited"
                        );
                        spool.set_uncertain(&message_id, error);
                    }
                    Err(error) => {
                        inbound_retry_attempt = 0;
                        let error = bounded_discord_route_error(&error);
                        tracing::error!(
                            message_id,
                            error,
                            path = %spool_path.display(),
                            "Discord route task ended unexpectedly; automatic replay is prohibited"
                        );
                        spool.set_uncertain(&message_id, error);
                    }
                }
                if !persist_discord_route_spool(&spool_path, &spool, &spool_write_fence).await {
                    return;
                }
                if let Some(message_id) = release_recovery_after_persist {
                    recovery.release(&message_id);
                }
            }
        }
    }
}

#[cfg(all(test, feature = "gateway-discord"))]
mod discord_ingress_tests {
    use super::*;

    fn sample_spool() -> DiscordRouteSpool {
        let message = hermes_gateway::platforms::discord::IncomingDiscordMessage {
            channel_id: "channel-1".to_string(),
            channel_type: Some(0),
            guild_id: Some("guild-1".to_string()),
            parent_channel_id: None,
            thread_id: None,
            is_dm: false,
            message_id: "message-1".to_string(),
            user_id: Some("user-1".to_string()),
            username: Some("Ada".to_string()),
            content: "private prompt".to_string(),
            is_bot: false,
            member_role_ids: vec!["role-1".to_string()],
            message_type: 0,
            mention_user_ids: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
            reply_to_attachments: Vec::new(),
            attachments: Vec::new(),
        };
        DiscordRouteSpool {
            schema_version: DISCORD_ROUTE_SPOOL_SCHEMA_VERSION,
            entries: vec![DiscordRouteSpoolEntry {
                journal:
                    hermes_gateway::platforms::discord::DiscordJournalEntry::new(
                        message,
                        Some("bot-self".to_string()),
                    ),
                state: DiscordRouteSpoolState::PrePolicy,
                recorded_at_unix_ms: 1,
                last_error: None,
                partial_delivery: None,
                partial_attempts: 0,
                partial_retry_after_unix_ms: None,
                retry_not_before_unix_ms: None,
            }],
        }
    }

    fn post_policy_journal(
        message: hermes_gateway::platforms::discord::IncomingDiscordMessage,
    ) -> hermes_gateway::platforms::discord::DiscordJournalEntry {
        let mut encoded = serde_json::to_value(
            hermes_gateway::platforms::discord::DiscordJournalEntry::new(
                message,
                Some("bot-self".to_string()),
            ),
        )
        .expect("serialize journal fixture");
        encoded["phase"] = serde_json::Value::String("post_policy".to_string());
        serde_json::from_value(encoded).expect("deserialize post-policy journal fixture")
    }

    fn post_policy_inbound(
        message_id: &str,
    ) -> hermes_gateway::platforms::discord::DiscordInboundMessage {
        let mut spool = sample_spool();
        let mut message = spool
            .entries
            .remove(0)
            .journal
            .message()
            .clone();
        message.message_id = message_id.to_string();
        hermes_gateway::platforms::discord::DiscordInboundMessage::from_post_policy_journal_entry(
            post_policy_journal(message),
        )
        .expect("post-policy fixture is routable")
    }

    #[test]
    fn private_spool_roundtrips_and_recovers_pre_route_pending_ingress() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("discord-route-spool.json");
        let expected = sample_spool();

        write_discord_route_spool(&path, &expected).unwrap();
        let loaded = DiscordRouteSpool::load(&path).unwrap();

        assert_eq!(loaded.schema_version, DISCORD_ROUTE_SPOOL_SCHEMA_VERSION);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].message_id(), "message-1");
        assert_eq!(
            loaded.entries[0].journal.message().content,
            "private prompt"
        );
        assert_eq!(
            loaded.entries[0].state,
            DiscordRouteSpoolState::PrePolicy
        );
        assert_eq!(loaded.recoverable_pre_policy_entries().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn legacy_v2_full_inbound_is_migrated_to_raw_pre_policy_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("discord-route-spool.json");
        let mut legacy = sample_spool();
        legacy.schema_version = 2;
        legacy.entries[0].state = DiscordRouteSpoolState::Pending;
        write_discord_route_spool(&path, &legacy).unwrap();

        let migrated = DiscordRouteSpool::load(&path).unwrap();
        assert_eq!(migrated.schema_version, DISCORD_ROUTE_SPOOL_SCHEMA_VERSION);
        assert_eq!(migrated.entries[0].state, DiscordRouteSpoolState::PrePolicy);
        assert!(migrated.entries[0].journal.is_pre_policy());
        assert_eq!(migrated.recoverable_pre_policy_entries().len(), 1);
        assert!(
            migrated.recoverable_post_policy_inbound_entries().is_empty(),
            "legacy records must not bypass Discord thread/policy resolution"
        );
    }

    #[test]
    fn post_policy_retry_recovery_preserves_absolute_deadline() {
        let inbound = post_policy_inbound("retry-after-restart");
        let mut spool = DiscordRouteSpool::default();
        assert_eq!(spool.record_routable(&inbound), DiscordSpoolAdmission::Recorded);
        spool.set_processing("retry-after-restart");
        spool.set_retry_pending(
            "retry-after-restart",
            "pre-effect retry".to_string(),
            Some(123_456),
        );

        let recovered = spool.recoverable_post_policy_inbound_entries();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].0.message().message_id, "retry-after-restart");
        assert_eq!(recovered[0].1, Some(123_456));
        assert_eq!(spool.entries[0].state, DiscordRouteSpoolState::RetryPending);
        assert!(spool.recoverable_pre_policy_entries().is_empty());
    }

    #[test]
    fn crashed_processing_entry_becomes_durable_operator_quarantine() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("discord-route-spool.json");
        let mut crashed = sample_spool();
        crashed.entries[0].state = DiscordRouteSpoolState::Processing;
        write_discord_route_spool(&path, &crashed).unwrap();

        let mut recovered = DiscordRouteSpool::load(&path).unwrap();
        assert_eq!(
            recovered.entries[0].state,
            DiscordRouteSpoolState::Uncertain
        );
        assert!(
            recovered.entries[0]
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("interrupted by restart"))
        );
        assert_eq!(recovered.recoverable_pre_policy_entries().len(), 0);
        let duplicate = recovered.entries[0].journal.clone();
        assert_eq!(
            recovered.record_journal(&duplicate),
            DiscordSpoolAdmission::Existing
        );

        // Startup persists the quarantine before the adapter can consume it;
        // a second restart must not turn unknown side effects into replay.
        write_discord_route_spool(&path, &recovered).unwrap();
        let restarted = DiscordRouteSpool::load(&path).unwrap();
        assert_eq!(restarted.entries[0].state, DiscordRouteSpoolState::Uncertain);
        assert_eq!(restarted.recoverable_pre_policy_entries().len(), 0);
        assert_eq!(restarted.entries.len(), 1);
    }

    #[test]
    fn safe_drop_cannot_remove_route_owned_entry() {
        let mut spool = sample_spool();
        spool.entries[0].state = DiscordRouteSpoolState::Processing;

        assert!(!spool.remove_pre_policy("message-1"));
        assert_eq!(spool.entries.len(), 1);
        assert!(spool.remove_completed("message-1"));
        assert!(spool.entries.is_empty());
    }

    #[test]
    fn spool_rejects_unknown_schema_without_replaying() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("discord-route-spool.json");
        let mut spool = sample_spool();
        spool.schema_version += 1;
        write_discord_route_spool(&path, &spool).unwrap();

        let error = DiscordRouteSpool::load(&path).unwrap_err().to_string();
        assert!(error.contains("unsupported Discord route spool schema"));
    }

    #[test]
    fn failed_entries_consume_bounded_spool_capacity_without_eviction() {
        let template = sample_spool().entries.into_iter().next().unwrap();
        let mut spool = DiscordRouteSpool::default();
        for index in 0..DISCORD_ROUTE_SPOOL_MAX_ENTRIES {
            let mut message = template.journal.message().clone();
            message.message_id = format!("failed-{index}");
            let mut entry = DiscordRouteSpoolEntry::pre_policy(hermes_gateway::platforms::discord::DiscordJournalEntry::new(
                message,
                template
                    .journal
                    .client_user_id()
                    .map(str::to_string),
            ));
            entry.state = DiscordRouteSpoolState::Failed;
            assert_eq!(spool.try_push(entry), DiscordSpoolAdmission::Recorded);
        }

        let mut next = template.journal.message().clone();
        next.message_id = "must-not-evict".to_string();
        assert_eq!(
            spool.record_journal(
                &hermes_gateway::platforms::discord::DiscordJournalEntry::new(
                    next,
                    template
                        .journal
                        .client_user_id()
                        .map(str::to_string),
                ),
            ),
            DiscordSpoolAdmission::AtCapacity
        );
        assert_eq!(spool.entries.len(), DISCORD_ROUTE_SPOOL_MAX_ENTRIES);
        assert!(spool
            .entries
            .iter()
            .all(|entry| entry.state == DiscordRouteSpoolState::Failed));
    }
    #[test]
    fn quarantined_error_redacts_embedded_url_credentials() {
        let error = bounded_discord_route_error(
            &"provider failed at https://agent:secret@example.test/path",
        );
        assert_eq!(error, "[REDACTED_URL]");
    }

    #[test]
    fn partial_retry_deadline_releases_durably_queued_inbound_after_intake_closes() {
        assert!(
            discord_ingress_can_finish(
                true,
                true,
                true,
                false,
                false,
                true,
                true,
                true,
            ),
            "a persisted inbound queued behind a PartialPending Retry-After must not extend drain"
        );
    }

    #[test]
    fn partial_delivery_persists_fractional_provider_retry_hint() {
        let mut spool = sample_spool();
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        spool.set_partial_delivery(
            "message-1",
            hermes_core::errors::PartialDelivery {
                platform: "discord".to_string(),
                chat_id: "channel-1".to_string(),
                thread_id: None,
                reply_to_message_id: None,
                reply_to_mode: "none".to_string(),
                non_conversational: false,
                next_chunk_index: 1,
                remaining_text: "unsent suffix".to_string(),
                accepted_message_ids: vec!["accepted-message".to_string()],
                accepted_chunks: 1,
                retryable: true,
                failure: "rate limited".to_string(),
                retry_after: Some(std::time::Duration::from_millis(125)),
                repair_required: None,
            },
        );
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis();
        let entry = spool.entries.first().expect("sample spool entry");
        let deadline = entry
            .partial_retry_after_unix_ms
            .expect("persisted provider retry deadline");

        assert_eq!(entry.state, DiscordRouteSpoolState::PartialPending);
        assert!(
            deadline >= before.saturating_add(125),
            "a 125ms provider hint must never be floored to an early retry"
        );
        assert!(
            deadline <= after.saturating_add(126),
            "a 125ms provider hint must win over the multi-second fallback"
        );
    }

    #[tokio::test]
    async fn partial_retry_backoff_over_thirty_seconds_yields_to_intake_shutdown() {
        let directory = tempfile::tempdir().expect("temporary spool directory");

        let spool_path = directory.path().join("discord-route-spool.json");
        let mut spool = sample_spool();
        let retry_after_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_millis()
            + 60_000;
        let entry = spool.entries.first_mut().expect("sample spool entry");
        entry.state = DiscordRouteSpoolState::PartialPending;
        entry.partial_delivery = Some(hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "channel-1".to_string(),
            thread_id: None,
            reply_to_message_id: None,
            reply_to_mode: "none".to_string(),
            non_conversational: false,
            next_chunk_index: 1,
            remaining_text: "unsent suffix".to_string(),
            accepted_message_ids: vec!["accepted-message".to_string()],
            accepted_chunks: 1,
            retryable: true,
            failure: "rate limited".to_string(),
            retry_after: Some(std::time::Duration::from_secs(60)),
            repair_required: None,
        });
        entry.partial_attempts = 1;
        entry.partial_retry_after_unix_ms = Some(retry_after_unix_ms);
        write_discord_route_spool(&spool_path, &spool).expect("persist partial spool");

        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
        let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(1);
        drop(inbound_tx);
        drop(journal_tx);
        let (intake_done_tx, mut intake_done_rx) = tokio::sync::watch::channel(false);
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            DmManager::with_pair_behavior(),
            RuntimeGatewayConfig::default(),
        ));

        let task = tokio::spawn(run_discord_incoming_loop(
            gateway,
            inbound_rx,
            journal_rx,
            spool_path.clone(),
            spool,
            intake_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), intake_done_rx.changed())
            .await
            .expect("closed intake must not wait for the 60-second Retry-After")
            .expect("ingress owner must signal completion");
        assert!(*intake_done_rx.borrow());
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("ingress owner must exit")
            .expect("ingress owner task must not panic");

        let retained = DiscordRouteSpool::load(&spool_path).expect("read retained spool");
        let retained = retained.entries.first().expect("retained partial entry");
        assert_eq!(retained.state, DiscordRouteSpoolState::PartialPending);
        assert_eq!(retained.partial_retry_after_unix_ms, Some(retry_after_unix_ms));
        assert!(
            retained
                .partial_delivery
                .as_ref()
                .is_some_and(|partial| partial.retryable),
            "shutdown must leave the retryable suffix durably pending"
        );
    }

    #[test]
    fn repair_required_partial_preserves_exact_cursor_and_suffix_without_auto_replay() {
        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let partial = hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "channel-1".to_string(),
            thread_id: Some("thread-1".to_string()),
            reply_to_message_id: Some("request-1".to_string()),
            reply_to_mode: "reply".to_string(),
            non_conversational: false,
            next_chunk_index: 2,
            remaining_text: "exact unsent suffix after accepted chunks".to_string(),
            accepted_message_ids: vec!["accepted-1".to_string(), "accepted-2".to_string()],
            accepted_chunks: 2,
            retryable: true,
            retry_after: None,
            failure: "Discord accepted chunks but tracker persistence failed".to_string(),
            repair_required: Some(
                "repair Discord response tracker before replaying the retained suffix".to_string(),
            ),
        };
        let mut spool = sample_spool();
        spool.set_partial_delivery("message-1", partial.clone());
        let entry = spool.entries.first().expect("sample spool entry");
        assert_eq!(entry.state, DiscordRouteSpoolState::PartialFailed);
        assert_eq!(entry.partial_delivery.as_ref(), Some(&partial));
        assert!(
            spool.retryable_partial_entries().is_empty(),
            "a repair-required suffix must never be scheduled for automatic replay"
        );

        write_discord_route_spool(&spool_path, &spool).expect("persist repair-required partial");
        let restored = DiscordRouteSpool::load(&spool_path).expect("reload repair-required partial");
        let restored_partial = restored.entries[0]
            .partial_delivery
            .as_ref()
            .expect("partial delivery is durable");
        assert_eq!(restored_partial.next_chunk_index, 2);
        assert_eq!(
            restored_partial.remaining_text,
            "exact unsent suffix after accepted chunks"
        );
        assert_eq!(
            restored_partial.repair_required.as_deref(),
            Some("repair Discord response tracker before replaying the retained suffix")
        );
    }

    #[test]
    fn partial_merge_preserves_repair_requirement_and_rejects_conflicts() {
        let previous = hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "channel-1".to_string(),
            thread_id: None,
            reply_to_message_id: None,
            reply_to_mode: "none".to_string(),
            non_conversational: false,
            next_chunk_index: 1,
            remaining_text: "old suffix".to_string(),
            accepted_message_ids: vec!["accepted-1".to_string()],
            accepted_chunks: 1,
            retryable: true,
            retry_after: None,
            failure: "tracker state failed".to_string(),
            repair_required: Some("repair tracker".to_string()),
        };
        let next = hermes_core::errors::PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "channel-1".to_string(),
            thread_id: None,
            reply_to_message_id: None,
            reply_to_mode: "none".to_string(),
            non_conversational: false,
            next_chunk_index: 2,
            remaining_text: "new suffix".to_string(),
            accepted_message_ids: vec!["accepted-2".to_string()],
            accepted_chunks: 1,
            retryable: true,
            retry_after: None,
            failure: "later failure".to_string(),
            repair_required: None,
        };
        let merged = merge_discord_partial_delivery(&previous, next)
            .expect("prior repair requirement remains authoritative");
        assert_eq!(merged.accepted_chunks, 2);
        assert_eq!(
            merged.accepted_message_ids,
            vec!["accepted-1".to_string(), "accepted-2".to_string()]
        );
        assert_eq!(merged.repair_required.as_deref(), Some("repair tracker"));

        let conflicting = hermes_core::errors::PartialDelivery {
            repair_required: Some("different repair".to_string()),
            ..merged
        };
        let error = merge_discord_partial_delivery(&previous, conflicting)
            .expect_err("conflicting repair requirements must fail closed");
        assert!(
            error.to_string().contains("repair requirements conflict"),
            "operator-visible conflict must prevent automatic suffix replay"
        );
    }

    #[test]
    fn ingress_backpressure_delay_is_nonzero_and_bounded() {
        assert_eq!(
            bounded_discord_ingress_backpressure_delay(std::time::Duration::ZERO),
            DISCORD_INGRESS_BACKPRESSURE_MIN_DELAY
        );
        assert_eq!(
            bounded_discord_ingress_backpressure_delay(std::time::Duration::from_secs(30)),
            DISCORD_INGRESS_BACKPRESSURE_MAX_DELAY
        );
    }

    #[tokio::test]
    async fn saturated_gateway_retries_the_same_durable_discord_inbound_after_capacity_frees() {
        use futures::{SinkExt as _, StreamExt as _};
        use hermes_core::PlatformAdapter;
        use hermes_gateway::platforms::discord::{opcodes, GatewayPayload};
        use tokio::net::TcpListener;
        use tokio::time::{timeout, Duration};
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };

        type MockSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

        async fn accept(listener: &TcpListener) -> MockSocket {
            let (stream, _) = listener.accept().await.expect("mock gateway accepts TCP");
            accept_async(stream)
                .await
                .expect("mock gateway accepts WebSocket")
        }

        async fn send_payload(socket: &mut MockSocket, payload: GatewayPayload) {
            socket
                .send(Message::Text(
                    serde_json::to_string(&payload)
                        .expect("mock payload serializes")
                        .into(),
                ))
                .await
                .expect("mock gateway sends payload");
        }

        async fn receive_opcode(socket: &mut MockSocket, opcode: u8) {
            for _ in 0..8 {
                let frame = timeout(Duration::from_secs(2), socket.next())
                    .await
                    .expect("client emits a gateway payload")
                    .expect("client socket remains open")
                    .expect("client gateway frame succeeds");
                if let Message::Text(text) = frame {
                    let payload = serde_json::from_str::<GatewayPayload>(text.as_str())
                        .expect("client payload is JSON");
                    if payload.op == opcode {
                        return;
                    }
                }
            }
            panic!("client did not emit opcode {opcode}");
        }

        fn hello() -> GatewayPayload {
            GatewayPayload {
                op: opcodes::HELLO,
                d: Some(serde_json::json!({ "heartbeat_interval": 60_000 })),
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

        fn message_create(sequence: u64) -> GatewayPayload {
            GatewayPayload {
                op: opcodes::DISPATCH,
                d: Some(serde_json::json!({
                    "id": "backpressured-message",
                    "channel_id": "dm-channel",
                    "channel_type": 1,
                    "content": "durable payload",
                    "author": { "id": "human-1", "username": "Ada", "bot": false }
                })),
                s: Some(sequence),
                t: Some("MESSAGE_CREATE".to_string()),
            }
        }

        let api_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gateway/bot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/dm-channel/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "delivered",
                "channel_id": "dm-channel"
            })))
            .expect(1)
            .mount(&api_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/channels/capacity-dm/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "capacity-delivered",
                "channel_id": "capacity-dm"
            })))
            .expect(1)
            .mount(&api_server)
            .await;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock Discord gateway");
        let gateway_url = format!(
            "ws://{}",
            listener.local_addr().expect("mock gateway address")
        );
        let platform_cfg = hermes_config::PlatformConfig::default();
        let mut discord_config =
            build_discord_config(&platform_cfg, None, "test-token".to_string());
        discord_config.gateway_url = Some(gateway_url.clone());
        discord_config.api_base_url = Some(api_server.uri());
        discord_config.liveness_interval_seconds = 0.0;
        discord_config.require_mention = false;
        discord_config.bot_policy =
            hermes_gateway::platforms::discord::DiscordBotMessagePolicy::None;
        let adapter = Arc::new(DiscordAdapter::new(discord_config).expect("Discord adapter"));

        let mut gateway_config = RuntimeGatewayConfig::default();
        gateway_config.resource_limits_enabled = true;
        gateway_config.ingress_worker_limit = Some(1);
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("capacity-holder");
        dm_manager.authorize_user("human-1");
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            dm_manager,
            gateway_config,
        ));
        gateway
            .register_adapter("discord", adapter.clone())
            .await;

        let (first_entered_tx, first_entered_rx) = tokio::sync::oneshot::channel();
        let first_entered_tx = Arc::new(Mutex::new(Some(first_entered_tx)));
        let (release_capacity_tx, release_capacity_rx) = tokio::sync::watch::channel(false);
        let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::clone(&handler_calls);
        let entered = Arc::clone(&first_entered_tx);
        gateway
            .set_message_handler(Arc::new(move |_| {
                let calls = Arc::clone(&calls);
                let entered = Arc::clone(&entered);
                let mut release_capacity_rx = release_capacity_rx.clone();
                Box::pin(async move {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        if let Some(tx) = entered
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send(());
                        }
                        release_capacity_rx
                            .changed()
                            .await
                            .expect("capacity release sender remains live");
                    }
                    Ok("handled".to_string())
                })
            }))
            .await;

        let occupancy_gateway = Arc::clone(&gateway);
        let occupancy = tokio::spawn(async move {
            occupancy_gateway
                .route_message(&hermes_gateway::gateway::IncomingMessage {
                    platform: "discord".to_string(),
                    chat_id: "capacity-dm".to_string(),
                    user_id: "capacity-holder".to_string(),
                    text: "occupy ingress".to_string(),
                    message_id: Some("capacity-holder-message".to_string()),
                    thread_id: None,
                    is_dm: true,
                })
                .await
        });
        timeout(Duration::from_secs(1), first_entered_rx)
            .await
            .expect("first handler holds the only ingress worker")
            .expect("first handler enters");

        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(4);
        let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(4);
        adapter
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;
        let (intake_done_tx, mut intake_done_rx) = tokio::sync::watch::channel(false);
        let loop_task = tokio::spawn(run_discord_incoming_loop(
            Arc::clone(&gateway),
            inbound_rx,
            journal_rx,
            spool_path.clone(),
            DiscordRouteSpool::default(),
            intake_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
        ));

        let server = tokio::spawn(async move {
            let mut socket = accept(&listener).await;
            send_payload(&mut socket, hello()).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(10, &gateway_url)).await;
            send_payload(&mut socket, message_create(11)).await;
            let _ = timeout(Duration::from_secs(2), socket.next()).await;
        });
        PlatformAdapter::start(adapter.as_ref())
            .await
            .expect("start Discord adapter");

        let retained = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(spool) = DiscordRouteSpool::load(&spool_path) {
                    if let Some(entry) = spool
                        .entries
                        .iter()
                        .find(|entry| entry.message_id() == "backpressured-message")
                    {
                        if entry.state == DiscordRouteSpoolState::RetryPending
                            && entry
                                .last_error
                                .as_deref()
                                .is_some_and(|error| error.contains("gateway ingress capacity"))
                        {
                            return spool;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("saturated ingress becomes a durable unresolved retry");
        assert_eq!(retained.entries.len(), 1);
        assert_eq!(
            retained.entries[0].message_id(),
            "backpressured-message",
            "capacity refusal must retain the exact durable entry"
        );
        assert_eq!(
            handler_calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a full gateway must reject before the second handler begins"
        );

        release_capacity_tx.send_replace(true);
        timeout(Duration::from_secs(2), async {
            while handler_calls.load(std::sync::atomic::Ordering::SeqCst) < 2 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the same queued inbound retries after capacity frees");
        occupancy
            .await
            .expect("capacity holder task joins")
            .expect("capacity holder route succeeds");

        timeout(Duration::from_secs(2), async {
            loop {
                if DiscordRouteSpool::load(&spool_path)
                    .is_ok_and(|spool| spool.entries.is_empty())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("successful same-process retry removes the durable entry");
        api_server
            .verify()
            .await;

        adapter
            .quiesce_inbound()
            .await
            .expect("quiesce Discord ingress before awaiting its completion");
        PlatformAdapter::stop(adapter.as_ref())
            .await
            .expect("stop Discord adapter");
        drop(adapter);
        timeout(Duration::from_secs(2), server)
            .await
            .expect("mock Discord gateway exits")
            .expect("mock Discord gateway task succeeds");
        timeout(Duration::from_secs(2), intake_done_rx.changed())
            .await
            .expect("closed ingress sends completion")
            .expect("ingress completion sender remains live");
        assert!(*intake_done_rx.borrow());
        timeout(Duration::from_secs(2), loop_task)
            .await
            .expect("ingress loop exits")
            .expect("ingress loop task succeeds");
        gateway.stop_all().await.expect("stop gateway");
    }

    #[tokio::test]
    async fn retryable_head_retries_before_later_durable_inbound() {
        use hermes_gateway::platforms::discord::DiscordInboundMessage;
        use tokio::time::{timeout, Duration};

        fn inbound(message_id: &str) -> DiscordInboundMessage {
            post_policy_inbound(message_id)
        }

        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(4);
        let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(1);
        let (intake_done_tx, mut intake_done_rx) = tokio::sync::watch::channel(false);
        let (first_retry_started_tx, first_retry_started_rx) = tokio::sync::oneshot::channel();
        let first_retry_started_tx =
            Arc::new(Mutex::new(Some(first_retry_started_tx)));
        let (release_retry_tx, release_retry_rx) = tokio::sync::oneshot::channel();
        let release_retry_rx = Arc::new(Mutex::new(Some(release_retry_rx)));
        let attempts = Arc::new(Mutex::new(Vec::<String>::new()));
        let attempts_for_router = Arc::clone(&attempts);
        let first_retry_for_router = Arc::clone(&first_retry_started_tx);
        let release_retry_for_router = Arc::clone(&release_retry_rx);
        let a_attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let a_attempts_for_router = Arc::clone(&a_attempts);
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            DmManager::with_pair_behavior(),
            RuntimeGatewayConfig::default(),
        ));

        let loop_task = tokio::spawn(run_discord_incoming_loop_with_router(
            gateway,
            inbound_rx,
            journal_rx,
            spool_path.clone(),
            DiscordRouteSpool::default(),
            intake_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
            move |_, inbound| {
                let attempts = Arc::clone(&attempts_for_router);
                let first_retry_started = Arc::clone(&first_retry_for_router);
                let release_retry = Arc::clone(&release_retry_for_router);
                let a_attempts = Arc::clone(&a_attempts_for_router);
                async move {
                    let message_id = inbound.message().message_id.clone();
                    attempts
                        .lock()
                        .expect("attempt tracker lock")
                        .push(message_id.clone());
                    if message_id == "retry-A"
                        && a_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                    {
                        if let Some(tx) = first_retry_started
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send(());
                        }
                        let release = release_retry
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                            .expect("first retry route owns its release gate");
                        release
                            .await
                            .expect("first retry release sender remains live");
                        Ok(hermes_gateway::GatewayRouteOutcome::Retryable {
                            error: hermes_core::errors::GatewayError::Platform(
                                "test pre-route retry".to_string(),
                            ),
                            retry_after: Some(Duration::from_millis(250)),
                        })
                    } else {
                        Ok(hermes_gateway::GatewayRouteOutcome::Committed)
                    }
                }
            },
        ));

        inbound_tx
            .send(inbound("retry-A"))
            .await
            .expect("send first durable inbound");
        timeout(Duration::from_secs(1), first_retry_started_rx)
            .await
            .expect("first inbound reaches the typed retry boundary")
            .expect("first retry boundary remains live");
        inbound_tx
            .send(inbound("later-B"))
            .await
            .expect("send later durable inbound");

        timeout(Duration::from_secs(1), async {
            loop {
                if DiscordRouteSpool::load(&spool_path).is_ok_and(|spool| {
                    spool.entries.len() == 2
                        && spool
                            .entries
                            .iter()
                            .filter(|entry| entry.message_id() == "retry-A")
                            .count()
                            == 1
                        && spool.entries.iter().any(|entry| {
                            entry.message_id() == "later-B"
                                && entry.state == DiscordRouteSpoolState::RetryPending
                        })
                }) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("both inbound messages are durably recorded exactly once before retry");

        release_retry_tx
            .send(())
            .expect("release typed retry result");
        timeout(Duration::from_secs(1), async {
            loop {
                if DiscordRouteSpool::load(&spool_path).is_ok_and(|spool| {
                    spool.entries.len() == 2
                        && spool.entries.iter().any(|entry| {
                            entry.message_id() == "retry-A"
                                && entry.state == DiscordRouteSpoolState::RetryPending
                                && entry.retry_not_before_unix_ms.is_some()
                        })
                        && spool.entries.iter().any(|entry| {
                            entry.message_id() == "later-B"
                                && entry.state == DiscordRouteSpoolState::RetryPending
                        })
                }) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("retryable head persists an absolute retry deadline while B remains behind it");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            attempts.lock().expect("attempt tracker lock").len(),
            1,
            "the provider's 250ms retry delay must not be shortened before retry"
        );

        timeout(Duration::from_secs(2), async {
            loop {
                if attempts.lock().expect("attempt tracker lock").len() == 3 {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("head retry and later inbound both complete");
        assert_eq!(
            attempts.lock().expect("attempt tracker lock").as_slice(),
            &[
                "retry-A".to_string(),
                "retry-A".to_string(),
                "later-B".to_string(),
            ],
            "a typed retry must retain FIFO ownership ahead of later durable inbound"
        );

        timeout(Duration::from_secs(1), async {
            loop {
                if DiscordRouteSpool::load(&spool_path)
                    .is_ok_and(|spool| spool.entries.is_empty())
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("both durably committed inbound records are removed once");
        drop(inbound_tx);
        drop(journal_tx);
        timeout(Duration::from_secs(1), intake_done_rx.changed())
            .await
            .expect("closed ingress signals completion")
            .expect("ingress completion sender remains live");
        timeout(Duration::from_secs(1), loop_task)
            .await
            .expect("ingress loop exits")
            .expect("ingress loop task succeeds");
    }

    #[tokio::test]
    async fn gateway_preparer_rate_limit_persists_full_post_policy_retry_before_dispatch() {
        use tokio::time::{timeout, Duration};

        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            DmManager::with_pair_behavior(),
            RuntimeGatewayConfig::default(),
        ));
        let handler_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = Arc::clone(&handler_calls);
        gateway
            .set_message_handler(Arc::new(move |_| {
                let calls = Arc::clone(&calls);
                Box::pin(async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("must not run before accepted-ingress preparation succeeds".to_string())
                })
            }))
            .await;

        let provider_retry_after = Duration::from_millis(250);
        gateway
            .register_accepted_ingress_preparer(
                "discord",
                Arc::new(move |_, _| {
                    Box::pin(async move {
                        Err(hermes_gateway::gateway::AcceptedIngressPreparationError::retryable(
                            hermes_core::errors::GatewayError::RateLimited {
                                retry_after: Some(provider_retry_after),
                            },
                        ))
                    })
                }),
            )
            .await;

        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
        let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(1);
        let (intake_done_tx, _intake_done_rx) = tokio::sync::watch::channel(false);
        let loop_task = tokio::spawn(run_discord_incoming_loop(
            Arc::clone(&gateway),
            inbound_rx,
            journal_rx,
            spool_path.clone(),
            DiscordRouteSpool::default(),
            intake_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
        ));

        let before_retry = discord_unix_ms_now();
        inbound_tx
            .send(post_policy_inbound("preparer-rate-limit"))
            .await
            .expect("send post-policy Discord inbound to CLI dispatcher");
        let retained = timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(spool) = DiscordRouteSpool::load(&spool_path) {
                    if spool.entries.len() == 1
                        && spool.entries[0].message_id() == "preparer-rate-limit"
                        && spool.entries[0].state == DiscordRouteSpoolState::RetryPending
                        && spool.entries[0].retry_not_before_unix_ms.is_some()
                    {
                        return spool;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("pre-handler rate limit becomes a durable CLI retry");
        let entry = &retained.entries[0];
        assert!(entry.journal.is_post_policy());
        assert!(
            entry
                .retry_not_before_unix_ms
                .expect("absolute retry deadline")
                >= before_retry.saturating_add(provider_retry_after.as_millis()),
            "the CLI spool must retain the full provider retry-after without early replay"
        );
        let recovered = retained.recoverable_post_policy_inbound_entries();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].0.message().message_id,
            "preparer-rate-limit",
            "the complete post-policy inbound must remain recoverable"
        );
        assert_eq!(
            handler_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "producer retry classification must prevent handler/model/tool dispatch"
        );

        drop(inbound_tx);
        drop(journal_tx);
        loop_task.abort();
        assert!(loop_task.await.is_err(), "test abort joins retry sleep task");
        gateway.stop_all().await.expect("stop producer gateway");
    }

    #[tokio::test]
    async fn post_handler_shutdown_quarantines_uncertain_without_replay() {
        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let session_manager = Arc::new(SessionManager::new(
            hermes_config::session::SessionConfig::default(),
        ));
        let mut dm_manager = DmManager::with_pair_behavior();
        dm_manager.authorize_user("user-1");
        let gateway = Arc::new(Gateway::new(
            session_manager,
            dm_manager,
            RuntimeGatewayConfig::default(),
        ));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_calls = Arc::clone(&calls);
        let handler_entered = Arc::clone(&entered_tx);
        gateway
            .set_message_handler(Arc::new(move |_| {
                let calls = Arc::clone(&handler_calls);
                let entered = Arc::clone(&handler_entered);
                Box::pin(async move {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                        if let Some(tx) = entered
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        {
                            let _ = tx.send(());
                        }
                        std::future::pending::<()>().await;
                    }
                    panic!("a handler-started ingress must never replay after shutdown");
                })
            }))
            .await;

        let mut first_spool = sample_spool();
        first_spool.entries[0].state = DiscordRouteSpoolState::RetryPending;
        let mut message = first_spool.entries[0].journal.message().clone();
        message.is_dm = true;
        message.guild_id = None;
        first_spool.entries[0].journal = post_policy_journal(message);
        let (inbound_tx, inbound_rx) = tokio::sync::mpsc::channel(1);
        let (journal_tx, journal_rx) = tokio::sync::mpsc::channel(1);
        drop(inbound_tx);
        drop(journal_tx);
        let (intake_done_tx, _intake_done_rx) = tokio::sync::watch::channel(false);
        let first_loop = tokio::spawn(run_discord_incoming_loop(
            Arc::clone(&gateway),
            inbound_rx,
            journal_rx,
            spool_path.clone(),
            first_spool,
            intake_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
        ));
        entered_rx.await.expect("first gateway turn enters handler");
        gateway.stop_all().await.expect("gateway shutdown");
        tokio::time::timeout(std::time::Duration::from_secs(1), first_loop)
            .await
            .expect("first ingress loop exits after uncertain shutdown")
            .expect("first ingress loop must not panic");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a handler-started ingress must not re-dispatch while stopping"
        );
        let retained = DiscordRouteSpool::load(&spool_path).expect("read retained ingress");
        assert_eq!(retained.entries.len(), 1);
        assert_eq!(retained.entries[0].state, DiscordRouteSpoolState::Uncertain);

        assert!(
            retained.recoverable_pre_policy_entries().is_empty()
                && retained.recoverable_post_policy_inbound_entries().is_empty(),
            "post-handler cancellation must be quarantined rather than eligible for replay"
        );

        gateway.start_all().await.expect("gateway restart");
        let (replay_inbound_tx, replay_inbound_rx) = tokio::sync::mpsc::channel(1);
        let (replay_journal_tx, replay_journal_rx) = tokio::sync::mpsc::channel(1);
        drop(replay_inbound_tx);
        drop(replay_journal_tx);
        let (replay_done_tx, _replay_done_rx) = tokio::sync::watch::channel(false);
        let replay_loop = tokio::spawn(run_discord_incoming_loop(
            Arc::clone(&gateway),
            replay_inbound_rx,
            replay_journal_rx,
            spool_path.clone(),
            retained,
            replay_done_tx,
            Arc::new(DiscordIngressRecovery::default()),
            Arc::new(DiscordSpoolWriteFence::default()),
        ));
        tokio::time::timeout(std::time::Duration::from_secs(1), replay_loop)
            .await
            .expect("quarantined ingress loop exits")
            .expect("quarantined ingress loop must not panic");
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "post-handler cancellation must never invoke the handler again after restart"
        );
        let quarantined = DiscordRouteSpool::load(&spool_path).expect("read quarantined ingress");
        assert_eq!(quarantined.entries.len(), 1);
        assert_eq!(
            quarantined.entries[0].state,
            DiscordRouteSpoolState::Uncertain
        );
        assert!(
            quarantined.recoverable_pre_policy_entries().is_empty()
                && quarantined.recoverable_post_policy_inbound_entries().is_empty(),
            "restart must preserve the uncertain quarantine without replay"
        );
        gateway.stop_all().await.expect("stop restarted gateway");
    }

    #[test]
    fn retryable_full_inbound_returns_to_durable_post_policy_retry_state() {
        let inbound = post_policy_inbound("message-1");
        let mut spool = DiscordRouteSpool::default();
        assert_eq!(spool.record_routable(&inbound), DiscordSpoolAdmission::Recorded);
        spool.set_processing("message-1");
        spool.set_retry_pending(
            "message-1",
            "pre-delivery transport failure".to_string(),
            Some(99),
        );

        let entry = spool.entries.first().expect("sample spool entry");
        assert_eq!(entry.state, DiscordRouteSpoolState::RetryPending);
        assert_eq!(
            entry.last_error.as_deref(),
            Some("pre-delivery transport failure")
        );
        assert_eq!(entry.retry_not_before_unix_ms, Some(99));
        assert!(entry.partial_delivery.is_none());
        assert!(entry.partial_retry_after_unix_ms.is_none());
    }
    #[test]
    fn recovery_buffer_retains_precommit_ingress_until_shutdown_export() {
        let entry = sample_spool()
            .entries
            .into_iter()
            .next()
            .expect("sample spool entry")
            .journal;
        let recovery = DiscordIngressRecovery::default();
        recovery.retain(entry.clone());
        recovery.retain(entry);

        let retained = recovery.snapshot();
        assert_eq!(retained.len(), 1, "recovery buffer must de-duplicate message IDs");
        assert_eq!(retained[0].message().message_id, "message-1");
        assert_eq!(
            recovery.snapshot().len(),
            1,
            "snapshot must retain ownership until a durable export succeeds"
        );
        recovery.release_entries(&retained);
        assert!(
            recovery.snapshot().is_empty(),
            "only a successful export acknowledgement may clear recovery"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn durable_spool_writes_do_not_block_ingress_runtime() {
        let write = run_discord_spool_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            Ok::<(), AgentError>(())
        });
        tokio::pin!(write);

        tokio::select! {
            result = &mut write => panic!("blocking spool write completed too soon: {result:?}"),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }
        write.await.expect("blocking spool write must complete");
    }

    #[tokio::test]
    async fn fenced_pre_route_processing_write_replays_after_forced_shutdown() {
        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let journal = sample_spool()
            .entries
            .into_iter()
            .next()
            .expect("sample spool entry")
            .journal;
        let initial = DiscordRouteSpool {
            schema_version: DISCORD_ROUTE_SPOOL_SCHEMA_VERSION,
            entries: vec![DiscordRouteSpoolEntry::pre_policy(journal.clone())],
        };
        write_discord_route_spool(&spool_path, &initial).expect("persist pre-route journal");

        let mut processing = initial.clone();
        processing.set_processing(journal.message().message_id.as_str());
        let fence = Arc::new(DiscordSpoolWriteFence::default());
        let (writer_started_tx, writer_started_rx) = tokio::sync::oneshot::channel();
        let writer_started_tx = Arc::new(Mutex::new(Some(writer_started_tx)));
        let (release_writer_tx, release_writer_rx) = std::sync::mpsc::channel::<()>();
        let release_writer_rx = Arc::new(Mutex::new(release_writer_rx));
        fence.set_before_replace_hook(Arc::new(move || {
            if let Some(tx) = writer_started_tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                let _ = tx.send(());
            }
            release_writer_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .expect("test releases blocked pre-replace writer");
        }));

        let writer_path = spool_path.clone();
        let writer_fence = Arc::clone(&fence);
        let writer = tokio::spawn(async move {
            let generation = writer_fence
                .capture()
                .expect("writer begins before forced shutdown");
            run_discord_spool_blocking(move || {
                write_discord_route_spool_if_current(
                    &writer_path,
                    &processing,
                    &writer_fence,
                    generation,
                )
            })
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(1), writer_started_rx)
            .await
            .expect("transition writer blocks before replace")
            .expect("writer hook remains live");

        let recovery = Arc::new(DiscordIngressRecovery::default());
        recovery.retain(journal.clone());
        let close_fence = Arc::clone(&fence);
        let mut close = tokio::spawn(async move {
            close_discord_spool_write_fence(&close_fence).await
        });
        tokio::select! {
            result = &mut close => panic!(
                "shutdown fence must wait for a writer that already passed its in-gate epoch check: {result:?}"
            ),
            _ = tokio::time::sleep(std::time::Duration::from_millis(20)) => {}
        }

        release_writer_tx
            .send(())
            .expect("release writer that holds the commit gate");
        close
            .await
            .expect("shutdown fence task joins")
            .expect("shutdown fence closes after the in-gate writer");
        assert_eq!(
            writer
                .await
                .expect("writer task joins")
                .expect("writer task result"),
            DiscordSpoolWriteOutcome::Written,
            "a writer that passed the in-gate check may finish before shutdown closes the fence"
        );

        let platform_cfg = hermes_config::PlatformConfig::default();
        let adapter = DiscordAdapter::new(build_discord_config(
            &platform_cfg,
            None,
            "test-token".to_string(),
        ))
        .expect("Discord adapter");
        export_discord_ingress_recovery(&adapter, &recovery, &spool_path, true)
            .await
            .expect("forced shutdown must make pre-route recovery authoritative");

        let restarted = DiscordRouteSpool::load(&spool_path).expect("load restart spool");
        assert_eq!(restarted.entries.len(), 1);
        assert_eq!(
            restarted.entries[0].state,
            DiscordRouteSpoolState::PrePolicy,
            "the accepted raw pre-policy journal must override a stale Processing marker"
        );
        assert_eq!(
            restarted
                .recoverable_pre_policy_entries()
                .into_iter()
                .map(|entry| entry.message().message_id.clone())
                .collect::<Vec<_>>(),
            vec!["message-1".to_string()],
            "restart must re-resolve the raw pre-policy journal rather than route it directly"
        );
        assert!(
            recovery.snapshot().is_empty(),
            "recovery is released only after its unresolved precedence record is durable"
        );
    }

    #[tokio::test]
    async fn timed_out_cli_drain_quarantines_runtime_recovery_before_acknowledgement() {
        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let platform_cfg = hermes_config::PlatformConfig::default();
        let adapter = Arc::new(
            DiscordAdapter::new(build_discord_config(&platform_cfg, None, "test-token".into()))
                .expect("Discord adapter"),
        );
        let journal = sample_spool()
            .entries
            .into_iter()
            .next()
            .expect("sample spool entry")
            .journal;
        adapter
            .queue_recovered_pre_policy_ingress(vec![journal])
            .expect("queue raw runtime recovery");
        let (_intake_done_tx, intake_done) = tokio::sync::watch::channel(false);
        let (cleanup_tx, cleanup_supervisor, cleanup_supervisor_done) =
            start_discord_ingress_cleanup_supervisor();
        let shutdown = DiscordIngressShutdown {
            adapter: Arc::clone(&adapter),
            task: Some(tokio::spawn(std::future::pending())),
            intake_done,
            spool_path: spool_path.clone(),
            recovery: Arc::new(DiscordIngressRecovery::default()),
            spool_write_fence: Arc::new(DiscordSpoolWriteFence::default()),
            cleanup_tx: Some(cleanup_tx),
            cleanup_supervisor_done: Some(cleanup_supervisor_done),
        };

        let error = shutdown
            .drain_with_timeout(std::time::Duration::from_millis(20))
            .await
            .expect_err("blocked ingress owner must time out");
        assert!(
            error.to_string().contains("recovered ingress was quarantined"),
            "timeout must report the durable quarantine outcome: {error}"
        );
        let quarantine_dir = spool_path
            .parent()
            .expect("spool path parent")
            .join("discord-route-spool-quarantine");
        let quarantine_paths = std::fs::read_dir(&quarantine_dir)
            .expect("quarantine directory")
            .map(|entry| entry.expect("quarantine entry").path())
            .collect::<Vec<_>>();
        assert_eq!(quarantine_paths.len(), 1, "exactly one recovery spool");
        let quarantine =
            DiscordRouteSpool::load(&quarantine_paths[0]).expect("read recovery spool");
        assert_eq!(quarantine.entries.len(), 1);
        assert_eq!(quarantine.entries[0].message_id(), "message-1");
        assert!(
            adapter.snapshot_recovered_ingress_for_cli().is_empty(),
            "runtime recovery may be acknowledged only after the quarantine fsync succeeds"
        );
        cleanup_supervisor
            .await
            .expect("cleanup supervisor exits after the timed-out drain handoff");
    }

    #[tokio::test]
    async fn cancelling_drain_continues_cleanup_and_durably_restores_recovery() {
        use std::{
            future::Future as _,
            task::{Context, Poll},
        };

        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let recovery = Arc::new(DiscordIngressRecovery::default());
        let journal = sample_spool()
            .entries
            .into_iter()
            .next()
            .expect("sample raw recovery entry")
            .journal;
        recovery.retain(journal);
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            DmManager::with_pair_behavior(),
            RuntimeGatewayConfig::default(),
        ));
        let weak_gateway = Arc::downgrade(&gateway);
        let (task_started_tx, task_started_rx) = tokio::sync::oneshot::channel();
        let (intake_done_tx, intake_done) = tokio::sync::watch::channel(false);
        let task_gateway = Arc::clone(&gateway);
        let ingress_task = tokio::spawn(async move {
            let _gateway = task_gateway;
            let _intake_done_tx = intake_done_tx;
            task_started_tx
                .send(())
                .expect("ingress task startup observer remains live");
            std::future::pending::<()>().await;
        });
        task_started_rx
            .await
            .expect("ingress task holds the final Gateway Arc");
        drop(gateway);
        let (cleanup_tx, cleanup_supervisor, cleanup_supervisor_done) =
            start_discord_ingress_cleanup_supervisor();

        let platform_cfg = hermes_config::PlatformConfig::default();
        let shutdown = DiscordIngressShutdown {
            adapter: Arc::new(
                DiscordAdapter::new(build_discord_config(
                    &platform_cfg,
                    None,
                    "test-token".to_string(),
                ))
                .expect("Discord adapter"),
            ),
            task: Some(ingress_task),
            intake_done,
            spool_path: spool_path.clone(),
            recovery: Arc::clone(&recovery),
            spool_write_fence: Arc::new(DiscordSpoolWriteFence::default()),
            cleanup_tx: Some(cleanup_tx),
            cleanup_supervisor_done: Some(cleanup_supervisor_done),
        };
        drop(recovery);

        let (first_poll_tx, first_poll_rx) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            let mut drain =
                Box::pin(shutdown.drain_with_timeout(std::time::Duration::from_secs(30)));
            let pending = {
                let waker = futures::task::noop_waker();
                let mut context = Context::from_waker(&waker);
                matches!(drain.as_mut().poll(&mut context), Poll::Pending)
            };
            first_poll_tx
                .send(pending)
                .expect("caller observes the drain's first suspension");
            std::future::pending::<()>().await;
        });
        assert!(
            first_poll_rx
                .await
                .expect("drain caller reaches its first await"),
            "the drain must be interruptible while waiting for durable intake"
        );
        caller.abort();
        assert!(
            caller.await.is_err(),
            "the outer caller task is cancelled after the first drain suspension"
        );

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while weak_gateway.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect(
            "cancelling the outer drain must abort the owned ingress task instead of detaching it",
        );
        let quarantine_dir = directory
            .path()
            .join("discord-route-spool-quarantine");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let restored = DiscordRouteSpool::load(&spool_path)
                    .expect("read cancellation-restored spool");
                if restored.entries.len() == 1
                    && restored.entries[0].message_id() == "message-1"
                    && restored.entries[0].state == DiscordRouteSpoolState::PrePolicy
                    && quarantine_dir.exists()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled drain cleanup must quarantine and restore retained raw ingress");
        let quarantine_entries = std::fs::read_dir(&quarantine_dir)
            .expect("cancellation cleanup quarantine directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("read cancellation cleanup quarantine directory");
        assert_eq!(quarantine_entries.len(), 1);
        let quarantined = DiscordRouteSpool::load(&quarantine_entries[0].path())
            .expect("read durable cancellation quarantine");
        assert_eq!(quarantined.entries.len(), 1);
        assert_eq!(quarantined.entries[0].message_id(), "message-1");
        cleanup_supervisor
            .await
            .expect("cleanup supervisor exits after the cancelled drain handoff");
    }

    #[tokio::test]
    async fn dropping_unpolled_drain_owner_still_aborts_and_exports_recovery() {
        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let recovery = Arc::new(DiscordIngressRecovery::default());
        let journal = sample_spool()
            .entries
            .into_iter()
            .next()
            .expect("sample raw recovery entry")
            .journal;
        recovery.retain(journal);
        let gateway = Arc::new(Gateway::new(
            Arc::new(SessionManager::new(
                hermes_config::session::SessionConfig::default(),
            )),
            DmManager::with_pair_behavior(),
            RuntimeGatewayConfig::default(),
        ));
        let weak_gateway = Arc::downgrade(&gateway);
        let (task_started_tx, task_started_rx) = tokio::sync::oneshot::channel();
        let (intake_done_tx, intake_done) = tokio::sync::watch::channel(false);
        let task_gateway = Arc::clone(&gateway);
        let ingress_task = tokio::spawn(async move {
            let _gateway = task_gateway;
            let _intake_done_tx = intake_done_tx;
            task_started_tx
                .send(())
                .expect("ingress task startup observer remains live");
            std::future::pending::<()>().await;
        });
        task_started_rx
            .await
            .expect("ingress task holds the final Gateway Arc");
        drop(gateway);
        let (cleanup_tx, cleanup_supervisor, cleanup_supervisor_done) =
            start_discord_ingress_cleanup_supervisor();

        let platform_cfg = hermes_config::PlatformConfig::default();
        let shutdown = DiscordIngressShutdown {
            adapter: Arc::new(
                DiscordAdapter::new(build_discord_config(
                    &platform_cfg,
                    None,
                    "test-token".to_string(),
                ))
                .expect("Discord adapter"),
            ),
            task: Some(ingress_task),
            intake_done,
            spool_path: spool_path.clone(),
            recovery: Arc::clone(&recovery),
            spool_write_fence: Arc::new(DiscordSpoolWriteFence::default()),
            cleanup_tx: Some(cleanup_tx),
            cleanup_supervisor_done: Some(cleanup_supervisor_done),
        };
        drop(recovery);

        // Never construct or poll `drain_with_timeout`: the shutdown owner
        // itself must transfer task ownership to the durable cleanup worker.
        drop(shutdown);

        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while weak_gateway.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("unpolled shutdown drop must abort the ingress task");
        let quarantine_dir = directory
            .path()
            .join("discord-route-spool-quarantine");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let restored = DiscordRouteSpool::load(&spool_path)
                    .expect("read unpolled-drop restored spool");
                if restored.entries.len() == 1
                    && restored.entries[0].message_id() == "message-1"
                    && restored.entries[0].state == DiscordRouteSpoolState::PrePolicy
                    && quarantine_dir.exists()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("unpolled shutdown drop must durably export recovery");
        cleanup_supervisor
            .await
            .expect("cleanup supervisor exits after the unpolled-drop handoff");
    }

    #[tokio::test]
    async fn durable_spool_fsync_gates_resume_sequence_and_replays_after_restart() {
        use futures::{SinkExt as _, StreamExt as _};
        use hermes_core::PlatformAdapter;
        use hermes_gateway::platforms::discord::{
            opcodes, DiscordIngressJournalEvent, DiscordIngressReservationOutcome, GatewayPayload,
        };
        use tokio::net::TcpListener;
        use tokio::time::{timeout, Duration};
        use tokio_tungstenite::{accept_async, tungstenite::Message};
        use wiremock::{
            matchers::{method, path},
            Mock, MockServer, ResponseTemplate,
        };

        type MockSocket = tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>;

        async fn accept(listener: &TcpListener) -> MockSocket {
            let (stream, _) = listener.accept().await.expect("mock gateway accepts TCP");
            accept_async(stream)
                .await
                .expect("mock gateway accepts WebSocket")
        }

        async fn send_payload(socket: &mut MockSocket, payload: GatewayPayload) {
            socket
                .send(Message::Text(
                    serde_json::to_string(&payload)
                        .expect("mock payload serializes")
                        .into(),
                ))
                .await
                .expect("mock gateway sends payload");
        }

        async fn receive_opcode(socket: &mut MockSocket, opcode: u8) -> GatewayPayload {
            for _ in 0..8 {
                let frame = timeout(Duration::from_secs(2), socket.next())
                    .await
                    .expect("client emits a gateway payload")
                    .expect("client socket remains open")
                    .expect("client gateway frame succeeds");
                if let Message::Text(text) = frame {
                    let payload =
                        serde_json::from_str::<GatewayPayload>(text.as_str())
                            .expect("client payload is JSON");
                    if payload.op == opcode {
                        return payload;
                    }
                }
            }
            panic!("client did not emit opcode {opcode}");
        }

        fn hello() -> GatewayPayload {
            GatewayPayload {
                op: opcodes::HELLO,
                d: Some(serde_json::json!({ "heartbeat_interval": 60_000 })),
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

        fn message_create(sequence: u64) -> GatewayPayload {
            GatewayPayload {
                op: opcodes::DISPATCH,
                d: Some(serde_json::json!({
                    "id": "durable-message",
                    "channel_id": "policy-channel",
                    "guild_id": "guild-1",
                    "content": "durable payload",
                    "author": { "id": "human-1", "username": "Ada", "bot": false }
                })),
                s: Some(sequence),
                t: Some("MESSAGE_CREATE".to_string()),
            }
        }

        fn adapter_for(
            gateway_url: String,
            api_base_url: String,
            ignored_channel: Option<&str>,
        ) -> Arc<DiscordAdapter> {
            let platform_cfg = hermes_config::PlatformConfig::default();
            let mut config = build_discord_config(&platform_cfg, None, "test-token".to_string());
            config.gateway_url = Some(gateway_url);
            config.api_base_url = Some(api_base_url);
            config.liveness_interval_seconds = 0.0;
            config.require_mention = false;
            config.bot_policy =
                hermes_gateway::platforms::discord::DiscordBotMessagePolicy::None;
            if let Some(channel_id) = ignored_channel {
                config
                    .channel_controls
                    .ignored_channels
                    .insert(channel_id.to_string());
            }
            Arc::new(DiscordAdapter::new(config).expect("Discord adapter"))
        }

        let api_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/gateway/bot"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&api_server)
            .await;

        let directory = tempfile::tempdir().expect("temporary spool directory");
        let spool_path = directory.path().join("discord-route-spool.json");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock gateway");
        let gateway_url = format!("ws://{}", listener.local_addr().expect("gateway address"));
        let adapter = adapter_for(gateway_url.clone(), api_server.uri(), None);

        let (inbound_tx, mut inbound_rx) = tokio::sync::mpsc::channel(4);
        let (journal_tx, mut journal_rx) = tokio::sync::mpsc::channel(1);
        adapter
            .set_durable_inbound_sender(inbound_tx, journal_tx)
            .await;

        let (reservation_seen_tx, reservation_seen_rx) = tokio::sync::oneshot::channel();
        let (writer_started_tx, writer_started_rx) = tokio::sync::oneshot::channel();
        let (stale_resume_tx, stale_resume_rx) = tokio::sync::oneshot::channel();
        let (advanced_resume_tx, advanced_resume_rx) = tokio::sync::oneshot::channel();
        let (release_writer_tx, release_writer_rx) = std::sync::mpsc::channel::<()>();
        let (commit_done_tx, commit_done_rx) = tokio::sync::watch::channel(false);

        let writer_path = spool_path.clone();
        let journal_task = tokio::spawn(async move {
            let event = timeout(Duration::from_secs(2), journal_rx.recv())
                .await
                .expect("driver reserves ingress for the CLI spool")
                .expect("journal sender remains live");
            let DiscordIngressJournalEvent::Reserve(mut reservation) = event else {
                panic!("unexpected journal removal before durable ingress reservation");
            };
            let entry = reservation.entry().clone();
            reservation_seen_tx
                .send(())
                .expect("mock gateway awaits the reservation");
            let spool = DiscordRouteSpool {
                schema_version: DISCORD_ROUTE_SPOOL_SCHEMA_VERSION,
                entries: vec![DiscordRouteSpoolEntry::pre_policy(entry)],
            };
            run_discord_spool_blocking(move || {
                writer_started_tx
                    .send(())
                    .expect("test observes blocked atomic spool writer");
                release_writer_rx
                    .recv()
                    .expect("test releases blocked spool writer");
                write_discord_route_spool(&writer_path, &spool)
            })
            .await
            .expect("atomic spool write and fsync");
            reservation.acknowledge_durable(DiscordIngressReservationOutcome::Recorded);
            commit_done_tx.send_replace(true);
        });

        let server = tokio::spawn(async move {
            let mut first = accept(&listener).await;
            send_payload(&mut first, hello()).await;
            receive_opcode(&mut first, opcodes::IDENTIFY).await;
            send_payload(&mut first, ready(10, &gateway_url)).await;
            send_payload(&mut first, message_create(11)).await;
            timeout(Duration::from_secs(2), reservation_seen_rx)
                .await
                .expect("CLI journal receives the message before reconnect")
                .expect("journal owner remains live");

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
            send_payload(&mut second, hello()).await;
            let stale_resume = receive_opcode(&mut second, opcodes::RESUME).await;
            assert_eq!(
                stale_resume
                    .d
                    .as_ref()
                    .and_then(|data| data.get("seq"))
                    .and_then(serde_json::Value::as_u64),
                Some(10),
                "the un-fsynced message sequence must not be resumable"
            );
            stale_resume_tx
                .send(())
                .expect("test awaits pre-commit resume");

            let mut commit_done_rx = commit_done_rx;
            while !*commit_done_rx.borrow() {
                commit_done_rx
                    .changed()
                    .await
                    .expect("journal owner reports durable commit");
            }
            drop(second);

            let mut third = accept(&listener).await;
            send_payload(&mut third, hello()).await;
            let advanced_resume = receive_opcode(&mut third, opcodes::RESUME).await;
            assert_eq!(
                advanced_resume
                    .d
                    .as_ref()
                    .and_then(|data| data.get("seq"))
                    .and_then(serde_json::Value::as_u64),
                Some(11),
                "the sequence may advance only after atomic spool write plus fsync acknowledgement"
            );
            advanced_resume_tx
                .send(())
                .expect("test awaits post-commit resume");
            let _ = timeout(Duration::from_secs(2), third.next()).await;
        });

        PlatformAdapter::start(adapter.as_ref())
            .await
            .expect("start Discord adapter");
        timeout(Duration::from_secs(2), writer_started_rx)
            .await
            .expect("atomic spool writer reaches its blocked commit point")
            .expect("writer task remains live");
        timeout(Duration::from_secs(2), stale_resume_rx)
            .await
            .expect("reconnect occurs while spool commit is blocked")
            .expect("mock gateway confirms stale resume");
        assert!(
            !spool_path.exists(),
            "the CLI must not ACK or advance the sequence before its spool exists"
        );

        release_writer_tx
            .send(())
            .expect("release atomic spool writer");
        timeout(Duration::from_secs(2), advanced_resume_rx)
            .await
            .expect("post-fsync reconnect advances the resume sequence")
            .expect("mock gateway confirms advanced resume");
        let inbound = timeout(Duration::from_secs(2), inbound_rx.recv())
            .await
            .expect("durably recorded inbound reaches CLI after ACK")
            .expect("inbound sender remains live");
        assert_eq!(inbound.message().message_id, "durable-message");
        journal_task.await.expect("journal owner joins");

        PlatformAdapter::stop(adapter.as_ref())
            .await
            .expect("stop first adapter");
        server.await.expect("mock gateway joins");

        let persisted = DiscordRouteSpool::load(&spool_path).expect("read fsynced spool");
        assert_eq!(persisted.entries.len(), 1);
        assert_eq!(persisted.entries[0].state, DiscordRouteSpoolState::PrePolicy);
        let replay_entries = persisted.recoverable_pre_policy_entries();
        assert_eq!(replay_entries.len(), 1);

        let replay_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind restart mock gateway");
        let replay_url = format!(
            "ws://{}",
            replay_listener.local_addr().expect("restart gateway address")
        );
        let replay_adapter = adapter_for(replay_url.clone(), api_server.uri(), None);
        let (replay_inbound_tx, mut replay_inbound_rx) = tokio::sync::mpsc::channel(1);
        let (replay_journal_tx, _replay_journal_rx) = tokio::sync::mpsc::channel(1);
        replay_adapter
            .set_durable_inbound_sender(replay_inbound_tx, replay_journal_tx)
            .await;
        replay_adapter
            .queue_recovered_pre_policy_ingress(replay_entries)
            .expect("queue raw pre-policy replay");

        let replay_server = tokio::spawn(async move {
            let mut socket = accept(&replay_listener).await;
            send_payload(&mut socket, hello()).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(12, &replay_url)).await;
            let _ = timeout(Duration::from_secs(2), socket.next()).await;
        });
        PlatformAdapter::start(replay_adapter.as_ref())
            .await
            .expect("start restarted Discord adapter");
        let replayed = timeout(Duration::from_secs(2), replay_inbound_rx.recv())
            .await
            .expect("durable ingress replays after restart")
            .expect("restart inbound sender remains live");
        assert_eq!(replayed.message().message_id, "durable-message");
        PlatformAdapter::stop(replay_adapter.as_ref())
            .await
            .expect("stop restarted adapter");
        replay_server.await.expect("restart mock gateway joins");

        // The persisted entry is raw ingress. On a restart with a stricter
        // channel policy it must re-enter the adapter resolver, be removed,
        // and never reach the CLI route queue.
        let disabled_spool_path = directory.path().join("disabled-policy-spool.json");
        write_discord_route_spool(&disabled_spool_path, &persisted)
            .expect("persist raw disabled-policy recovery fixture");
        let disabled_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind disabled-policy mock gateway");
        let disabled_url = format!(
            "ws://{}",
            disabled_listener
                .local_addr()
                .expect("disabled-policy gateway address")
        );
        let disabled_adapter = adapter_for(
            disabled_url.clone(),
            api_server.uri(),
            Some("policy-channel"),
        );
        let (disabled_inbound_tx, mut disabled_inbound_rx) = tokio::sync::mpsc::channel(1);
        let (disabled_journal_tx, mut disabled_journal_rx) = tokio::sync::mpsc::channel(1);
        disabled_adapter
            .set_durable_inbound_sender(disabled_inbound_tx, disabled_journal_tx)
            .await;
        disabled_adapter
            .queue_recovered_pre_policy_ingress(persisted.recoverable_pre_policy_entries())
            .expect("queue raw disabled-policy recovery");
        let disabled_server = tokio::spawn(async move {
            let mut socket = accept(&disabled_listener).await;
            send_payload(&mut socket, hello()).await;
            receive_opcode(&mut socket, opcodes::IDENTIFY).await;
            send_payload(&mut socket, ready(13, &disabled_url)).await;
            let _ = timeout(Duration::from_secs(2), socket.next()).await;
        });
        PlatformAdapter::start(disabled_adapter.as_ref())
            .await
            .expect("start disabled-policy adapter");
        let removal = timeout(Duration::from_secs(2), disabled_journal_rx.recv())
            .await
            .expect("disabled policy resolves recovered raw ingress")
            .expect("disabled-policy journal sender remains live");
        assert!(matches!(
            removal,
            DiscordIngressJournalEvent::Remove { ref message_id }
                if message_id == "durable-message"
        ));
        assert!(
            timeout(Duration::from_millis(100), disabled_inbound_rx.recv())
                .await
                .is_err(),
            "a raw entry rejected by current policy must make zero CLI route calls"
        );
        let mut disabled_spool =
            DiscordRouteSpool::load(&disabled_spool_path).expect("load raw recovery spool");
        assert!(disabled_spool.remove_pre_policy("durable-message"));
        write_discord_route_spool(&disabled_spool_path, &disabled_spool)
            .expect("persist policy removal");
        assert!(
            DiscordRouteSpool::load(&disabled_spool_path)
                .expect("reload policy removal")
                .entries
                .is_empty(),
            "policy-rejected raw ingress must be removed rather than retained for route replay"
        );
        PlatformAdapter::stop(disabled_adapter.as_ref())
            .await
            .expect("stop disabled-policy adapter");
        disabled_server
            .await
            .expect("disabled-policy mock gateway joins");
    }
}
