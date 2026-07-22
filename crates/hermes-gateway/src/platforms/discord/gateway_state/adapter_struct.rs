/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    api_base_url: String,
    client: Client,
    gateway_driver: Arc<DiscordGatewayRuntime>,
    liveness_failed: Arc<AtomicBool>,
    liveness_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// Shared admission gate for Discord's account-wide REST 429s.
    global_rate_limit: Arc<DiscordGlobalRateLimitBarrier>,
    lifecycle: tokio::sync::Mutex<()>,
    /// Fences accepted REST side effects and their required tracker commits
    /// while the adapter is stopping.
    accepted_state_admission: Arc<DiscordAcceptedStateAdmission>,
    thread_participation: Arc<Mutex<DiscordThreadParticipationTracker>>,
    non_conversational_messages: Arc<Mutex<DiscordNonConversationalMessageTracker>>,
    /// Supervised writer owns all durable tracker snapshots and acknowledges
    /// callers only after the atomic replacement completes.
    thread_participation_writer: Mutex<Option<DiscordTrackerWriter<DiscordThreadParticipationTracker>>>,
    thread_participation_repair_pending: Arc<AtomicBool>,
    non_conversational_writer:
        Mutex<Option<DiscordTrackerWriter<DiscordNonConversationalMessageTracker>>>,
    non_conversational_repair_pending: Arc<AtomicBool>,
}

/// Arms immediately after Discord REST is reopened for startup. Every
/// unsuccessful startup path must re-close REST and leave no socket or
/// liveness task owned by an adapter that reports stopped.
struct DiscordStartupRollback<'a> {
    adapter: &'a DiscordAdapter,
    armed: bool,
}

impl<'a> DiscordStartupRollback<'a> {
    fn arm(adapter: &'a DiscordAdapter) -> Self {
        Self {
            adapter,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DiscordStartupRollback<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        self.adapter.close_accepted_state_admission();
        self.adapter.base.mark_stopped();
        self.adapter.stop_liveness_probe();
        self.adapter.gateway_driver.cancel_and_abort_on_drop();
    }
}
