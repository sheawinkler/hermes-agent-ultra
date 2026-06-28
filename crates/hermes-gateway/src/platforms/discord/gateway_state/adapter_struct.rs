/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    thread_participation: Mutex<DiscordThreadParticipationTracker>,
    non_conversational_messages: Mutex<DiscordNonConversationalMessageTracker>,
}
