impl AgentLoop {
    /// Run the agent loop (non-streaming).
    ///
    /// Sends the initial messages to the LLM, then iteratively:
    /// - Executes any tool calls the LLM makes
    /// - Feeds results back as tool messages
    /// - Stops when the LLM responds without tool calls, or max turns exceeded
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        let mut ctx = ContextManager::default_budget();
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut messages = messages;
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let context_length = get_model_context_length(&self.config.model);
        for msg in messages.iter_mut() {
            if msg.role != MessageRole::User {
                continue;
            }
            let Some(content) = msg.content.clone() else {
                continue;
            };
            let result =
                preprocess_context_references_async(&content, &cwd, context_length, None).await;
            if result.expanded && result.message != content {
                msg.content = Some(result.message);
            }
        }

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Determine which tools to expose
        let tool_schemas: Vec<ToolSchema> = tools.unwrap_or_else(|| self.tool_registry.schemas());
        let (advertised_tool_names, advertised_tool_names_display) =
            advertised_tool_name_index(&tool_schemas);

        // Build and inject system prompt (or reuse SQLite-cached prompt for session continuity)
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        let prefill_start = ctx.get_messages().len();
        for msg in &self.config.prefill_messages {
            ctx.add_message(msg.clone());
        }
        let prefill_end = ctx.get_messages().len();
        let prefill_range = (prefill_end > prefill_start).then_some(prefill_start..prefill_end);

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);
        if let Some(hint) = contextlattice_connect_system_hint(ctx.get_messages()) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = exploratory_problem_solving_system_hint(ctx.get_messages()) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = web_research_system_hint(ctx.get_messages(), &tool_schemas) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = terminal_command_system_hint(&tool_schemas) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = self.google_workspace_system_hint(ctx.get_messages(), &tool_schemas) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = objective_mode_system_hint(ctx.get_messages()) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) =
            contextlattice_intelligence_system_hint(ctx.get_messages(), &tool_schemas)
        {
            ctx.add_message(Message::system(hint));
        }

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .filter(|(_, m)| m.role == MessageRole::User)
                .last()
                .map(|(i, _)| i)
        } else {
            None
        };
        let mut codex_ack_continuations: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch for first user message
        let first_user = ctx
            .get_messages()
            .iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id);
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        if self.config.preflight_context_compress {
            self.preflight_context_compress_with_status(&mut ctx);
        }
        let replay = ReplayRecorder::for_session(&self.config, session_id);
        let max_turns_limit = effective_max_turns(self.config.max_turns);
        replay.record(
            "session_start",
            serde_json::json!({
                "session_id": session_id,
                "mode": "run",
                "model": self.config.model,
                "max_turns": self.config.max_turns,
                "max_turns_effective": max_turns_limit,
                "max_turns_unlimited": max_turns_limit.is_none(),
            }),
        );

        let mut total_turns: u32 = 0;
        let mut _total_api_time_ms: u64 = 0;
        let mut _total_tool_time_ms: u64 = 0;
        let mut accumulated_usage: Option<UsageStats> = None;
        let mut session_cost_usd: f64 = 0.0;
        let mut cost_warned = false;
        let mut forced_runtime_route: Option<TurnRuntimeRoute> = None;
        let mut last_checkpoint_messages: Option<Vec<Message>> = None;
        let mut invalid_tool_retries: u32 = 0;
        let mut invalid_json_retries: u32 = 0;
        let mut last_content_with_tools: Option<String> = None;
        let mut context_pressure_warned_at: f64 = 0.0;
        let mut context_pressure_last_warn_at: Option<Instant> = None;
        let mut context_pressure_last_warn_percent: f64 = 0.0;
        let mut governor_llm_latency_window: VecDeque<u64> = VecDeque::new();
        let mut governor_tool_error_window: VecDeque<f64> = VecDeque::new();
        let mut governor_consecutive_error_turns: u32 = 0;
        let mut repo_review_budget_state = RepoReviewBudgetState::default();
        let mut objective_guard_retries: u32 = 0;
        let mut finalizer_evidence_retries: u32 = 0;
        let mut finalizer_output_quality_retries: u32 = 0;
        let mut finalizer_action_execution_retries: u32 = 0;
        let mut finalizer_web_research_retries: u32 = 0;
        let mut finalizer_google_workspace_retries: u32 = 0;
        let mut finalizer_task_focus_retries: u32 = 0;
        let mut finalizer_repo_research_plan_retries: u32 = 0;
        let governor_window_limit = governor_window_size();

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    prefill_range.clone(),
                ));
            }

            if let Some(max_turns) = max_turns_limit {
                if total_turns >= max_turns {
                    tracing::warn!(
                        "Max turns ({}) exceeded, requesting final summary",
                        max_turns
                    );
                    let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                    if let Some(msg) = summary_msg {
                        ctx.add_message(msg);
                    }
                    self.memory_on_session_end(ctx.get_messages());
                    replay.record(
                        "session_end",
                        serde_json::json!({
                            "reason": "max_turns",
                            "total_turns": total_turns,
                            "session_cost_usd": session_cost_usd,
                        }),
                    );
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(
                            &ctx,
                            persist_user_idx,
                            prefill_range.clone(),
                        ),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
            }

            total_turns += 1;
            tracing::debug!("Agent turn {}", total_turns);

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.refresh_oauth_store_tokens_if_needed().await;

            // Skill nudge counter — Python `run_agent.py`: increment at the start of each inner API iteration.
            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config.checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            // Notify memory + plugins of new turn
            self.memory_on_turn_start(total_turns, "");

            // Memory sync at flush interval
            if total_turns % self.config.memory_flush_interval == 0 && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id, msgs);
            }

            // --- Pre-LLM hook ---
            let turn_runtime_route = forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let turn_governor_runtime = governor_runtime_state(
                &governor_llm_latency_window,
                &governor_tool_error_window,
                governor_consecutive_error_turns,
            );
            let llm_governor =
                governor_for_turn(&self.config, &ctx, 0, Some(&turn_governor_runtime));
            if forced_runtime_route.is_none()
                && (turn_governor_runtime.consecutive_error_turns >= 2
                    || turn_governor_runtime
                        .avg_llm_latency_ms
                        .map(|v| v >= governor_latency_warn_ms())
                        .unwrap_or(false))
                && (llm_governor.error_degraded || llm_governor.latency_degraded)
            {
                if let Some(model) = self
                    .resolve_reliability_degrade_model(active_model, turn_runtime_route.as_ref())
                {
                    forced_runtime_route = Some(self.turn_route_reliability_guard(model.clone()));
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Reliability guard switching route to `{}` after degradation.",
                            model
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Reliability guard: runtime degradation detected. Switching next turns to `{}`.",
                        model
                    )));
                }
            }
            tracing::debug!(
                turn = total_turns,
                model = active_model,
                governor_pressure = llm_governor.pressure,
                governor_max_tokens = ?llm_governor.max_tokens,
                governor_avg_latency_ms = ?turn_governor_runtime.avg_llm_latency_ms,
                governor_avg_tool_error_rate = turn_governor_runtime.avg_tool_error_rate,
                governor_consecutive_error_turns = turn_governor_runtime.consecutive_error_turns,
                "turn governor snapshot"
            );
            replay.record(
                "turn_start",
                serde_json::json!({
                    "turn": total_turns,
                    "model": active_model,
                    "pressure": llm_governor.pressure,
                    "max_tokens": llm_governor.max_tokens,
                    "latency_degraded": llm_governor.latency_degraded,
                    "error_degraded": llm_governor.error_degraded,
                    "avg_llm_latency_ms": turn_governor_runtime.avg_llm_latency_ms,
                    "avg_tool_error_rate": turn_governor_runtime.avg_tool_error_rate,
                    "consecutive_error_turns": turn_governor_runtime.consecutive_error_turns,
                }),
            );
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- LLM API call with transport retry + semantic empty/thinking recovery (Python parity) ---
            let llm_span = tracing::info_span!(
                "hermes.llm",
                turn = total_turns,
                request_model = %active_model,
                response_model = tracing::field::Empty,
                api_time_ms = tracing::field::Empty,
                finish_reason = tracing::field::Empty,
                tool_call_count = tracing::field::Empty
            );
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let mut response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        accumulated_usage.clone(),
                        session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        prefill_range.clone(),
                    ));
                }
                let r = match self
                    .call_llm_with_retry(
                        &ctx,
                        &tool_schemas,
                        turn_runtime_route.as_ref(),
                        llm_governor.max_tokens,
                    )
                    .await
                {
                    Ok(r) => r,
                    Err(AgentError::Interrupted { .. }) => {
                        return Ok(self.graceful_interrupt_result(
                            &ctx,
                            total_turns,
                            &tool_errors,
                            accumulated_usage.clone(),
                            session_cost_usd,
                            session_started_hooks_fired,
                            persist_user_idx,
                            prefill_range.clone(),
                        ));
                    }
                    Err(e) => {
                        let api_elapsed = api_start.elapsed().as_millis() as u64;
                        self.update_route_learning(
                            turn_runtime_route.as_ref(),
                            Some(active_model),
                            api_elapsed,
                            false,
                        );
                        return Err(e);
                    }
                };
                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                let has_tools = r
                    .message
                    .tool_calls
                    .as_ref()
                    .map_or(false, |tc| !tc.is_empty());
                if has_tools {
                    break r;
                }
                if Self::assistant_visible_text(&r.message) {
                    break r;
                }
                if Self::assistant_has_reasoning(&r.message)
                    && inner_thinking < self.config.thinking_prefill_max_retries
                {
                    inner_thinking += 1;
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Reasoning-only response — retrying ({}/{})",
                            inner_thinking, self.config.thinking_prefill_max_retries
                        ),
                    );
                    ctx.add_message(r.message.clone());
                    continue;
                }
                // Accept explicit stop/end-turn responses even when assistant text is empty.
                // Anthropic can return this shape after trivial tool side-effects.
                if !Self::assistant_has_reasoning(&r.message)
                    && r.finish_reason.as_deref() == Some("stop")
                {
                    break r;
                }
                if !Self::assistant_has_reasoning(&r.message)
                    && inner_empty < self.config.empty_content_max_retries
                {
                    inner_empty += 1;
                    tracing::warn!(
                        "empty assistant response — retrying ({}/{})",
                        inner_empty,
                        self.config.empty_content_max_retries
                    );
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Empty assistant response — retrying ({}/{})",
                            inner_empty, self.config.empty_content_max_retries
                        ),
                    );
                    continue;
                }
                break r;
            };
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            _total_api_time_ms += api_elapsed;
            self.update_route_learning(
                turn_runtime_route.as_ref(),
                Some(response.model.as_str()),
                api_elapsed,
                true,
            );
            push_window_u64(
                &mut governor_llm_latency_window,
                api_elapsed,
                governor_window_limit,
            );
            let response_tool_call_count = response
                .message
                .tool_calls
                .as_ref()
                .map(|v| v.len())
                .unwrap_or(0);
            llm_span.record("response_model", response.model.as_str());
            llm_span.record("api_time_ms", api_elapsed);
            if let Some(finish_reason) = response.finish_reason.as_deref() {
                llm_span.record("finish_reason", finish_reason);
            }
            llm_span.record("tool_call_count", response_tool_call_count);
            llm_span.in_scope(|| {
                tracing::info!(
                    target: "hermes.langfuse",
                    turn = total_turns,
                    model = %response.model,
                    api_time_ms = api_elapsed,
                    tool_call_count = response_tool_call_count,
                    finish_reason = response.finish_reason.as_deref().unwrap_or(""),
                    "llm response"
                );
            });
            drop(llm_span);
            replay.record(
                "llm_response",
                serde_json::json!({
                    "turn": total_turns,
                    "model": response.model,
                    "finish_reason": response.finish_reason,
                    "api_time_ms": api_elapsed,
                    "tool_call_count": response_tool_call_count,
                    "has_visible_text": Self::assistant_visible_text(&response.message),
                    "route_learning": self.route_learning_snapshot(
                        turn_runtime_route.as_ref(),
                        Some(response.model.as_str()),
                    ),
                }),
            );

            // --- Post-LLM hook ---
            let post_ctx = serde_json::json!({
                "turn": total_turns,
                "api_time_ms": api_elapsed,
                "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
            });
            let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
            self.inject_hook_context(&post_results, &mut ctx);
            self.apply_hook_output_transforms(&post_results, &mut response.message.content);

            // Accumulate usage (merged across semantic-retried sub-calls)
            if let Some(ref usage) = turn_usage_acc {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) =
                    estimate_usage_cost_usd(usage, response.model.as_str(), &self.config)
                {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config.max_cost_usd {
                if !cost_warned
                    && session_cost_usd >= limit * self.config.cost_guard_degrade_at_ratio
                {
                    cost_warned = true;
                    if forced_runtime_route.is_none() {
                        if let Some(model) = self.resolve_cost_degrade_model() {
                            forced_runtime_route = Some(self.turn_route_cost_guard(model.clone()));
                            ctx.add_message(Message::system(format!(
                                "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                                session_cost_usd, limit, model
                            )));
                        } else {
                            ctx.add_message(Message::system(format!(
                                "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                                session_cost_usd, limit
                            )));
                        }
                    }
                }
                if session_cost_usd >= limit {
                    ctx.add_message(Message::system(format!(
                        "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                        session_cost_usd, limit
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(
                            &ctx,
                            persist_user_idx,
                            prefill_range.clone(),
                        ),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
            }

            let history_includes_tool = ctx
                .get_messages()
                .iter()
                .any(|m| m.role == MessageRole::Tool);
            let (assistant_msg, parsed_tool_calls, parsed_textual_tool_calls) =
                Self::coerce_textual_tool_calls(response.message.clone());
            if parsed_textual_tool_calls {
                self.emit_status(
                    "lifecycle",
                    "Parsed textual tool-call markup from assistant output; executing parsed calls.",
                );
            }
            ctx.add_message(assistant_msg.clone());
            if assistant_msg
                .tool_calls
                .as_ref()
                .map_or(false, |v| !v.is_empty())
                && Self::assistant_visible_text_after_think_blocks(&assistant_msg)
            {
                last_content_with_tools = assistant_msg
                    .content
                    .as_deref()
                    .map(strip_think_blocks_for_ack)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }

            // Step complete callback
            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done
            let tool_calls = if !parsed_tool_calls.is_empty() {
                parsed_tool_calls
            } else {
                if !tool_schemas.is_empty()
                    && codex_ack_continuations < 2
                    && looks_like_codex_intermediate_ack(
                        &task_hint,
                        assistant_msg.content.as_deref().unwrap_or(""),
                        history_includes_tool,
                    )
                {
                    codex_ack_continuations += 1;
                    ctx.add_message(Message::user(CODEX_CONTINUE_USER_MESSAGE));
                    continue;
                }
                if !Self::assistant_visible_text_after_think_blocks(&assistant_msg) {
                    if let Some(fallback) = last_content_with_tools.take() {
                        if let Some(last) = ctx.get_messages_mut().last_mut() {
                            if last.role == MessageRole::Assistant {
                                last.content = Some(fallback);
                            }
                        }
                    }
                }
                if finalizer_web_research_requires_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_web_research_retries,
                ) {
                    finalizer_web_research_retries =
                        finalizer_web_research_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected missing web research evidence; forcing web tool pass.",
                    );
                    ctx.add_message(Message::system(web_research_retry_prompt()));
                    ctx.add_message(Message::user(
                        "Run the required web research now and re-issue the answer with URLs.",
                    ));
                    continue;
                }
                if finalizer_google_workspace_requires_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_google_workspace_retries,
                ) {
                    finalizer_google_workspace_retries =
                        finalizer_google_workspace_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected ungrounded Google Workspace conclusion; forcing skill setup probe.",
                    );
                    ctx.add_message(Message::system(self.google_workspace_retry_prompt()));
                    ctx.add_message(Message::user(
                        "Run the Google Workspace setup/token probe now and re-issue the answer with exact blockers or email evidence.",
                    ));
                    continue;
                }
                if finalizer_task_focus_requires_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_task_focus_retries,
                ) {
                    finalizer_task_focus_retries = finalizer_task_focus_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected final answer drift from explicit user anchors; forcing focused rewrite.",
                    );
                    ctx.add_message(Message::system(
                        "[SYSTEM] Task-focus contract: the final answer must stay anchored to the user's explicit task nouns, paths, accounts, URLs, or identifiers. If an anchor is unverified, mark it UNPROVEN or BLOCKED instead of switching topics.",
                    ));
                    ctx.add_message(Message::user(
                        "Re-issue the final response now, anchored to the explicit user task and verified evidence.",
                    ));
                    continue;
                }
                if finalizer_repo_research_plan_requires_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_repo_research_plan_retries,
                ) {
                    finalizer_repo_research_plan_retries =
                        finalizer_repo_research_plan_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected shallow repo research synthesis; forcing workstream evidence map.",
                    );
                    ctx.add_message(Message::system(repo_research_retry_prompt()));
                    ctx.add_message(Message::user(
                        "Re-issue the final response with grounded repo research workstreams now.",
                    ));
                    continue;
                }
                if finalizer_claim_requires_evidence_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_evidence_retries,
                ) {
                    finalizer_evidence_retries = finalizer_evidence_retries.saturating_add(1);
                    ctx.add_message(Message::system(
                        "[SYSTEM] Finalizer evidence contract: include explicit evidence lines and confidence calibration.\n\
                         Required format:\n\
                         - confidence=<high|medium|low>\n\
                         - file=<absolute-or-repo-path>\n\
                         - cmd=<verification command or exact probe>\n\
                         - every file/path evidence marker must refer to a path that exists now\n\
                         If evidence is missing, state `objective_state=unproven` and blockers.",
                    ));
                    ctx.add_message(Message::user(
                        "Re-issue the final response with explicit evidence + confidence now.",
                    ));
                    continue;
                }
                if finalizer_output_quality_requires_retry(
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_output_quality_retries,
                ) {
                    finalizer_output_quality_retries =
                        finalizer_output_quality_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected templated/duplicated output; forcing concrete unique rewrite.",
                    );
                    ctx.add_message(Message::system(
                        "[SYSTEM] Output quality contract: do not use placeholders or template filler.\n\
                         Requirements:\n\
                         - no unresolved placeholders (`[URL](URL)`, `(URL)`, `pack of authors`, `<insert...>`)\n\
                         - no repeated list items or duplicated paragraphs\n\
                         - provide concrete, unique, user-relevant items only; if unknown, mark as `UNPROVEN` instead of fabricating.",
                    ));
                    ctx.add_message(Message::user(
                        "Re-issue the response now with concrete unique items and zero placeholders.",
                    ));
                    continue;
                }
                if finalizer_action_execution_requires_retry(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    finalizer_action_execution_retries,
                ) {
                    finalizer_action_execution_retries =
                        finalizer_action_execution_retries.saturating_add(1);
                    self.emit_status(
                        "lifecycle",
                        "Detected intent narration without execution evidence; forcing action run.",
                    );
                    ctx.add_message(Message::system(
                        "[SYSTEM] Execution contract: this request requires concrete execution now.\n\
                         Requirements:\n\
                         - run the relevant tool calls in this turn (do not only describe intent)\n\
                         - if blocked, output `BLOCKED:` with exact command/tool error and next probe\n\
                         - include at least one evidence line (`cmd=...` or `file=...`) in the final response.",
                    ));
                    ctx.add_message(Message::user(
                        "Execute now. Do not narrate intent; return concrete evidence or explicit BLOCKED state.",
                    ));
                    continue;
                }
                finalizer_evidence_retries = 0;
                finalizer_output_quality_retries = 0;
                finalizer_action_execution_retries = 0;
                finalizer_web_research_retries = 0;
                finalizer_google_workspace_retries = 0;
                finalizer_task_focus_retries = 0;
                finalizer_repo_research_plan_retries = 0;
                let (objective_guard_active, requires_analytics, deep_audit_required) =
                    objective_guard_policy(ctx.get_messages());
                if objective_guard_active {
                    let assistant_text = assistant_msg.content.as_deref().unwrap_or_default();
                    let max_guard_retries = if deep_audit_required {
                        OBJECTIVE_DEEP_AUDIT_MAX_RETRIES
                    } else {
                        OBJECTIVE_GUARD_MAX_RETRIES
                    };
                    if !objective_guard_satisfied(
                        assistant_text,
                        requires_analytics,
                        deep_audit_required,
                    ) && objective_guard_retries < max_guard_retries
                    {
                        objective_guard_retries = objective_guard_retries.saturating_add(1);
                        ctx.add_message(Message::system(objective_guard_retry_prompt(
                            requires_analytics,
                            deep_audit_required,
                        )));
                        ctx.add_message(Message::user(
                            "Re-issue the final response with required verified sections now.",
                        ));
                        continue;
                    }
                }
                tracing::debug!("No tool calls in response, finishing naturally");
                if let Err(err) = self.append_objective_runtime_ledger(
                    ctx.get_messages(),
                    assistant_msg.content.as_deref().unwrap_or_default(),
                    total_turns,
                ) {
                    self.emit_status(
                        "lifecycle",
                        &format!("Objective runtime ledger append skipped: {}", err),
                    );
                }
                // Final memory sync
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id, msgs);
                self.spawn_background_review(total_turns, &ctx, review_memory_at_end);
                self.memory_on_session_end(ctx.get_messages());
                replay.record(
                    "session_end",
                    serde_json::json!({
                        "reason": "finished_naturally",
                        "total_turns": total_turns,
                        "session_cost_usd": session_cost_usd,
                    }),
                );
                return Ok(AgentResult {
                    messages: self.messages_for_persisted_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                    ),
                    finished_naturally: true,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                    interrupted: false,
                    session_cost_usd: Some(session_cost_usd),
                    session_started_hooks_fired,
                });
            };

            codex_ack_continuations = 0;

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            if let Some(note) =
                apply_repo_review_tool_profile_narrowing(&mut tool_calls, ctx.get_messages())
            {
                self.emit_status("lifecycle", "Applied repo-review tool profile narrowing.");
                ctx.add_message(Message::system(note));
            }
            if let Some(note) = apply_repo_review_discovery_budget_policy(
                &mut tool_calls,
                ctx.get_messages(),
                &mut repo_review_budget_state,
            ) {
                self.emit_status("lifecycle", "Applied repo-review discovery budget policy.");
                ctx.add_message(Message::system(note));
            }
            if let Some(note) =
                google_workspace_auth_blocker_mutation_guard(ctx.get_messages(), &tool_calls)
            {
                self.emit_status(
                    "lifecycle",
                    "Blocked Google Workspace credential/setup mutation after auth blocker.",
                );
                ctx.add_message(Message::system(note));
                ctx.add_message(Message::user(
                    "Stop setup/remediation. Final-answer the exact Google Workspace auth blocker now.",
                ));
                continue;
            }
            if tool_calls.is_empty() {
                ctx.add_message(Message::system(
                    "[SYSTEM] Tool profile/budget policy filtered this turn's calls. Propose refined, scoped code-inspection calls next.",
                ));
                continue;
            }
            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| !advertised_tool_names.contains(tc.function.name.as_str()))
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                invalid_tool_retries = invalid_tool_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        invalid_tool_retries, self.config.invalid_tool_call_max_retries
                    ),
                );
                if invalid_tool_retries >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    self.memory_on_session_end(ctx.get_messages());
                    return Ok(AgentResult {
                        messages: self.messages_for_persisted_result(
                            &ctx,
                            persist_user_idx,
                            prefill_range.clone(),
                        ),
                        finished_naturally: false,
                        total_turns,
                        tool_errors,
                        usage: accumulated_usage,
                        interrupted: false,
                        session_cost_usd: Some(session_cost_usd),
                        session_started_hooks_fired,
                    });
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, advertised_tool_names_display
                        )
                    } else if !advertised_tool_names.contains(tc.function.name.as_str()) {
                        format!(
                            "Tool '{}' is not enabled in this session. Available tools: {}",
                            tc.function.name, advertised_tool_names_display
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Self::named_tool_result_message(
                        tc.id.clone(),
                        tc.function.name.clone(),
                        content,
                    ));
                }
                continue;
            }
            invalid_tool_retries = 0;

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                invalid_json_retries = invalid_json_retries.saturating_add(1);
                if invalid_json_retries < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            invalid_json_retries, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                invalid_json_retries = 0;
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    ctx.add_message(Self::named_tool_result_message(
                        tc.id.clone(),
                        tc.function.name.clone(),
                        content,
                    ));
                }
                continue;
            }
            invalid_json_retries = 0;
            self.apply_tool_request_middleware_to_calls(&mut tool_calls, total_turns);

            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }

            // Cap concurrent delegate_task calls
            self.cap_delegates(&mut tool_calls);
            let contextlattice_connect_intent =
                detect_contextlattice_connect_intent(ctx.get_messages());

            // --- Pre-tool hook ---
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            let mut steer_markers = Vec::new();
            if self.collect_tool_batch_interrupt(&mut steer_markers) {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    prefill_range.clone(),
                ));
            }
            let tool_span = tracing::info_span!(
                "hermes.tool_batch",
                turn = total_turns,
                tool_count = tool_calls.len(),
                tool_concurrency = tracing::field::Empty,
                tool_time_ms = tracing::field::Empty,
                errors = tracing::field::Empty,
                error_rate = tracing::field::Empty
            );
            let tool_start = Instant::now();
            let tool_governor = governor_for_turn(
                &self.config,
                &ctx,
                tool_calls.len(),
                Some(&turn_governor_runtime),
            );
            let results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    tool_governor.tool_concurrency,
                    &tool_schemas,
                    contextlattice_connect_intent,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            _total_tool_time_ms += tool_elapsed;
            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            let turn_tool_error_rate = if results.is_empty() {
                0.0
            } else {
                turn_tool_error_count as f64 / results.len() as f64
            };
            push_window_f64(
                &mut governor_tool_error_window,
                turn_tool_error_rate,
                governor_window_limit,
            );
            if turn_tool_error_count > 0 {
                governor_consecutive_error_turns =
                    governor_consecutive_error_turns.saturating_add(1);
            } else {
                governor_consecutive_error_turns = 0;
            }
            tool_span.record("tool_concurrency", tool_governor.tool_concurrency);
            tool_span.record("tool_time_ms", tool_elapsed);
            tool_span.record("errors", turn_tool_error_count);
            tool_span.record("error_rate", turn_tool_error_rate);
            tool_span.in_scope(|| {
                tracing::info!(
                    target: "hermes.langfuse",
                    turn = total_turns,
                    tool_count = tool_calls.len(),
                    tool_concurrency = tool_governor.tool_concurrency,
                    tool_time_ms = tool_elapsed,
                    errors = turn_tool_error_count,
                    error_rate = turn_tool_error_rate,
                    "tool batch"
                );
            });
            drop(tool_span);
            replay.record(
                "tool_batch",
                serde_json::json!({
                    "turn": total_turns,
                    "tool_count": tool_calls.len(),
                    "tool_concurrency": tool_governor.tool_concurrency,
                    "tool_time_ms": tool_elapsed,
                    "errors": turn_tool_error_count,
                    "error_rate": turn_tool_error_rate,
                }),
            );
            update_repo_review_budget_state_from_results(
                &mut repo_review_budget_state,
                ctx.get_messages(),
                &results,
            );
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // --- Post-tool hook ---
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "is_error": res.is_error,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            // Enforce budget on tool results
            let mut results = results;
            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }
            let stop_after_tool_results = self.collect_tool_batch_interrupt(&mut steer_markers);
            Self::append_steer_markers_to_last_tool_result(&mut results, &steer_markers);
            let lsp_note = self.lsp_context_note(&tool_calls, &results);

            for result in results {
                replay.record(
                    "tool_result",
                    serde_json::json!({
                        "turn": total_turns,
                        "tool_call_id": result.tool_call_id,
                        "is_error": result.is_error,
                        "content_preview": result.content.chars().take(240).collect::<String>(),
                    }),
                );
                ctx.add_message(Self::tool_result_message_from_execution(
                    &result,
                    &tool_calls,
                ));
            }
            if let Some(note) = lsp_note {
                ctx.add_message(Message::system(note));
            }
            if stop_after_tool_results {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    accumulated_usage.clone(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    prefill_range.clone(),
                ));
            }
            if should_trip_tool_loop_guard(
                governor_consecutive_error_turns,
                tool_calls.len(),
                turn_tool_error_count,
            ) {
                let guard_message = format!(
                    "Tool-loop guard tripped after {} consecutive error turn(s); latest turn failed {}/{} tool call(s).",
                    governor_consecutive_error_turns,
                    turn_tool_error_count,
                    tool_calls.len()
                );
                self.emit_status("lifecycle", &guard_message);
                replay.record(
                    "tool_loop_guard",
                    serde_json::json!({
                        "turn": total_turns,
                        "consecutive_error_turns": governor_consecutive_error_turns,
                        "failed_calls": turn_tool_error_count,
                        "total_calls": tool_calls.len(),
                    }),
                );
                if let Some(summary) = self
                    .handle_tool_loop_guard_summary(
                        &mut ctx,
                        governor_consecutive_error_turns,
                        turn_tool_error_count,
                        tool_calls.len(),
                    )
                    .await?
                {
                    ctx.add_message(summary);
                }
                self.memory_on_session_end(ctx.get_messages());
                return Ok(AgentResult {
                    messages: self.messages_for_persisted_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                    ),
                    finished_naturally: false,
                    total_turns,
                    tool_errors,
                    usage: accumulated_usage,
                    interrupted: false,
                    session_cost_usd: Some(session_cost_usd),
                    session_started_hooks_fired,
                });
            }
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_chars = ctx.total_chars();
            let threshold = ((ctx.max_context_chars().max(1) as f64) * 0.8) as usize;
            if threshold > 0 {
                let progress = total_chars as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if Self::should_emit_context_pressure_warning(
                    progress,
                    tier,
                    &mut context_pressure_warned_at,
                    &mut context_pressure_last_warn_at,
                    &mut context_pressure_last_warn_percent,
                ) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        total_chars,
                        threshold
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx);
        }
    }

}
