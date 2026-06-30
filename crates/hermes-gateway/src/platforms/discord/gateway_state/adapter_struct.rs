/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    api_base_url: String,
    client: Client,
    stop_signal: Arc<Notify>,
    liveness_failed: Arc<AtomicBool>,
    liveness_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    thread_participation: Mutex<DiscordThreadParticipationTracker>,
    non_conversational_messages: Mutex<DiscordNonConversationalMessageTracker>,
}
