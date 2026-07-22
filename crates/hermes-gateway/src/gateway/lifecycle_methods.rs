#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AdapterStopFailureKind {
    ConnectionFailed,
    SendFailed,
    MediaPreflightFailed,
    MediaRemoteRejected,
    Platform,
    Auth,
    RateLimited,
    SessionExpired,
    PartialDelivery,
    AcceptedStateUndurable,
    Panic,
    Timeout,
}

impl AdapterStopFailureKind {
    fn from_gateway_error(error: &GatewayError) -> Self {
        match error {
            GatewayError::ConnectionFailed(_) => Self::ConnectionFailed,
            GatewayError::SendFailed(_) => Self::SendFailed,
            GatewayError::MediaPreflightFailed { .. } => Self::MediaPreflightFailed,
            GatewayError::MediaRemoteRejected { .. } => Self::MediaRemoteRejected,
            GatewayError::Platform(_) => Self::Platform,
            GatewayError::Auth(_) => Self::Auth,
            GatewayError::RateLimited { .. } => Self::RateLimited,
            GatewayError::SessionExpired(_) => Self::SessionExpired,
            GatewayError::PartialDelivery(_) => Self::PartialDelivery,
            GatewayError::AcceptedStateUndurable { .. } => Self::AcceptedStateUndurable,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::ConnectionFailed => "connection_failed",
            Self::SendFailed => "send_failed",
            Self::MediaPreflightFailed => "media_preflight_failed",
            Self::MediaRemoteRejected => "media_remote_rejected",
            Self::Platform => "platform",
            Self::Auth => "auth",
            Self::RateLimited => "rate_limited",
            Self::SessionExpired => "session_expired",
            Self::PartialDelivery => "partial_delivery",
            Self::AcceptedStateUndurable => "accepted_state_undurable",
            Self::Panic => "panic",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct AdapterStopFailure {
    platform: String,
    kind: AdapterStopFailureKind,
}

impl AdapterStopFailure {
    fn new(platform: &str, kind: AdapterStopFailureKind) -> Self {
        let mut sanitized = String::with_capacity(platform.len().min(64));
        for character in platform.chars().take(64) {
            sanitized.push(if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            });
        }
        if sanitized.is_empty() {
            sanitized.push_str("unknown");
        }
        Self {
            platform: sanitized,
            kind,
        }
    }

    fn summary(&self) -> String {
        format!("{}:{}", self.platform, self.kind.as_str())
    }
}

/// Truthful outcome of removing gateway-owned transient session state.
///
/// Gateway session cleanup has no authority to erase provider-owned persistent
/// memory. A memory-erased outcome can only come from the runtime's separately
/// authorized, all-provider verified governance call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GatewaySessionCleanupReceipt {
    session_state_removed: bool,
}

impl GatewaySessionCleanupReceipt {
    const fn session_state_removed_memory_retained() -> Self {
        Self {
            session_state_removed: true,
        }
    }

    pub(crate) const fn session_state_removed(self) -> bool {
        self.session_state_removed
    }

    /// Session cleanup never implies memory erasure.
    pub(crate) const fn memory_erased(self) -> bool {
        false
    }

