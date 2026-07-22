impl Gateway {
    /// Route a message whose sender provenance is unavailable.
    ///
    /// Production adapters must prefer [`Self::route_message_from_sender`] and
    /// pass metadata verified by their platform parser.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        self.route_message_from_sender(incoming, IncomingSender::unknown())
            .await
    }

    /// Route an incoming message with platform-verified sender metadata.
    pub async fn route_message_from_sender(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<(), GatewayError> {
        self.route_message_from_sender_outcome(incoming, sender)
            .await?
            .into_result()
    }

    /// Route accepted ingress and preserve whether a failed delivery is safe to
    /// retry or must remain quarantined because side effects are uncertain.
    ///
    /// Duplicate suppression happens exactly once here, before a private queue
    /// envelope is created. Drained entries therefore never re-enter dedup.
    pub async fn route_message_from_sender_outcome(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<GatewayRouteOutcome, GatewayError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(GatewayError::Platform(
                "gateway is shutting down".to_string(),
            ));
        }
        let worker_id = self.next_turn_worker_id.fetch_add(1, Ordering::Relaxed);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        let gateway = self.clone();
        let workers = self.turn_workers.clone();
        let active_ingress_workers = self.active_ingress_workers.clone();
        let incoming = incoming.clone();
        {
        let mut registered_workers = self.turn_workers.lock().unwrap();
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(GatewayError::Platform(
                "gateway is shutting down".to_string(),
            ));
        }
        let ingress_worker_limit = self
            .config
            .ingress_worker_limit
            .unwrap_or(MAX_INGRESS_WORKERS)
            .max(1);
        if self.config.resource_limits_enabled
            && self.active_ingress_workers.load(Ordering::Acquire) >= ingress_worker_limit
        {
            // This admission check runs before a worker, dedup claim, handler,
            // model, tool, or outbound notification exists. Preserve it as a
            // typed live-retry signal rather than turning it into an uncertain
            // durable quarantine.
            return Ok(GatewayRouteOutcome::Backpressured {
                retry_after: INGRESS_BACKPRESSURE_RETRY_AFTER,
            });
        }
        self.active_ingress_workers.fetch_add(1, Ordering::AcqRel);
        let ingress_guard = IngressWorkerGuard {
            active: active_ingress_workers,
        };
        let handle = tokio::spawn(async move {
            let _ingress_guard = ingress_guard;
            let result = tokio::select! {
                biased;
                _ = gateway.wait_for_shutdown() => Err(GatewayError::Platform(
                    "gateway is shutting down".to_string(),
                )),
                result = AssertUnwindSafe(
                    gateway.route_validated_ingress(incoming, sender),
                ).catch_unwind() => match result {
                    Ok(result) => result,
                    Err(_) => Err(GatewayError::Platform(
                        "gateway ingress worker panicked".to_string(),
                    )),
                },
            };
            let _ = completion_tx.send(result);
            workers.lock().unwrap().remove(&worker_id);
        });
        registered_workers.insert(worker_id, handle);
        }
        completion_rx.await.map_err(|_| {
            GatewayError::Platform(
                "gateway ingress worker ended before reporting completion".to_string(),
            )
        })?
    }

    /// Build the authoritative session scope from platform-verified ingress.
    ///
    /// `chat_id` remains the parent channel used for authorization and
    /// outbound parent targeting; `thread_id` only refines the conversation
    /// lane and reply destination.
    fn conversation_scope_for(incoming: &IncomingMessage) -> ConversationScope<'_> {
        ConversationScope::new(
            &incoming.platform,
            &incoming.chat_id,
            incoming.thread_id.as_deref(),
            &incoming.user_id,
            incoming.is_dm,
        )
    }

    async fn get_or_create_runtime_session(
        &self,
        incoming: &IncomingMessage,
    ) -> Option<Session> {
        let scope = Self::conversation_scope_for(incoming);
        if self.config.resource_limits_enabled {
            self.session_manager
                .try_get_or_create_session_for_scope(scope, MAX_RETAINED_SESSIONS)
                .await
        } else {
            Some(
                self.session_manager
                    .get_or_create_session_for_scope(scope)
                    .await,
            )
        }
    }

    async fn route_validated_ingress(
        &self,
        mut incoming: IncomingMessage,
        sender: IncomingSender,
    ) -> Result<GatewayRouteOutcome, GatewayError> {
        let access_policy = self.platform_access_policy(&incoming.platform).await;
        let is_slash_command = incoming.text.trim_start().starts_with('/');
        if let Some(policy) = access_policy.as_ref() {
            // DM grants are intentionally absent from this calculation. Group
            // and slash traffic is governed only by its configured policy plus
            // the explicit, verified Discord bot exception below.
            let bypasses_user_allowlist =
                policy.allows_sender_without_user_allowlist(&incoming, &sender);
            let identity_allowed = policy.is_identity_allowed(&incoming, &sender);
            if !incoming.is_dm {
                if policy.is_channel_ignored(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel is ignored by platform policy"
                    );
                    return Ok(GatewayRouteOutcome::Committed);
                }
                if !policy.is_channel_allowed(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel not in platform allowlist"
                    );
                    return Ok(GatewayRouteOutcome::Committed);
                }
                match policy.group_mode {
                    GroupAccessMode::Disabled => {
                        debug!(
                            platform = incoming.platform,
                            user_id = incoming.user_id,
                            "Group traffic denied by platform policy"
                        );
                        return Ok(GatewayRouteOutcome::Committed);
                    }
                    GroupAccessMode::Allowlist => {
                        if !bypasses_user_allowlist
                            && !identity_allowed
                            && !policy.is_group_chat_authorized(&incoming.chat_id)
                        {
                            self.warn_fail_closed_default_once(&incoming.platform, policy)
                                .await;
                            debug!(
                                platform = incoming.platform,
                                user_id = incoming.user_id,
                                "Group message denied: user not in allowlist"
                            );
                            return Ok(GatewayRouteOutcome::Committed);
                        }
                    }
                    GroupAccessMode::Open => {}
                }
            }
            if is_slash_command
                && policy.slash_requires_allowlist
                && policy.has_allowlist()
                && !bypasses_user_allowlist
                && !identity_allowed
            {
                self.warn_fail_closed_default_once(&incoming.platform, policy)
                    .await;
                debug!(
                    platform = incoming.platform,
                    user_id = incoming.user_id,
                    "Slash command denied: user not in platform allowlist"
                );
                return Ok(GatewayRouteOutcome::Committed);
            }
        }

        // Pure channel/group/slash policy rejects happen before claiming. Claim
        // immediately before the first stateful or outbound boundary so a
        // redelivery cannot create or send a second pairing response.
        let (dedup_key, mut dedup_claim) = match self.acquire_dedup_claim(&incoming).await {
            DedupIngressClaim::Untracked => (None, None),
            DedupIngressClaim::Completed => {
                debug!(
                    platform = incoming.platform,
                    chat_id = incoming.chat_id,
                    message_id = incoming.message_id.as_deref().unwrap_or_default(),
                    "Completed platform message redelivery suppressed"
                );
                return Ok(GatewayRouteOutcome::Committed);
            }
            DedupIngressClaim::Owner { key, guard } => (Some(key), Some(guard)),
        };

        if incoming.is_dm {
            let dm_decision = {
                let dm_manager = self.dm_manager.read().await;
                dm_manager
                    .handle_dm_for_chat(
                        &incoming.user_id,
                        &incoming.platform,
                        &incoming.chat_id,
                        is_slash_command,
                    )
                    .await
            };
            match dm_decision {
                DmDecision::Allow => {}
                DmDecision::Pair { message } => {
                    if let Some(message) = message {
                        self.send_message(&incoming.platform, &incoming.chat_id, &message, None)
                            .await?;
                    }
                    if let Some(claim) = dedup_claim.as_mut() {
                        self.complete_dedup_key(dedup_key.as_deref()).await;
                        claim.disarm();
                    }
                    return Ok(GatewayRouteOutcome::Committed);
                }
                DmDecision::Deny => {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied for unauthorized user"
                    );
                    if let Some(claim) = dedup_claim.as_mut() {
                        self.complete_dedup_key(dedup_key.as_deref()).await;
                        claim.disarm();
                    }
                    return Ok(GatewayRouteOutcome::Committed);
                }
            }
        }

        if let Some(preparer) = self.accepted_ingress_preparer(&incoming.platform).await {
            let preparation = match preparer(&incoming, &sender).await {
                Ok(preparation) => preparation,
                Err(AcceptedIngressPreparationError::Retryable { error, retry_after }) => {
                    // This preparation boundary is before session allocation,
                    // handler/model/tool execution, or outbound delivery.
                    // Preserve any exact provider retry hint and release dedup
                    // ownership so the CLI spool can replay the full ingress.
                    if let Some(claim) = dedup_claim.as_mut() {
                        self.release_dedup_key(dedup_key.as_deref()).await;
                        claim.disarm();
                    }
                    return Ok(GatewayRouteOutcome::Retryable { error, retry_after });
                }
                Err(AcceptedIngressPreparationError::Rejected(error))
                | Err(AcceptedIngressPreparationError::Ambiguous(error)) => return Err(error),
            };
            if let Some(thread_id) = preparation.thread_id {
                let thread_id = thread_id.trim();
                if thread_id.is_empty() {
                    return Err(GatewayError::Platform(
                        "accepted ingress preparation returned an empty thread ID".to_string(),
                    ));
                }
                if incoming.thread_id.is_some() {
                    return Err(GatewayError::Platform(
                        "accepted ingress preparation cannot replace an existing thread"
                            .to_string(),
                    ));
                }
                incoming.thread_id = Some(thread_id.to_string());
            }
        }

        let session_key = self
            .session_manager
            .compose_session_key_for_scope(Self::conversation_scope_for(&incoming));
        let reset_requested = is_slash_command
            && matches!(
                handle_command(&incoming.text),
                GatewayCommandResult::ResetSession(_)
            );
        let mut session_capacity_reached = false;
        {
            let turn_lock = self.session_turn_lock(&session_key).await;
            let _session_guard = turn_lock.lock().await;
            let busy = self.busy_sessions.read().await;
            let transition = if busy.is_active(&session_key) {
                None
            } else {
                let existing_session = self.session_manager.get_session(&session_key).await;
                if let Some(stale_session) = existing_session
                    .as_ref()
                    .filter(|session| session.should_reset())
                    .cloned()
                {
                    if reset_requested {
                        None
                    } else {
                        Some((stale_session, true))
                    }
                } else {
                    match self.get_or_create_runtime_session(&incoming).await {
                        Some(session) => existing_session.is_none().then_some((session, false)),
                        None => {
                            session_capacity_reached = true;
                            None
                        }
                    }
                }
            };
            drop(busy);

            if let Some((session, auto_reset)) = transition {
                if auto_reset {
                    self.emit_session_finalize(&session_key, &session, "auto_reset")
                        .await;
                    if let Some((old_session, new_session)) = self
                        .session_manager
                        .reset_session_with_snapshots(&session_key)
                        .await
                    {
                        self.clear_session_boundary_security_state(&session_key)
                            .await;
                        let _ = tokio::time::timeout(
                            SESSION_COMMIT_TIMEOUT,
                            self.emit_hook_event(
                                "session:reset",
                                serde_json::json!({
                                    "platform": incoming.platform,
                                    "chat_id": incoming.chat_id,
                                    "user_id": incoming.user_id,
                                    "session_id": session_key,
                                    "old_logical_session_id": old_session.id,
                                    "new_logical_session_id": new_session.id,
                                    "reason": "auto_reset"
                                }),
                            ),
                        )
                        .await;
                        let _ = tokio::time::timeout(
                            SESSION_COMMIT_TIMEOUT,
                            self.emit_session_reset_lifecycle(
                                &session_key,
                                &new_session,
                                "auto_reset",
                            ),
                        )
                        .await;
                    }
                } else {
                    let _ = tokio::time::timeout(
                        SESSION_COMMIT_TIMEOUT,
                        self.emit_hook_event(
                            "session:start",
                            serde_json::json!({
                                "platform": incoming.platform,
                                "chat_id": incoming.chat_id,
                                "user_id": incoming.user_id,
                                "session_id": session_key,
                                "logical_session_id": session.id,
                                "reason": "new"
                            }),
                        ),
                    )
                    .await;
                }
            }
        }
        if session_capacity_reached {
            if let Some(claim) = dedup_claim.as_mut() {
                self.release_dedup_key(dedup_key.as_deref()).await;
                claim.disarm();
            }
            self.send_message_threaded(
                &incoming.platform,
                &incoming.chat_id,
                "Gateway session capacity reached; retry later.",
                None,
                Self::reply_thread_id(&incoming),
            )
            .await?;
            return Ok(GatewayRouteOutcome::Committed);
        }

        let agent_text_override = if is_slash_command {
            let outcome = match self
                .execute_slash_command(
                    &incoming,
                    &sender,
                    &session_key,
                    dedup_key.clone(),
                    &mut dedup_claim,
                )
                .await
            {
                Ok(outcome) => outcome,
                Err(error) => {
                    if let Some(claim) = dedup_claim.as_mut() {
                        self.release_dedup_key(dedup_key.as_deref()).await;
                        claim.disarm();
                    }
                    return Err(error);
                }
            };
            match outcome {
                SlashCommandOutcome::Handled => {
                    if let Some(claim) = dedup_claim.as_mut() {
                        self.complete_dedup_key(dedup_key.as_deref()).await;
                        claim.disarm();
                    }
                    return Ok(GatewayRouteOutcome::Committed);
                }
                SlashCommandOutcome::ForwardToAgent { message } => Some(message),
            }
        } else {
            None
        };

        let (turn_completion_tx, turn_completion_rx) = tokio::sync::oneshot::channel();
        let reply_target = incoming.clone();
        let entry = match agent_text_override {
            Some(message) => QueuedIncoming::from_validated_ingress(incoming, sender)
                .with_agent_text_override(message),
            None => QueuedIncoming::from_validated_ingress(incoming, sender),
        };
        let entry = entry
            .with_dedup_key(dedup_key.clone())
            .with_completion_sender(turn_completion_tx);
        let admission = self
            .claim_session_lane(&session_key, entry, self.busy_input_mode())
            .await;
        let ownership_transferred = match &admission {
            BusyAdmission::Claimed { .. } => true,
            BusyAdmission::Queued(decision) => decision.queued,
            BusyAdmission::AtCapacity => false,
        };
        if ownership_transferred {
            if let Some(claim) = dedup_claim.as_mut() {
                claim.disarm();
            }
        }

        match admission {
            BusyAdmission::Claimed {
                lease,
                mut entry,
            } => {
                entry.clear_completion_sender();
                drop(turn_completion_rx);
                self.start_turn_chain(lease, entry).await
            }
            BusyAdmission::Queued(decision) => {
                if decision.steered && !decision.queued {
                    self.complete_dedup_key(dedup_key.as_deref()).await;
                }
                if self.config.display.busy_ack_enabled() {
                    if let Some(ack) = decision.ack {
                        if let Err(error) = self
                            .send_message_threaded(
                                &reply_target.platform,
                                &reply_target.chat_id,
                                &ack,
                                None,
                                Self::reply_thread_id(&reply_target),
                            )
                            .await
                        {
                            warn!(
                                platform = %reply_target.platform,
                                chat_id = %reply_target.chat_id,
                                error = %error,
                                "failed to send busy-session acknowledgement; queued turn remains owned"
                            );
                        }
                    }
                }
                if decision.queued {
                    turn_completion_rx.await.map_err(|_| {
                        GatewayError::Platform(
                            "queued gateway turn ended before reporting completion".into(),
                        )
                    })?
                } else {
                    Ok(GatewayRouteOutcome::Committed)
                }
            }
            BusyAdmission::AtCapacity => {
                if let Some(claim) = dedup_claim.as_mut() {
                    self.release_dedup_key(dedup_key.as_deref()).await;
                    claim.disarm();
                }
                self.send_message_threaded(
                    &reply_target.platform,
                    &reply_target.chat_id,
                    "Gateway session capacity reached; retry later.",
                    None,
                    Self::reply_thread_id(&reply_target),
                )
                .await?;
                Ok(GatewayRouteOutcome::Committed)
            }
        }
    }

    async fn start_turn_chain(
        &self,
        lease: TurnLease,
        entry: QueuedIncoming,
    ) -> Result<GatewayRouteOutcome, GatewayError> {
        let completion_rx = self.spawn_turn_chain_worker(lease, entry);
        completion_rx.await.map_err(|_| {
            GatewayError::Platform("gateway turn worker ended before reporting completion".into())
        })?
    }

    fn spawn_turn_chain_detached(&self, lease: TurnLease, entry: QueuedIncoming) {
        drop(self.spawn_turn_chain_worker(lease, entry));
    }

    fn spawn_turn_chain_worker(
        &self,
        lease: TurnLease,
        entry: QueuedIncoming,
    ) -> tokio::sync::oneshot::Receiver<Result<GatewayRouteOutcome, GatewayError>> {
        let worker_id = self.next_turn_worker_id.fetch_add(1, Ordering::Relaxed);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let gateway = self.clone();
        let workers = self.turn_workers.clone();
        let handle = tokio::spawn(async move {
            if start_rx.await.is_ok() {
                let _ = AssertUnwindSafe(gateway.run_turn_chain(lease, entry, completion_tx))
                    .catch_unwind()
                    .await;
            }
            workers.lock().unwrap().remove(&worker_id);
        });
        let mut registered_workers = self.turn_workers.lock().unwrap();
        if self.shutting_down.load(Ordering::Acquire) {
            drop(registered_workers);
            handle.abort();
            return completion_rx;
        }
        registered_workers.insert(worker_id, handle);
        drop(registered_workers);
        if start_tx.send(()).is_err() {
            if let Some(handle) = self.turn_workers.lock().unwrap().remove(&worker_id) {
                handle.abort();
            }
        }
        completion_rx
    }

    async fn wait_for_shutdown(&self) {
        let mut receiver = self.shutdown_tx.subscribe();
        if *receiver.borrow() {
            return;
        }
        let _ = receiver.wait_for(|shutdown| *shutdown).await;
    }

    /// Drain one session lane in a detached worker so cancellation of the
    /// caller's future cannot orphan its owned turn or its queued successors.
    async fn run_turn_chain(
        self,
        mut lease: TurnLease,
        mut entry: QueuedIncoming,
        completion_tx: tokio::sync::oneshot::Sender<Result<GatewayRouteOutcome, GatewayError>>,
    ) {
        let mut completion_tx = Some(completion_tx);
        let mut staged_execution_guard = Some(lease.execution_guard());

        loop {
            let _execution_guard = staged_execution_guard
                .take()
                .expect("turn chain lost its execution guard");
            let mutation_baseline = self
                .capture_turn_mutation_baseline(lease.session_key())
                .await;
            let delivery_started = AtomicBool::new(false);
            let incoming = entry.incoming();
            let mut reaction_plan = if self.is_turn_current(&lease).await
                && !self.shutting_down.load(Ordering::Acquire)
            {
                let access_policy = self.platform_access_policy(&incoming.platform).await;
                Self::reaction_lifecycle_plan(incoming, access_policy.as_ref())
            } else {
                None
            };
            let reaction_adapter = if reaction_plan.is_some() {
                self.get_adapter(&incoming.platform).await
            } else {
                None
            };
            let reaction_chat_id = reaction_adapter.as_ref().map_or(
                incoming.chat_id.as_str(),
                |adapter| {
                    adapter.reaction_target_chat_id(
                        incoming.chat_id.as_str(),
                        incoming.thread_id.as_deref(),
                    )
                },
            );
            if reaction_plan.is_some()
                && (!self.is_turn_current(&lease).await
                    || self.shutting_down.load(Ordering::Acquire))
            {
                reaction_plan = None;
            }
            if let (Some(adapter), Some(message_id), Some(plan)) = (
                &reaction_adapter,
                incoming.message_id.as_deref(),
                reaction_plan,
            ) {
                if let Err(err) = Self::await_adapter_call(
                    "start reaction",
                    NONCRITICAL_ADAPTER_TIMEOUT,
                    || adapter.add_reaction(reaction_chat_id, message_id, plan.start),
                )
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
            let processing_result = match AssertUnwindSafe(async {
                tokio::select! {
                    biased;
                    _ = self.wait_for_shutdown() => Err(GatewayError::SessionExpired(
                        "gateway is shutting down".to_string(),
                    )),
                    _ = lease.cancelled() => Err(GatewayError::SessionExpired(
                        "gateway turn was cancelled before reset".to_string(),
                    )),
                    result = self.process_claimed_turn(&entry, &lease, &delivery_started) => result,
                }
            })
            .catch_unwind()
            .await
            {
                Ok(result) => result,
                Err(_) => Err(GatewayError::Platform(
                    "gateway turn handler panicked".to_string(),
                )),
            };
            let classify_outcome = |current: bool, shutting_down: bool| {
                if !current || shutting_down {
                    ProcessingOutcome::Cancelled
                } else if processing_result.is_ok() {
                    ProcessingOutcome::Success
                } else {
                    ProcessingOutcome::Failure
                }
            };
            let mut current = self.is_turn_current(&lease).await;
            let mut shutting_down = self.shutting_down.load(Ordering::Acquire);
            let mut outcome = classify_outcome(current, shutting_down);
            let mut shutdown_rollback_attempted = false;
            if current && shutting_down {
                shutdown_rollback_attempted = true;
                let _ = self
                    .restore_retryable_turn_failure(
                        &lease,
                        &mutation_baseline,
                        delivery_started.load(Ordering::Acquire),
                    )
                    .await;
                current = self.is_turn_current(&lease).await;
                shutting_down = self.shutting_down.load(Ordering::Acquire);
                outcome = classify_outcome(current, shutting_down);
            }
            if let (Some(adapter), Some(message_id), Some(plan)) = (
                &reaction_adapter,
                incoming.message_id.as_deref(),
                reaction_plan,
            ) {
                if let Err(err) = Self::await_adapter_call(
                    "reaction removal",
                    NONCRITICAL_ADAPTER_TIMEOUT,
                    || adapter.remove_reaction(reaction_chat_id, message_id, plan.start),
                )
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
                current = self.is_turn_current(&lease).await;
                shutting_down = self.shutting_down.load(Ordering::Acquire);
                outcome = classify_outcome(current, shutting_down);
            }

            if outcome == ProcessingOutcome::Failure {
                // Preserve local state when it is demonstrably free of a
                // delivery side effect, but raw ingress remains terminal:
                // handler/model/tool execution may already have side effected.
                let _ = self
                    .restore_retryable_turn_failure(
                        &lease,
                        &mutation_baseline,
                        delivery_started.load(Ordering::Acquire),
                    )
                    .await;
            }
            let notification_sent = if outcome == ProcessingOutcome::Failure
                && !matches!(
                    &processing_result,
                    Err(
                        GatewayError::PartialDelivery(_)
                            | GatewayError::AcceptedStateUndurable { .. }
                    )
                )
            {
                if let Err(error) = &processing_result {
                    self.send_processing_error_notification(
                        entry.incoming(),
                        &lease,
                        error,
                        &delivery_started,
                    )
                    .await
                } else {
                    false
                }
            } else {
                false
            };
            let completion = loop {
                let mut busy = self.busy_sessions.write().await;
                current = busy.is_current(&lease);
                shutting_down = self.shutting_down.load(Ordering::Acquire);
                if current && shutting_down && !shutdown_rollback_attempted {
                    drop(busy);
                    shutdown_rollback_attempted = true;
                    let _ = self
                        .restore_retryable_turn_failure(
                            &lease,
                            &mutation_baseline,
                            delivery_started.load(Ordering::Acquire),
                        )
                        .await;
                    continue;
                }
                outcome = classify_outcome(current, shutting_down);
                if outcome == ProcessingOutcome::Cancelled {
                    break busy.complete_turn(&lease, outcome);
                }
                let candidate_outcome = outcome;
                drop(busy);

                let terminal_emoji =
                    if let (Some(adapter), Some(message_id), Some(plan)) = (
                        &reaction_adapter,
                        incoming.message_id.as_deref(),
                        reaction_plan,
                    ) {
                        let emoji = if candidate_outcome == ProcessingOutcome::Success {
                            plan.success
                        } else {
                            plan.error
                        };
                        if let Err(err) = Self::await_adapter_call(
                            "completion reaction",
                            NONCRITICAL_ADAPTER_TIMEOUT,
                            || adapter.add_reaction(reaction_chat_id, message_id, emoji),
                        )
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
                        Some(emoji)
                    } else {
                        None
                    };

                let mut busy = self.busy_sessions.write().await;
                current = busy.is_current(&lease);
                shutting_down = self.shutting_down.load(Ordering::Acquire);
                let final_outcome = classify_outcome(current, shutting_down);
                let needs_shutdown_rollback =
                    current && shutting_down && !shutdown_rollback_attempted;
                if needs_shutdown_rollback || final_outcome != candidate_outcome {
                    drop(busy);
                    if needs_shutdown_rollback {
                        shutdown_rollback_attempted = true;
                        let _ = self
                            .restore_retryable_turn_failure(
                                &lease,
                                &mutation_baseline,
                                delivery_started.load(Ordering::Acquire),
                            )
                            .await;
                    }
                    if let (Some(adapter), Some(message_id), Some(emoji)) = (
                        &reaction_adapter,
                        incoming.message_id.as_deref(),
                        terminal_emoji,
                    ) {
                        if let Err(err) = Self::await_adapter_call(
                            "stale completion reaction removal",
                            NONCRITICAL_ADAPTER_TIMEOUT,
                            || adapter.remove_reaction(reaction_chat_id, message_id, emoji),
                        )
                        .await
                        {
                            debug!(
                                platform = incoming.platform,
                                chat_id = incoming.chat_id,
                                message_id = message_id,
                                "Failed to remove stale completion reaction: {}",
                                err
                            );
                        }
                    }
                    continue;
                }
                outcome = final_outcome;
                break busy.complete_turn(&lease, outcome);
            };
            let (owned, next) = match completion {
                TurnCompletion::Stale => (false, None),
                TurnCompletion::Completed { next } => (true, next),
            };
            // `complete_turn` may atomically claim a queued successor. Guard it
            // before any adapter or dedup await so reset can always join it.
            let next_execution_guard = next
                .as_ref()
                .map(|(next_lease, _)| next_lease.execution_guard());
            if owned {
                // This route has progressed past admission. Even when local
                // state rolls back, handler/model/tool work may already have
                // crossed an external boundary, so retain terminal dedup.
                self.complete_dedup_key(entry.dedup_key()).await;
            }
            // `entry` is mutably consumed below for its completion sender.
            // Snapshot only the eventual partial-record identity first so no
            // borrow of the ingress receipt survives that handoff.
            let incoming_platform = incoming.platform.clone();
            let incoming_chat_id = incoming.chat_id.clone();
            let incoming_thread_id = incoming.thread_id.clone();
            // A reset may promote an already-queued entry into a fresh detached
            // worker. Its ingress receipt remains authoritative; claimed turns
            // clear that receipt and use the worker-lifecycle sender instead.
            if let Some(completion_tx) = entry
                .take_completion_sender()
                .or_else(|| completion_tx.take())
            {
                let caller_result = if !owned {
                    // Preserve the stale/reset non-owner behavior: a newer
                    // owner is responsible for the same lane's completion.
                    Ok(GatewayRouteOutcome::Committed)
                } else if outcome == ProcessingOutcome::Cancelled {
                    // This worker was admitted and may have invoked arbitrary
                    // handler/model/tool code. Cancellation is therefore
                    // quarantine-only even when local rollback succeeded.
                    let error = match processing_result {
                        Err(error) => error,
                        Ok(()) => GatewayError::SessionExpired(
                            "gateway shut down with an uncertain turn boundary".to_string(),
                        ),
                    };
                    Ok(GatewayRouteOutcome::Uncertain(error))
                } else {
                    match processing_result {
                        Ok(()) => Ok(GatewayRouteOutcome::Committed),
                        Err(GatewayError::PartialDelivery(partial)) => {
                            Ok(GatewayRouteOutcome::Partial(partial))
                        }
                        Err(GatewayError::AcceptedStateUndurable {
                            accepted_message_ids,
                            repair,
                        }) => {
                            let accepted_chunks = accepted_message_ids.len().max(1);
                            Ok(GatewayRouteOutcome::Partial(
                                hermes_core::errors::PartialDelivery {
                                    platform: incoming_platform,
                                    chat_id: incoming_chat_id,
                                    thread_id: incoming_thread_id,
                                    reply_to_message_id: None,
                                    reply_to_mode: "none".to_string(),
                                    non_conversational: false,
                                    next_chunk_index: 0,
                                    remaining_text: String::new(),
                                    accepted_message_ids,
                                    accepted_chunks,
                                    retryable: false,
                                    retry_after: None,
                                    failure: repair.clone(),
                                    repair_required: Some(repair),
                                },
                            ))
                        }
                        // A fully accepted error notification conclusively
                        // handles this inbound turn. Its original failure is
                        // still logged above; retaining it in the Discord
                        // ingress spool would otherwise fill capacity with
                        // ordinary, already-notified model/tool failures.
                        Err(_) if notification_sent => Ok(GatewayRouteOutcome::Committed),
                        // Any rejected, timed-out, panicked, or ambiguous
                        // notification is not safe to replay as raw ingress:
                        // hooks/tools may already have side effected.
                        Err(error) => Ok(GatewayRouteOutcome::Uncertain(error)),
                    }
                };
                let _ = completion_tx.send(caller_result);
            }

            let Some((next_lease, next_entry)) = next else {
                break;
            };
            staged_execution_guard = next_execution_guard;
            lease = next_lease;
            entry = next_entry;
        }
    }

    async fn is_turn_current(&self, lease: &TurnLease) -> bool {
        self.busy_sessions.read().await.is_current(lease)
    }

    async fn session_turn_lock(&self, session_key: &str) -> SessionTurnLock {
        let mut registry = self.session_turn_locks.lock();
        if let Some((key, weak)) = registry.get_key_value(session_key) {
            if let Some(lock) = weak.upgrade() {
                return SessionTurnLock {
                    key: key.clone(),
                    lock: Some(lock),
                    registry: self.session_turn_locks.clone(),
                };
            }
        }
        let key = Arc::<str>::from(session_key);
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        registry.insert(key.clone(), Arc::downgrade(&lock));
        SessionTurnLock {
            key,
            lock: Some(lock),
            registry: self.session_turn_locks.clone(),
        }
    }

    async fn claim_session_lane(
        &self,
        session_key: &str,
        entry: QueuedIncoming,
        mode: BusyInputMode,
    ) -> BusyAdmission {
        let turn_lock = self.session_turn_lock(session_key).await;
        let _turn_guard = turn_lock.lock().await;
        self.busy_sessions.write().await.claim_idle_or_enqueue(
            session_key,
            entry,
            mode,
            std::time::Instant::now(),
        )
    }

    async fn enqueue_session_if_active(
        &self,
        session_key: &str,
        entry: QueuedIncoming,
    ) -> QueueAdmission {
        let turn_lock = self.session_turn_lock(session_key).await;
        let _turn_guard = turn_lock.lock().await;
        self.busy_sessions
            .write()
            .await
            .enqueue_if_active(session_key, entry)
    }

    async fn steer_session_if_active(
        &self,
        session_key: &str,
        entry: QueuedIncoming,
    ) -> Option<BusyMessageDecision> {
        let turn_lock = self.session_turn_lock(session_key).await;
        let _turn_guard = turn_lock.lock().await;
        let mut busy = self.busy_sessions.write().await;
        if !busy.is_active(session_key) {
            return None;
        }
        match busy.claim_idle_or_enqueue(
            session_key,
            entry,
            BusyInputMode::Steer,
            std::time::Instant::now(),
        ) {
            BusyAdmission::Queued(decision) => Some(decision),
            BusyAdmission::Claimed { .. } => None,
            BusyAdmission::AtCapacity => Some(BusyMessageDecision {
                handled: true,
                queued: false,
                interrupted: false,
                steered: false,
                ack: Some("The active session queue is full. Retry the prompt later.".to_string()),
            }),
        }
    }

    async fn try_lock_session_mutation(
        &self,
        session_key: &str,
    ) -> Option<OwnedSessionMutationGuard> {
        let lock = self.session_turn_lock(session_key).await;
        let guard = lock
            .lock
            .as_ref()
            .expect("session lock missing")
            .clone()
            .try_lock_owned()
            .ok()?;
        if self.busy_sessions.read().await.is_active(session_key) {
            return None;
        }
        Some(OwnedSessionMutationGuard {
            guard: Some(guard),
            lock: Some(lock),
        })
    }

    async fn process_claimed_turn(
        &self,
        entry: &QueuedIncoming,
        lease: &TurnLease,
        delivery_started: &AtomicBool,
    ) -> Result<(), GatewayError> {
        let session_key = lease.session_key();
        let incoming = entry.incoming();
        let agent_text = entry.agent_text_override().unwrap_or(&incoming.text);
        let enriched_text =
            self.enrich_message_with_transcription(&self.enrich_message_with_vision(agent_text));

        if !self.is_turn_current(lease).await {
            return Err(GatewayError::SessionExpired(
                "gateway turn was invalidated before processing".to_string(),
            ));
        }

        let messages = {
            let turn_lock = self.session_turn_lock(session_key).await;
            let _turn_guard = turn_lock.lock().await;
            let busy = self.busy_sessions.read().await;
            if !busy.is_current(lease) {
                return Err(GatewayError::SessionExpired(
                    "gateway turn was invalidated before processing".to_string(),
                ));
            }
            let session_was_missing = self
                .session_manager
                .get_session(session_key)
                .await
                .is_none();
            let created_session = if session_was_missing {
                self.get_or_create_runtime_session(incoming).await
            } else {
                None
            };
            let session_creation_failed = session_was_missing && created_session.is_none();
            drop(busy);
            if let Some(session) = created_session {
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.emit_hook_event(
                        "session:start",
                        serde_json::json!({
                            "platform": incoming.platform,
                            "chat_id": incoming.chat_id,
                            "user_id": incoming.user_id,
                            "session_id": session_key,
                            "logical_session_id": session.id,
                            "reason": "post_boundary"
                        }),
                    ),
                )
                .await;
            }
            let busy = self.busy_sessions.read().await;
            if !busy.is_current(lease) {
                return Err(GatewayError::SessionExpired(
                    "gateway turn was invalidated before session mutation".to_string(),
                ));
            }
            drop(busy);
            match (session_creation_failed, entry.turn_kind()) {
                (true, _) => Err("Gateway session capacity reached; retry later."),
                (false, QueuedTurnKind::Message) => {
                    self.maybe_apply_smart_model_routing(session_key, &enriched_text)
                        .await;
                    if self
                        .session_manager
                        .try_add_user_message(session_key, Message::user(enriched_text))
                        .await
                    {
                        self.bump_input_usage(session_key, agent_text.chars().count())
                            .await;
                        Ok(Some(self.session_manager.get_messages(session_key).await))
                    } else {
                        Err("This message exceeds the available session context capacity. Shorten it or retry after context is freed.")
                    }
                }
                (false, QueuedTurnKind::Retry) => {
                    let mut messages = self.session_manager.get_messages(session_key).await;
                    if matches!(
                        messages.last().map(|message| message.role),
                        Some(MessageRole::Assistant)
                    ) {
                        messages.pop();
                    }
                    if messages.is_empty() {
                        Ok(None)
                    } else {
                        self.session_manager
                            .replace_messages(session_key, messages.clone())
                            .await;
                        Ok(Some(messages))
                    }
                }
            }
        };

        match messages {
            Ok(Some(messages)) if self.config.streaming_enabled => {
                self.route_streaming(
                    incoming,
                    messages,
                    session_key,
                    lease,
                    delivery_started,
                )
                .await
            }
            Ok(Some(messages)) => {
                self.route_non_streaming(
                    incoming,
                    messages,
                    session_key,
                    lease,
                    delivery_started,
                )
                .await
            }
            Ok(None) => {
                self.send_turn_bound_message(incoming, lease, "No previous message to retry.")
                    .await
            }
            Err(message) => self.send_turn_bound_message(incoming, lease, message).await,
        }

    }

    async fn capture_turn_mutation_baseline(
        &self,
        session_key: &str,
    ) -> TurnMutationBaseline {
        let turn_lock = self.session_turn_lock(session_key).await;
        let _turn_guard = turn_lock.lock().await;
        TurnMutationBaseline {
            session: self.session_manager.get_session(session_key).await,
            usage: self.usage_stats.read().await.get(session_key).cloned(),
            runtime: self.runtime_state.read().await.get(session_key).cloned(),
        }
    }

    async fn restore_retryable_turn_failure(
        &self,
        lease: &TurnLease,
        baseline: &TurnMutationBaseline,
        delivery_started: bool,
    ) -> bool {
        let turn_lock = self.session_turn_lock(lease.session_key()).await;
        let _turn_guard = turn_lock.lock().await;
        if !self.busy_sessions.read().await.is_current(lease) {
            return false;
        }
        if delivery_started {
            return false;
        }
        let assistant_messages_before = baseline
            .usage
            .as_ref()
            .map(|usage| usage.assistant_messages)
            .unwrap_or(0);
        let assistant_messages_after = self
            .usage_stats
            .read()
            .await
            .get(lease.session_key())
            .map(|usage| usage.assistant_messages)
            .unwrap_or(0);
        if assistant_messages_after > assistant_messages_before {
            return false;
        }

        self.session_manager
            .restore_session_snapshot(lease.session_key(), baseline.session.clone())
            .await;
        {
            let mut usage = self.usage_stats.write().await;
            match baseline.usage.as_ref() {
                Some(snapshot) => {
                    usage.insert(lease.session_key().to_string(), snapshot.clone());
                }
                None => {
                    usage.remove(lease.session_key());
                }
            }
        }
        {
            let mut runtime = self.runtime_state.write().await;
            match baseline.runtime.as_ref() {
                Some(snapshot) => {
                    runtime.insert(lease.session_key().to_string(), snapshot.clone());
                }
                None => {
                    runtime.remove(lease.session_key());
                }
            }
        }
        true
    }

    async fn warn_fail_closed_default_once(&self, platform: &str, policy: &PlatformAccessPolicy) {
        let Some(message) = policy.fail_closed_default_warning(platform) else {
            return;
        };
        let key = format!("{}:{message}", platform.trim().to_ascii_lowercase());
        let mut warned = self.fail_closed_default_warnings.write().await;
        if warned.insert(key) {
            warn!(platform = %platform, "{}", message);
        }
    }

    async fn send_processing_error_notification(
        &self,
        incoming: &IncomingMessage,
        lease: &TurnLease,
        original_error: &GatewayError,
        delivery_started: &AtomicBool,
    ) -> bool {
        let mut detail: String = original_error.to_string().chars().take(300).collect();
        if detail.trim().is_empty() {
            detail = "no details available".to_string();
        }
        let notice = format!(
            "Sorry, I encountered an error ({}).\n{}\nTry again or use /reset to start a fresh session.",
            Self::gateway_error_variant_name(original_error),
            detail
        );

        let turn_lock = self.session_turn_lock(lease.session_key()).await;
        let _commit_guard = turn_lock.lock().await;
        let busy = self.busy_sessions.read().await;
        if !busy.is_current(lease) {
            return false;
        }
        drop(busy);
        // The adapter may accept this notification even if the await later
        // errors or shutdown cancels it, so establish the effect boundary
        // immediately before the first network attempt.
        delivery_started.store(true, Ordering::Release);
        match tokio::time::timeout(
            SESSION_COMMIT_TIMEOUT,
            AssertUnwindSafe(self.send_message_threaded(
                &incoming.platform,
                &incoming.chat_id,
                &notice,
                Some(ParseMode::Plain),
                Self::reply_thread_id(incoming),
            ))
            .catch_unwind(),
        )
        .await
        {
            Ok(Ok(Ok(()))) => true,
            Ok(Ok(Err(notify_error))) => {
                error!(
                    platform = %incoming.platform,
                    chat_id = %incoming.chat_id,
                    original_error = %original_error,
                    notify_error = %notify_error,
                    "failed to send gateway error notification"
                );
                false
            }
            Ok(Err(_)) => {
                error!(
                    platform = %incoming.platform,
                    chat_id = %incoming.chat_id,
                    original_error = %original_error,
                    "gateway error notification panicked"
                );
                false
            }
            Err(_) => {
                error!(
                    platform = %incoming.platform,
                    chat_id = %incoming.chat_id,
                    original_error = %original_error,
                    "timed out sending gateway error notification"
                );
                false
            }
        }
    }

    fn gateway_error_variant_name(error: &GatewayError) -> &'static str {
        match error {
            GatewayError::ConnectionFailed(_) => "ConnectionFailed",
            GatewayError::SendFailed(_) => "SendFailed",
            GatewayError::MediaPreflightFailed { .. } => "MediaPreflightFailed",
            GatewayError::MediaRemoteRejected { .. } => "MediaRemoteRejected",
            GatewayError::Platform(_) => "Platform",
            GatewayError::Auth(_) => "Auth",
            GatewayError::RateLimited { .. } => "RateLimited",
            GatewayError::SessionExpired(_) => "SessionExpired",
            GatewayError::PartialDelivery(_) => "PartialDelivery",
            GatewayError::AcceptedStateUndurable { .. } => "AcceptedStateUndurable",
        }
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
            .env_clear()
            .envs(hermes_core::subprocess_env::hermes_subprocess_env(false))
            .arg("-c")
            .arg(command)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .suppress_windows_console()
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

}
