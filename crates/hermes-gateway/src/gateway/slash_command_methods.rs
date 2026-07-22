impl Gateway {
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

    async fn mirror_session_title_to_platform_thread(
        &self,
        incoming: &IncomingMessage,
        title: &str,
    ) -> Option<String> {
        if !incoming.platform.eq_ignore_ascii_case("discord") {
            return None;
        }
        let thread_id = incoming
            .thread_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let adapter = self.get_adapter(&incoming.platform).await?;
        match Self::await_adapter_call("thread rename", SESSION_COMMIT_TIMEOUT, || {
            adapter.rename_thread(thread_id, title)
        })
        .await
        {
            Ok(_) => None,
            Err(err) => {
                warn!(
                    platform = incoming.platform,
                    chat_id = incoming.chat_id,
                    thread_id,
                    error = %err,
                    "failed to mirror session title to platform thread"
                );
                Some(format!("⚠️ Discord thread title mirror failed: {err}"))
            }
        }
    }

    fn format_session_list(&self, heading: &str, sessions: &[Session]) -> String {
        if sessions.is_empty() {
            return format!("📚 No {} found for your user.", heading.to_ascii_lowercase());
        }

        let mut out = format!("📚 **{}:**\n\n", heading);
        for s in sessions {
            let key = self
                .session_manager
                .compose_session_key_for_scope(s.conversation_scope());
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
    }

    async fn apply_verbose_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
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
        let mut modes = self.tool_progress_modes.write().await;
        let current = modes
            .get(&platform)
            .cloned()
            .unwrap_or_else(|| default_mode.clone());
        let next = Self::next_tool_progress_mode(&current).to_string();
        let mut states = self.runtime_state.write().await;
        modes.insert(platform.clone(), next.clone());
        let state = states.entry(session_key.to_string()).or_default();
        state.tool_progress = Some(next.clone());
        state.verbose = next == "verbose";
        drop(states);
        drop(modes);
        self.commit_command_dedup(dedup_key, dedup_claim).await;

        Ok(format!(
            "📝 Tool progress for {platform}: {}",
            next.to_ascii_uppercase()
        ))
    }

    async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        sender: &IncomingSender,
        session_key: &str,
        dedup_key: Option<String>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) -> Result<SlashCommandOutcome, GatewayError> {
        let command_text = Self::normalize_slash_command_text(&incoming.text);
        let memory_governance_command = matches!(
            crate::commands::canonical_command_name(&command_text).as_deref(),
            Some("memory") | Some("forget_memory") | Some("memory_forget")
        );
        if !memory_governance_command {
            if let Some(reply) = self.resolve_quick_command(&command_text).await? {
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                return Ok(SlashCommandOutcome::Handled);
            }
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
        let _mutation_guard = if Self::command_requires_session_mutation_barrier(&result) {
            let Some(guard) = self.try_lock_session_mutation(session_key).await else {
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    "Session is busy. Retry this command after the active turn or reset finishes.",
                    None,
                )
                .await?;
                return Ok(SlashCommandOutcome::Handled);
            };
            Some(guard)
        } else {
            None
        };
        let handled = self
            .apply_command_result(
                incoming,
                sender,
                session_key,
                result,
                dedup_key,
                dedup_claim,
            )
            .await?;
        Ok(if handled {
            SlashCommandOutcome::Handled
        } else {
            SlashCommandOutcome::ForwardToAgent {
                message: command_text,
            }
        })
    }

    fn command_requires_session_mutation_barrier(result: &GatewayCommandResult) -> bool {
        matches!(
            result,
            GatewayCommandResult::SwitchModel { .. }
                | GatewayCommandResult::SwitchPersonality { .. }
                | GatewayCommandResult::ForgetMemory
                | GatewayCommandResult::CompressContext(_)
                | GatewayCommandResult::EnableTool { .. }
                | GatewayCommandResult::DisableTool { .. }
                | GatewayCommandResult::SwitchSession { .. }
                | GatewayCommandResult::ShowBudget {
                    new_budget: Some(_)
                }
                | GatewayCommandResult::ToggleVerbose(_)
                | GatewayCommandResult::ToggleYolo(_)
                | GatewayCommandResult::SetHome { .. }
                | GatewayCommandResult::SetTitle { .. }
                | GatewayCommandResult::ToggleReasoning(_)
                | GatewayCommandResult::SwitchFast { .. }
                | GatewayCommandResult::Undo
                | GatewayCommandResult::SwitchProvider { .. }
                | GatewayCommandResult::SwitchProfile { .. }
                | GatewayCommandResult::SwitchBranch { branch: Some(_) }
                | GatewayCommandResult::Rollback { .. }
        )
    }

    async fn commit_command_dedup(
        &self,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) {
        if dedup_claim.is_none() {
            return;
        }
        self.complete_dedup_key(dedup_key).await;
        Self::transfer_command_dedup(dedup_claim);
    }

    fn transfer_command_dedup(dedup_claim: &mut Option<DedupClaimGuard>) {
        if let Some(mut claim) = dedup_claim.take() {
            claim.disarm();
        }
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
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
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
        self.commit_command_dedup(dedup_key, dedup_claim).await;
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
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
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
        self.commit_command_dedup(dedup_key, dedup_claim).await;

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

    async fn run_reset_session_command(
        &self,
        incoming: IncomingMessage,
        session_key: String,
        reply: String,
        mut dedup_claim: Option<DedupClaimGuard>,
    ) -> Result<(), GatewayError> {
        let reset_committed = Arc::new(AtomicBool::new(false));
        if let Some(claim) = dedup_claim.as_mut() {
            claim.complete_on_drop_when(reset_committed.clone());
        }
        let worker_id = self.next_turn_worker_id.fetch_add(1, Ordering::Relaxed);
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        let gateway = self.clone();
        let workers = self.turn_workers.clone();
        let handle = tokio::spawn(async move {
            if start_rx.await.is_ok() {
                let result = match AssertUnwindSafe(gateway.perform_reset_session_command(
                    incoming,
                    session_key,
                    reply,
                    reset_committed.clone(),
                ))
                .catch_unwind()
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(GatewayError::Platform(
                        "gateway reset worker panicked before cleanup".to_string(),
                    )),
                };
                if reset_committed.load(Ordering::Acquire) || result.is_ok() {
                    if let Some(claim) = dedup_claim.as_mut() {
                        let key = claim.key.clone();
                        gateway.complete_dedup_key(key.as_deref()).await;
                        claim.disarm();
                    }
                }
                let _ = completion_tx.send(result);
            }
            workers.lock().unwrap().remove(&worker_id);
        });
        let mut handle = Some(handle);
        let rejected = {
            let mut registered_workers = self.turn_workers.lock().unwrap();
            if self.shutting_down.load(Ordering::Acquire) {
                true
            } else {
                registered_workers.insert(
                    worker_id,
                    handle.take().expect("reset worker handle missing"),
                );
                false
            }
        };
        if rejected {
            handle
                .take()
                .expect("rejected reset worker handle missing")
                .abort();
            return Err(GatewayError::Platform(
                "gateway is shutting down".to_string(),
            ));
        }
        if start_tx.send(()).is_err() {
            if let Some(handle) = self.turn_workers.lock().unwrap().remove(&worker_id) {
                handle.abort();
            }
        }
        completion_rx.await.map_err(|_| {
            GatewayError::Platform(
                "gateway reset worker ended before reporting completion".to_string(),
            )
        })?
    }

    async fn perform_reset_session_command(
        &self,
        incoming: IncomingMessage,
        session_key: String,
        reply: String,
        reset_committed: Arc<AtomicBool>,
    ) -> Result<(), GatewayError> {
        let admission = {
            let mut busy = self.busy_sessions.write().await;
            busy.begin_reset_session(&session_key)
        };
        let (reset_lease, revoked_lease, released_dedup_keys) = match admission {
            ResetAdmission::Started {
                lease,
                revoked_lease,
                released_dedup_keys,
            } => (lease, revoked_lease, released_dedup_keys),
            ResetAdmission::AlreadyInProgress {
                draining_after_failure,
            } => {
                let message = if draining_after_failure {
                    "A previous reset is draining a turn that did not stop safely; retry /reset after queued messages resume."
                } else {
                    "Session reset is already in progress; retry after it completes."
                };
                tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        message,
                        None,
                    ),
                )
                .await
                .map_err(|_| {
                    GatewayError::Platform("gateway reset status delivery timed out".to_string())
                })??;
                return if draining_after_failure {
                    Err(GatewayError::Platform(message.to_string()))
                } else {
                    Ok(())
                };
            }
            ResetAdmission::TransitionInProgress => {
                let message =
                    "A session lifecycle transition is in progress; retry /reset after it completes.";
                tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        message,
                        None,
                    ),
                )
                .await
                .map_err(|_| {
                    GatewayError::Platform("gateway reset status delivery timed out".to_string())
                })??;
                return Err(GatewayError::Platform(message.to_string()));
            }
            ResetAdmission::AtCapacity => {
                let message = "Gateway session capacity reached; retry reset later.";
                tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        message,
                        None,
                    ),
                )
                .await
                .map_err(|_| {
                    GatewayError::Platform("gateway reset status delivery timed out".to_string())
                })??;
                return Err(GatewayError::Platform(message.to_string()));
            }
        };
        self.complete_dedup_keys(&released_dedup_keys).await;
        if let Some(revoked_lease) = revoked_lease {
            if tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                revoked_lease.wait_finished(),
            )
            .await
            .is_err()
            {
                let marked_draining = self
                    .busy_sessions
                    .write()
                    .await
                    .mark_reset_draining_after_failure(&reset_lease);
                if !marked_draining {
                    return Err(GatewayError::SessionExpired(
                        "gateway reset barrier changed before timeout recovery".to_string(),
                    ));
                }
                self.spawn_reset_barrier_recovery(
                    revoked_lease,
                    reset_lease,
                    incoming.clone(),
                );
                let message = "Session reset was not applied because the active turn did not stop safely. Queued messages remain paused until that turn exits; retry /reset after they resume.";
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        message,
                        None,
                    ),
                )
                .await;
                return Err(GatewayError::Platform(message.to_string()));
            }
        }

        self.run_reset_barrier_work(&reset_lease, async {

        {
            let turn_lock = self.session_turn_lock(&session_key).await;
            let _reset_guard = turn_lock.lock().await;
            let current_session = self.session_manager.get_session(&session_key).await;
            let _ = tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                self.emit_hook_event(
                    "session:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": current_session.as_ref().map(|s| s.id.clone())
                    }),
                ),
            )
            .await;
            if let Some(old_session) = current_session.as_ref() {
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.emit_session_finalize(&session_key, old_session, "reset"),
                )
                .await;
            }

            let reset_snapshot = self
                .session_manager
                .reset_session_with_snapshots(&session_key)
                .await;
            reset_committed.store(true, Ordering::Release);
            self.clear_session_boundary_security_state(&session_key)
                .await;
            let reset_session = reset_snapshot
                .as_ref()
                .map(|(_, new_session)| new_session)
                .or(current_session.as_ref());
            let _ = tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                self.emit_hook_event(
                    "session:reset",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": reset_session.map(|s| s.id.clone())
                    }),
                ),
            )
            .await;
            if let Some(new_session) = reset_session {
                let _ = tokio::time::timeout(
                    SESSION_COMMIT_TIMEOUT,
                    self.emit_session_reset_lifecycle(&session_key, new_session, "reset"),
                )
                .await;
            }
        }
            let reply_result = match tokio::time::timeout(
                SESSION_COMMIT_TIMEOUT,
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(GatewayError::Platform(
                    "gateway reset reply delivery timed out".to_string(),
                )),
            };
            reply_result
        })
        .await
    }

    async fn run_reset_barrier_work<F>(
        &self,
        reset_lease: &TurnLease,
        work: F,
    ) -> Result<(), GatewayError>
    where
        F: Future<Output = Result<(), GatewayError>>,
    {
        let work_result = match AssertUnwindSafe(work).catch_unwind().await {
            Ok(result) => result,
            Err(_) => Err(GatewayError::Platform(
                "gateway reset worker panicked".to_string(),
            )),
        };
        let completion = {
            let mut busy = self.busy_sessions.write().await;
            busy.complete_turn(reset_lease, ProcessingOutcome::Success)
        };
        let barrier_result = match completion {
            TurnCompletion::Completed {
                next: Some((lease, entry)),
            } => {
                self.spawn_turn_chain_detached(lease, entry);
                Ok(())
            }
            TurnCompletion::Completed { next: None } => Ok(()),
            TurnCompletion::Stale => Err(GatewayError::SessionExpired(
                "gateway reset barrier was invalidated before completion".to_string(),
            )),
        };
        work_result?;
        barrier_result
    }

    fn spawn_reset_barrier_recovery(
        &self,
        revoked_lease: TurnLease,
        reset_lease: TurnLease,
        incoming: IncomingMessage,
    ) {
        let gateway = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                _ = revoked_lease.wait_finished() => {
                    match gateway.release_reset_barrier(&reset_lease).await {
                        Ok(()) => {
                            let _ = tokio::time::timeout(
                                SESSION_COMMIT_TIMEOUT,
                                gateway.send_message_threaded(
                                    &incoming.platform,
                                    &incoming.chat_id,
                                    "Queued messages resumed; the previous reset was not applied. Retry /reset if it is still needed.",
                                    None,
                                    Gateway::reply_thread_id(&incoming),
                                ),
                            )
                            .await;
                        }
                        Err(error) => {
                            warn!(%error, "Failed to recover a timed-out reset barrier");
                        }
                    }
                }
                _ = gateway.wait_for_shutdown() => {}
            }
        });
    }

    async fn release_reset_barrier(
        &self,
        reset_lease: &TurnLease,
    ) -> Result<(), GatewayError> {
        let completion = {
            let mut busy = self.busy_sessions.write().await;
            busy.complete_turn(reset_lease, ProcessingOutcome::Cancelled)
        };
        match completion {
            TurnCompletion::Completed {
                next: Some((lease, entry)),
            } => {
                self.spawn_turn_chain_detached(lease, entry);
                Ok(())
            }
            TurnCompletion::Completed { next: None } => Ok(()),
            TurnCompletion::Stale => Err(GatewayError::SessionExpired(
                "gateway reset barrier was invalidated before recovery".to_string(),
            )),
        }
    }

    fn memory_forget_completed(response: &GatewayMemoryForgetResponse) -> bool {
        response.erased
            && response.remediation.is_none()
            && !response.providers.is_empty()
            && response.providers.iter().all(|provider| {
                matches!(
                    provider.state,
                    GatewayMemoryForgetProviderState::VerifiedAbsent
                )
            })
    }

    fn render_memory_forget_response(response: &GatewayMemoryForgetResponse) -> String {
        let completed = Self::memory_forget_completed(response);
        let mut text = if completed {
            "Persistent memory was removed for the current authenticated conversation."
                .to_string()
        } else {
            "Persistent-memory removal was not confirmed for the current authenticated conversation."
                .to_string()
        };
        if !response.providers.is_empty() {
            text.push_str("\n\nProvider status:");
            for provider in &response.providers {
                let state = match &provider.state {
                    GatewayMemoryForgetProviderState::VerifiedAbsent => "verified absent".to_string(),
                    GatewayMemoryForgetProviderState::Incomplete {
                        error: Some(hermes_core::MemoryProviderGovernanceError::Unsupported {
                            operation,
                            ..
                        }),
                        ..
                    } => format!("unsupported ({})", operation.as_str()),
                    GatewayMemoryForgetProviderState::Incomplete {
                        error: Some(hermes_core::MemoryProviderGovernanceError::Failed {
                            operation,
                            ..
                        }),
                        ..
                    } => format!("failed ({})", operation.as_str()),
                    GatewayMemoryForgetProviderState::Incomplete {
                        error: Some(hermes_core::MemoryProviderGovernanceError::Incomplete {
                            operation,
                            ..
                        }),
                        ..
                    } => format!("incomplete ({})", operation.as_str()),
                    GatewayMemoryForgetProviderState::Incomplete {
                        error: None,
                        readback_absent: false,
                    } => "readback residue detected".to_string(),
                    GatewayMemoryForgetProviderState::Incomplete {
                        error: None,
                        readback_absent: true,
                    } => "incomplete".to_string(),
                };
                text.push_str(&format!("\n• {}: {state}", provider.provider));
            }
        }
        if !completed {
            let remediation = match response.remediation {
                Some(GatewayMemoryForgetRemediation::Reconfigure) => {
                    "Restore the configured memory provider, then retry this command."
                }
                Some(GatewayMemoryForgetRemediation::Retry) | None => {
                    "Retry this command after the provider is available."
                }
            };
            text.push_str("\n\n");
            text.push_str(remediation);
        }
        text
    }

    async fn apply_memory_forget_command(
        &self,
        incoming: &IncomingMessage,
        sender: &IncomingSender,
        session_key: &str,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) -> Result<(), GatewayError> {
        if !matches!(sender.kind, IncomingSenderKind::Human) {
            self.send_message(
                &incoming.platform,
                &incoming.chat_id,
                "Memory governance requires an authenticated human gateway identity. No memory operation was started.",
                None,
            )
            .await?;
            return Ok(());
        }

        let context = self.build_runtime_context(incoming, session_key, None).await;
        if context.memory_subject().is_err() {
            self.send_message(
                &incoming.platform,
                &incoming.chat_id,
                "Memory governance could not verify the current gateway identity. No memory operation was started.",
                None,
            )
            .await?;
            return Ok(());
        }

        let handler = self.memory_forget_handler.read().await.clone();
        let response = match handler {
            Some(handler) => match handler(context).await {
                Ok(response) => response,
                Err(_) => GatewayMemoryForgetResponse {
                    erased: false,
                    providers: Vec::new(),
                    remediation: Some(GatewayMemoryForgetRemediation::Retry),
                },
            },
            None => GatewayMemoryForgetResponse {
                erased: false,
                providers: Vec::new(),
                remediation: Some(GatewayMemoryForgetRemediation::Reconfigure),
            },
        };
        self.commit_command_dedup(dedup_key, dedup_claim).await;
        let text = Self::render_memory_forget_response(&response);
        self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
            .await
    }

    fn memory_export_completed(response: &GatewayMemoryExportResponse) -> bool {
        response.remediation.is_none() && response.artifact.is_some()
    }

    fn render_memory_export_response(
        metadata: Option<&GatewayMemoryExportDownloadMetadata>,
        remediation: Option<GatewayMemoryExportRemediation>,
    ) -> String {
        if let Some(metadata) = metadata {
            return format!(
                "Persistent-memory export is ready for the current authenticated conversation.\n\n\
                 One-time download reference: `{}`\n\
                 Manifest digest: `{}`\n\
                 Records: {}\n\
                 Pages: {}\n\
                 Providers: {}",
                metadata.download_ref,
                metadata.manifest_digest.as_str(),
                metadata.record_count,
                metadata.page_count,
                metadata.provider_count,
            );
        }

        let remediation = match remediation {
            Some(GatewayMemoryExportRemediation::Reconfigure) => {
                "Restore the configured memory provider, then retry this command."
            }
            Some(GatewayMemoryExportRemediation::Retry) | None => {
                "Retry this command after the provider is available."
            }
        };
        format!(
            "Persistent-memory export was not completed for the current authenticated conversation.\n\n{remediation}"
        )
    }

    async fn apply_memory_export_command(
        &self,
        incoming: &IncomingMessage,
        sender: &IncomingSender,
        session_key: &str,
        dedup_key: Option<&str>,
        dedup_claim: &mut Option<DedupClaimGuard>,
    ) -> Result<(), GatewayError> {
        if !matches!(sender.kind, IncomingSenderKind::Human) {
            self.send_message(
                &incoming.platform,
                &incoming.chat_id,
                "Memory governance requires an authenticated human gateway identity. No memory operation was started.",
                None,
            )
            .await?;
            return Ok(());
        }

        let context = self.build_runtime_context(incoming, session_key, None).await;
        if context.memory_subject().is_err() {
            self.send_message(
                &incoming.platform,
                &incoming.chat_id,
                "Memory governance could not verify the current gateway identity. No memory operation was started.",
                None,
            )
            .await?;
            return Ok(());
        }

        let handler = self.memory_export_handler.read().await.clone();
        let response = match handler {
            Some(handler) => match handler(context.clone()).await {
                Ok(response) => response,
                Err(_) => GatewayMemoryExportResponse {
                    artifact: None,
                    remediation: Some(GatewayMemoryExportRemediation::Retry),
                },
            },
            None => GatewayMemoryExportResponse {
                artifact: None,
                remediation: Some(GatewayMemoryExportRemediation::Reconfigure),
            },
        };
        let metadata = if Self::memory_export_completed(&response) {
            match response.artifact.as_ref() {
                Some(artifact) => self.issue_memory_export_download(&context, artifact).await.ok(),
                None => None,
            }
        } else {
            None
        };
        let remediation = if metadata.is_some() {
            None
        } else {
            response
                .remediation
                .or(Some(GatewayMemoryExportRemediation::Retry))
        };
        self.commit_command_dedup(dedup_key, dedup_claim).await;
        let text = Self::render_memory_export_response(metadata.as_ref(), remediation);
        self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
            .await
    }

    async fn apply_command_result(
        &self,
        incoming: &IncomingMessage,
        sender: &IncomingSender,
        session_key: &str,
        result: GatewayCommandResult,
        dedup_key: Option<String>,
        dedup_claim: &mut Option<DedupClaimGuard>,
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
                self.run_reset_session_command(
                    incoming.clone(),
                    session_key.to_string(),
                    reply,
                    dedup_claim.take(),
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::ForgetMemory => {
                self.apply_memory_forget_command(
                    incoming,
                    sender,
                    session_key,
                    dedup_key.as_deref(),
                    dedup_claim,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::ExportMemory => {
                self.apply_memory_export_command(
                    incoming,
                    sender,
                    session_key,
                    dedup_key.as_deref(),
                    dedup_claim,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { request } => {
                self.apply_model_switch_command(
                    incoming,
                    session_key,
                    request,
                    dedup_key.as_deref(),
                    dedup_claim,
                )
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ApproveUser { pairing_code } => {
                let approval = {
                    let mut dm = self.dm_manager.write().await;
                    dm.approve_pairing_code(
                        &incoming.platform,
                        &incoming.user_id,
                        &incoming.chat_id,
                        incoming.is_dm,
                        &pairing_code,
                    )
                };
                match approval {
                    Ok(grant) => {
                        self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                            .await;
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "✅ User '{}' has been approved for DM access in chat '{}'.",
                                grant.user_id, grant.chat_id
                            ),
                            None,
                        )
                        .await?;
                        Ok(true)
                    }
                    Err(error) if error.is_persistence_error() => Err(GatewayError::Platform(
                        format!("durable DM grant approval failed: {error}"),
                    )),
                    Err(error) => {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!("🚫 /approve denied: {error}."),
                            None,
                        )
                        .await?;
                        Ok(true)
                    }
                }
            }
            GatewayCommandResult::DenyUser { pairing_code } => {
                let revocation = {
                    let mut dm = self.dm_manager.write().await;
                    dm.revoke_pairing_code(
                        &incoming.platform,
                        &incoming.user_id,
                        &incoming.chat_id,
                        incoming.is_dm,
                        &pairing_code,
                    )
                };
                match revocation {
                    Ok(target) => {
                        self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                            .await;
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!(
                                "⛔ User '{}' has been removed from DM access in chat '{}'.",
                                target.user_id, target.chat_id
                            ),
                            None,
                        )
                        .await?;
                        Ok(true)
                    }
                    Err(error) if error.is_persistence_error() => Err(GatewayError::Platform(
                        format!("durable DM grant revocation failed: {error}"),
                    )),
                    Err(error) => {
                        self.send_message(
                            &incoming.platform,
                            &incoming.chat_id,
                            &format!("🚫 /deny denied: {error}."),
                            None,
                        )
                        .await?;
                        Ok(true)
                    }
                }
            }
            GatewayCommandResult::StopAgent(reply) => {
                let mut cancelled_task = false;
                for (task_id, status, _) in self.background_tasks.list_tasks() {
                    if status == TaskStatus::Running {
                        cancelled_task |= self.background_tasks.cancel(&task_id);
                    }
                }
                let interrupted = {
                    let mut busy = self.busy_sessions.write().await;
                    busy.interrupt_active(
                        session_key,
                        "User requested /stop for the active gateway task.",
                    )
                };
                if cancelled_task || interrupted {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::QueuePrompt { prompt } => {
                let mut queued_incoming = incoming.clone();
                queued_incoming.text = prompt.clone();
                let admission = self
                    .enqueue_session_if_active(
                        session_key,
                        QueuedIncoming::from_validated_ingress(queued_incoming, sender.clone())
                            .with_dedup_key(dedup_key.clone()),
                    )
                    .await;
                let owned = admission == QueueAdmission::Queued;
                if owned {
                    Self::transfer_command_dedup(dedup_claim);
                }
                let reply = match admission {
                    QueueAdmission::Queued => {
                        format!("🧵 Queued follow-up for the active session: {prompt}")
                    }
                    QueueAdmission::Full => {
                        "The active session queue is full. Retry the prompt later.".to_string()
                    }
                    QueueAdmission::Inactive => {
                        "No active gateway turn is running. Send the prompt normally to start it."
                            .to_string()
                    }
                };
                if let Err(error) = self
                    .send_message_threaded(
                        &incoming.platform,
                        &incoming.chat_id,
                        &reply,
                        None,
                        Self::reply_thread_id(incoming),
                    )
                    .await
                {
                    if owned {
                        warn!(
                            platform = %incoming.platform,
                            chat_id = %incoming.chat_id,
                            error = %error,
                            "failed to acknowledge /queue; queued work remains owned"
                        );
                    } else {
                        return Err(error);
                    }
                }
                Ok(true)
            }
            GatewayCommandResult::SteerPrompt { prompt } => {
                let mut queued_incoming = incoming.clone();
                queued_incoming.text = prompt.clone();
                let decision = self
                    .steer_session_if_active(
                        session_key,
                        QueuedIncoming::from_validated_ingress(queued_incoming, sender.clone())
                            .with_dedup_key(dedup_key.clone()),
                    )
                    .await;
                let owned = decision
                    .as_ref()
                    .is_some_and(|decision| decision.steered || decision.queued);
                let complete_immediately = decision
                    .as_ref()
                    .is_some_and(|decision| decision.steered && !decision.queued);
                if complete_immediately {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
                } else if owned {
                    Self::transfer_command_dedup(dedup_claim);
                }
                let reply = match decision {
                    Some(decision) if decision.steered => {
                        format!("🧭 Steered the running task: {prompt}")
                    }
                    Some(decision) if decision.queued => {
                        format!("🧵 No live steering hook was ready; queued follow-up: {prompt}")
                    }
                    Some(decision) => decision.ack.unwrap_or_else(|| {
                        "The running task could not accept this steering message.".to_string()
                    }),
                    None => {
                        "No active gateway turn is running. Use /steer while a task is in flight."
                            .to_string()
                    }
                };
                if let Err(error) = self
                    .send_message_threaded(
                        &incoming.platform,
                        &incoming.chat_id,
                        &reply,
                        None,
                        Self::reply_thread_id(incoming),
                    )
                    .await
                {
                    if owned {
                        warn!(
                            platform = %incoming.platform,
                            chat_id = %incoming.chat_id,
                            error = %error,
                            "failed to acknowledge /steer; steered or queued work remains owned"
                        );
                    } else {
                        return Err(error);
                    }
                }
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
                let outcome = self
                    .compress_context(
                        session_key,
                        24,
                        dedup_key.as_deref(),
                        dedup_claim,
                    )
                    .await;
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
                let reply = self
                    .apply_verbose_command(
                        incoming,
                        session_key,
                        dedup_key.as_deref(),
                        dedup_claim,
                    )
                    .await?;
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
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
                if count > 0 {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
                }
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
                    drop(states);
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
                let mut response = match &stored {
                    Some(stored_title) if stored_title.as_str() == title => reply,
                    Some(stored_title) => format!("🏷 Session title set to: {}", stored_title),
                    None => "🏷 Session title cleared.".to_string(),
                };
                if let Some(stored_title) = stored.as_deref() {
                    if let Some(warning) = self
                        .mirror_session_title_to_platform_thread(incoming, stored_title)
                        .await
                    {
                        response.push('\n');
                        response.push_str(&warning);
                    }
                }
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
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
                self.apply_reload_skills_command(
                    incoming,
                    session_key,
                    dedup_key.as_deref(),
                    dedup_claim,
                )
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProvider { provider, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
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
                    Err(err) if rejects_profile_switch(&err) => {
                        render_profile_switch_rejected_reply(&profile, &err)
                    }
                    Err(err) => {
                        let mut states = self.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().profile =
                            Some(profile.clone());
                        drop(states);
                        format!("{reply}\n[WARN] Profile file not applied: {err}")
                    }
                };
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
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
                        drop(states);
                        self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                            .await;
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
                let removed = self
                    .session_manager
                    .remove_last_messages(session_key, steps)
                    .await;
                if removed > 0 {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
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
                    .handle_background_command(
                        incoming,
                        session_key,
                        &prompt,
                        false,
                        dedup_key.as_deref(),
                        dedup_claim,
                    )
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::BtwTask { prompt } => {
                let handled = self
                    .handle_background_command(
                        incoming,
                        session_key,
                        &prompt,
                        true,
                        dedup_key.as_deref(),
                        dedup_claim,
                    )
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
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
                self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                    .await;
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Retry => {
                let (turn_completion_tx, turn_completion_rx) = tokio::sync::oneshot::channel();
                let entry =
                    QueuedIncoming::from_validated_ingress(incoming.clone(), sender.clone())
                        .with_turn_kind(QueuedTurnKind::Retry)
                        .with_dedup_key(dedup_key.clone())
                        .with_completion_sender(turn_completion_tx);
                let admission = self
                    .claim_session_lane(session_key, entry, BusyInputMode::Queue)
                    .await;
                match admission {
                    BusyAdmission::Claimed {
                        lease,
                        mut entry,
                    } => {
                        if let Some(mut claim) = dedup_claim.take() {
                            claim.disarm();
                        }
                        entry.clear_completion_sender();
                        drop(turn_completion_rx);
                        self.start_turn_chain(lease, entry).await?;
                    }
                    BusyAdmission::Queued(decision) => {
                        if decision.queued {
                            if let Some(mut claim) = dedup_claim.take() {
                                claim.disarm();
                            }
                        }
                        if self.config.display.busy_ack_enabled() {
                            if let Some(ack) = decision.ack {
                                if let Err(error) = self
                                    .send_message(
                                        &incoming.platform,
                                        &incoming.chat_id,
                                        &ack,
                                        None,
                                    )
                                    .await
                                {
                                    warn!(
                                        platform = %incoming.platform,
                                        chat_id = %incoming.chat_id,
                                        error = %error,
                                        "failed to send retry queue acknowledgement; queued retry remains owned"
                                    );
                                }
                            }
                        }
                        if decision.queued {
                            turn_completion_rx
                                .await
                                .map_err(|_| {
                                    GatewayError::Platform(
                                        "queued retry ended before reporting completion".into(),
                                    )
                                })??;
                        }
                    }
                    BusyAdmission::AtCapacity => {
                        self.send_message_threaded(
                            &incoming.platform,
                            &incoming.chat_id,
                            "Gateway session capacity reached; retry later.",
                            None,
                            Self::reply_thread_id(incoming),
                        )
                        .await?;
                    }
                }
                Ok(true)
            }
            GatewayCommandResult::Undo => {
                let removed = self
                    .session_manager
                    .undo_last_exchange(session_key)
                    .await;
                if removed > 0 {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
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
                let text = self.format_session_list("Your sessions", &sessions);
                self.send_message(&incoming.platform, &incoming.chat_id, &text, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SearchSessions { query } => {
                let sessions = self
                    .session_manager
                    .search_user_sessions(&incoming.user_id, &query, 10)
                    .await;
                let text =
                    self.format_session_list(&format!("Sessions matching `{}`", query), &sessions);
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
                    let key = self
                        .session_manager
                        .compose_session_key_for_scope(s.conversation_scope());
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
                        self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
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
                let budget_changed = new_budget.is_some();
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
                if budget_changed {
                    self.commit_command_dedup(dedup_key.as_deref(), dedup_claim)
                        .await;
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }
}