    pub(crate) const fn message(self) -> &'static str {
        "Gateway session state was removed. Persistent memory remains unchanged; a separate authorized memory forget with all-provider verification is required."
    }
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
        let resource_limits_enabled = config.resource_limits_enabled;
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);

        Self {
            adapters: Arc::new(RwLock::new(HashMap::new())),
            session_manager,
            dm_manager: Arc::new(RwLock::new(dm_manager)),
            stream_manager,
            config: Arc::new(config),
            message_handler: Arc::new(RwLock::new(None)),
            message_handler_with_context: Arc::new(RwLock::new(None)),
            streaming_handler: Arc::new(RwLock::new(None)),
            memory_forget_handler: Arc::new(RwLock::new(None)),
            memory_export_handler: Arc::new(RwLock::new(None)),
            memory_export_open_handler: Arc::new(RwLock::new(None)),
            memory_export_download_grants: Arc::new(RwLock::new(HashMap::new())),
            streaming_handler_with_context: Arc::new(RwLock::new(None)),
            accepted_ingress_preparers: Arc::new(RwLock::new(HashMap::new())),
            runtime_state: Arc::new(RwLock::new(HashMap::new())),
            default_model: Arc::new(RwLock::new(default_model)),
            tool_progress_modes: Arc::new(RwLock::new(BTreeMap::new())),
            usage_stats: Arc::new(RwLock::new(HashMap::new())),
            background_tasks: Arc::new(BackgroundTaskManager::with_resource_limits(
                8,
                resource_limits_enabled,
            )),
            turn_workers: Arc::new(StdMutex::new(HashMap::new())),
            next_turn_worker_id: Arc::new(AtomicU64::new(0)),
            active_ingress_workers: Arc::new(AtomicUsize::new(0)),
            shutting_down: Arc::new(AtomicBool::new(false)),
            shutdown_tx: Arc::new(shutdown_tx),
            mcp_reload_generation: Arc::new(RwLock::new(0)),
            hook_registry: Arc::new(RwLock::new(None)),
            platform_access_policies: Arc::new(RwLock::new(HashMap::new())),
            fail_closed_default_warnings: Arc::new(RwLock::new(HashSet::new())),
            message_deduplicator: Arc::new(ParkingMutex::new(MessageDeduplicator::default())),
            busy_sessions: Arc::new(RwLock::new(
                BusySessionCoordinator::with_resource_limits(
                    Duration::from_secs(30),
                    resource_limits_enabled,
                ),
            )),
            session_turn_locks: Arc::new(ParkingMutex::new(HashMap::new())),
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
    async fn purge_removed_session_state(
        &self,
        session_key: &str,
    ) -> GatewaySessionCleanupReceipt {
        self.clear_session_boundary_security_state(session_key).await;
        self.runtime_state.write().await.remove(session_key);
        self.usage_stats.write().await.remove(session_key);
        let receipt = GatewaySessionCleanupReceipt::session_state_removed_memory_retained();
        info!(
            session_state_removed = receipt.session_state_removed(),
            memory_erased = receipt.memory_erased(),
            "{}",
            receipt.message()
        );
        receipt
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

    /// Set the authenticated, subject-bound memory-forget handler.
    pub async fn set_memory_forget_handler(&self, handler: GatewayMemoryForgetHandler) {
        *self.memory_forget_handler.write().await = Some(handler);
    }

    /// Set the authenticated, subject-bound memory-export handler.
    pub async fn set_memory_export_handler(&self, handler: GatewayMemoryExportHandler) {
        *self.memory_export_handler.write().await = Some(handler);
    }

    /// Set the handler that resolves redeemed downloads through a bound
    /// manager's verified artifact descriptor API.
    pub async fn set_memory_export_open_handler(&self, handler: GatewayMemoryExportOpenHandler) {
        *self.memory_export_open_handler.write().await = Some(handler);
    }

    async fn issue_memory_export_download(
        &self,
        context: &GatewayRuntimeContext,
        artifact: &GatewayMemoryExportArtifact,
    ) -> Result<GatewayMemoryExportDownloadMetadata, GatewayError> {
        let subject = context.memory_subject().map_err(|_| {
            GatewayError::Auth(
                "memory export download requires an authenticated gateway identity".to_string(),
            )
        })?;
        let now = Instant::now();
        let mut grants = self.memory_export_download_grants.write().await;
        grants.retain(|_, grant| now.duration_since(grant.issued_at) < MEMORY_EXPORT_DOWNLOAD_GRANT_TTL);
        while grants.len() >= MAX_MEMORY_EXPORT_DOWNLOAD_GRANTS {
            let Some(oldest) = grants
                .iter()
                .min_by_key(|(_, grant)| grant.issued_at)
                .map(|(download_ref, _)| download_ref.clone())
            else {
                break;
            };
            grants.remove(&oldest);
        }

        let mut download_ref = None;
        for _ in 0..4 {
            let candidate = format!("medl1_{}", uuid::Uuid::new_v4().simple());
            if !grants.contains_key(&candidate) {
                download_ref = Some(candidate);
                break;
            }
        }
        let download_ref = download_ref.ok_or_else(|| {
            GatewayError::Platform("memory export download reference allocation failed".to_string())
        })?;
        grants.insert(
            download_ref.clone(),
            GatewayMemoryExportDownloadGrant {
                subject,
                artifact: artifact.clone(),
                issued_at: now,
            },
        );

        Ok(GatewayMemoryExportDownloadMetadata {
            download_ref,
            manifest_digest: artifact.manifest_digest.clone(),
            record_count: artifact.record_count,
            page_count: artifact.page_count,
            provider_count: artifact.provider_count,
        })
    }

    /// Atomically take an owner-only export grant after re-deriving the
    /// authenticated subject. A wrong subject or token leaves the real grant
    /// usable by its owner.
    async fn take_memory_export_download(
        &self,
        context: &GatewayRuntimeContext,
        download_ref: &str,
    ) -> Result<GatewayMemoryExportArtifact, GatewayError> {
        let subject = context.memory_subject().map_err(|_| {
            GatewayError::Auth(
                "memory export download requires an authenticated gateway identity".to_string(),
            )
        })?;
        let now = Instant::now();
        let mut grants = self.memory_export_download_grants.write().await;
        grants.retain(|_, grant| now.duration_since(grant.issued_at) < MEMORY_EXPORT_DOWNLOAD_GRANT_TTL);
        let authorized = grants
            .get(download_ref)
            .is_some_and(|grant| grant.subject == subject);
        if !authorized {
            return Err(GatewayError::Auth(
                "memory export download is unavailable".to_string(),
            ));
        }
        grants
            .remove(download_ref)
            .map(|grant| grant.artifact)
            .ok_or_else(|| GatewayError::Auth("memory export download is unavailable".to_string()))
    }

    /// Redeem a gateway-local, single-use export reference.
    ///
    /// The token is consumed before the descriptor is opened, so replays fail
    /// even if the verified manager open later fails. No path or bytes are
    /// cached: the configured handler must rebind the accepted context to a
    /// fresh bound manager and open the exact committed descriptor.
    pub async fn redeem_memory_export_download(
        &self,
        context: &GatewayRuntimeContext,
        download_ref: &str,
    ) -> Result<std::fs::File, GatewayError> {
        let handler = self
            .memory_export_open_handler
            .read()
            .await
            .clone()
            .ok_or_else(|| GatewayError::Auth("memory export download is unavailable".to_string()))?;
        let artifact = self.take_memory_export_download(context, download_ref).await?;
        handler(context.clone(), artifact).await
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

    /// Register a platform-specific accepted-ingress preparer.
    ///
    /// The preparer runs after access control and dedup ownership, before the
    /// session scope is composed. Side effects must be externally idempotent.
    pub async fn register_accepted_ingress_preparer(
        &self,
        platform: impl Into<String>,
        preparer: AcceptedIngressPreparer,
    ) {
        self.accepted_ingress_preparers
            .write()
            .await
            .insert(platform.into().trim().to_ascii_lowercase(), preparer);
    }

    async fn accepted_ingress_preparer(
        &self,
        platform: &str,
    ) -> Option<AcceptedIngressPreparer> {
        self.accepted_ingress_preparers
            .read()
            .await
            .get(&platform.trim().to_ascii_lowercase())
            .cloned()
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
            "thread_id": session.thread_id,
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

    async fn shutdown_runtime_workers(&self) -> usize {
        let handles = {
            let mut workers = self.turn_workers.lock().unwrap();
            std::mem::take(&mut *workers)
                .into_values()
                .collect::<Vec<_>>()
        };
        let worker_count = handles.len();
        if handles.is_empty() {
            return 0;
        }

        let deadline = tokio::time::Instant::now() + SESSION_COMMIT_TIMEOUT;
        while handles.iter().any(|handle| !handle.is_finished())
            && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        for handle in &handles {
            if !handle.is_finished() {
                handle.abort();
            }
        }
        let _ = tokio::time::timeout(SESSION_COMMIT_TIMEOUT, async move {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;
        worker_count
    }

    /// Start all registered and enabled platform adapters.
    pub async fn start_all(&self) -> Result<(), GatewayError> {
        self.shutting_down.store(false, Ordering::Release);
        self.shutdown_tx.send_replace(false);
        let adapters = self.adapters.read().await.clone();
        for (name, adapter) in adapters {
            info!("Starting platform adapter: {}", name);
            match tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                AssertUnwindSafe(async { adapter.start().await }).catch_unwind(),
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    error!("Failed to start adapter '{}': {}", name, error);
                    return Err(error);
                }
                Ok(Err(_)) => {
                    error!("Platform adapter '{}' panicked during startup", name);
                    return Err(GatewayError::Platform(format!(
                        "platform adapter '{name}' panicked during startup"
                    )));
                }
                Err(_) => {
                    error!("Platform adapter '{}' timed out during startup", name);
                    return Err(GatewayError::Platform(format!(
                        "platform adapter '{name}' timed out during startup"
                    )));
                }
            }
        }
        self.background_tasks.reopen();
        info!("All platform adapters started successfully");
        Ok(())
    }

    /// Stop all platform adapters gracefully.
    pub async fn stop_all(&self) -> Result<(), GatewayError> {
        self.shutting_down.store(true, Ordering::Release);
        let controls = {
            let busy = self.busy_sessions.write().await;
            let controls = busy.active_controls_for_shutdown();
            self.shutdown_tx.send_replace(true);
            controls
        };
        let interrupted_runtime = controls
            .iter()
            .filter(|control| {
                interrupt_control(
                    control.as_ref(),
                    "Gateway shutdown requested; stop active agent and tool work.",
                )
            })
            .count();
        if interrupted_runtime > 0 {
            info!(
                interrupted_runtime,
                "Signalled active gateway controls before worker shutdown"
            );
        }
        let cancelled_background = self.background_tasks.cancel_all().await;
        let cancelled_runtime = self.shutdown_runtime_workers().await;
        let released_dedup_keys = self
            .busy_sessions
            .write()
            .await
            .abort_all_for_shutdown();
        self.release_dedup_keys(&released_dedup_keys).await;
        let cleared_streams = self.stream_manager.clear_active_streams().await;
        if cleared_streams > 0 {
            info!(
                cleared_streams,
                "Cleared cancelled gateway streams before adapter shutdown"
            );
        }
        if cancelled_background + cancelled_runtime > 0 {
            info!(
                background_workers = cancelled_background,
                runtime_workers = cancelled_runtime,
                "Cancelled gateway workers before adapter shutdown"
            );
        }
        let finalized = self.finalize_active_sessions("shutdown").await;
        if finalized > 0 {
            info!(
                finalized,
                "Finalized active gateway sessions before shutdown"
            );
        }
        let adapters = self.adapters.read().await.clone();
        let mut failures = Vec::new();
        for (name, adapter) in adapters {
            let failure_platform = AdapterStopFailure::new(&name, AdapterStopFailureKind::Platform)
                .platform;
            info!(adapter = %failure_platform, "Stopping platform adapter");
            match tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                AssertUnwindSafe(async { adapter.stop().await }).catch_unwind(),
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(error))) => {
                    let kind = AdapterStopFailureKind::from_gateway_error(&error);
                    warn!(
                        adapter = %failure_platform,
                        failure = kind.as_str(),
                        "Platform adapter returned an error during shutdown"
                    );
                    failures.push(AdapterStopFailure {
                        platform: failure_platform,
                        kind,
                    });
                }
                Ok(Err(_)) => {
                    warn!(
                        adapter = %failure_platform,
                        failure = AdapterStopFailureKind::Panic.as_str(),
                        "Platform adapter panicked during shutdown"
                    );
                    failures.push(AdapterStopFailure {
                        platform: failure_platform,
                        kind: AdapterStopFailureKind::Panic,
                    });
                }
                Err(_) => {
                    warn!(
                        adapter = %failure_platform,
                        failure = AdapterStopFailureKind::Timeout.as_str(),
                        "Platform adapter timed out during shutdown"
                    );
                    failures.push(AdapterStopFailure {
                        platform: failure_platform,
                        kind: AdapterStopFailureKind::Timeout,
                    });
                }
            }
        }
        if failures.is_empty() {
            info!("All platform adapters stopped");
            return Ok(());
        }

        failures.sort();
        let summary = failures
            .iter()
            .map(AdapterStopFailure::summary)
            .collect::<Vec<_>>()
            .join(", ");
        error!(
            failures = %summary,
            "One or more platform adapters failed during shutdown"
        );
        Err(GatewayError::Platform(format!(
            "platform adapter shutdown failed: {summary}"
        )))
    }

    // -----------------------------------------------------------------------
    // Message routing
    // -----------------------------------------------------------------------

}

#[cfg(test)]
mod memory_cleanup_tests {
    use super::*;
    use crate::session::SessionManager;
    use hermes_config::session::SessionConfig;
    use std::sync::Arc;

    #[tokio::test]
    async fn removing_gateway_session_state_never_claims_memory_erasure() {
        let gateway = Gateway::with_defaults(
            Arc::new(SessionManager::new(SessionConfig::default())),
            GatewayConfig::default(),
        );
        let session_key = "lifecycle-memory-honesty";
        gateway
            .runtime_state
            .write()
            .await
            .insert(session_key.to_string(), SessionRuntimeState::default());

        let receipt = gateway.purge_removed_session_state(session_key).await;

        assert!(receipt.session_state_removed());
        assert!(!receipt.memory_erased());
        assert!(receipt
            .message()
            .contains("Persistent memory remains unchanged"));
        assert!(!gateway.runtime_state.read().await.contains_key(session_key));
    }
}
