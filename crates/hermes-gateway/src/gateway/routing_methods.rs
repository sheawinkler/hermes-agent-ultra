impl Gateway {
    /// DM check → session lookup → agent loop → response.
    pub async fn route_message(&self, incoming: &IncomingMessage) -> Result<(), GatewayError> {
        self.route_message_from_sender(incoming, IncomingSender::human())
            .await
    }

    /// Route an incoming message with platform-provided sender metadata.
    pub async fn route_message_from_sender(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<(), GatewayError> {
        let mut current = incoming.clone();
        for drain_depth in 0..32 {
            match self
                .route_message_once_from_sender(&current, sender)
                .await?
            {
                Some(next) => {
                    current = next;
                }
                None => return Ok(()),
            }
            if drain_depth == 31 {
                warn!(
                    platform = current.platform,
                    chat_id = current.chat_id,
                    "busy-session drain depth reached safety cap"
                );
            }
        }
        Ok(())
    }

    async fn route_message_once_from_sender(
        &self,
        incoming: &IncomingMessage,
        sender: IncomingSender,
    ) -> Result<Option<IncomingMessage>, GatewayError> {
        let access_policy = self.platform_access_policy(&incoming.platform).await;
        let is_slash_command = incoming.text.trim_start().starts_with('/');
        if let Some(policy) = access_policy.as_ref() {
            let bypasses_user_allowlist =
                policy.allows_sender_without_user_allowlist(incoming, sender);
            if !incoming.is_dm {
                if policy.is_channel_ignored(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel is ignored by platform policy"
                    );
                    return Ok(None);
                }
                if !policy.is_channel_allowed(&incoming.chat_id) {
                    debug!(
                        platform = incoming.platform,
                        chat_id = incoming.chat_id,
                        "Group message denied: channel not in platform allowlist"
                    );
                    return Ok(None);
                }
                match policy.group_mode {
                    GroupAccessMode::Disabled => {
                        debug!(
                            platform = incoming.platform,
                            user_id = incoming.user_id,
                            "Group traffic denied by platform policy"
                        );
                        return Ok(None);
                    }
                    GroupAccessMode::Allowlist => {
                        if !bypasses_user_allowlist
                            && !policy.is_user_allowed(&incoming.user_id)
                            && !policy.is_group_chat_authorized(&incoming.chat_id)
                        {
                            debug!(
                                platform = incoming.platform,
                                user_id = incoming.user_id,
                                "Group message denied: user not in allowlist"
                            );
                            return Ok(None);
                        }
                    }
                    GroupAccessMode::Open => {}
                }
            }
            if is_slash_command
                && policy.slash_requires_allowlist
                && policy.has_allowlist()
                && !bypasses_user_allowlist
                && !policy.is_user_allowed(&incoming.user_id)
            {
                debug!(
                    platform = incoming.platform,
                    user_id = incoming.user_id,
                    "Slash command denied: user not in platform allowlist"
                );
                return Ok(None);
            }
        }

        // 1. Check DM authorization if this is a direct message
        if incoming.is_dm {
            let dm_manager = self.dm_manager.read().await;
            let decision = dm_manager
                .handle_dm(&incoming.user_id, &incoming.platform)
                .await;

            match decision {
                DmDecision::Allow => {
                    // Proceed
                }
                DmDecision::Pair { message } => {
                    // Send pairing message and return
                    if let Some(msg) = message {
                        self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                            .await?;
                    }
                    return Ok(None);
                }
                DmDecision::Deny => {
                    debug!(
                        user_id = incoming.user_id,
                        platform = incoming.platform,
                        "DM denied for unauthorized user"
                    );
                    return Ok(None);
                }
            }
        }

        if self.should_suppress_duplicate(incoming).await {
            debug!(
                platform = incoming.platform,
                chat_id = incoming.chat_id,
                message_id = incoming.message_id.as_deref().unwrap_or_default(),
                "Duplicate platform message redelivery suppressed"
            );
            return Ok(None);
        }

        // 2. Get or create session
        let session_key = self.session_manager.compose_session_key(
            &incoming.platform,
            &incoming.chat_id,
            &incoming.user_id,
        );
        let existing_session = self.session_manager.get_session(&session_key).await;
        let session = self
            .session_manager
            .get_or_create_session(&incoming.platform, &incoming.chat_id, &incoming.user_id)
            .await;
        let session_started = existing_session.is_none();
        let session_auto_reset = existing_session
            .as_ref()
            .map(|s| s.created_at != session.created_at)
            .unwrap_or(false);
        if session_started || session_auto_reset {
            self.emit_hook_event(
                "session:start",
                serde_json::json!({
                    "platform": incoming.platform,
                    "chat_id": incoming.chat_id,
                    "user_id": incoming.user_id,
                    "session_id": session_key,
                    "reason": if session_started { "new" } else { "auto_reset" }
                }),
            )
            .await;
        }

        let mut agent_text_override: Option<String> = None;

        if !is_slash_command {
            let decision = {
                let mut busy = self.busy_sessions.write().await;
                busy.handle_busy_message(
                    &session_key,
                    Self::incoming_to_busy_event(incoming, incoming.text.clone()),
                    self.busy_input_mode(),
                )
            };
            if decision.handled {
                if self.config.display.busy_ack_enabled() {
                    if let Some(ack) = decision.ack {
                        self.send_message_threaded(
                            &incoming.platform,
                            &incoming.chat_id,
                            &ack,
                            None,
                            Self::reply_thread_id(incoming),
                        )
                        .await?;
                    }
                }
                return Ok(None);
            }
        }

        // Slash commands are executed directly by the gateway command runtime.
        // Installed skill commands are the exception: after built-ins and quick
        // commands decline them, they are converted into a normal agent turn
        // containing the resolved SKILL.md content.
        if is_slash_command {
            match self.execute_slash_command(incoming, &session_key).await? {
                SlashCommandOutcome::Handled => return Ok(None),
                SlashCommandOutcome::ForwardToAgent { message } => {
                    agent_text_override = Some(message);
                }
            }
        }

        let reaction_plan = Self::reaction_lifecycle_plan(incoming, access_policy.as_ref());
        let reaction_adapter = if reaction_plan.is_some() {
            self.get_adapter(&incoming.platform).await
        } else {
            None
        };
        if let (Some(adapter), Some(message_id), Some(plan)) = (
            &reaction_adapter,
            incoming.message_id.as_deref(),
            reaction_plan,
        ) {
            if let Err(err) = adapter
                .add_reaction(&incoming.chat_id, message_id, plan.start)
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

        let agent_text = agent_text_override.as_deref().unwrap_or(&incoming.text);
        let enriched_text =
            self.enrich_message_with_transcription(&self.enrich_message_with_vision(agent_text));
        self.maybe_apply_smart_model_routing(&session_key, &enriched_text)
            .await;

        // 3. Add the user message to the session
        self.session_manager
            .add_message(&session_key, Message::user(enriched_text))
            .await;
        self.bump_input_usage(&session_key, agent_text.chars().count())
            .await;

        // 4. Get all session messages for the agent loop
        let messages = self.session_manager.get_messages(&session_key).await;

        // 5. Process through agent loop (streaming or non-streaming)
        {
            let mut busy = self.busy_sessions.write().await;
            busy.mark_active(&session_key, None);
        }
        let processing_result = if self.config.streaming_enabled {
            self.route_streaming(incoming, messages, &session_key).await
        } else {
            self.route_non_streaming(incoming, messages, &session_key)
                .await
        };

        if let (Some(adapter), Some(message_id), Some(plan)) = (
            &reaction_adapter,
            incoming.message_id.as_deref(),
            reaction_plan,
        ) {
            if let Err(err) = adapter
                .remove_reaction(&incoming.chat_id, message_id, plan.start)
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
            let emoji = if processing_result.is_ok() {
                plan.success
            } else {
                plan.error
            };
            if let Err(err) = adapter
                .add_reaction(&incoming.chat_id, message_id, emoji)
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
        }

        let pending = {
            let mut busy = self.busy_sessions.write().await;
            busy.finish(
                &session_key,
                if processing_result.is_ok() {
                    ProcessingOutcome::Success
                } else {
                    ProcessingOutcome::Failure
                },
            )
            .map(Self::busy_event_to_incoming)
        };
        processing_result?;
        Ok(pending)
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
            .arg("-c")
            .arg(command)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
