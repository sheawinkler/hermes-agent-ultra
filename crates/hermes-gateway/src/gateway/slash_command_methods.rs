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

    fn format_session_list(&self, heading: &str, sessions: &[Session]) -> String {
        if sessions.is_empty() {
            return format!("📚 No {} found for your user.", heading.to_ascii_lowercase());
        }

        let mut out = format!("📚 **{}:**\n\n", heading);
        for s in sessions {
            let key = self
                .session_manager
                .compose_session_key(&s.platform, &s.chat_id, &s.user_id);
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
        let next = {
            let mut modes = self.tool_progress_modes.write().await;
            let current = modes
                .get(&platform)
                .cloned()
                .unwrap_or_else(|| default_mode.clone());
            let next = Self::next_tool_progress_mode(&current).to_string();
            modes.insert(platform.clone(), next.clone());
            next
        };

        let mut states = self.runtime_state.write().await;
        let state = states.entry(session_key.to_string()).or_default();
        state.tool_progress = Some(next.clone());
        state.verbose = next == "verbose";
        drop(states);

        Ok(format!(
            "📝 Tool progress for {platform}: {}",
            next.to_ascii_uppercase()
        ))
    }

    async fn execute_slash_command(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
    ) -> Result<SlashCommandOutcome, GatewayError> {
        let command_text = Self::normalize_slash_command_text(&incoming.text);
        if let Some(reply) = self.resolve_quick_command(&command_text).await? {
            self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                .await?;
            return Ok(SlashCommandOutcome::Handled);
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
        let handled = self
            .apply_command_result(incoming, session_key, result)
            .await?;
        Ok(if handled {
            SlashCommandOutcome::Handled
        } else {
            SlashCommandOutcome::ForwardToAgent {
                message: command_text,
            }
        })
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

    async fn apply_command_result(
        &self,
        incoming: &IncomingMessage,
        session_key: &str,
        result: GatewayCommandResult,
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
                let current_session = self.session_manager.get_session(session_key).await;
                self.emit_hook_event(
                    "session:end",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": current_session.as_ref().map(|s| s.id.clone())
                    }),
                )
                .await;
                if let Some(old_session) = current_session.as_ref() {
                    self.emit_session_finalize(session_key, old_session, "reset")
                        .await;
                }
                let reset_snapshot = self
                    .session_manager
                    .reset_session_with_snapshots(session_key)
                    .await;
                self.clear_session_boundary_security_state(session_key)
                    .await;
                let reset_session = reset_snapshot
                    .as_ref()
                    .map(|(_, new_session)| new_session)
                    .or(current_session.as_ref());
                self.emit_hook_event(
                    "session:reset",
                    serde_json::json!({
                        "platform": incoming.platform,
                        "chat_id": incoming.chat_id,
                        "user_id": incoming.user_id,
                        "session_id": session_key,
                        "logical_session_id": reset_session.map(|s| s.id.clone())
                    }),
                )
                .await;
                if let Some(new_session) = reset_session {
                    self.emit_session_reset_lifecycle(session_key, new_session, "reset")
                        .await;
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchModel { request } => {
                self.apply_model_switch_command(incoming, session_key, request)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::ApproveUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /approve requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.authorize_user(user_id.clone());
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("✅ User '{}' has been approved for DM access.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::DenyUser { user_id } => {
                let mut dm = self.dm_manager.write().await;
                if !dm.is_admin(&incoming.user_id) {
                    drop(dm);
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "🚫 /deny requires admin privileges.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                dm.deauthorize_user(&user_id);
                drop(dm);
                self.send_message(
                    &incoming.platform,
                    &incoming.chat_id,
                    &format!("⛔ User '{}' has been removed from DM allowlist.", user_id),
                    None,
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::StopAgent(reply) => {
                for (task_id, status, _) in self.background_tasks.list_tasks() {
                    if status == TaskStatus::Running {
                        let _ = self.background_tasks.cancel(&task_id);
                    }
                }
                {
                    let mut busy = self.busy_sessions.write().await;
                    let _ = busy.interrupt_active(
                        session_key,
                        "User requested /stop for the active gateway task.",
                    );
                }
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::QueuePrompt { prompt } => {
                let active = {
                    let mut busy = self.busy_sessions.write().await;
                    let active = busy.is_active(session_key);
                    if active {
                        busy.queue_message(
                            session_key,
                            Self::incoming_to_busy_event(incoming, prompt.clone()),
                        );
                    }
                    active
                };
                let reply = if active {
                    format!("🧵 Queued follow-up for the active session: {prompt}")
                } else {
                    "No active gateway turn is running. Send the prompt normally to start it."
                        .to_string()
                };
                self.send_message_threaded(
                    &incoming.platform,
                    &incoming.chat_id,
                    &reply,
                    None,
                    Self::reply_thread_id(incoming),
                )
                .await?;
                Ok(true)
            }
            GatewayCommandResult::SteerPrompt { prompt } => {
                let decision = {
                    let mut busy = self.busy_sessions.write().await;
                    busy.handle_busy_message(
                        session_key,
                        Self::incoming_to_busy_event(incoming, prompt.clone()),
                        BusyInputMode::Steer,
                    )
                };
                let reply = if decision.steered {
                    format!("🧭 Steered the running task: {prompt}")
                } else if decision.queued {
                    format!("🧵 No live steering hook was ready; queued follow-up: {prompt}")
                } else {
                    "No active gateway turn is running. Use /steer while a task is in flight."
                        .to_string()
                };
                self.send_message_threaded(
                    &incoming.platform,
                    &incoming.chat_id,
                    &reply,
                    None,
                    Self::reply_thread_id(incoming),
                )
                .await?;
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
                let outcome = self.compress_context(session_key, 24).await;
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
                let reply = self.apply_verbose_command(incoming, session_key).await?;
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
                let response = match &stored {
                    Some(stored_title) if stored_title.as_str() == title => reply,
                    Some(stored_title) => format!("🏷 Session title set to: {}", stored_title),
                    None => "🏷 Session title cleared.".to_string(),
                };
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
                self.apply_reload_skills_command(incoming, session_key)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::SwitchProvider { provider, reply } => {
                let mut states = self.runtime_state.write().await;
                states.entry(session_key.to_string()).or_default().provider = Some(provider);
                drop(states);
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
                    Err(err) => {
                        let mut states = self.runtime_state.write().await;
                        states.entry(session_key.to_string()).or_default().profile = Some(profile);
                        format!("{reply}\n⚠️ Profile file not applied: {err}")
                    }
                };
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
                let mut removed = 0usize;
                for _ in 0..steps {
                    if self
                        .session_manager
                        .pop_last_message(session_key)
                        .await
                        .is_some()
                    {
                        removed += 1;
                    } else {
                        break;
                    }
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
                    .handle_background_command(incoming, session_key, &prompt, false)
                    .await?;
                Ok(handled)
            }
            GatewayCommandResult::BtwTask { prompt } => {
                let handled = self
                    .handle_background_command(incoming, session_key, &prompt, true)
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
                self.send_message(&incoming.platform, &incoming.chat_id, &reply, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Retry => {
                let mut messages = self.session_manager.get_messages(session_key).await;
                if matches!(
                    messages.last().map(|m| m.role),
                    Some(MessageRole::Assistant)
                ) {
                    messages.pop();
                }
                if messages.is_empty() {
                    self.send_message(
                        &incoming.platform,
                        &incoming.chat_id,
                        "No previous message to retry.",
                        None,
                    )
                    .await?;
                    return Ok(true);
                }
                self.session_manager
                    .replace_messages(session_key, messages.clone())
                    .await;
                self.route_non_streaming(incoming, messages, session_key)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Undo => {
                let mut removed = 0usize;
                if let Some(last) = self.session_manager.pop_last_message(session_key).await {
                    removed += 1;
                    if last.role == MessageRole::Assistant {
                        if let Some(prev) = self.session_manager.pop_last_message(session_key).await
                        {
                            if prev.role == MessageRole::User {
                                removed += 1;
                            }
                        }
                    }
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
                    let key = self.session_manager.compose_session_key(
                        &s.platform,
                        &s.chat_id,
                        &s.user_id,
                    );
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
                self.send_message(&incoming.platform, &incoming.chat_id, &msg, None)
                    .await?;
                Ok(true)
            }
            GatewayCommandResult::Noop => Ok(true),
        }
    }
}
