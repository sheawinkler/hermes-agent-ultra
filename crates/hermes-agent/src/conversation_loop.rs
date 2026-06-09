//! Python `agent.conversation_loop.run_conversation`

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use hermes_core::{
    AgentError, AgentResult, Message, MessageRole, StreamChunk, ToolCall, ToolSchema, UsageStats,
};
use serde_json::Value;

use crate::agent_loop::{
    AgentLoop, LoopExit, OBJECTIVE_DEEP_AUDIT_MAX_RETRIES, OBJECTIVE_GUARD_MAX_RETRIES,
    ReplayRecorder, RepoReviewBudgetState, StreamCollectOutcome, ToolProgressWatchdog,
    TurnRuntimeRoute, apply_repo_review_discovery_budget_policy,
    apply_repo_review_tool_profile_narrowing, apply_web_tool_budget,
    build_auxiliary_arc_for_config, contextlattice_connect_system_hint,
    contextlattice_intelligence_system_hint, detect_contextlattice_connect_intent,
    effective_max_turns, estimate_usage_cost_usd, exploratory_problem_solving_system_hint,
    extract_last_user_assistant, finalizer_action_execution_requires_retry,
    finalizer_claim_requires_evidence_retry, finalizer_output_quality_requires_retry,
    governor_for_turn, governor_runtime_state, governor_window_size, is_budgeted_web_tool,
    latest_user_content, merge_usage, objective_guard_policy, objective_guard_retry_prompt,
    objective_guard_satisfied, objective_mode_system_hint, push_window_f64, push_window_u64,
    should_apply_turn_reliability_guard, should_trip_tool_loop_guard,
    update_repo_review_budget_state_from_results, web_tool_budget_max_consecutive_errors,
    web_tool_budget_user_notice,
};
use crate::budget;
use crate::codex_responses_adapter::summarize_user_message_for_log_str;
use crate::context::ContextManager;
use crate::message_sanitization::{
    CLARIFY_TOOL_RETRY_MAX, CLARIFY_TOOL_RETRY_USER_MESSAGE, CODEX_CONTINUE_USER_MESSAGE,
    budget_pressure_text, clarify_tool_invocation_requires_retry, continuation_prompt_for_response,
    inject_budget_pressure_into_last_tool_result, sanitize_surrogates,
    strip_system_messages_from_history, strip_think_blocks_for_ack,
};
use crate::plugins::{HookResult, HookType};
use crate::session_persistence::leading_system_prompt_for_persist;
use crate::web_research::WebResearchController;

/// Inputs for one `run_conversation` call (Python `run_conversation` kwargs).
pub struct RunConversationParams {
    pub user_message: String,
    pub conversation_history: Vec<Message>,
    pub task_id: Option<String>,
    pub stream_callback: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    pub persist_user_message: Option<String>,
    pub tools: Option<Vec<ToolSchema>>,
    /// When set, persist session messages to SQLite after the turn (gateway/HTTP parity).
    pub persist_session: bool,
}

/// Python `run_conversation` return value.
///
/// Composes the C?D [`AgentResult`] with E-segment fields. Not a flat duplicate of the dict:
/// loop mechanics live in [`Self::loop_result`]; presentation lives in the top-level optional fields.
#[derive(Debug, Clone)]
pub struct ConversationResult {
    /// Autonomous loop output (messages, usage, `api_calls`, `turn_exit_reason`, ??.
    pub loop_result: AgentResult,
    /// Last assistant visible text after turn-level hooks (Python `final_response`).
    pub final_response: Option<String>,
    /// Reasoning from the current turn only (Python `last_reasoning`).
    pub last_reasoning: Option<String>,
    /// `finished_naturally && !interrupted` (Python `completed`).
    pub completed: bool,
}

impl ConversationResult {
    pub fn messages(&self) -> &[Message] {
        &self.loop_result.messages
    }

    pub fn interrupted(&self) -> bool {
        self.loop_result.interrupted
    }

    pub fn pending_steer(&self) -> Option<&str> {
        self.loop_result.pending_steer.as_deref()
    }

    pub fn api_calls(&self) -> u32 {
        self.loop_result.api_calls
    }

    pub fn turn_exit_reason(&self) -> &str {
        &self.loop_result.turn_exit_reason
    }

    pub fn failed(&self) -> bool {
        self.loop_result.failed
    }

    pub fn partial(&self) -> bool {
        self.loop_result.partial
    }

    pub fn guardrail(&self) -> Option<&serde_json::Value> {
        self.loop_result.guardrail.as_ref()
    }

    pub fn interrupt_message(&self) -> Option<&str> {
        self.loop_result.interrupt_message.as_deref()
    }

    pub fn session_cost_usd(&self) -> Option<f64> {
        self.loop_result.session_cost_usd
    }

    pub fn cost_status(&self) -> Option<&str> {
        self.loop_result.cost_status.as_deref()
    }

    pub fn cost_source(&self) -> Option<&str> {
        self.loop_result.cost_source.as_deref()
    }

    pub fn input_tokens(&self) -> Option<u64> {
        self.loop_result.input_tokens
    }

    pub fn output_tokens(&self) -> Option<u64> {
        self.loop_result.output_tokens
    }

    pub fn runtime_model(&self) -> Option<&str> {
        self.loop_result.model.as_deref()
    }

    pub fn runtime_provider(&self) -> Option<&str> {
        self.loop_result.provider.as_deref()
    }

    pub fn runtime_base_url(&self) -> Option<&str> {
        self.loop_result.base_url.as_deref()
    }

    /// Consume into the loop result (for callers that only need [`AgentResult`]).
    pub fn into_loop_result(self) -> AgentResult {
        self.loop_result
    }
}

/// Metadata carried from B segment through E segment.
#[derive(Debug, Clone)]
pub struct TurnFinalizeMeta {
    /// Inbound user turn text (Python `user_message` kwarg), not `persist_user_message`.
    pub inbound_user_message: String,
    pub original_user_message: String,
    pub task_id: String,
    pub persist_session: bool,
}

/// Output of B-segment [`AgentLoop::prepare_turn`].
#[derive(Debug, Clone)]
pub struct PreparedTurn {
    pub meta: TurnFinalizeMeta,
    pub messages: Vec<Message>,
}

/// Split gateway/session messages into prior history and the current user turn text.
///
/// Session storage includes the inbound user line; `run_conversation` appends it again in
/// `prepare_turn`, so callers must pass history **without** the current turn's user message,
/// or use this helper on the full session slice.
pub fn split_messages_for_run_conversation(messages: &[Message]) -> Option<(Vec<Message>, String)> {
    let messages = strip_system_messages_from_history(messages);
    let idx = messages.iter().rposition(|m| m.role == MessageRole::User)?;
    let user_message = messages[idx].content.clone().unwrap_or_default();
    let mut history = messages;
    history.remove(idx);
    Some((history, user_message))
}

/// Last assistant visible text in the message list.
pub fn extract_last_assistant_reply(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|m| {
        if m.role == MessageRole::Assistant {
            m.content.clone().filter(|c| !c.trim().is_empty())
        } else {
            None
        }
    })
}

