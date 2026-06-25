use super::*;

impl AgentLoop {
    async fn context_compression_should_run(&self, ctx: &ContextManager) -> bool {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        let char_threshold = (max_c as f64 * 0.8) as usize;
        if total_chars > char_threshold {
            return true;
        }
        let system_prompt = ctx
            .get_messages()
            .first()
            .filter(|m| m.role == MessageRole::System)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("");
        let tool_schemas = self.tool_registry.schemas();
        let estimated = estimate_request_tokens_for_compression(
            ctx.get_messages(),
            system_prompt,
            &tool_schemas,
        );
        self.context_compressor
            .inner
            .lock()
            .await
            .should_compress(Some(estimated))
    }

    /// Run context compression on `ctx` (auxiliary LLM summary + tool-pair sanitiser).
    /// Returns `true` when messages were actually compressed and session rotation occurred.
    pub(crate) async fn compress_context(&self, ctx: &mut ContextManager) -> bool {
        let task_hint = ctx
            .get_messages()
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let tool_schemas = self.tool_registry.schemas();
        let old_session_id = self.config().session_id.clone().unwrap_or_default();
        let lock_holder = self.compression_lock_holder();
        let sp = self.session_persistence();
        let lock_acquired = if old_session_id.is_empty() {
            true
        } else if let Some(ref db) = sp {
            db.try_acquire_compression_lock(&old_session_id, &lock_holder, 300.0)
                .unwrap_or(false)
        } else {
            true
        };
        if !lock_acquired {
            if let (Some(db), true) = (&sp, !old_session_id.is_empty()) {
                if let Ok(existing) = db.get_compression_lock_holder(&old_session_id) {
                    tracing::warn!(
                        session_id = %old_session_id,
                        holder = ?existing,
                        "compression skipped: another path holds the compression lock"
                    );
                }
            }
            return false;
        }

        let pre_len = ctx.get_messages().len();
        let context_length = get_model_context_length(&crate::runtime_provider::active_model(self));
        let messages = ctx.get_messages().to_vec();
        let memory_hint = self
            .memory_pre_compress_note(&messages)
            .filter(|n| !n.trim().is_empty());
        let estimated_tokens = estimate_messages_tokens(&messages);
        let compressed = {
            let mut compressor = self.context_compressor.inner.lock().await;
            compressor.set_context_length(context_length);
            compressor
                .compress(messages, Some(estimated_tokens), memory_hint.as_deref())
                .await
        };

        let release_lock = || {
            if let (Some(db), true) = (&sp, !old_session_id.is_empty()) {
                let _ = db.release_compression_lock(&old_session_id, &lock_holder);
            }
        };

        if compressed.len() >= pre_len {
            release_lock();
            return false;
        }

        self.invalidate_cached_system_prompt();

        // Increment compaction count for cache diagnostics — each compaction
        // resets the byte-stable prefix, so this counter lets trace_turn
        // report "log_rewrite" as a cache-miss reason.
        if let Ok(mut state) = self.state.lock() {
            state.compaction_count = state.compaction_count.saturating_add(1);
        }

        let (new_system, _) = self.active_cached_system_prompt(&task_hint, &tool_schemas);
        let mut final_messages = compressed;
        Self::patch_leading_system_message(&mut final_messages, &new_system);
        self.reset_interest_sync_cursor();
        self.invalidate_turn_api_messages_cache();

        let new_session_id = Self::new_compression_session_id();
        if let Ok(mut guard) = self.config_runtime.write() {
            let mut updated = (*guard).as_ref().clone();
            updated.session_id = Some(new_session_id.clone());
            *guard = Arc::new(updated);
        }
        self.memory_on_session_switch(&new_session_id, &old_session_id, false, "compression");
        self.reset_session_db_flush_cursor();

        if let Some(ref db) = sp {
            let cfg = self.config();
            let platform = cfg.platform.as_deref();
            let model = crate::runtime_provider::active_model(self);
            let _ = db.create_compression_continuation_session(
                &new_session_id,
                &old_session_id,
                Some(model.as_str()),
                platform,
                &new_system,
            );
            let transcript: Vec<Message> = final_messages
                .iter()
                .filter(|m| m.role != MessageRole::System)
                .cloned()
                .collect();
            let mut cursor = SessionFlushCursor::new();
            let _ = db.replace_session_messages(&new_session_id, &transcript, &mut cursor);
            let _ = db.update_system_prompt(&new_session_id, &new_system);
        }

        ctx.replace_messages(final_messages);
        release_lock();
        true
    }

    /// Compress when char budget or model token threshold is exceeded (Python auto-compaction).
    pub(crate) async fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        let total_chars = ctx.total_chars();
        let max_c = ctx.max_context_chars().max(1);
        let pct = (total_chars * 100) / max_c;

