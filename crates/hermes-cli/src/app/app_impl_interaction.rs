impl App {
    /// Run the interactive REPL loop.
    ///
    /// This is the main entry point for interactive mode. It delegates
    /// to the TUI subsystem for rendering and event handling.
    pub async fn run_interactive(&mut self) -> Result<(), AgentError> {
        // The actual TUI loop is in crate::tui::run()
        // This method exists so non-TUI callers can drive the loop manually.
        if self.running {
            loop {
                if !self.running {
                    break;
                }
                // In a real implementation, the TUI event loop would drive this.
                // Here we just mark that we're ready.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        Ok(())
    }

    /// Handle a line of user input.
    ///
    /// If the input starts with `/` it is treated as a slash command.
    /// Otherwise it is sent as a user message to the agent.
    pub async fn handle_input(&mut self, input: &str) -> Result<(), AgentError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        // Store in input history
        self.input_history.push(trimmed.to_string());
        self.history_index = self.input_history.len();

        if trimmed.starts_with('/') {
            if self.stream_handle.is_some() {
                self.push_ui_user(trimmed);
            }
            // Parse the slash command and its arguments
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let cmd = parts[0];
            let args: Vec<&str> = parts
                .get(1)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();

            let result = crate::commands::handle_slash_command(self, cmd, &args).await?;
            if result == crate::commands::CommandResult::Quit {
                self.running = false;
            }
        } else {
            // Regular user message
            self.submit_user_message(trimmed).await?;
        }

        Ok(())
    }

    /// Handle a slash command string (without the leading `/`).
    pub async fn handle_command(&mut self, cmd: &str) -> Result<(), AgentError> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
        let slash_cmd = if parts[0].starts_with('/') {
            parts[0]
        } else {
            // Prepend / if not present
            return self.handle_input(&format!("/{}", trimmed)).await;
        };

        if self.stream_handle.is_some() {
            self.push_ui_user(trimmed);
        }

        let args: Vec<&str> = parts
            .get(1)
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        let result = crate::commands::handle_slash_command(self, slash_cmd, &args).await?;
        if result == crate::commands::CommandResult::Quit {
            self.running = false;
        }
        Ok(())
    }

    /// Create a new session, clearing all messages.
    pub fn new_session(&mut self) {
        let old_session_id = self.session_id.clone();
        let old_message_count = self.messages.len();
        let old_has_session_objective = self.session_objective.is_some();
        self.invoke_session_lifecycle_hook(HookType::OnSessionFinalize, &old_session_id);
        self.discard_session_if_empty(
            &old_session_id,
            old_message_count,
            old_has_session_objective,
        );
        self.session_id = Uuid::new_v4().to_string();
        self.notify_memory_session_switch(&self.session_id, &old_session_id, false);
        self.messages.clear();
        self.ui_messages.clear();
        self.last_usage = None;
        self.session_usage = None;
        self.session_cost_usd = 0.0;
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
        self.ensure_session_stub_snapshot();
        self.invoke_session_lifecycle_hook(HookType::OnSessionReset, &self.session_id);
        self.rebuild_agent_for_active_session();
    }

    /// Reset the current session (clear messages but keep session ID).
    pub fn reset_session(&mut self) {
        let session_id = self.session_id.clone();
        self.invoke_session_lifecycle_hook(HookType::OnSessionFinalize, &session_id);
        self.notify_memory_session_switch(&session_id, "", true);
        self.messages.clear();
        self.ui_messages.clear();
        self.last_usage = None;
        self.session_usage = None;
        self.session_cost_usd = 0.0;
        self.pending_image_hint = None;
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
        self.invoke_session_lifecycle_hook(HookType::OnSessionReset, &session_id);
    }

    fn invoke_session_lifecycle_hook(&self, hook: HookType, session_id: &str) {
        let Some(plugin_manager) = self.agent.plugin_manager.as_ref() else {
            return;
        };
        let Ok(plugin_manager) = plugin_manager.lock() else {
            tracing::warn!(hook = hook.as_str(), "Plugin manager lock poisoned");
            return;
        };
        let context = serde_json::json!({
            "session_id": session_id,
            "platform": "cli",
        });
        let _ = plugin_manager.invoke_hook(hook, &context);
    }

    fn notify_memory_session_end(&self, messages: &[hermes_core::Message]) {
        let Some(memory_manager) = self.agent.memory_manager.as_ref() else {
            return;
        };
        let Ok(memory_manager) = memory_manager.lock() else {
            tracing::warn!("Memory manager lock poisoned during interrupted session finalize");
            return;
        };
        let as_values = messages
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect::<Vec<_>>();
        memory_manager.on_session_end(&as_values);
    }

    fn invoke_interrupted_session_end_hook(&self, reason: &str) {
        let Some(plugin_manager) = self.agent.plugin_manager.as_ref() else {
            return;
        };
        let Ok(plugin_manager) = plugin_manager.lock() else {
            tracing::warn!(
                hook = HookType::OnSessionEnd.as_str(),
                "Plugin manager lock poisoned"
            );
            return;
        };
        let context = serde_json::json!({
            "session_id": self.session_id.as_str(),
            "completed": false,
            "interrupted": true,
            "model": self.current_model.as_str(),
            "platform": "tui",
            "reason": reason,
        });
        let _ = plugin_manager.invoke_hook(HookType::OnSessionEnd, &context);
    }

    /// Flush the best available TUI transcript when the process exits before
    /// `AgentRunComplete` can publish the final agent result.
    pub fn finalize_interrupted_tui_session(
        &mut self,
        partial_assistant: Option<&str>,
        reason: &str,
    ) -> Result<(), AgentError> {
        if let Some(partial) = partial_assistant
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            let duplicate_tail = self
                .messages
                .last()
                .and_then(|message| message.content.as_deref())
                .is_some_and(|content| content.trim() == partial);
            if !duplicate_tail {
                self.messages.push(hermes_core::Message::assistant(partial));
            }
        }

        if self.messages.is_empty() && self.session_objective.is_none() {
            return Ok(());
        }

        self.persist_session_snapshot(None)?;
        self.notify_memory_session_end(&self.messages);
        self.invoke_interrupted_session_end_hook(reason);
        Ok(())
    }

    fn notify_memory_session_switch(
        &self,
        new_session_id: &str,
        parent_session_id: &str,
        reset: bool,
    ) {
        let Some(memory_manager) = self.agent.memory_manager.as_ref() else {
            return;
        };
        let Ok(memory_manager) = memory_manager.lock() else {
            tracing::warn!("Memory manager lock poisoned during session switch");
            return;
        };
        memory_manager.on_session_switch(new_session_id, parent_session_id, reset);
    }

    /// Set or clear a durable session objective.
    ///
    /// The objective is represented as a synthetic system message so it is
    /// applied consistently on every turn without requiring user re-entry.
    pub fn set_session_objective(&mut self, objective: Option<String>) {
        self.messages.retain(|m| {
            if m.role != hermes_core::MessageRole::System {
                return true;
            }
            !m.content
                .as_deref()
                .unwrap_or_default()
                .starts_with(Self::SESSION_OBJECTIVE_PREFIX)
        });

        self.session_objective = objective
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(obj) = &self.session_objective {
            let system =
                hermes_core::Message::system(format!("{}{}", Self::SESSION_OBJECTIVE_PREFIX, obj));
            self.messages.insert(0, system);
        }
        self.prune_ui_after_current_messages();
    }

    /// Retry the last user message by re-sending it to the agent.
    ///
    /// Finds the last user message in history, removes all messages after it
    /// (including the assistant response), and re-runs the agent.
    pub async fn retry_last(&mut self) -> Result<(), AgentError> {
        // Find the last user message
        let last_user_idx = self
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User);

        if let Some(idx) = last_user_idx {
            let last_user_msg = self.messages[idx].clone();
            // Truncate messages to just before the last user message
            self.messages.truncate(idx);
            // Re-add the user message
            self.messages.push(last_user_msg);
            // Re-run the agent
            self.run_agent().await?;
            self.prune_ui_after_current_messages();
        }

        Ok(())
    }

    /// Undo one or more user turns, returning the text staged for editing.
    pub fn undo_last(&mut self) -> Option<String> {
        self.undo_last_n(1)
    }

    pub fn undo_last_n(&mut self, user_turns: usize) -> Option<String> {
        let user_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| (msg.role == hermes_core::MessageRole::User).then_some(idx))
            .collect();
        if user_indices.is_empty() {
            return None;
        }
        let count = user_turns.max(1);
        let target_pos = user_indices.len().saturating_sub(count);
        let target_idx = user_indices[target_pos];
        let prefill = self.messages[target_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();

        match SessionPersistence::new(&self.state_root)
            .rewind_active_user_turns(&self.session_id, count)
        {
            Ok(Some(outcome)) => tracing::debug!(
                "Soft-rewound session {} at message {} (inactive={}, active={})",
                self.session_id,
                outcome.target_message_id,
                outcome.inactive_count,
                outcome.active_message_count
            ),
            Ok(None) => tracing::debug!(
                "No persisted session row available for undo in session {}",
                self.session_id
            ),
            Err(err) => tracing::debug!("Failed to soft-rewind persisted session: {}", err),
        }

        self.messages.truncate(target_idx);
        self.prune_ui_after_current_messages();
        if prefill.trim().is_empty() {
            self.pending_input_prefill = None;
        } else {
            self.pending_input_prefill = Some(prefill.clone());
        }
        Some(prefill)
    }

    /// Switch the active model, rebuilding the provider and agent loop.
    pub fn switch_model(&mut self, provider_model: &str) {
        if let Err(err) = self.try_switch_model(provider_model) {
            tracing::warn!(
                model = provider_model,
                error = %err,
                "Model switch failed; keeping previous model"
            );
        }
    }
}