/// Reasoning from the current turn only (stop at the prior user boundary).
pub fn extract_last_reasoning_current_turn(messages: &[Message]) -> Option<String> {
    for msg in messages.iter().rev() {
        if msg.role == MessageRole::User {
            break;
        }
        if msg.role == MessageRole::Assistant {
            if let Some(ref r) = msg.reasoning_content {
                if !r.trim().is_empty() {
                    return Some(r.clone());
                }
            }
        }
    }
    None
}

impl AgentLoop {
    /// Run one full user turn (Python `run_conversation`).
    pub async fn run_conversation(
        &self,
        params: RunConversationParams,
    ) -> Result<ConversationResult, AgentError> {
        let prepared = self.prepare_turn(&params).await?;
        let tools = params.tools;
        let stream_callback = params.stream_callback;
        let loop_result = self
            .run_with_message_prelude(prepared.messages.clone(), tools, stream_callback, true)
            .await?;
        Ok(self.finalize_turn(loop_result, &prepared.meta))
    }

    /// build message list and apply per-turn prelude.
    pub(crate) async fn prepare_turn(
        &self,
        params: &RunConversationParams,
    ) -> Result<PreparedTurn, AgentError> {
        // Sanitize surrogate characters from user input.  Clipboard paste from
        // rich-text editors (Google Docs, Word, etc.) can inject lone surrogates
        // that are invalid UTF-8 and crash JSON serialization in the OpenAI SDK.
        let mut user_message = params.user_message.clone();
        user_message = sanitize_surrogates(&user_message).into_owned();

        let persist_override = params
            .persist_user_message
            .as_ref()
            .map(|s| sanitize_surrogates(s).into_owned());

        let original_user_message = persist_override
            .clone()
            .unwrap_or_else(|| user_message.clone());

        {
            let mut cfg = self
                .config_runtime
                .write()
                .unwrap_or_else(|e| e.into_inner());
            cfg.persist_user_message = persist_override;
        }

        let task_id = params
            .task_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if let Ok(mut slot) = self.current_task_id.lock() {
            *slot = Some(task_id.clone());
        }

        let conversation_history = strip_system_messages_from_history(&params.conversation_history);
        self.hydrate_memory_nudge_counters_from_history(&conversation_history);
        let user_turn_count = {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.user_turn_count = c.user_turn_count.saturating_add(1);
                c.user_turn_count
            } else {
                1
            }
        };

        let inbound_user_message = user_message.clone();
        let history_len = conversation_history.len();

        let mut messages: Vec<Message> = conversation_history;
        messages.push(Message::user(user_message));

        self.apply_turn_message_prelude(&mut messages).await;

        crate::session_log::set_session_context(self.config().session_id.as_deref());
        self.replay_compression_warning_at_turn_start().await;
        self.reset_vision_supported_for_turn();
        self.cleanup_dead_connections_at_turn_start().await;

        let preview_text = summarize_user_message_for_log_str(&inbound_user_message);
        let msg_preview = if preview_text.chars().count() > 80 {
            format!("{}...", preview_text.chars().take(80).collect::<String>())
        } else {
            preview_text.clone()
        };
        let msg_preview = msg_preview.replace('\n', " ");
        let rt = self.primary_runtime_snapshot();
        tracing::info!(
            session_id = %crate::session_log::current_session_tag(),
            task_id = %task_id,
            user_turn = user_turn_count,
            model = %self.active_model(),
            provider = rt.provider.as_deref().unwrap_or("unknown"),
            platform = %self.config().platform.as_deref().unwrap_or("unknown"),
            history_len = history_len,
            msg = %msg_preview,
            "conversation turn"
        );
        if !self.config().quiet_mode {
            let print_preview = summarize_user_message_for_log_str(&inbound_user_message);
            let suffix = if print_preview.chars().count() > 60 {
                "..."
            } else {
                ""
            };
            let short: String = print_preview.chars().take(60).collect();
            self.emit_status(
                "lifecycle",
                &format!("💬 Starting conversation: '{short}{suffix}'"),
            );
        }
        self.memory_on_turn_start(user_turn_count, &original_user_message);

        self.apply_turn_prep_infrastructure_hooks();
        crate::skill_provenance::set_current_write_origin("assistant_tool");