        // Soft compaction threshold (50%): report growing context once
        // WITHOUT triggering compaction, preserving the cache-first prefix.
        // Ported from Reasonix compact.go maybeCompact softCompactRatio.
        const SOFT_THRESHOLD_PCT: usize = 50;
        const COMPACT_TRIGGER_PCT: usize = 80;
        if (SOFT_THRESHOLD_PCT..COMPACT_TRIGGER_PCT).contains(&pct) {
            let should_notice = if let Ok(mut state) = self.state.lock() {
                let was_noticed = state.soft_compact_noticed;
                state.soft_compact_noticed = true;
                // Context dropped below the trigger — a healthy compaction
                // buys breathing room.  Clear the stuck latch so auto-compaction
                // resumes if context grows past the trigger again.
                state.consecutive_compacts = 0;
                state.compact_stuck = false;
                !was_noticed
            } else {
                false
            };
            if should_notice {
                tracing::info!(
                    "Context reached {}% of window; keeping cache-first prefix until compact threshold 80%",
                    pct
                );
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    &format!(
                        "Context at {}% — cache prefix preserved until 80% threshold",
                        pct
                    ),
                );
            }
            return;
        }

        // Below soft threshold: clear the noticed flag so the notice
        // fires again the next time context grows past 50%.
        if pct < SOFT_THRESHOLD_PCT {
            let _ = self.state.lock().map(|mut state| {
                state.soft_compact_noticed = false;
                state.consecutive_compacts = 0;
                state.compact_stuck = false;
            });
        }

        if !self.context_compression_should_run(ctx).await {
            return;
        }

        // Stuck guard: if two consecutive compactions failed to bring
        // context below the trigger, pause auto-compaction.  The system
        // prompt plus one verbatim turn already exceeds the window —
        // re-firing every turn is the loop users hit, so pause and say
        // why.  Ported from Reasonix compact.go compactStuck.
        let is_stuck = if let Ok(state) = self.state.lock() {
            state.compact_stuck
        } else {
            false
        };
        if is_stuck {
            tracing::warn!(
                "Auto-compaction paused: context window too small for compaction to help \
                 (system prompt + one turn exceeds 80% of window). Raise context_window \
                 or shrink tool output. Manual /compress still works."
            );
            return;
        }

        tracing::info!("Context pressure at {}%, triggering compression", pct);
        crate::hooks::emit_status(
            self,
            "lifecycle",
            &format!("Context pressure at {}% — triggering compression", pct),
        );
        self.compress_context(ctx).await;
        let after_chars = ctx.total_chars();

        // Stuck guard: if context is still above the trigger after
        // compaction, increment the consecutive counter.  Two strikes
        // pauses auto-compaction to avoid cache thrashing.
        let after_pct = (after_chars * 100) / max_c;
        if after_pct >= COMPACT_TRIGGER_PCT {
            let should_warn = if let Ok(mut state) = self.state.lock() {
                state.consecutive_compacts = state.consecutive_compacts.saturating_add(1);
                if state.consecutive_compacts >= 2 {
                    state.compact_stuck = true;
                    true
                } else {
                    false
                }
            } else {
                false
            };
            if should_warn {
                tracing::warn!(
                    "Auto-compaction paused: context window too small for compaction to help \
                     (system prompt + one turn exceeds 80% of window). Raise context_window \
                     or shrink tool output."
                );
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    "Auto-compaction paused — context window too small",
                );
            }
        } else {
            // Compaction brought context under the trigger — healthy.
            if let Ok(mut state) = self.state.lock() {
                state.consecutive_compacts = 0;
                state.compact_stuck = false;
            }
        }

        self.emit_compaction_contextlattice_checkpoint(total_chars, after_chars, max_c);
    }

    fn emit_compaction_contextlattice_checkpoint(
        &self,
        before_chars: usize,
        after_chars: usize,
        max_context_chars: usize,
    ) {
        let mode = compaction_governance_mode_runtime();
        if matches!(mode, CompactionGovernanceMode::Off) {
            return;
        }

        let Some(script_path) = contextlattice_orchestration_script_path() else {
            if matches!(mode, CompactionGovernanceMode::Enforce) {
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    "Compaction governance enforce-mode: ContextLattice script missing; checkpoint skipped.",
                );
            }
            return;
        };

        let session = self
            .config()
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "session".to_string());
        let session = session.as_str();
        let topic = format!("runbooks/alpha/compaction/{}", session);
        let pressure_before = ((before_chars as f64 / max_context_chars as f64) * 100.0).round();
        let pressure_after = ((after_chars as f64 / max_context_chars as f64) * 100.0).round();
        let content = format!(
            "compaction_event mode={} session={} before_chars={} after_chars={} max_chars={} pressure_before_pct={} pressure_after_pct={}",
            mode.as_str(),
            session,
            before_chars,
            after_chars,
            max_context_chars,
            pressure_before,
            pressure_after
        );

        let output = Command::new("python3")
            .arg(script_path)
            .arg("write")
            .arg("hermes-agent-ultra")
            .arg(topic)
            .arg(content)
            .env(
                "MEMMCP_ORCHESTRATOR_URL",
                std::env::var("MEMMCP_ORCHESTRATOR_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
            )
            .env(
                "CONTEXTLATTICE_ORCHESTRATOR_URL",
                std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
                    .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
            )
            .env(
                "CONTEXTLATTICE_AGENT_ID",
                std::env::var("CONTEXTLATTICE_AGENT_ID")
                    .unwrap_or_else(|_| "codex_gpt5".to_string()),
            )
            .env(
                "MEMMCP_AGENT_ID",
                std::env::var("MEMMCP_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
            )
            .output();

        match output {
            Ok(out) if out.status.success() => {
                crate::hooks::emit_status(
                    self,
                    "lifecycle",
                    &format!(
                        "ContextLattice compaction checkpoint written ({}% -> {}%).",
                        pressure_before, pressure_after
                    ),
                );
            }
            Ok(out) => {
                if matches!(mode, CompactionGovernanceMode::Enforce) {
                    crate::hooks::emit_status(
                        self,
                        "lifecycle",
                        &format!(
                            "Compaction governance enforce-mode: checkpoint failed (exit={}) {}",
                            out.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&out.stderr)
                        ),
                    );
                }
            }
            Err(err) => {
                if matches!(mode, CompactionGovernanceMode::Enforce) {
                    crate::hooks::emit_status(
                        self,
                        "lifecycle",
                        &format!(
                            "Compaction governance enforce-mode: checkpoint error: {}",
                            err
                        ),
                    );
                }
            }
        }
    }

    /// Drop oldest non-system messages until context is at or below `target_percent` of max.
    fn emergency_trim_context_to_percent(&self, ctx: &mut ContextManager, target_percent: usize) {
        let max_c = ctx.max_context_chars().max(1);
        let target_chars = (max_c * target_percent.min(100)) / 100;
        if ctx.total_chars() <= target_chars {
            return;
        }
        let before = ctx.total_chars();
        let budget = hermes_core::BudgetConfig {
            max_aggregate_chars: target_chars,
            max_result_size_chars: 100_000,
        };
        ctx.truncate_to_budget(&budget);
        let after = ctx.total_chars();
        tracing::warn!(
            "Emergency context trim: {} -> {} chars (target {}% of {} max)",
            before,
            after,
            target_percent,
            max_c
        );
    }

    /// Emit explicit preflight compression status before first LLM call.
    pub(crate) async fn preflight_context_compress_with_status(&self, ctx: &mut ContextManager) {
        let model = crate::runtime_provider::active_model(self);
        let model_tokens = get_model_context_length(model.as_str());
        let max_c = ctx.max_context_chars().max(1);
        let before = ctx.total_chars();
        let before_pct = (before * 100) / max_c;
        let gateway_msgs = ctx
            .get_messages()
            .iter()
            .filter(|m| m.role != MessageRole::System)
            .count();
        if !self.context_compression_should_run(ctx).await {
            // tracing::debug!(
            //     model = %model,
            //     model_context_tokens = model_tokens,
            //     max_context_chars = max_c,
            //     transcript_chars = before,
            //     gateway_messages = gateway_msgs,
            //     context_usage_pct = before_pct,
            //     "Preflight compression check: no compression needed"
            // );
            let _ = (model_tokens, gateway_msgs, before_pct);
            crate::hooks::emit_status(
                self,
                "lifecycle",
                &format!(
                    "Preflight compression check: no compression needed ({}% of context)",
                    before_pct
                ),
            );
            return;
        }
        tracing::info!(
            model = %model,
            model_context_tokens = model_tokens,
            max_context_chars = max_c,
            transcript_chars = before,
            gateway_messages = gateway_msgs,
            context_usage_pct = before_pct,
            "📦 Preflight compression: preparing session"
        );
        crate::hooks::emit_status(
            self,
            "lifecycle",
            &format!(
                "Preflight: compressing before first turn ({}% of context)",
                before_pct
            ),
        );
        // Avoid auxiliary summarisation on multi-megabyte histories (very slow, often ineffective).
        if before_pct > 150 {
            let trim_target = if before_pct > 400 { 40 } else { 60 };
            self.emergency_trim_context_to_percent(ctx, trim_target);
            if !self.context_compression_should_run(ctx).await {
                let after_pct = (ctx.total_chars() * 100) / max_c;
                tracing::info!(
                    "Preflight: emergency trim sufficient ({}% -> {}%)",
                    before_pct,
                    after_pct
                );
                return;
            }
        }
        self.auto_compress_if_over_threshold(ctx).await;
        let mut after = ctx.total_chars();
        let mut after_pct = (after * 100) / max_c;
        let threshold_pct = {
            let compressor = self.context_compressor.inner.lock().await;
            (compressor.threshold_percent() * 100.0) as usize
        };
        if after_pct >= threshold_pct {
            self.emergency_trim_context_to_percent(ctx, 50);
            after = ctx.total_chars();
            after_pct = (after * 100) / max_c;
        }
        tracing::info!(
            "Preflight compression complete: {}% -> {}% context usage",
            before_pct,
            after_pct
        );
        crate::hooks::emit_status(
            self,
            "lifecycle",
            &format!(
                "Preflight compression complete: {}% -> {}% context usage",
                before_pct, after_pct
            ),
        );
        if after_pct >= threshold_pct {
            crate::hooks::emit_status(
                self,
                "lifecycle",
                &format!(
                    "会话上下文仍超过窗口容量（约 {}%）。请发送 /new 或 /reset 开始新会话后再问。",
                    after_pct
                ),
            );
            tracing::warn!(
                "Preflight compression did not reduce context enough ({}% -> {}%): \
                 LLM call may hit context limit",
                before_pct,
                after_pct
            );
        }
    }

    pub(crate) fn should_emit_context_pressure_warning(
        progress_ratio: f64,
        tier: f64,
        warned_tier: &mut f64,
        last_warn_at: &mut Option<Instant>,
        last_warn_percent: &mut f64,
    ) -> bool {
        if tier <= 0.0 {
            return false;
        }
        let progress_percent = progress_ratio * 100.0;
        let now = Instant::now();
        const WARN_COOLDOWN_SECS: u64 = 20;
        const WARN_PERCENT_STEP: f64 = 5.0;

        let tier_upgraded = tier > *warned_tier;
        let cooldown_elapsed = last_warn_at
            .map(|t| now.duration_since(t) >= Duration::from_secs(WARN_COOLDOWN_SECS))
            .unwrap_or(true);
        let percent_advanced = (progress_percent - *last_warn_percent) >= WARN_PERCENT_STEP;

        if tier_upgraded || (cooldown_elapsed && percent_advanced) {
            if tier_upgraded {
                *warned_tier = tier;
            }
            *last_warn_at = Some(now);
            *last_warn_percent = progress_percent;
            return true;
        }
        false
    }

    pub(crate) fn assistant_visible_text(m: &Message) -> bool {
        m.content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub(crate) fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        let Some(content) = m.content.as_deref() else {
            return false;
        };
        !agent_runtime_helpers::strip_think_blocks(content)
            .trim()
            .is_empty()
    }

    pub(crate) fn assistant_has_reasoning(m: &Message) -> bool {
        m.reasoning_content
            .as_deref()
            .map(str::trim)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    fn finish_reason_requires_continuation(finish_reason: Option<&str>) -> bool {
        matches!(finish_reason, Some("length" | "pause_turn"))
    }

    fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        if tool_calls.is_empty() || tool_calls.iter().all(|tc| tc.function.name.is_empty()) {
            let mut m = Message::assistant(content.to_string());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        } else {
            let content_opt = if content.is_empty() {
                None
            } else {
                Some(content.to_string())
            };
            let mut m = Message::assistant_with_tool_calls(content_opt, tool_calls.to_vec());
            if !reasoning_content.is_empty() {
                m.reasoning_content = Some(reasoning_content.to_string());
            }
            m
        }
    }

    fn partial_stream_stub_outcome(
        recovered_text: &str,
        tool_calls: &[ToolCall],
        last_usage: Option<UsageStats>,
        model: &str,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
        err: &AgentError,
    ) -> StreamCollectOutcome {
        let dropped = partial_stream_dropped_tool_names(tool_calls);
        let mut content = recovered_text.to_string();
        if !dropped.is_empty() {
            let warn = format_partial_stream_tool_call_warning(&dropped);
            on_chunk(StreamChunk {
                delta: Some(hermes_core::StreamDelta {
                    content: Some(warn.clone()),
                    tool_calls: None,
                    extra: None,
                }),
                finish_reason: None,
                usage: None,
            });
            content.push_str(&warn);
            tracing::warn!(
                dropped_tools = ?dropped,
                recovered_chars = recovered_text.chars().count(),
                error = %err,
                "Partial stream dropped tool call(s); returning length stub for continuation"
            );
        } else {
            tracing::warn!(
                recovered_chars = recovered_text.chars().count(),
                error = %err,
                "Partial stream delivered before error; returning length stub for continuation"
            );
        }
        let mut response = build_partial_stream_stub_response(
            model,
            content,
            if dropped.is_empty() {
                None
            } else {
                Some(dropped)
            },
        );
        response.usage = last_usage;
        StreamCollectOutcome::Complete(response)
    }
}