impl App {
    /// Switch the active model transactionally.
    ///
    /// The new provider/agent is built before mutating `current_model`, runtime
    /// env, or session persistence so a failed rebuild is a no-op for the
    /// current conversation.
    pub fn try_switch_model(&mut self, provider_model: &str) -> Result<(), AgentError> {
        let next_model = provider_model.trim();
        if next_model.is_empty() {
            return Err(AgentError::Config("model cannot be empty".to_string()));
        }
        if let Some(preset) = Self::moa_preset_name_for_model(next_model) {
            let Some(next_model) = Self::moa_virtual_model_name(next_model) else {
                return Err(AgentError::Config(format!(
                    "unsupported MoA preset '{preset}'; supported presets: {MOA_DEFAULT_PRESET}"
                )));
            };
            self.current_model = next_model;
            sync_runtime_model_env(&self.config, &self.current_model);
            match SessionPersistence::new(&self.state_root)
                .update_session_model(&self.session_id, &self.current_model)
            {
                Ok(true) => tracing::debug!(
                    "Persisted virtual MoA model switch for session {} to {}",
                    self.session_id,
                    self.current_model
                ),
                Ok(false) => {}
                Err(err) => {
                    tracing::debug!(
                        "Failed to persist virtual MoA model switch to session DB: {}",
                        err
                    )
                }
            }
            tracing::info!(
                "Switched model to virtual MoA preset: {}",
                self.current_model
            );
            return Ok(());
        }

        let next_agent = self.build_agent_for_model(next_model)?;
        self.current_model = next_model.to_string();
        sync_runtime_model_env(&self.config, &self.current_model);
        self.agent = next_agent;
        match SessionPersistence::new(&self.state_root)
            .update_session_model(&self.session_id, &self.current_model)
        {
            Ok(true) => tracing::debug!(
                "Persisted model switch for session {} to {}",
                self.session_id,
                self.current_model
            ),
            Ok(false) => {}
            Err(err) => tracing::debug!("Failed to persist model switch to session DB: {}", err),
        }

        tracing::info!("Switched model to: {}", provider_model);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn force_model_rebuild_failure_for_test(&mut self, provider_model: &str) {
        self.fail_model_rebuild_for = Some(provider_model.to_string());
    }

    /// Warn before a user-initiated model switch if the current transcript is
    /// likely to trigger preflight compression under the new context window.
    pub fn model_switch_preflight_warning(&self, provider_model: &str) -> Option<String> {
        let values = self
            .messages
            .iter()
            .filter_map(|message| serde_json::to_value(message).ok())
            .collect::<Vec<_>>();
        let estimate = estimate_messages_tokens_rough(&values);
        build_model_switch_preflight_warning(Some(&self.current_model), provider_model, estimate)
    }

    fn rebuild_agent_for_active_session(&mut self) {
        match self.build_agent_for_model(&self.current_model) {
            Ok(agent) => {
                self.agent = agent;
            }
            Err(err) => {
                tracing::warn!(
                    model = %self.current_model,
                    error = %err,
                    "Agent rebuild failed; keeping previous agent"
                );
            }
        }
    }

    fn build_agent_for_model(&self, provider_model: &str) -> Result<Arc<AgentLoop>, AgentError> {
        #[cfg(test)]
        if self
            .fail_model_rebuild_for
            .as_deref()
            .is_some_and(|model| model == provider_model)
        {
            return Err(AgentError::Config(format!(
                "test forced rebuild failure for {provider_model}"
            )));
        }

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let provider = build_provider(&self.config, provider_model);
            let mut agent_config = build_agent_config(&self.config, provider_model);
            agent_config.session_id = Some(self.session_id.clone());
            let agent_tool_registry = Arc::new(bridge_tool_registry(&self.tool_registry));

            let agent_inner = hermes_agent::attach_discovered_memory(AgentLoop::new(
                agent_config,
                agent_tool_registry,
                provider,
            ))
            .with_callbacks(Self::stream_callbacks(self.stream_handle_shared.clone()));
            let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
                &agent_inner,
                self.state_root.clone(),
            ));
            Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator))
        }));

        result.map_err(|_| {
            AgentError::Config(format!(
                "model switch rebuild panicked for {provider_model}"
            ))
        })
    }

    pub fn refresh_agent_tool_snapshot(&mut self) -> AgentToolSnapshotRefresh {
        let before = sorted_tool_schema_names(&self.tool_schemas);
        self.tool_schemas = hermes_tool_planning::resolve_platform_tool_schemas(
            &self.config,
            "cli",
            &self.tool_registry,
        );
        self.rebuild_agent_for_active_session();
        let after = sorted_tool_schema_names(&self.tool_schemas);
        let before_set: BTreeSet<_> = before.iter().cloned().collect();
        let after_set: BTreeSet<_> = after.iter().cloned().collect();

        AgentToolSnapshotRefresh {
            before_count: before.len(),
            after_count: after.len(),
            added: after_set.difference(&before_set).cloned().collect(),
            removed: before_set.difference(&after_set).cloned().collect(),
        }
    }

    /// Switch the active personality.
    pub fn switch_personality(&mut self, name: &str) {
        self.current_personality = Some(name.to_string());
        tracing::info!("Switched personality to: {}", name);
    }

    /// Return the normalized runtime provider for the active model.
    pub fn current_runtime_provider(&self) -> String {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        normalize_runtime_provider_name(provider_name.as_str())
    }

    /// Refresh and verify runtime credentials for the active provider.
    ///
    /// This is the command-surface lifecycle helper used by `/auth`.
    pub async fn verify_runtime_auth(&mut self, force_refresh: bool) -> Result<String, AgentError> {
        let provider = self.current_runtime_provider();
        let before_present = provider_api_key_from_env(&provider).is_some();
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        let after = provider_api_key_from_env(&provider);
        let after_present = after.is_some();
        let status = if let Some(key) = after {
            format!(
                "present (masked={} chars)",
                key.chars().count().max(1).saturating_sub(8).max(1)
            )
        } else {
            "missing".to_string()
        };
        let refresh_mode = if force_refresh { "forced" } else { "passive" };
        let changed = if before_present == after_present {
            "unchanged"
        } else {
            "updated"
        };
        Ok(format!(
            "Auth verify\nprovider: {}\nmode: {}\ncredential: {}\nstate: {}\nmodel: {}",
            provider, refresh_mode, status, changed, self.current_model
        ))
    }

    async fn run_messages_with_current_agent(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        self.run_messages_with_current_agent_tools(messages, stream_enabled, true)
            .await
    }

    async fn run_messages_with_current_agent_tools(
        &self,
        messages: Vec<hermes_core::Message>,
        stream_enabled: bool,
        include_tools: bool,
    ) -> Result<hermes_core::AgentResult, AgentError> {
        let tool_schemas = include_tools.then(|| self.tool_schemas.clone());
        if stream_enabled && self.config.streaming.enabled {
            let stream_handle = self.stream_handle.clone();
            let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                stream_handle.map(|h| {
                    Box::new(move |chunk: hermes_core::StreamChunk| {
                        h.send_chunk(chunk);
                    }) as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
                });
            self.agent
                .run_stream(messages, tool_schemas, stream_cb)
                .await
        } else {
            self.agent.run(messages, tool_schemas).await
        }
    }

    async fn run_quorum_fanout_turn(
        &mut self,
        run_started_at: Instant,
        policy: QuorumPolicy,
    ) -> Result<bool, AgentError> {
        let quorum_contract = self.load_quorum_agent_contract_text();
        let (voter_models, model_resolution_notes) = self.resolve_quorum_models(&policy).await;
        for note in model_resolution_notes {
            Self::emit_lifecycle_event(&self.stream_handle_shared, note);
        }
        if voter_models.len() < 2 {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum armed but only {} distinct model configured; falling back to normal run",
                    voter_models.len()
                ),
            );
            return Ok(false);
        }

        let (base_messages, reformulated) = self.build_inference_messages();
        if reformulated {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
            );
        }
        let original_model = self.current_model.clone();
        let mut outcomes: Vec<QuorumVoterOutcome> = Vec::new();
        let mut succeeded = 0usize;
        let output_char_cap = Self::quorum_output_char_cap();

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "quorum",
            "multi-voter fan-out dispatch",
            30,
        );

        for (idx, model) in voter_models.iter().enumerate() {
            let display_index = idx + 1;
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "quorum voter {}/{} dispatch -> {}",
                    display_index,
                    voter_models.len(),
                    model
                ),
            );
            if self.current_model != *model {
                self.switch_model(model);
            }
            let force_refresh = display_index == 1 || Self::quorum_force_refresh_each_voter();
            self.refresh_runtime_provider_credentials_if_needed(force_refresh)
                .await;

            let started = Instant::now();
            let max_attempts = Self::quorum_voter_retry_limit();
            let voter_passes = Self::quorum_voter_passes();
            let mut pass_errors: Vec<String> = Vec::new();
            let mut combined_output = String::new();
            let mut combined_turns: u32 = 0;
            let mut combined_tool_errors: usize = 0;
            let mut last_err: Option<AgentError> = None;
            let mut toolless_fallback_used = false;
            let voter_tools_enabled = Self::quorum_voter_tools_enabled();

            for pass_idx in 0..voter_passes {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!(
                        "quorum voter {}/{} pass {}/{}",
                        display_index,
                        voter_models.len(),
                        pass_idx + 1,
                        voter_passes
                    ),
                );

                let mut system_sections = Vec::new();
                if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
                    system_sections.push(format!(
                        "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for this voter pass:\n{}",
                        contract_path.display(),
                        contract_text
                    ));
                }
                system_sections.push(Self::build_quorum_voter_prompt(
                    pass_idx,
                    voter_passes,
                    model,
                ));
                let trailing_user_context = if pass_idx > 0 && !combined_output.trim().is_empty() {
                    Some(format!(
                        "[PRIOR_VOTER_DRAFT]\n{}\n\nCritique and strengthen this prior draft for pass {}/{}.",
                        combined_output,
                        pass_idx + 1,
                        voter_passes
                    ))
                } else {
                    None
                };
                let pass_messages = Self::compose_quorum_messages(
                    system_sections,
                    base_messages.clone(),
                    trailing_user_context,
                );

                let mut attempts = 0usize;
                let mut maybe_result: Option<hermes_core::AgentResult> = None;
                while attempts < max_attempts {
                    attempts += 1;
                    match self
                        .run_messages_with_current_agent_tools(
                            pass_messages.clone(),
                            false,
                            voter_tools_enabled,
                        )
                        .await
                    {
                        Ok(result) => {
                            maybe_result = Some(result);
                            break;
                        }
                        Err(err) => {
                            if Self::is_provider_tool_payload_error(&err)
                                && Self::quorum_toolless_provider_fallback_enabled()
                                && voter_tools_enabled
                                && !toolless_fallback_used
                            {
                                toolless_fallback_used = true;
                                pass_errors.push(format!(
                                    "pass {}: provider rejected tool schema on requested model; retried this voter pass without tool schemas",
                                    pass_idx + 1
                                ));
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} provider rejected tool schema; retrying this voter pass without tool schemas",
                                        display_index,
                                        voter_models.len()
                                    ),
                                );
                                match self
                                    .run_messages_with_current_agent_tools(
                                        pass_messages.clone(),
                                        false,
                                        false,
                                    )
                                    .await
                                {
                                    Ok(result) => {
                                        maybe_result = Some(result);
                                        break;
                                    }
                                    Err(fallback_err) => {
                                        last_err = Some(fallback_err);
                                        break;
                                    }
                                }
                            }
                            if Self::is_provider_auth_or_session_error(&err)
                                && attempts < max_attempts
                            {
                                let refreshed = self.force_auth_refresh_after_error().await;
                                if refreshed {
                                    continue;
                                }
                            }
                            if Self::is_transient_retryable_error(&err) && attempts < max_attempts {
                                let backoff_ms = (attempts as u64).saturating_mul(750).max(500);
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "quorum voter {}/{} transient error (attempt {}/{}): {} — retrying after {}ms",
                                        display_index,
                                        voter_models.len(),
                                        attempts,
                                        max_attempts,
                                        err,
                                        backoff_ms
                                    ),
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                                    .await;
                                continue;
                            }
                            last_err = Some(err);
                            break;
                        }
                    }
                }

                let Some(result) = maybe_result else {
                    if let Some(err) = &last_err {
                        pass_errors.push(format!("pass {}: {}", pass_idx + 1, err));
                    } else {
                        pass_errors.push(format!("pass {}: unknown error", pass_idx + 1));
                    }
                    break;
                };

                combined_turns = combined_turns.saturating_add(result.total_turns);
                combined_tool_errors =
                    combined_tool_errors.saturating_add(result.tool_errors.len());
                let latest = Self::extract_last_assistant_output(&result.messages);
                if !latest.trim().is_empty() {
                    combined_output = latest;
                } else {
                    pass_errors.push(format!("pass {}: empty assistant output", pass_idx + 1));
                    break;
                }
            }

            if !combined_output.trim().is_empty() {
                let output = Self::truncate_for_quorum(&combined_output, output_char_cap);
                let degraded = Self::quorum_output_is_degraded_non_answer(&output);
                let status = if output.trim().is_empty() {
                    "empty"
                } else if degraded {
                    pass_errors.push("voter returned degraded non-answer".to_string());
                    "degraded"
                } else {
                    succeeded += 1;
                    "ok"
                };
                let error = if !pass_errors.is_empty() {
                    Some(pass_errors.join(" | "))
                } else if output.trim().is_empty() {
                    Some("voter returned empty assistant output".to_string())
                } else {
                    None
                };
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: status.to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output,
                    error,
                });
            } else {
                let err_text = last_err
                    .as_ref()
                    .map(ToString::to_string)
                    .or_else(|| (!pass_errors.is_empty()).then(|| pass_errors.join(" | ")))
                    .unwrap_or_else(|| "unknown voter error".to_string());
                outcomes.push(QuorumVoterOutcome {
                    model: model.clone(),
                    status: "error".to_string(),
                    duration_ms: started.elapsed().as_millis() as u64,
                    total_turns: combined_turns,
                    tool_errors: combined_tool_errors,
                    output: String::new(),
                    error: Some(err_text),
                });
            }
        }

        if self.current_model != original_model {
            self.switch_model(&original_model);
        }
        let synthesis_model = Self::quorum_synthesis_model_for_original(&original_model);
        let artifact_path = self.persist_quorum_artifact(&policy, &outcomes)?;
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!("quorum voter artifact saved: {}", artifact_path.display()),
        );

        let required_success = Self::required_quorum_success(voter_models.len());
        if succeeded < required_success {
            let error_summary = outcomes
                .iter()
                .map(|o| {
                    format!(
                        "{} => {}",
                        o.model,
                        match (o.status.as_str(), o.error.as_deref()) {
                            ("ok", _) => "ok".to_string(),
                            ("empty", Some(e)) => format!("empty ({})", e),
                            (_, Some(e)) => e.to_string(),
                            _ => "unknown error".to_string(),
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(" | ");
            return Err(AgentError::LlmApi(format!(
                "Quorum fan-out did not meet success threshold (required={}, got={}): {}",
                required_success, succeeded, error_summary
            )));
        }

        if self.current_model != synthesis_model {
            self.try_switch_model(&synthesis_model)?;
        }
        let synthesis_system = Self::build_quorum_synthesis_prompt(&policy, &outcomes);
        let mut synthesis_system_sections = Vec::new();
        if let Some((contract_path, contract_text)) = quorum_contract.as_ref() {
            synthesis_system_sections.push(format!(
                "[QUORUM_AGENT_CONTRACT]\npath={}\nApply this contract strictly for synthesis:\n{}",
                contract_path.display(),
                contract_text
            ));
        }
        synthesis_system_sections.push(synthesis_system);
        let synthesis_messages =
            Self::compose_quorum_messages(synthesis_system_sections, base_messages, None);

        Self::emit_phase_event(
            &self.stream_handle_shared,
            "synthesis",
            "quorum synthesis from voter outputs",
            75,
        );
        let synthesis_result = self
            .run_messages_with_current_agent_tools(
                synthesis_messages,
                true,
                Self::quorum_synthesis_tools_enabled(),
            )
            .await;
        if self.current_model != original_model {
            if let Err(err) = self.try_switch_model(&original_model) {
                tracing::warn!(
                    model = %original_model,
                    error = %err,
                    "Failed to restore original model after quorum synthesis"
                );
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    format!(
                        "warning: failed to restore original model after quorum synthesis: {}",
                        err
                    ),
                );
            }
        }
        let result = synthesis_result?;
        let total_turns = result.total_turns;
        let synthesis_text = Self::extract_last_assistant_output(&result.messages);
        if let Err(err) =
            Self::update_quorum_artifact_with_synthesis(&artifact_path, &synthesis_text)
        {
            tracing::warn!("quorum synthesis artifact update skipped: {}", err);
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!("warning: quorum synthesis artifact update skipped: {}", err),
            );
        }
        if let Err(err) = self.apply_agent_result_and_persist(result) {
            tracing::warn!("session autosave skipped: {}", err);
        }
        Self::emit_lifecycle_event(
            &self.stream_handle_shared,
            format!(
                "quorum run finished in {:.2}s (voters={} succeeded={} total_turns={})",
                run_started_at.elapsed().as_secs_f64(),
                voter_models.len(),
                succeeded,
                total_turns
            ),
        );
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "finalize",
            "transcript finalization + persistence",
            100,
        );
        if let Some(handle) = &self.stream_handle {
            handle.send_done();
        }
        Ok(true)
    }

    fn required_quorum_success(voter_count: usize) -> usize {
        let n = voter_count.max(1);
        (n / 2) + 1
    }

    /// Run the agent on the current message history.
    ///
    /// Sends all messages to the agent loop and appends the result.
    /// Checks the interrupt controller before running and clears it after.
    async fn run_agent(&mut self) -> Result<(), AgentError> {
        let run_started_at = Instant::now();
        self.maybe_autopin_contextlattice_topic_from_objective();
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "preflight",
            "runtime preflight + credential hydration",
            5,
        );
        self.emit_contextlattice_connectivity_status().await;
        let provider = self.current_runtime_provider();
        let force_refresh = Self::should_force_preflight_auth_refresh(provider.as_str());
        self.refresh_runtime_provider_credentials_if_needed(force_refresh)
            .await;
        if force_refresh {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!("preflight auth refresh forced for provider {}", provider),
            );
        }
        if let Some(policy) = self.quorum_mode_armed_for_turn() {
            self.quorum_armed_once = false;
            self.clear_quorum_system_hints_inplace();
            self.interrupt_controller.clear_interrupt();
            match self.run_quorum_fanout_turn(run_started_at, policy).await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(err) => return Err(err),
            }
        }
        Self::emit_phase_event(
            &self.stream_handle_shared,
            "dispatch",
            "dispatching model request",
            15,
        );
        self.interrupt_controller.clear_interrupt();
        let mut remediation_attempted = false;
        let mut auth_refresh_attempts = 0usize;
        let auth_refresh_retry_limit = Self::auth_refresh_retry_limit();
        let mut transient_retry_attempts = 0usize;
        let transient_retry_limit = Self::transient_retry_limit();
        let mut objective_continuation_attempts = 0usize;
        let objective_continuation_limit = Self::objective_continuation_retry_limit();
        loop {
            Self::emit_lifecycle_event(
                &self.stream_handle_shared,
                format!(
                    "dispatching request to {} (messages={})",
                    self.current_model,
                    self.messages.len()
                ),
            );
            Self::emit_phase_event(
                &self.stream_handle_shared,
                "inference",
                "model inference + tool execution",
                35,
            );
            let baseline_len = self.messages.len();
            let (messages, reformulated) = self.build_inference_messages();
            if reformulated {
                Self::emit_lifecycle_event(
                    &self.stream_handle_shared,
                    "runtime prompt reformulation injected (anti-scheming + context + tool routing + contradiction self-check)",
                );
            }
            let result = self.run_messages_with_current_agent(messages, true).await;

            match result {
                Ok(result) => {
                    let total_turns = result.total_turns;
                    let interrupted = result.interrupted;
                    let finished_naturally = result.finished_naturally;
                    if objective_continuation_attempts < objective_continuation_limit {
                        if let Some(reason) = self
                            .should_force_objective_continuation(&result, baseline_len)
                            .await
                        {
                            self.messages = result.messages;
                            self.messages.push(hermes_core::Message::system(
                                Self::objective_continuation_system_prompt(&reason),
                            ));
                            self.prune_ui_after_current_messages();
                            objective_continuation_attempts += 1;
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!(
                                    "objective continuation enforcer triggered ({}/{}): {}",
                                    objective_continuation_attempts,
                                    objective_continuation_limit,
                                    reason
                                ),
                            );
                            Self::emit_phase_event(
                                &self.stream_handle_shared,
                                "objective",
                                "auto-continuing objective loop for concrete execution",
                                50,
                            );
                            continue;
                        }
                    }
                    if let Err(err) = self.apply_agent_result_and_persist(result) {
                        tracing::warn!("session autosave skipped: {}", err);
                    }
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run finished in {:.2}s (total_turns={})",
                            run_started_at.elapsed().as_secs_f64(),
                            total_turns
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "finalize",
                        "transcript finalization + persistence",
                        100,
                    );
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if interrupted {
                        tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                        if self.stream_handle.is_some() {
                            self.push_ui_assistant("[Agent execution interrupted]");
                        } else {
                            println!("[Agent execution interrupted]");
                        }
                    } else if !finished_naturally {
                        tracing::warn!(
                            "Agent stopped after {} turns (did not finish naturally)",
                            total_turns
                        );
                    }
                    break;
                }
                Err(AgentError::Interrupted { message }) => {
                    self.interrupt_controller.clear_interrupt();
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run interrupted after {:.2}s",
                            run_started_at.elapsed().as_secs_f64()
                        ),
                    );
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if let Some(redirect) = message {
                        tracing::info!("Agent interrupted with redirect: {}", redirect);
                    } else {
                        tracing::info!("Agent interrupted by user");
                    }
                    if self.stream_handle.is_some() {
                        self.push_ui_assistant("[Agent execution interrupted]");
                    } else {
                        println!("[Agent execution interrupted]");
                    }
                    break;
                }
                Err(e) => {
                    Self::emit_lifecycle_event(
                        &self.stream_handle_shared,
                        format!(
                            "run error after {:.2}s: {}",
                            run_started_at.elapsed().as_secs_f64(),
                            e
                        ),
                    );
                    Self::emit_phase_event(
                        &self.stream_handle_shared,
                        "recovery",
                        "error handling + remediation",
                        60,
                    );
                    if Self::is_provider_auth_or_session_error(&e) {
                        if auth_refresh_attempts < auth_refresh_retry_limit {
                            if self.force_auth_refresh_after_error().await {
                                auth_refresh_attempts += 1;
                                Self::emit_lifecycle_event(
                                    &self.stream_handle_shared,
                                    format!(
                                        "auth refresh retry {}/{}",
                                        auth_refresh_attempts, auth_refresh_retry_limit
                                    ),
                                );
                                continue;
                            }
                        } else {
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!(
                                    "auth refresh retries exhausted ({})",
                                    auth_refresh_retry_limit
                                ),
                            );
                        }
                    }
                    if Self::is_transient_retryable_error(&e)
                        && transient_retry_attempts < transient_retry_limit
                    {
                        transient_retry_attempts += 1;
                        let backoff_ms = (transient_retry_attempts as u64)
                            .saturating_mul(1_000)
                            .max(800);
                        Self::emit_lifecycle_event(
                            &self.stream_handle_shared,
                            format!(
                                "transient runtime error retry {}/{} after {}ms: {}",
                                transient_retry_attempts, transient_retry_limit, backoff_ms, e
                            ),
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                        continue;
                    }
                    if !remediation_attempted {
                        if let Some((next_model, notice)) =
                            self.model_auto_remediation_target(&e).await
                        {
                            tracing::warn!(
                                "Model auto-remediation triggered: {} -> {}",
                                self.current_model,
                                next_model
                            );
                            if self.stream_handle.is_some() {
                                self.push_ui_assistant(notice.clone());
                            } else {
                                println!("{notice}");
                            }
                            Self::emit_lifecycle_event(
                                &self.stream_handle_shared,
                                format!("auto-remediation switching model to {}", next_model),
                            );
                            self.switch_model(&next_model);
                            remediation_attempted = true;
                            continue;
                        }
                    }
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Append a UI-only message anchored to the current conversation size.
    pub fn push_ui_message(&mut self, message: hermes_core::Message) {
        self.ui_messages.push(UiTranscriptMessage {
            insert_at: self.messages.len(),
            message,
        });
    }

    /// Append a UI-only user transcript line.
    pub fn push_ui_user(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::user(text.into()));
    }

    /// Append a UI-only assistant transcript line.
    pub fn push_ui_assistant(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::assistant(text.into()));
    }

    /// Build the merged transcript for TUI rendering.
    ///
    /// This includes durable conversation history and UI-only events in
    /// chronological order, while preserving model-facing context purity.
    pub fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        let mut merged = Vec::with_capacity(self.messages.len() + self.ui_messages.len());
        for idx in 0..=self.messages.len() {
            for ui in self.ui_messages.iter().filter(|m| m.insert_at == idx) {
                merged.push(ui.message.clone());
            }
            if idx < self.messages.len() {
                merged.push(self.messages[idx].clone());
            }
        }
        merged
    }

    fn prune_ui_after_current_messages(&mut self) {
        let cap = self.messages.len();
        self.ui_messages.retain(|m| m.insert_at <= cap);
    }

    /// Apply the finalized messages returned by an agent run.
    pub fn apply_agent_result(&mut self, result: hermes_core::AgentResult) {
        let usage = result.usage.clone();
        let run_cost = result
            .session_cost_usd
            .or_else(|| usage.as_ref().and_then(|usage| usage.estimated_cost))
            .filter(|cost| cost.is_finite() && *cost >= 0.0);

        self.last_usage = usage.clone();
        if let Some(usage) = usage {
            self.session_usage = Some(merge_usage_stats(self.session_usage.take(), &usage));
        }
        if let Some(run_cost) = run_cost {
            self.session_cost_usd += run_cost;
        }
        self.messages = result.messages;
        self.prune_ui_after_current_messages();
    }

    /// Apply finalized messages and persist the session snapshot.
    pub fn apply_agent_result_and_persist(
        &mut self,
        result: hermes_core::AgentResult,
    ) -> Result<(), AgentError> {
        self.apply_agent_result(result);
        self.persist_session_snapshot(None).map(|_| ())
    }

    /// Count background jobs currently queued/running.
    pub fn running_background_job_count(&self) -> usize {
        let jobs_dir = hermes_config::hermes_home().join("background_jobs");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(jobs_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "queued" | "running") {
                active += 1;
            }
        }
        active
    }

    /// Count sub-agent lineage files still marked as started.
    pub fn active_subagent_count(&self) -> usize {
        let subagents_dir = hermes_config::hermes_home().join("subagents");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(subagents_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "started" | "running" | "background_pending") {
                active += 1;
            }
        }
        active
    }

    fn prune_session_snapshot_entry(
        entry: &SessionSnapshotEntry,
        total_bytes: &mut u64,
    ) -> Result<(), AgentError> {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                *total_bytes = total_bytes.saturating_sub(entry.size_bytes);
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(AgentError::Io(format!(
                "Failed to prune session snapshot {}: {}",
                entry.path.display(),
                err
            ))),
        }
    }

    fn enforce_session_snapshot_guardrails(
        &self,
        sessions_dir: &Path,
        preserve_path: &Path,
    ) -> Result<(), AgentError> {
        let preserve = preserve_path.to_path_buf();
        let mut entries = list_session_snapshot_entries(sessions_dir);
        let mut total_bytes = entries.iter().map(|e| e.size_bytes).sum::<u64>();

        let max_files = snapshot_max_files();
        if max_files > 0 {
            while entries.len() > max_files {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let max_total_bytes = snapshot_max_total_bytes();
        if max_total_bytes > 0 {
            while total_bytes > max_total_bytes {
                let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                    break;
                };
                let removed = entries.remove(idx);
                Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
            }
        }

        let min_free_bytes = snapshot_min_free_bytes();
        if min_free_bytes > 0 {
            if let Some(mut free_bytes) = available_disk_space_bytes(sessions_dir) {
                while free_bytes < min_free_bytes {
                    let Some(idx) = entries.iter().position(|entry| entry.path != preserve) else {
                        break;
                    };
                    let removed = entries.remove(idx);
                    Self::prune_session_snapshot_entry(&removed, &mut total_bytes)?;
                    free_bytes = available_disk_space_bytes(sessions_dir).unwrap_or(free_bytes);
                }
                if free_bytes < min_free_bytes {
                    return Err(AgentError::Io(format!(
                        "Session snapshot write blocked by disk guardrail: free={} bytes, required_min={} bytes (dir={})",
                        free_bytes,
                        min_free_bytes,
                        sessions_dir.display()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Get a serializable snapshot of the current session info.
    pub fn session_info(&self) -> SessionInfo {
        SessionInfo {
            session_id: self.session_id.clone(),
            model: self.current_model.clone(),
            personality: self.current_personality.clone(),
            message_count: self.messages.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Persist a JSON session snapshot to `<state_root>/sessions`.
    ///
    /// When `name_override` is provided, that value is used as the file stem.
    /// Otherwise the active `session_id` is used.
    pub fn persist_session_snapshot(
        &self,
        name_override: Option<&str>,
    ) -> Result<PathBuf, AgentError> {
        let sessions_dir = self.state_root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create sessions dir {}: {}",
                sessions_dir.display(),
                e
            ))
        })?;
        let stem = name_override
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(self.session_id.as_str());
        let path = sessions_dir.join(format!("{stem}.json"));
        let payload = serde_json::json!({
            "session_info": self.session_info(),
            "messages": self.messages.iter().map(|m| {
                serde_json::json!({
                    "role": format!("{:?}", m.role),
                    "content": m.content.as_deref().unwrap_or(""),
                    "tool_call_id": m.tool_call_id,
                    "tool_calls": m.tool_calls,
                    "reasoning_content": m.reasoning_content,
                })
            }).collect::<Vec<_>>(),
        });
        let json = serde_json::to_string_pretty(&payload).map_err(|e| {
            AgentError::Config(format!("Failed to serialize session snapshot: {e}"))
        })?;
        std::fs::write(&path, json).map_err(|e| {
            AgentError::Io(format!(
                "Failed to write session snapshot {}: {}",
                path.display(),
                e
            ))
        })?;
        self.enforce_session_snapshot_guardrails(&sessions_dir, &path)?;
        Ok(path)
    }

    fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn is_model_not_found_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let model_not_found = message.contains("model not found")
            || message.contains("requested model does not exist")
            || message.contains("404 not found")
            || message.contains("openrouter catalog");
        model_not_found && message.contains("model")
    }

    fn is_provider_auth_or_session_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        message.contains("401")
            || message.contains("403")
            || message.contains("unauthorized")
            || message.contains("invalid token")
            || message.contains("token_expired")
            || message.contains("expired_token")
            || message.contains("token expired")
            || message.contains("invalid_token")
            || message.contains("expired")
            || message.contains("authentication")
            || message.contains("session expired")
    }

    fn is_provider_tool_payload_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg)
            | AgentError::AuthFailed(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let mentions_tool_payload =
            message.contains("tool") || message.contains("function") || message.contains("schema");
        let provider_payload_rejected = message.contains("provider returned error")
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("check the model name")
                || message.contains("invalid"));
        let openai_shape_rejected = (message.contains("no choices in response")
            || message.contains("empty choices array"))
            && mentions_tool_payload
            && (message.contains("request is not valid")
                || message.contains("valid payload")
                || message.contains("provider returned error")
                || message.contains("invalid"));
        let explicit_tool_schema_rejected =
            message.contains("tool") && (message.contains("invalid") || message.contains("schema"));
        let strict_function_shape =
            message.contains("invalid input") && message.contains("function");
        provider_payload_rejected
            || openai_shape_rejected
            || explicit_tool_schema_rejected
            || strict_function_shape
            || (message.contains("422") && message.contains("valid payload"))
    }

    fn quorum_output_is_degraded_non_answer(output: &str) -> bool {
        let lower = output.to_ascii_lowercase();
        lower.contains("objective delivery compromised")
            || lower.contains("reverting to hermes")
            || lower.contains("safe-mode response")
            || lower.contains("safe mode response")
            || (lower.contains("i do not have") && lower.contains("tools"))
            || (lower.contains("cannot access") && lower.contains("tools"))
    }

    async fn force_auth_refresh_after_error(&mut self) -> bool {
        let (provider_name, _) = resolve_provider_and_model(&self.config, &self.current_model);
        let provider = normalize_runtime_provider_name(provider_name.as_str());
        let (notice, refreshed) = match provider.as_str() {
            "nous" => match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(creds) => {
                    let changed = Self::apply_nous_runtime_credentials(&creds);
                    if changed {
                        self.switch_model(&self.current_model.clone());
                    }
                    (
                        Some("Nous auth auto-refresh succeeded; retrying request.".to_string()),
                        true,
                    )
                }
                Err(err) => {
                    if Self::nous_refresh_contention_error(&err) {
                        match resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous refresh busy; reused cached runtime credential and retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(cache_err) => (
                                Some(format!(
                                    "Nous cached credential hydration failed after refresh contention: {}",
                                    cache_err
                                )),
                                false,
                            ),
                        }
                    } else if Self::auth_error_requires_nous_login(&err)
                        && self
                            .attempt_interactive_nous_login("runtime auth refresh failed")
                            .await
                    {
                        match resolve_nous_runtime_credentials(
                            true,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            Ok(creds) => {
                                let changed = Self::apply_nous_runtime_credentials(&creds);
                                if changed {
                                    self.switch_model(&self.current_model.clone());
                                }
                                (
                                    Some(
                                        "Nous auth re-login succeeded; retrying request."
                                            .to_string(),
                                    ),
                                    true,
                                )
                            }
                            Err(retry_err) => (
                                Some(format!("Nous auth auto-refresh failed: {}", retry_err)),
                                false,
                            ),
                        }
                    } else {
                        (
                            Some(format!("Nous auth auto-refresh failed: {}", err)),
                            false,
                        )
                    }
                }
            },
            "qwen-oauth" => {
                match resolve_qwen_runtime_credentials(
                    true,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await
                {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_QWEN_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("DASHSCOPE_API_KEY", &creds.api_key);
                        if !creds.base_url.trim().is_empty() {
                            changed |=
                                Self::set_env_if_changed("HERMES_QWEN_BASE_URL", &creds.base_url);
                        }
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        (
                            Some(
                                "Qwen OAuth auto-refresh succeeded; retrying request.".to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Qwen OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
                }
            }
            "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
                match resolve_gemini_oauth_runtime_credentials(true).await {
                    Ok(creds) => {
                        let mut changed = false;
                        changed |=
                            Self::set_env_if_changed("HERMES_GEMINI_OAUTH_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GOOGLE_API_KEY", &creds.api_key);
                        changed |= Self::set_env_if_changed("GEMINI_API_KEY", &creds.api_key);
                        if changed {
                            self.switch_model(&self.current_model.clone());
                        }
                        (
                            Some(
                                "Gemini OAuth auto-refresh succeeded; retrying request."
                                    .to_string(),
                            ),
                            true,
                        )
                    }
                    Err(err) => (
                        Some(format!("Gemini OAuth auto-refresh failed: {}", err)),
                        false,
                    ),
                }
            }
            _ => (None, false),
        };

        if let Some(text) = notice {
            Self::emit_lifecycle_event(&self.stream_handle_shared, &text);
            if self.stream_handle.is_some() {
                self.push_ui_assistant(text);
            } else {
                println!("{}", text);
            }
        }
        refreshed
    }

    async fn model_auto_remediation_target(&self, err: &AgentError) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.current_model.as_str()));
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return None;
        }

        let catalog = provider_model_ids(&provider).await;
        if catalog.is_empty() {
            return None;
        }

        let selected = Self::resolve_quorum_catalog_candidate(current_model_id, &catalog)
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let close = Self::rank_catalog_candidates(current_model_id, &catalog, 3);
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once. close matches: {}",
            self.current_model,
            next_model,
            if close.is_empty() {
                "(none)".to_string()
            } else {
                close.join(", ")
            }
        );
        Some((next_model, notice))
    }

    /// Navigate backward in input history.
    pub fn history_prev(&mut self) -> Option<&str> {
        if self.history_index > 0 {
            self.history_index -= 1;
            self.input_history
                .get(self.history_index)
                .map(|s| s.as_str())
        } else {
            None
        }
    }

    /// Navigate forward in input history.
    pub fn history_next(&mut self) -> Option<&str> {
        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index < self.input_history.len() {
                self.input_history
                    .get(self.history_index)
                    .map(|s| s.as_str())
            } else {
                None
            }
        } else {
            None
        }
    }
}