        Ok(PreparedTurn {
            meta: TurnFinalizeMeta {
                inbound_user_message,
                original_user_message,
                task_id,
                persist_session: params.persist_session,
            },
            messages,
        })
    }

    /// turn-level hooks + [`ConversationResult`] + optional session persist.
    pub(crate) fn finalize_turn(
        &self,
        mut loop_result: AgentResult,
        meta: &TurnFinalizeMeta,
    ) -> ConversationResult {
        let messages = loop_result.messages.clone();
        let mut final_response = extract_last_assistant_reply(&messages);
        let last_reasoning = extract_last_reasoning_current_turn(&messages);
        let interrupted = loop_result.interrupted;
        let max_iterations =
            effective_max_turns(self.config().max_turns).unwrap_or(self.config().max_turns);
        let completed = final_response.is_some()
            && !loop_result.failed
            && !interrupted
            && loop_result.api_calls < max_iterations;

        if let Some(ref mut text) = final_response {
            if !interrupted {
                *text = self.apply_turn_level_output_hooks(text, meta, &messages);
            }
        }

        let mut messages = messages;
        self.apply_turn_finalize_infrastructure_hooks(meta, &mut messages, &loop_result, completed);
        self.log_turn_exit_diagnostic(&loop_result, &messages);

        self.sync_external_memory_for_turn(
            &meta.original_user_message,
            final_response.as_deref(),
            interrupted,
        );

        // Python `_persist_session` always runs at turn end; `persist_turn_session` no-ops without session_id.
        self.persist_turn_session(&messages, &loop_result);

        loop_result.messages = messages;
        loop_result.messages.shrink_to_fit();
        let loop_result = self.finalize_agent_result(loop_result);
        hermes_telemetry::record_agent_turn();
        crate::session_log::clear_session_context();

        ConversationResult {
            final_response,
            last_reasoning,
            completed,
            loop_result,
        }
    }

    fn hydrate_memory_nudge_counters_from_history(&self, history: &[Message]) {
        if history.is_empty() {
            return;
        }
        let interval = self.config().memory_nudge_interval;
        if interval == 0 {
            return;
        }
        let prior_user_turns = history
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count();
        if prior_user_turns == 0 {
            return;
        }
        if let Ok(mut c) = self.evolution_counters.lock() {
            if c.user_turn_count == 0 {
                c.user_turn_count = prior_user_turns as u32;
            }
            if c.turns_since_memory == 0 {
                c.turns_since_memory = (prior_user_turns % interval as usize) as u32;
            }
        }
    }

    fn apply_turn_level_output_hooks(
        &self,
        response: &str,
        meta: &TurnFinalizeMeta,
        messages: &[Message],
    ) -> String {
        let history_json: Vec<serde_json::Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let hook_ctx = serde_json::json!({
            "session_id": self.config().session_id,
            "user_message": meta.original_user_message,
            "assistant_response": response,
            "conversation_history": history_json,
            "model": self.active_model(),
            "platform": self.config().platform,
            "task_id": meta.task_id,
        });
        let results = self.invoke_hook(HookType::PostLlmCall, &hook_ctx);
        let mut out = response.to_string();
        for r in results {
            if let HookResult::TransformLlmOutput(next) = r {
                if !next.is_empty() {
                    out = next;
                }
            }
        }
        out
    }

    fn persist_turn_session(&self, messages: &[Message], inner: &AgentResult) {
        let cfg = self.config();
        let Some(ref sid) = cfg.session_id else {
            return;
        };
        if sid.trim().is_empty() {
            return;
        }
        let Some(sp) = self.session_persistence() else {
            return;
        };
        let transcript = strip_system_messages_from_history(messages);
        let sys = leading_system_prompt_for_persist(messages);
        let platform = cfg.platform.as_deref();
        let model = self.active_model();
        let mut cursor = self
            .session_db_flush
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        let result = sp.persist_session(
            sid,
            &transcript,
            &mut cursor,
            Some(model.as_str()),
            platform,
            None,
            sys.as_deref(),
        );
        if let Ok(mut guard) = self.session_db_flush.lock() {
            *guard = cursor;
        }
        if let Err(err) = result {
            tracing::warn!(session_id = %sid, "persist_session after run_conversation: {}", err);
            return;
        }

        if let Some(ref usage) = inner.usage {
            let update = hermes_tools::state_db::TokenCountUpdate::increment(
                i64::try_from(usage.prompt_tokens).unwrap_or(i64::MAX),
                i64::try_from(usage.completion_tokens).unwrap_or(i64::MAX),
                Some(model.clone()),
                usage.estimated_cost.or(inner.session_cost_usd),
            );
            if let Err(err) = sp.update_token_counts(sid, &update) {
                tracing::warn!(session_id = %sid, "update_token_counts after run_conversation: {}", err);
            }
        }
    }

    /// Active task id for this turn (Python `agent._current_task_id`).
    pub fn current_task_id(&self) -> Option<String> {
        self.current_task_id.lock().ok().and_then(|g| g.clone())
    }

    /// Returns `(prompt, restored_from_storage)` using session-level cache when warm.
    pub(crate) fn active_cached_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        if let Ok(guard) = self.cached_system_prompt.lock() {
            if let Some(ref cached) = *guard {
                if !cached.trim().is_empty() {
                    return (cached.clone(), true);
                }
            }
        }
        let (prompt, restored) = self.resolve_initial_system_prompt(task_hint, tool_schemas);
        if let Ok(mut guard) = self.cached_system_prompt.lock() {
            *guard = Some(prompt.clone());
        }
        (prompt, restored)
    }

    /// Returns `(prompt, restored_from_storage)` - restored prompts skip fresh `build_system_prompt`.
    pub(crate) fn resolve_initial_system_prompt(
        &self,
        task_hint: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        if let Some(ref s) = self.config().stored_system_prompt {
            let t = s.trim();
            if !t.is_empty() {
                return (s.clone(), true);
            }
        }
        (
            self.build_system_prompt(task_hint, tool_schemas, &self.active_model()),
            false,
        )
    }

    /// Run the autonomous loop with non-streaming LLM transport.
    ///
    /// Non-streaming variant; see [`Self::run_with_message_prelude`] for the shared core.
    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        self.run_with_message_prelude(messages, tools, None, false)
            .await
    }
    #[inline]
    fn emit_stream_chunk(emit: Option<&(dyn Fn(StreamChunk) + Send + Sync)>, chunk: StreamChunk) {
        if let Some(f) = emit {
            f(chunk);
        }
    }

    async fn run_with_message_prelude(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
        on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
        skip_message_prelude: bool,
    ) -> Result<AgentResult, AgentError> {
        // Python `_has_stream_consumers` (UI) vs `_use_streaming` (transport) ? see `use_streaming_llm_transport`.
        let ui_streaming = on_chunk.is_some();
        let stream_mute = ui_streaming.then(|| Arc::new(AtomicBool::new(false)));
        let stream_needs_break = ui_streaming.then(|| Arc::new(AtomicBool::new(false)));
        let stream_chunk_sink: Box<dyn Fn(StreamChunk) + Send + Sync> =
            if let Some(raw_emit) = on_chunk {
                let stream_mute = stream_mute.clone().expect("streaming");
                let break_for_emit = stream_needs_break.clone().expect("streaming");
                Box::new(move |chunk: StreamChunk| {
                    let StreamChunk {
                        delta,
                        finish_reason,
                        usage,
                    } = chunk;
                    if let Some(delta_val) = delta {
                        if let Some(content) = delta_val.content.clone() {
                            if stream_mute.load(Ordering::Acquire) {
                                return;
                            }
                            let mut out = content;
                            if break_for_emit.swap(false, Ordering::AcqRel) {
                                out = format!("\n\n{}", out);
                            }
                            raw_emit(StreamChunk {
                                delta: Some(hermes_core::StreamDelta {
                                    content: Some(out),
                                    tool_calls: delta_val.tool_calls,
                                    extra: delta_val.extra,
                                }),
                                finish_reason,
                                usage,
                            });
                            return;
                        }
                        raw_emit(StreamChunk {
                            delta: Some(delta_val),
                            finish_reason,
                            usage,
                        });
                        return;
                    }
                    raw_emit(StreamChunk {
                        delta: None,
                        finish_reason,
                        usage,
                    });
                })
            } else {
                Box::new(|_| ())
            };

        let mut ctx = ContextManager::for_model(self.active_model().as_str());
        let mut tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
        let session_id_owned = self.config().session_id.clone().unwrap_or_default();
        let session_id = session_id_owned.as_str();
        let mut messages = messages;
        if !skip_message_prelude {
            self.apply_turn_message_prelude(&mut messages).await;
        }

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Determine which tools to expose
        let tool_schemas: Arc<[ToolSchema]> = match tools {
            Some(v) => Arc::from(v),
            None => self.tool_registry.schemas(),
        };
        // Build and inject system prompt (or reuse session-level cache for prefix stability)
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config().session_id,
                "model": self.active_model(),
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        let prefill_start = ctx.get_messages().len();
        for msg in &self.config().prefill_messages {
            ctx.add_message(msg.clone());
        }
        let prefill_end = ctx.get_messages().len();
        let prefill_range = (prefill_end > prefill_start).then_some(prefill_start..prefill_end);

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.interest_sync_user_messages(ctx.get_messages());
        self.hydrate_todo_store(&ctx);
        if let Some(hint) = contextlattice_connect_system_hint(ctx.get_messages()) {
            ctx.add_message(Message::system(hint));
        }
        if let Some(hint) = exploratory_problem_solving_system_hint(ctx.get_messages()) {
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

        let persist_user_idx = if self.config().persist_user_message.is_some() {
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
        if self.config().memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config().memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }
        let mut api_call_count: u32 = 0;

        // Memory prefetch
        let first_user = ctx
            .get_messages()
            .iter()
            .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
            .last()
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id);
        self.set_turn_ext_prefetch_cache(mem_ctx_raw);

        self.apply_pre_llm_call_hooks_once(&mut ctx, &first_user, session_id);

        if self.api_mode_is_codex_app_server() {
            let loop_messages: Vec<Message> = ctx.get_messages().to_vec();
            let loop_result = self
                .run_codex_app_server_turn(
                    &first_user,
                    loop_messages,
                    review_memory_at_end,
                    session_started_hooks_fired,
                )
                .await;
            return Ok(loop_result);
        }

        if self.config().preflight_context_compress {
            self.preflight_context_compress_with_status(&mut ctx).await;
        }
        let replay = ReplayRecorder::for_session(&self.config(), session_id);
        let max_turns_limit = effective_max_turns(self.config().max_turns);
        replay.record(
            "session_start",
            serde_json::json!({
                "session_id": session_id,
                "mode": if self.use_streaming_llm_transport(ui_streaming, 0, None) {
                    "stream"
                } else {
                    "run"
                },
                "model": self.active_model(),
                "max_turns": self.config().max_turns,
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
        let mut truncated_tool_call_retries: u32 = 0;
        let mut continuation_retries: u32 = 0;
        let mut last_content_with_tools: Option<String> = None;
        let mut continuation_trigger_count: u32 = 0;
        let mut ack_trigger_count: u32 = 0;
        let mut premature_finalize_suspected_count: u32 = 0;
        let mut context_pressure_warned_at: f64 = 0.0;
        let mut context_pressure_last_warn_at: Option<Instant> = None;
        let mut context_pressure_last_warn_percent: f64 = 0.0;
        let mut governor_llm_latency_window: VecDeque<u64> = VecDeque::new();
        let mut governor_tool_error_window: VecDeque<f64> = VecDeque::new();
        let mut governor_consecutive_error_turns: u32 = 0;
        let mut web_tool_calls_used: u32 = 0;
        let mut web_search_calls_used: u32 = 0;
        let mut web_tool_consecutive_error_turns: u32 = 0;
        let mut repo_review_budget_state = RepoReviewBudgetState::default();
        let mut objective_guard_retries: u32 = 0;
        let mut finalizer_evidence_retries: u32 = 0;
        let mut finalizer_output_quality_retries: u32 = 0;
        let mut finalizer_action_execution_retries: u32 = 0;
        let mut clarify_tool_retries: u32 = 0;
        let governor_window_limit = governor_window_size();
        let budget_cap = max_turns_limit.unwrap_or(self.config().max_turns);
        let mut iteration_budget = crate::iteration_budget::IterationBudget::new(budget_cap);
        let mut tool_guardrails = crate::tool_guardrails::ToolGuardrailController::new();
        let mut file_mutation = crate::file_mutation_tracker::FileMutationTracker::new(
            self.config().checkpoints_enabled,
        );
        let mut stream_scrubber = if self.use_streaming_llm_transport(ui_streaming, 0, None) {
            Some(crate::stream_scrubber::ThinkBlockScrubber::new())
        } else {
            None
        };
        let mut checkpoint_mgr = hermes_tools::CheckpointManager::new(
            self.config().checkpoints_enabled,
            self.config().hermes_home.as_deref().map(Path::new),
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        );

        let web_research_cfg = self.config().web_research.clone();
        let web_auxiliary = if web_research_cfg.enabled
            && (web_research_cfg.planner_enabled || web_research_cfg.evaluator_enabled)
        {
            Some(build_auxiliary_arc_for_config(&self.config()))
        } else {
            None
        };
        let mut web_research_ctrl = web_research_cfg
            .enabled
            .then(|| WebResearchController::new(web_research_cfg));
        let mut active_tool_schemas = tool_schemas.clone();
        let mut web_finalize_hint_injected = false;

        loop {
            if let Some(scrubber) = stream_scrubber.as_mut() {
                scrubber.reset();
            }
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    std::mem::take(&mut tool_errors),
                    accumulated_usage.take(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    prefill_range.clone(),
                    api_call_count,
                ));
            }

            if let Some(max_turns) = max_turns_limit {
                if iteration_budget.exhausted() || total_turns >= max_turns {
                    tracing::warn!(
                        "Max turns ({}) exceeded, requesting final summary",
                        max_turns
                    );
                    let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                    if let Some(msg) = summary_msg {
                        ctx.add_message(msg);
                    }
                    self.turn_end_plugin_hooks(
                        ctx.get_messages(),
                        false,
                        false,
                        total_turns,
                        session_started_hooks_fired,
                    );
                    replay.record(
                        "session_end",
                        serde_json::json!({
                            "reason": "max_turns",
                            "total_turns": total_turns,
                            "session_cost_usd": session_cost_usd,
                        }),
                    );
                    return Ok(self.seal_loop_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                        LoopExit {
                            turn_exit_reason: "max_iterations_reached",
                            api_calls: api_call_count,
                            failed: false,
                            partial: false,
                            finished_naturally: false,
                            interrupted: false,
                        },
                        total_turns,
                        std::mem::take(&mut tool_errors),
                        accumulated_usage.take(),
                        session_cost_usd,
                        session_started_hooks_fired,
                    ));
                }
            }

            total_turns += 1;
            self.invalidate_turn_api_messages_cache();
            checkpoint_mgr.new_turn();
            iteration_budget.consume();
            tracing::debug!("Agent turn {}", total_turns);

            // Housekeeping-only turns enable mute_post_response for the *current* turn's
            // pre-tool narration. Reset each turn so the next LLM stream (especially the
            // final natural-language reply) is delivered to gateway native streaming.
            if let Some(mute) = stream_mute.as_ref() {
                mute.store(false, Ordering::Release);
            }

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.refresh_oauth_store_tokens_if_needed().await;

            // Skill nudge counter ? Python `run_agent.py`: increment at the start of each inner API iteration.
            if self.config().skill_creation_nudge_interval > 0
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

            if self.config().checkpoint_interval_turns > 0
                && (total_turns - 1) % self.config().checkpoint_interval_turns == 0
            {
                last_checkpoint_messages = Some(ctx.get_messages().to_vec());
            }

            // Memory sync at flush interval
            if total_turns % self.config().memory_flush_interval == 0 && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id);
            }

            // --- Pre-LLM hook ---
            let turn_runtime_route = forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let turn_default_model = self.active_model();
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(turn_default_model.as_str());
            let turn_governor_runtime = governor_runtime_state(
                &governor_llm_latency_window,
                &governor_tool_error_window,
                governor_consecutive_error_turns,
            );
            let llm_governor =
                governor_for_turn(&self.config(), &ctx, 0, Some(&turn_governor_runtime));

            if let Some(ref ctrl) = web_research_ctrl {
                active_tool_schemas = Arc::from(ctrl.filter_tool_schemas(tool_schemas.as_ref()));
                if !web_finalize_hint_injected {
                    if let Some(hint) = ctrl.finalization_system_hint() {
                        ctx.add_message(Message::system(hint));
                        web_finalize_hint_injected = true;
                    }
                }
            }

            let approx_request_tokens = crate::compression::estimate_request_tokens_for_compression(
                ctx.get_messages(),
                &system_content,
                active_tool_schemas.as_ref(),
            ) as u32;
            let rt_snap = self.primary_runtime_snapshot();
            if let Some(err) = crate::message_sanitization::ollama_context_limit_error(
                self.config().ollama_num_ctx,
                !active_tool_schemas.is_empty(),
                approx_request_tokens,
                active_model,
                rt_snap.provider.as_deref().unwrap_or("unknown"),
                rt_snap.base_url.as_deref().unwrap_or("unknown"),
                active_tool_schemas.len(),
                self.config().session_id.as_deref(),
            ) {
                ctx.add_message(Message::assistant(err));
                iteration_budget.refund(1);
                total_turns = total_turns.saturating_sub(1);
                return Ok(self.seal_loop_result(
                    &ctx,
                    persist_user_idx,
                    prefill_range.clone(),
                    LoopExit {
                        turn_exit_reason: "ollama_runtime_context_too_small",
                        api_calls: api_call_count,
                        failed: true,
                        partial: false,
                        finished_naturally: false,
                        interrupted: false,
                    },
                    total_turns,
                    std::mem::take(&mut tool_errors),
                    accumulated_usage.take(),
                    session_cost_usd,
                    session_started_hooks_fired,
                ));
            }

            if forced_runtime_route.is_none()
                && should_apply_turn_reliability_guard(
                    &turn_governor_runtime,
                    &llm_governor,
                    governor_llm_latency_window.len(),
                )
            {
                if let Some(model) = self
                    .resolve_reliability_degrade_model(active_model, turn_runtime_route.as_ref())
                {
                    tracing::info!(
                        turn = total_turns,
                        model = %model,
                        consecutive_error_turns = turn_governor_runtime.consecutive_error_turns,
                        avg_llm_latency_ms = ?turn_governor_runtime.avg_llm_latency_ms,
                        "reliability guard switching runtime route after degradation"
                    );
                    forced_runtime_route = Some(self.turn_route_reliability_guard(model.clone()));
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
            // --- Streaming first attempt + semantic empty/thinking recovery as `run()` (retries use non-stream) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let mut inner_attempt: u32 = 0;
            let mut response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        std::mem::take(&mut tool_errors),
                        accumulated_usage.take(),
                        session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        prefill_range.clone(),
                        api_call_count,
                    ));
                }
                let r = if self.use_streaming_llm_transport(
                    ui_streaming,
                    inner_attempt,
                    turn_runtime_route.as_ref(),
                ) {
                    match self
                        .collect_stream_llm_response(
                            &mut ctx,
                            &active_tool_schemas,
                            turn_runtime_route.as_ref(),
                            active_model,
                            llm_governor.max_tokens,
                            stream_chunk_sink.as_ref(),
                            &mut api_call_count,
                            stream_scrubber.as_mut(),
                        )
                        .await
                    {
                        Ok(StreamCollectOutcome::Complete(resp)) => resp,
                        Ok(StreamCollectOutcome::Interrupted(partial)) => {
                            if let Some(ref u) = partial.usage {
                                self.record_api_usage(u);
                                accumulated_usage = Some(merge_usage(accumulated_usage.take(), u));
                                if let Some(cost) = estimate_usage_cost_usd(
                                    u,
                                    partial.model.as_str(),
                                    &self.config(),
                                ) {
                                    session_cost_usd += cost;
                                }
                            }
                            ctx.add_message(partial.message);
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                std::mem::take(&mut tool_errors),
                                accumulated_usage.take(),
                                session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                prefill_range.clone(),
                                api_call_count,
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
                    }
                } else {
                    match self
                        .call_llm_with_retry(
                            &mut ctx,
                            &active_tool_schemas,
                            turn_runtime_route.as_ref(),
                            llm_governor.max_tokens,
                            &mut api_call_count,
                        )
                        .await
                    {
                        Ok(r) => r,
                        Err(AgentError::Interrupted { .. }) => {
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                std::mem::take(&mut tool_errors),
                                accumulated_usage.take(),
                                session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                                prefill_range.clone(),
                                api_call_count,
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
                    }
                };
                inner_attempt = inner_attempt.saturating_add(1);

                if let Some(ref u) = r.usage {
                    self.record_api_usage(u);
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
                    && inner_thinking < self.config().thinking_prefill_max_retries
                {
                    inner_thinking += 1;
                    self.handle_reasoning_only_prefill(
                        &r.message,
                        inner_thinking,
                        self.config().thinking_prefill_max_retries,
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
                    && inner_empty < self.config().empty_content_max_retries
                {
                    inner_empty += 1;
                    tracing::warn!(
                        "empty assistant response (stream path) - retrying ({}/{})",
                        inner_empty,
                        self.config().empty_content_max_retries
                    );
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Empty assistant response - retrying ({}/{})",
                            inner_empty,
                            self.config().empty_content_max_retries
                        ),
                    );
                    continue;
                }
                break r;
            };
            Self::upgrade_finish_reason_for_truncated_tool_args(&mut response);
            let _api_elapsed_ms = api_start.elapsed().as_millis() as u64;
            _total_api_time_ms += _api_elapsed_ms;
            self.update_route_learning(
                turn_runtime_route.as_ref(),
                Some(response.model.as_str()),
                _api_elapsed_ms,
                true,
            );
            push_window_u64(
                &mut governor_llm_latency_window,
                _api_elapsed_ms,
                governor_window_limit,
            );
            replay.record(
                    "llm_response",
                    serde_json::json!({
                        "turn": total_turns,
                        "model": response.model,
                        "finish_reason": response.finish_reason,
                        "api_time_ms": _api_elapsed_ms,
                        "tool_call_count": response.message.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0),
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
                "api_time_ms": _api_elapsed_ms,
                "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
            });
            let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
            self.inject_hook_context(&post_results, &mut ctx);
            self.apply_hook_output_transforms(&post_results, &mut response.message.content);
            self.apply_transform_llm_output_hooks(&mut response.message.content);

            // Accumulate usage (merged across semantic-retried sub-calls)
            if let Some(ref usage) = turn_usage_acc {
                accumulated_usage = Some(merge_usage(accumulated_usage, usage));
                if let Some(cost) =
                    estimate_usage_cost_usd(usage, response.model.as_str(), &self.config())
                {
                    session_cost_usd += cost;
                }
            }

            if let Some(limit) = self.config().max_cost_usd {
                if !cost_warned
                    && session_cost_usd >= limit * self.config().cost_guard_degrade_at_ratio
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
                    self.turn_end_plugin_hooks(
                        ctx.get_messages(),
                        false,
                        false,
                        total_turns,
                        session_started_hooks_fired,
                    );
                    replay.record(
                        "session_end",
                        serde_json::json!({
                            "reason": "cost_guard",
                            "total_turns": total_turns,
                            "session_cost_usd": session_cost_usd,
                        }),
                    );
                    return Ok(self.seal_loop_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                        LoopExit {
                            turn_exit_reason: "max_iterations_reached",
                            api_calls: api_call_count,
                            failed: false,
                            partial: false,
                            finished_naturally: false,
                            interrupted: false,
                        },
                        total_turns,
                        std::mem::take(&mut tool_errors),
                        accumulated_usage.take(),
                        session_cost_usd,
                        session_started_hooks_fired,
                    ));
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
            if response.finish_reason.as_deref() == Some("length")
                && assistant_msg
                    .tool_calls
                    .as_ref()
                    .map_or(false, |calls| !calls.is_empty())
                && truncated_tool_call_retries < self.config().truncated_tool_call_max_retries
            {
                truncated_tool_call_retries = truncated_tool_call_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Truncated tool arguments - retrying ({}/{})",
                        truncated_tool_call_retries,
                        self.config().truncated_tool_call_max_retries
                    ),
                );
                let _ = ctx.get_messages_mut().pop();
                continue;
            }
            truncated_tool_call_retries = 0;

            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done
            let tool_calls: Vec<ToolCall> = parsed_tool_calls
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                let effective_finish_reason = self.effective_finish_reason(
                    &response,
                    &assistant_msg,
                    history_includes_tool,
                    turn_runtime_route.as_ref(),
                );
                let finalization_signals = self.build_finalization_signals(
                    &task_hint,
                    ctx.get_messages(),
                    &assistant_msg,
                    effective_finish_reason.as_deref(),
                );
                tracing::debug!(
                    turn = total_turns,
                    finish_reason = ?finalization_signals.finish_reason,
                    has_tool_calls = finalization_signals.has_tool_calls,
                    has_visible_text = finalization_signals.has_visible_text,
                    has_visible_text_after_think = finalization_signals.has_visible_text_after_think,
                    has_reasoning = finalization_signals.has_reasoning,
                    continuation_required = finalization_signals.continuation_required,
                    ack_detected = finalization_signals.ack_detected,
                    final_gate_passed = finalization_signals.final_gate_passed(),
                    "finalization gate evaluation (stream)"
                );
                replay.record(
                        "final_gate",
                        serde_json::json!({
                            "turn": total_turns,
                            "stream": true,
                            "finish_reason": finalization_signals.finish_reason,
                            "has_tool_calls": finalization_signals.has_tool_calls,
                            "has_visible_text": finalization_signals.has_visible_text,
                            "has_visible_text_after_think": finalization_signals.has_visible_text_after_think,
                            "has_reasoning": finalization_signals.has_reasoning,
                            "continuation_required": finalization_signals.continuation_required,
                            "ack_detected": finalization_signals.ack_detected,
                            "final_gate_passed": finalization_signals.final_gate_passed(),
                        }),
                    );
                if finalization_signals.continuation_required {
                    if continuation_retries < self.config().continuation_max_retries {
                        continuation_retries = continuation_retries.saturating_add(1);
                        continuation_trigger_count = continuation_trigger_count.saturating_add(1);
                        self.emit_status(
                                "lifecycle",
                                &format!(
                                    "Assistant response incomplete ({:?}) - requesting continuation ({}/{})",
                                    response.finish_reason,
                                    continuation_retries,
                                    self.config().continuation_max_retries
                                ),
                            );
                        ctx.add_message(Message::user(&continuation_prompt_for_response(
                            &response,
                        )));
                        continue;
                    }
                    premature_finalize_suspected_count =
                        premature_finalize_suspected_count.saturating_add(1);
                    self.emit_status(
                            "lifecycle",
                            &format!(
                                "Continuation retries exhausted ({}) - finalizing with best effort output",
                                self.config().continuation_max_retries
                            ),
                        );
                } else {
                    continuation_retries = 0;
                }
                if clarify_tool_invocation_requires_retry(
                    &task_hint,
                    active_tool_schemas.iter().any(|s| s.name == "clarify"),
                    clarify_tool_retries,
                ) {
                    clarify_tool_retries = clarify_tool_retries.saturating_add(1);
                    tracing::info!(
                        retry = clarify_tool_retries,
                        max = CLARIFY_TOOL_RETRY_MAX,
                        "clarify tool not invoked for user-requested clarify; retrying"
                    );
                    ctx.add_message(Message::user(CLARIFY_TOOL_RETRY_USER_MESSAGE));
                    continue;
                }
                if finalization_signals.ack_detected {
                    if !tool_schemas.is_empty()
                        && codex_ack_continuations < self.config().ack_continuation_max_retries
                    {
                        codex_ack_continuations = codex_ack_continuations.saturating_add(1);
                        ack_trigger_count = ack_trigger_count.saturating_add(1);
                        self.emit_status(
                            "lifecycle",
                            &format!(
                                "Detected intermediate ack - requesting continuation ({}/{})",
                                codex_ack_continuations,
                                self.config().ack_continuation_max_retries
                            ),
                        );
                        ctx.add_message(Message::user(CODEX_CONTINUE_USER_MESSAGE));
                        continue;
                    }
                    premature_finalize_suspected_count =
                        premature_finalize_suspected_count.saturating_add(1);
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
                if file_mutation.has_failures() {
                    let footer = file_mutation.format_advisory_footer();
                    for msg in ctx.get_messages_mut().iter_mut().rev() {
                        if matches!(msg.role, MessageRole::Assistant) {
                            if let Some(content) = msg.content.as_mut() {
                                content.push_str(&footer);
                            }
                            break;
                        }
                    }
                }
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
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
                self.spawn_background_review(total_turns, &ctx, review_memory_at_end);
                self.turn_end_plugin_hooks(
                    ctx.get_messages(),
                    true,
                    false,
                    total_turns,
                    session_started_hooks_fired,
                );
                replay.record(
                    "session_end",
                    serde_json::json!({
                        "reason": "finished_naturally",
                        "total_turns": total_turns,
                        "session_cost_usd": session_cost_usd,
                        "continuation_trigger_count": continuation_trigger_count,
                        "ack_trigger_count": ack_trigger_count,
                        "premature_finalize_suspected_count": premature_finalize_suspected_count,
                    }),
                );
                if stream_mute
                    .as_ref()
                    .is_some_and(|m| m.swap(false, Ordering::AcqRel))
                {
                    Self::emit_stream_chunk(
                        Some(stream_chunk_sink.as_ref()),
                        StreamChunk {
                            delta: Some(hermes_core::StreamDelta {
                                content: None,
                                tool_calls: None,
                                extra: Some(serde_json::json!({
                                    "control": "mute_post_response",
                                    "enabled": false
                                })),
                            }),
                            finish_reason: None,
                            usage: None,
                        },
                    );
                }
                return Ok(self.seal_loop_result(
                    &ctx,
                    persist_user_idx,
                    prefill_range.clone(),
                    LoopExit {
                        turn_exit_reason: "text_response",
                        api_calls: api_call_count,
                        failed: false,
                        partial: false,
                        finished_naturally: true,
                        interrupted: false,
                    },
                    total_turns,
                    std::mem::take(&mut tool_errors),
                    accumulated_usage.take(),
                    session_cost_usd,
                    session_started_hooks_fired,
                ));
            }

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
            if tool_calls.is_empty() {
                ctx.add_message(Message::system(
                        "[SYSTEM] Tool profile/budget policy filtered this turn's calls. Propose refined, scoped code-inspection calls next.",
                    ));
                continue;
            }
            let all_housekeeping = tool_calls.iter().all(|tc| {
                matches!(
                    tc.function.name.as_str(),
                    "memory" | "todo" | "skill_manage" | "session_search"
                )
            });
            let should_mute_post =
                all_housekeeping && Self::assistant_visible_text_after_think_blocks(&assistant_msg);
            let was_muted = stream_mute
                .as_ref()
                .map(|m| m.swap(should_mute_post, Ordering::AcqRel))
                .unwrap_or(false);
            if was_muted != should_mute_post {
                Self::emit_stream_chunk(
                    Some(stream_chunk_sink.as_ref()),
                    StreamChunk {
                        delta: Some(hermes_core::StreamDelta {
                            content: None,
                            tool_calls: None,
                            extra: Some(serde_json::json!({
                                "control": "mute_post_response",
                                "enabled": should_mute_post
                            })),
                        }),
                        finish_reason: None,
                        usage: None,
                    },
                );
            }

            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                invalid_tool_retries = invalid_tool_retries.saturating_add(1);
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected - retrying ({}/{})",
                        invalid_tool_retries,
                        self.config().invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if invalid_tool_retries >= self.config().invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config().invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config().invalid_tool_call_max_retries,
                        invalid_tool_calls[0]
                    )));
                    self.turn_end_plugin_hooks(
                        ctx.get_messages(),
                        false,
                        false,
                        total_turns,
                        session_started_hooks_fired,
                    );
                    return Ok(self.seal_loop_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                        LoopExit {
                            turn_exit_reason: "invalid_tool_calls",
                            api_calls: api_call_count,
                            failed: false,
                            partial: true,
                            finished_naturally: false,
                            interrupted: false,
                        },
                        total_turns,
                        std::mem::take(&mut tool_errors),
                        accumulated_usage.take(),
                        session_cost_usd,
                        session_started_hooks_fired,
                    ));
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
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
                if invalid_json_retries < self.config().invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments - retrying ({}/{})",
                            invalid_json_retries,
                            self.config().invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config().invalid_tool_json_max_retries
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
                    ctx.add_message(Message::tool_result(tc.id.clone(), content));
                }
                continue;
            }
            invalid_json_retries = 0;
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
            let deferred_web_budget_results = if let Some(ref mut ctrl) = web_research_ctrl {
                ctrl.ensure_plan_on_first_web(web_auxiliary.as_ref(), &first_user, &tool_calls)
                    .await;
                let (blocked, notices) = ctrl
                    .gate_web_batch(
                        web_auxiliary.as_ref(),
                        ctx.get_messages(),
                        &mut tool_calls,
                        total_turns,
                    )
                    .await;
                for notice in notices {
                    self.emit_status("tool_failure", &notice);
                }
                blocked
            } else {
                let blocked = apply_web_tool_budget(
                    &mut tool_calls,
                    web_tool_calls_used,
                    web_search_calls_used,
                    web_tool_consecutive_error_turns,
                    total_turns,
                );
                if !blocked.is_empty() {
                    let blocked_by_errors = web_tool_consecutive_error_turns
                        >= web_tool_budget_max_consecutive_errors();
                    for (tool_name, _) in &blocked {
                        self.emit_status(
                            "tool_failure",
                            &web_tool_budget_user_notice(tool_name, blocked_by_errors),
                        );
                    }
                }
                blocked
            };
            let contextlattice_connect_intent =
                detect_contextlattice_connect_intent(ctx.get_messages());
            if tool_calls.is_empty() {
                for (_, result) in deferred_web_budget_results {
                    ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
                }
                continue;
            }

            // Pre-tool hooks + callbacks
            let tool_names_for_log: Vec<&str> = tool_calls
                .iter()
                .map(|tc| tc.function.name.as_str())
                .collect();
            tracing::debug!(
                turn = total_turns,
                tool_count = tool_calls.len(),
                tools = ?tool_names_for_log,
                streaming = true,
                "agent tool batch start"
            );
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    cb(&tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    std::mem::take(&mut tool_errors),
                    accumulated_usage.take(),
                    session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    prefill_range.clone(),
                    api_call_count,
                ));
            }

            let _tool_start = Instant::now();
            let _tool_governor = governor_for_turn(
                &self.config(),
                &ctx,
                tool_calls.len(),
                Some(&turn_governor_runtime),
            );
            let _parent_budget_remaining_usd = self
                .config()
                .max_cost_usd
                .map(|limit| (limit - session_cost_usd).max(0.0));

            for tc in &tool_calls {
                let args: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                match tool_guardrails.before_call(&tc.function.name, &args) {
                    crate::tool_guardrails::GuardrailDecision::Halt(reason) => {
                        ctx.add_message(Message::assistant(format!(
                            "[Tool guardrail halt] {reason}"
                        )));
                        return Ok(self.seal_loop_result(
                            &ctx,
                            persist_user_idx,
                            prefill_range.clone(),
                            LoopExit {
                                turn_exit_reason: "guardrail_halt",
                                api_calls: api_call_count,
                                failed: false,
                                partial: false,
                                finished_naturally: true,
                                interrupted: false,
                            },
                            total_turns,
                            std::mem::take(&mut tool_errors),
                            accumulated_usage.take(),
                            session_cost_usd,
                            session_started_hooks_fired,
                        ));
                    }
                    crate::tool_guardrails::GuardrailDecision::Block(reason) => {
                        tracing::warn!(tool = %tc.function.name, %reason, "tool guardrail block");
                    }
                    crate::tool_guardrails::GuardrailDecision::Allow => {}
                }
            }
            let tool_start = Instant::now();
            let tool_progress_names: Vec<String> = tool_calls
                .iter()
                .map(|tc| tc.function.name.clone())
                .collect();
            let _tool_progress = ToolProgressWatchdog::start(
                self.callbacks.status_callback.clone(),
                total_turns,
                tool_progress_names,
            );
            let mut results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    governor_for_turn(
                        &self.config(),
                        &ctx,
                        tool_calls.len(),
                        Some(&turn_governor_runtime),
                    )
                    .tool_concurrency,
                    contextlattice_connect_intent,
                    self.config()
                        .max_cost_usd
                        .map(|limit| (limit - session_cost_usd).max(0.0)),
                    &mut tool_errors,
                    Some(&mut checkpoint_mgr),
                    latest_user_content(ctx.get_messages()).map(str::to_string),
                )
                .await;
            if let Some(ref mut ctrl) = web_research_ctrl {
                if ctrl.record_results(&tool_calls, &results) {
                    active_tool_schemas =
                        Arc::from(ctrl.filter_tool_schemas(tool_schemas.as_ref()));
                }
            }
            if !deferred_web_budget_results.is_empty() {
                results.extend(
                    deferred_web_budget_results
                        .into_iter()
                        .map(|(_, result)| result),
                );
            }
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            _total_tool_time_ms += tool_elapsed;
            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            let mut web_turn_calls: u32 = 0;
            let mut web_turn_errors: u32 = 0;
            for tc in &tool_calls {
                if !is_budgeted_web_tool(&tc.function.name) {
                    continue;
                }
                web_turn_calls = web_turn_calls.saturating_add(1);
                if results
                    .iter()
                    .any(|r| r.tool_call_id == tc.id && r.is_error)
                {
                    web_turn_errors = web_turn_errors.saturating_add(1);
                }
            }
            if web_turn_calls > 0 {
                web_tool_calls_used = web_tool_calls_used.saturating_add(web_turn_calls);
                if web_turn_errors == web_turn_calls {
                    web_tool_consecutive_error_turns =
                        web_tool_consecutive_error_turns.saturating_add(1);
                } else {
                    web_tool_consecutive_error_turns = 0;
                }
            }
            for tc in &tool_calls {
                if tc.function.name == "web_search" {
                    web_search_calls_used = web_search_calls_used.saturating_add(1);
                }
            }
            tracing::info!(
                turn = total_turns,
                tool_count = tool_calls.len(),
                result_count = results.len(),
                errors = turn_tool_error_count,
                elapsed_ms = tool_elapsed,
                streaming = true,
                "agent tool batch finished"
            );
            self.emit_tool_failure_notices(&tool_calls, &results);
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
            replay.record(
                "tool_batch",
                serde_json::json!({
                    "turn": total_turns,
                    "tool_count": tool_calls.len(),
                    "tool_concurrency": governor_for_turn(
                        &self.config(),
                        &ctx,
                        tool_calls.len(),
                        Some(&turn_governor_runtime),
                    )
                    .tool_concurrency,
                    "errors": turn_tool_error_count,
                    "error_rate": turn_tool_error_rate,
                }),
            );
            update_repo_review_budget_state_from_results(
                &mut repo_review_budget_state,
                ctx.get_messages(),
                &results,
            );
            if self.config().rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config().rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = last_checkpoint_messages.clone() {
                    *ctx.get_messages_mut() = snapshot;
                    let _ = checkpoint_mgr.restore_latest();
                    ctx.add_message(Message::system(format!(
                            "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                            turn_tool_error_count
                        )));
                    continue;
                }
            }

            // Post-tool hooks + callbacks
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    cb(&tc.function.name, &res.content);
                }
            }

            for (tc, res) in tool_calls.iter().zip(results.iter()) {
                let args: Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                tool_guardrails.after_call(&tc.function.name, res.is_error, &res.content);
                file_mutation.record_tool_result(
                    &tc.function.name,
                    &args,
                    &res.content,
                    res.is_error,
                );
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            // Enforce budget on tool results
            budget::enforce_budget(&mut results, &self.config().budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config().max_turns,
                    self.config().budget_caution_threshold,
                    self.config().budget_warning_threshold,
                    self.config().budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }
            let lsp_note = self.lsp_context_note(&tool_calls, &results);

            let execute_code_refund = !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
                && !results.iter().any(|r| r.is_error);

            let num_tool_msgs = results.len();
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
                ctx.add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            self.pending_steer
                .apply_to_tool_results(ctx.get_messages_mut(), num_tool_msgs);
            if let Some(note) = lsp_note {
                ctx.add_message(Message::system(note));
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
                if stream_mute
                    .as_ref()
                    .is_some_and(|m| m.swap(false, Ordering::AcqRel))
                {
                    Self::emit_stream_chunk(
                        Some(stream_chunk_sink.as_ref()),
                        StreamChunk {
                            delta: Some(hermes_core::StreamDelta {
                                content: None,
                                tool_calls: None,
                                extra: Some(serde_json::json!({
                                    "control": "mute_post_response",
                                    "enabled": false
                                })),
                            }),
                            finish_reason: None,
                            usage: None,
                        },
                    );
                }
                self.turn_end_plugin_hooks(
                    ctx.get_messages(),
                    false,
                    false,
                    total_turns,
                    session_started_hooks_fired,
                );
                return Ok(self.enrich_turn_telemetry(
                    self.seal_loop_result(
                        &ctx,
                        persist_user_idx,
                        prefill_range.clone(),
                        LoopExit {
                            turn_exit_reason: "tool_loop_guard",
                            api_calls: api_call_count,
                            failed: false,
                            partial: false,
                            finished_naturally: false,
                            interrupted: false,
                        },
                        total_turns,
                        std::mem::take(&mut tool_errors),
                        accumulated_usage.take(),
                        session_cost_usd,
                        session_started_hooks_fired,
                    ),
                    Some(&tool_guardrails),
                ));
            }
            if execute_code_refund {
                iteration_budget.refund(1);
                total_turns = total_turns.saturating_sub(1);
            }
            if let Some(brk) = stream_needs_break.as_ref() {
                brk.store(true, Ordering::Release);
            }
            Self::emit_stream_chunk(
                Some(stream_chunk_sink.as_ref()),
                StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(serde_json::json!({"control": "stream_break"})),
                    }),
                    finish_reason: None,
                    usage: None,
                },
            );
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
                    tracing::warn!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        total_chars,
                        threshold
                    );
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_messages_strips_system_from_history() {
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("old"),
            Message::assistant("hi"),
            Message::user("new"),
        ];
        let (hist, user) = split_messages_for_run_conversation(&messages).expect("split");
        assert_eq!(user, "new");
        assert_eq!(hist.len(), 2);
        assert!(hist.iter().all(|m| m.role != MessageRole::System));
    }

    #[test]
    fn split_messages_peels_last_user() {
        let messages = vec![
            Message::user("old"),
            Message::assistant("hi"),
            Message::user("new"),
        ];
        let (hist, user) = split_messages_for_run_conversation(&messages).expect("split");
        assert_eq!(user, "new");
        assert_eq!(hist.len(), 2);
    }

    #[tokio::test]
    async fn prepare_turn_appends_user_and_sets_task_id() {
        use crate::agent_loop::AgentConfig;
        use crate::agent_loop::ToolRegistry;
        use futures::StreamExt;
        use std::sync::Arc;

        struct StopProvider;
        #[async_trait::async_trait]
        impl hermes_core::LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
            {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StopProvider),
        );
        let prepared = agent
            .prepare_turn(&RunConversationParams {
                user_message: "current".into(),
                conversation_history: vec![Message::user("prior")],
                task_id: Some("task-x".into()),
                stream_callback: None,
                persist_user_message: None,
                tools: None,
                persist_session: false,
            })
            .await
            .expect("prepare");
        assert_eq!(prepared.meta.task_id, "task-x");
        assert_eq!(agent.current_task_id().as_deref(), Some("task-x"));
        assert_eq!(prepared.messages.len(), 2);
    }

    #[test]
    fn hydrate_memory_nudge_from_history() {
        use crate::agent_loop::AgentConfig;
        use crate::agent_loop::ToolRegistry;
        use futures::StreamExt;
        use std::sync::Arc;

        struct StopProvider;
        #[async_trait::async_trait]
        impl hermes_core::LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
            {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig {
                memory_nudge_interval: 4,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(StopProvider),
        );
        let history: Vec<Message> = (0..5).map(|i| Message::user(format!("u{i}"))).collect();
        agent.hydrate_memory_nudge_counters_from_history(&history);
        let counters = agent.evolution_counters.lock().expect("lock");
        assert_eq!(counters.turns_since_memory, 1);
        assert_eq!(counters.user_turn_count, 5);
    }

    #[test]
    fn hydrate_user_turn_count_from_history() {
        use crate::agent_loop::AgentConfig;
        use crate::agent_loop::ToolRegistry;
        use futures::StreamExt;
        use std::sync::Arc;

        struct StopProvider;
        #[async_trait::async_trait]
        impl hermes_core::LlmProvider for StopProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermes_core::LlmResponse, AgentError> {
                Ok(hermes_core::LlmResponse {
                    message: Message::assistant("ok"),
                    usage: None,
                    model: "t".into(),
                    finish_reason: Some("stop".into()),
                    ..Default::default()
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[hermes_core::ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>>
            {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StopProvider),
        );
        let history: Vec<Message> = (0..3).map(|i| Message::user(format!("u{i}"))).collect();
        agent.hydrate_memory_nudge_counters_from_history(&history);
        assert_eq!(
            agent
                .evolution_counters
                .lock()
                .expect("lock")
                .user_turn_count,
            3
        );
    }

    #[test]
    fn last_reasoning_stops_at_turn_boundary() {
        let mut stale = Message::assistant("old");
        stale.reasoning_content = Some("stale".into());
        let mut fresh = Message::assistant("ok");
        fresh.reasoning_content = Some("fresh".into());
        let messages = vec![
            Message::user("prior"),
            stale,
            Message::user("current"),
            fresh,
        ];
        assert_eq!(
            extract_last_reasoning_current_turn(&messages).as_deref(),
            Some("fresh")
        );
    }
}
