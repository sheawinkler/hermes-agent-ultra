//! Python `agent.conversation_loop.run_conversation`

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::agent_loop::{
    AgentLoop, ReplayRecorder, RepoReviewBudgetState, TurnRuntimeRoute,
    build_auxiliary_arc_for_config, contextlattice_connect_system_hint,
    contextlattice_intelligence_system_hint, effective_max_turns,
    exploratory_problem_solving_system_hint, objective_mode_system_hint,
};
use crate::agent_runtime_helpers::strip_think_blocks;
use crate::codex_responses_adapter::summarize_user_message_for_log_str;
use crate::context::ContextManager;
use crate::governor::governor_window_size;
use crate::message_sanitization::{sanitize_surrogates, strip_system_messages_from_history};
use crate::plugins::{HookResult, HookType};
use crate::session_persistence::leading_system_prompt_for_persist;
use crate::turn_state::{TurnContext, TurnState};
use crate::web_research::WebResearchController;
use hermes_core::{
    AgentError, AgentResult, Message, MessageRole, StreamChunk, ToolSchema, UsageStats,
};

/// Non-streaming variant; see [`run_with_message_prelude`] for the shared core.
pub async fn run_agent_loop(
    agent: &AgentLoop,
    messages: Vec<Message>,
    tools: Option<Vec<ToolSchema>>,
) -> Result<AgentResult, AgentError> {
    run_with_message_prelude(agent, messages, tools, None, false).await
}

/// Run one full user turn (Python `run_conversation`).
pub async fn run_conversation(
    agent: &AgentLoop,
    params: RunConversationParams,
) -> Result<ConversationResult, AgentError> {
    let prepared = prepare_turn(agent, &params).await?;
    let tools = params.tools;
    let stream_callback = params.stream_callback;
    let loop_result = run_with_message_prelude(
        agent,
        prepared.messages.clone(),
        tools,
        stream_callback,
        true,
    )
    .await?;
    Ok(finalize_turn(agent, loop_result, &prepared.meta))
}

/// Build message list and apply per-turn prelude.
pub(crate) async fn prepare_turn(
    agent: &AgentLoop,
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
        let mut guard = agent
            .config_runtime
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut updated = (*guard).as_ref().clone();
        updated.persist_user_message = persist_override;
        *guard = Arc::new(updated);
    }

    let task_id = params
        .task_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if let Ok(mut state) = agent.state.lock() {
        state.current_task_id = Some(task_id.clone());
    }

    let conversation_history = strip_system_messages_from_history(&params.conversation_history);
    hydrate_memory_nudge_counters_from_history(agent, &conversation_history);
    let user_turn_count = {
        if let Ok(mut state) = agent.state.lock() {
            state.evolution_counters.user_turn_count =
                state.evolution_counters.user_turn_count.saturating_add(1);
            state.evolution_counters.user_turn_count
        } else {
            1
        }
    };

    let inbound_user_message = user_message.clone();
    let history_len = conversation_history.len();

    let mut messages: Vec<Message> = conversation_history;
    messages.push(Message::user(user_message));

    agent.apply_turn_message_prelude(&mut messages).await;

    crate::session_log::set_session_context(agent.config().session_id.as_deref());
    agent.replay_compression_warning_at_turn_start().await;
    agent.reset_vision_supported_for_turn();
    agent.cleanup_dead_connections_at_turn_start().await;

    let preview_text = summarize_user_message_for_log_str(&inbound_user_message);
    let msg_preview = if preview_text.chars().count() > 80 {
        format!("...{}", preview_text.chars().take(80).collect::<String>())
    } else {
        preview_text.clone()
    };
    let msg_preview = msg_preview.replace('\n', " ");
    let rt = crate::route_learning::primary_runtime_snapshot(agent);
    tracing::info!(
        session_id = %crate::session_log::current_session_tag(),
        task_id = %task_id,
        user_turn = user_turn_count,
        model = %crate::runtime_provider::active_model(agent),
        provider = rt.provider.as_deref().unwrap_or("unknown"),
        platform = %agent.config().platform.as_deref().unwrap_or("unknown"),
        history_len = history_len,
        msg = %msg_preview,
        "conversation turn"
    );
    if !agent.config().quiet_mode {
        let print_preview = summarize_user_message_for_log_str(&inbound_user_message);
        let suffix = if print_preview.chars().count() > 60 {
            "..."
        } else {
            ""
        };
        let short: String = print_preview.chars().take(60).collect();
        crate::hooks::emit_status(
            agent,
            "lifecycle",
            &format!("💬 Starting conversation: '{short}{suffix}'"),
        );
    }
    crate::tool_executor::memory_on_turn_start(agent, user_turn_count, &original_user_message);

    agent.apply_turn_prep_infrastructure_hooks();
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

/// Turn-level hooks + [`ConversationResult`] + optional session persist.
pub(crate) fn finalize_turn(
    agent: &AgentLoop,
    mut loop_result: AgentResult,
    meta: &TurnFinalizeMeta,
) -> ConversationResult {
    let messages = loop_result.messages.clone();
    let mut final_response = extract_last_assistant_reply(&messages);
    let last_reasoning = extract_last_reasoning_current_turn(&messages);
    let interrupted = loop_result.interrupted;
    let max_iterations =
        effective_max_turns(agent.config().max_turns).unwrap_or(agent.config().max_turns);
    let completed = final_response.is_some()
        && !loop_result.failed
        && !interrupted
        && loop_result.api_calls < max_iterations;

    if let Some(ref mut text) = final_response {
        if !interrupted {
            *text = apply_turn_level_output_hooks(agent, text, meta, &messages);
        }
        // Strip reasoning/think blocks leaked by models that emit them inline
        // (e.g. MiniMax without dedicated reasoning_content). Parity with the
        // streaming path's ThinkBlockScrubber.
        *text = strip_think_blocks(text);
        if text.trim().is_empty() {
            final_response = None;
        }
    }

    let mut messages = messages;
    agent.apply_turn_finalize_infrastructure_hooks(meta, &mut messages, &loop_result, completed);
    agent.log_turn_exit_diagnostic(&loop_result, &messages);

    agent.sync_external_memory_for_turn(
        &meta.original_user_message,
        final_response.as_deref(),
        interrupted,
    );

    // Python `_persist_session` always runs at turn end; `persist_turn_session` no-ops without session_id.
    persist_turn_session(agent, &messages, &loop_result);

    loop_result.messages = messages;
    loop_result.messages.shrink_to_fit();
    let loop_result = agent.finalize_agent_result(loop_result);
    hermes_telemetry::record_agent_turn();
    crate::session_log::clear_session_context();

    ConversationResult {
        final_response,
        last_reasoning,
        completed,
        loop_result,
    }
}

fn hydrate_memory_nudge_counters_from_history(agent: &AgentLoop, history: &[Message]) {
    if history.is_empty() {
        return;
    }
    let interval = agent.config().memory_nudge_interval;
    if interval == 0 {
        return;
    }
    if let Ok(state) = agent.state.lock() {
        if state.evolution_counters.user_turn_count > 0
            && state.evolution_counters.turns_since_memory > 0
        {
            return;
        }
    }
    let prior_user_turns = history
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .count();
    if prior_user_turns == 0 {
        return;
    }
    if let Ok(mut state) = agent.state.lock() {
        if state.evolution_counters.user_turn_count == 0 {
            state.evolution_counters.user_turn_count = prior_user_turns as u32;
        }
        if state.evolution_counters.turns_since_memory == 0 {
            state.evolution_counters.turns_since_memory =
                (prior_user_turns % interval as usize) as u32;
        }
    }
}

fn apply_turn_level_output_hooks(
    agent: &AgentLoop,
    response: &str,
    meta: &TurnFinalizeMeta,
    messages: &[Message],
) -> String {
    let history_json: Vec<serde_json::Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let hook_ctx = serde_json::json!({
        "session_id": agent.config().session_id,
        "user_message": meta.original_user_message,
        "assistant_response": response,
        "conversation_history": history_json,
        "model": crate::runtime_provider::active_model(agent),
        "platform": agent.config().platform,
        "task_id": meta.task_id,
    });
    let results = crate::hooks::invoke_hook(agent, HookType::PostLlmCall, &hook_ctx);
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

fn persist_turn_session(agent: &AgentLoop, messages: &[Message], inner: &AgentResult) {
    let cfg = agent.config();
    let Some(ref sid) = cfg.session_id else {
        return;
    };
    if sid.trim().is_empty() {
        return;
    }
    let Some(sp) = agent.session_persistence() else {
        return;
    };
    let transcript = strip_system_messages_from_history(messages);
    let sys = leading_system_prompt_for_persist(messages);
    let platform = cfg.platform.as_deref();
    let model = crate::runtime_provider::active_model(agent);
    let mut cursor = agent
        .state
        .lock()
        .map(|mut state| std::mem::take(&mut state.session_db_flush))
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
    if let Ok(mut state) = agent.state.lock() {
        state.session_db_flush = cursor;
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

/// Returns `(full_prompt, restored_from_storage)` using session-level cache when warm.
pub(crate) fn active_cached_system_prompt(
    agent: &AgentLoop,
    task_hint: &str,
    tool_schemas: &[ToolSchema],
) -> (String, bool) {
    if let Ok(state) = agent.state.lock() {
        if let Some(ref cached) = state.cached_system_prompt {
            if !cached.trim().is_empty() {
                return (cached.clone(), true);
            }
        }
    }
    let resolved = resolve_initial_system_prompt(agent, task_hint, tool_schemas);
    let restored = resolved.restored;
    let prompt = resolved.full_prompt.clone();
    if let Ok(mut state) = agent.state.lock() {
        state.cached_system_prompt = Some(prompt.clone());
    }
    (prompt, restored)
}

/// Resolved system prompt for a session turn.
pub(crate) struct SystemPromptResolved {
    /// Full joined prompt (static + dynamic). Used for restored sessions and persistence.
    pub full_prompt: String,
    /// Cache-stable static prefix (no timestamps, session IDs, or env hints).
    /// `Some` only for fresh (non-restored) sessions.
    pub static_prefix: Option<String>,
    /// Per-session dynamic suffix (timestamps, model, session ID, env hints).
    /// `Some` only for fresh (non-restored) sessions.
    pub dynamic_suffix: Option<String>,
    /// Whether the prompt was restored from stored/SQLite state.
    pub restored: bool,
}

/// Returns the resolved system prompt, splitting static/dynamic tiers for new sessions.
///
/// Restored sessions return the stored full prompt as a single system message
/// (preserving the stable cache prefix from that session's first turn).
/// Fresh sessions return a split so the static prefix can be cached independently
/// of per-session metadata.
pub(crate) fn resolve_initial_system_prompt(
    agent: &AgentLoop,
    task_hint: &str,
    tool_schemas: &[ToolSchema],
) -> SystemPromptResolved {
    if let Some(ref s) = agent.config().stored_system_prompt {
        let t = s.trim();
        if !t.is_empty() {
            return SystemPromptResolved {
                full_prompt: s.clone(),
                static_prefix: None,
                dynamic_suffix: None,
                restored: true,
            };
        }
    }
    let model = crate::runtime_provider::active_model(agent);
    let (static_prefix, dynamic_suffix) =
        agent.build_system_prompt_parts(task_hint, tool_schemas, &model);
    let full_prompt = if dynamic_suffix.is_empty() {
        static_prefix.clone()
    } else {
        format!("{static_prefix}\n\n{dynamic_suffix}")
    };
    SystemPromptResolved {
        full_prompt,
        static_prefix: Some(static_prefix),
        dynamic_suffix: Some(dynamic_suffix),
        restored: false,
    }
}

#[inline]
pub(crate) fn emit_stream_chunk(
    emit: Option<&(dyn Fn(StreamChunk) + Send + Sync)>,
    chunk: StreamChunk,
) {
    if let Some(f) = emit {
        f(chunk);
    }
}

async fn run_with_message_prelude(
    agent: &AgentLoop,
    messages: Vec<Message>,
    tools: Option<Vec<ToolSchema>>,
    on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    skip_message_prelude: bool,
) -> Result<AgentResult, AgentError> {
    // Python `_has_stream_consumers` (UI) vs `_use_streaming` (transport) ? see `use_streaming_llm_transport`.
    let ui_streaming = on_chunk.is_some();
    let stream_mute = ui_streaming.then(|| Arc::new(AtomicBool::new(false)));
    let stream_needs_break = ui_streaming.then(|| Arc::new(AtomicBool::new(false)));
    let stream_chunk_sink: Box<dyn Fn(StreamChunk) + Send + Sync> = if let Some(raw_emit) = on_chunk
    {
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

    let mut ctx = ContextManager::for_model(crate::runtime_provider::active_model(agent).as_str());
    let _tool_errors: Vec<hermes_core::ToolErrorRecord> = Vec::new();
    let session_id_owned = agent.config().session_id.clone().unwrap_or_default();
    let session_id = session_id_owned.as_str();
    let mut messages = messages;
    if !skip_message_prelude {
        agent.apply_turn_message_prelude(&mut messages).await;
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
        None => agent.tool_registry.schemas(),
    };
    // Build and inject system prompt (or reuse session-level cache for prefix stability).
    // Fresh sessions split into a stable static system message (cached by Anthropic/providers)
    // and a small dynamic suffix (timestamps, model, session ID) as a second system message.
    let resolved = resolve_initial_system_prompt(agent, &task_hint, &tool_schemas);
    let restored_system = resolved.restored;
    let system_content = resolved.full_prompt.clone();
    match (resolved.static_prefix, resolved.dynamic_suffix) {
        (Some(static_p), Some(dynamic_s)) => {
            ctx.add_message(Message::system(&static_p));
            if !dynamic_s.trim().is_empty() {
                ctx.add_message(Message::system(&dynamic_s));
            }
        }
        _ => {
            ctx.add_message(Message::system(&resolved.full_prompt));
        }
    }

    let mut session_started_hooks_fired = false;
    if !restored_system {
        let hook_ctx = serde_json::json!({
            "session_id": agent.config().session_id,
            "model": crate::runtime_provider::active_model(agent),
        });
        let _results = crate::hooks::invoke_hook(agent, HookType::OnSessionStart, &hook_ctx);
        crate::hooks::inject_hook_context(agent, &_results, &mut ctx);
        session_started_hooks_fired = true;
    }

    let prefill_start = ctx.get_messages().len();
    for msg in &agent.config().prefill_messages {
        ctx.add_message(msg.clone());
    }
    let prefill_end = ctx.get_messages().len();
    let prefill_range = (prefill_end > prefill_start).then_some(prefill_start..prefill_end);

    // Add initial messages
    for msg in messages {
        ctx.add_message(msg);
    }
    agent.interest_sync_user_messages(ctx.get_messages());
    agent.hydrate_todo_store(&ctx);
    if let Some(hint) = contextlattice_connect_system_hint(ctx.get_messages()) {
        ctx.add_message(Message::system(hint));
    }
    if let Some(hint) = exploratory_problem_solving_system_hint(ctx.get_messages()) {
        ctx.add_message(Message::system(hint));
    }
    if let Some(hint) = objective_mode_system_hint(ctx.get_messages()) {
        ctx.add_message(Message::system(hint));
    }
    if let Some(hint) = contextlattice_intelligence_system_hint(ctx.get_messages(), &tool_schemas) {
        ctx.add_message(Message::system(hint));
    }

    let persist_user_idx = if agent.config().persist_user_message.is_some() {
        ctx.get_messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == MessageRole::User)
            .last()
            .map(|(i, _)| i)
    } else {
        None
    };
    let _codex_ack_continuations: u32 = 0;

    let mut review_memory_at_end = false;
    if agent.config().memory_nudge_interval > 0
        && agent.tool_registry.names().iter().any(|n| n == "memory")
    {
        if let Ok(mut state) = agent.state.lock() {
            state.evolution_counters.turns_since_memory = state
                .evolution_counters
                .turns_since_memory
                .saturating_add(1);
            if state.evolution_counters.turns_since_memory >= agent.config().memory_nudge_interval {
                review_memory_at_end = true;
                state.evolution_counters.turns_since_memory = 0;
            }
        }
    }
    let _api_call_count: u32 = 0;

    // Memory prefetch
    let first_user = ctx
        .get_messages()
        .iter()
        .filter(|m| matches!(m.role, hermes_core::MessageRole::User))
        .last()
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let mem_ctx_raw = agent.memory_prefetch(&first_user, session_id);
    let recall_block = agent.recall_prefetch(&first_user, session_id).await;
    let combined = if recall_block.is_empty() {
        mem_ctx_raw
    } else if mem_ctx_raw.is_empty() {
        recall_block
    } else {
        format!("{mem_ctx_raw}\n\n{recall_block}")
    };
    agent.set_turn_ext_prefetch_cache(combined);

    crate::hooks::apply_pre_llm_call_hooks_once(agent, &mut ctx, &first_user, session_id);

    if agent.api_mode_is_codex_app_server() {
        let loop_messages: Vec<Message> = ctx.get_messages().to_vec();
        let loop_result = agent
            .run_codex_app_server_turn(
                &first_user,
                loop_messages,
                review_memory_at_end,
                session_started_hooks_fired,
            )
            .await;
        return Ok(loop_result);
    }

    if agent.config().preflight_context_compress {
        agent.preflight_context_compress_with_status(&mut ctx).await;
    }
    let replay = ReplayRecorder::for_session(&agent.config(), session_id);
    let max_turns_limit = effective_max_turns(agent.config().max_turns);
    replay.record(
        "session_start",
        serde_json::json!({
            "session_id": session_id,
            "mode": if crate::llm_caller::use_streaming_llm_transport(agent, ui_streaming, 0, None) {
                "stream"
            } else {
                "run"
            },
            "model": crate::runtime_provider::active_model(agent),
            "max_turns": agent.config().max_turns,
            "max_turns_effective": max_turns_limit,
            "max_turns_unlimited": max_turns_limit.is_none(),
        }),
    );

    let _total_turns: u32 = 0;
    let mut _total_api_time_ms: u64 = 0;
    let mut _total_tool_time_ms: u64 = 0;
    let _accumulated_usage: Option<UsageStats> = None;
    let _session_cost_usd: f64 = 0.0;
    let _cost_warned = false;
    let _forced_runtime_route: Option<TurnRuntimeRoute> = None;
    let _last_checkpoint_messages: Option<Vec<Message>> = None;
    let _invalid_tool_retries: u32 = 0;
    let _invalid_json_retries: u32 = 0;
    let _truncated_tool_call_retries: u32 = 0;
    let _continuation_retries: u32 = 0;
    let _last_content_with_tools: Option<String> = None;
    let _continuation_trigger_count: u32 = 0;
    let _ack_trigger_count: u32 = 0;
    let _premature_finalize_suspected_count: u32 = 0;
    let _context_pressure_warned_at: f64 = 0.0;
    let _context_pressure_last_warn_at: Option<Instant> = None;
    let _context_pressure_last_warn_percent: f64 = 0.0;
    let _governor_llm_latency_window: VecDeque<u64> = VecDeque::new();
    let _governor_tool_error_window: VecDeque<f64> = VecDeque::new();
    let _governor_consecutive_error_turns: u32 = 0;
    let _web_tool_calls_used: u32 = 0;
    let _web_search_calls_used: u32 = 0;
    let _web_tool_consecutive_error_turns: u32 = 0;
    let _repo_review_budget_state = RepoReviewBudgetState::default();
    let _objective_guard_retries: u32 = 0;
    let _finalizer_evidence_retries: u32 = 0;
    let _finalizer_output_quality_retries: u32 = 0;
    let _finalizer_action_execution_retries: u32 = 0;
    let _clarify_tool_retries: u32 = 0;
    let _governor_window_limit = governor_window_size();
    let budget_cap = max_turns_limit.unwrap_or(agent.config().max_turns);
    let _iteration_budget = crate::iteration_budget::IterationBudget::new(budget_cap);
    let _tool_guardrails = crate::tool_guardrails::ToolGuardrailController::new();
    let _file_mutation =
        crate::file_mutation_tracker::FileMutationTracker::new(agent.config().checkpoints_enabled);
    let stream_scrubber =
        if crate::llm_caller::use_streaming_llm_transport(agent, ui_streaming, 0, None) {
            Some(crate::stream_scrubber::ThinkBlockScrubber::new())
        } else {
            None
        };
    let _checkpoint_mgr = hermes_tools::CheckpointManager::new(
        agent.config().checkpoints_enabled,
        agent.config().hermes_home.as_deref().map(Path::new),
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );

    let web_research_cfg = agent.config().web_research.clone();
    let web_auxiliary = if web_research_cfg.enabled
        && (web_research_cfg.planner_enabled || web_research_cfg.evaluator_enabled)
    {
        Some(build_auxiliary_arc_for_config(&agent.config()))
    } else {
        None
    };
    let web_research_ctrl = web_research_cfg
        .enabled
        .then(|| WebResearchController::with_user_message(web_research_cfg, &first_user));

    // Enter the turn state machine
    let max_turns_limit = effective_max_turns(agent.config().max_turns);
    let budget_cap = max_turns_limit.unwrap_or(agent.config().max_turns);
    let mut turn_ctx = TurnContext::new(
        ctx,
        system_content,
        tool_schemas,
        ui_streaming,
        stream_mute,
        stream_needs_break,
        stream_chunk_sink,
        session_id_owned,
        session_started_hooks_fired,
        prefill_range,
        persist_user_idx,
        replay,
        task_hint,
        first_user.clone(),
        review_memory_at_end,
        budget_cap,
        agent.config().checkpoints_enabled,
        agent.config().hermes_home.clone(),
        web_research_ctrl,
        web_auxiliary,
        stream_scrubber,
    );
    if turn_ctx.equity_research_gate.is_enabled() {
        if let Some(sym) = hermes_tools::try_resolve_a_share_from_user_message(&first_user).await {
            tracing::info!(symbol = %sym, "equity research: seeded symbol from user message");
            turn_ctx.equity_research_gate.seed_pending_symbol(&sym);
            turn_ctx.ctx.add_message(Message::system(format!(
                "Listed-equity: resolved A-share symbol {sym} from the user's message. \
                 Call analyze_stock(symbol=\"{sym}\", use_providers=true) before web_search or get_market_data for research."
            )));
        }
    }
    let mut state = TurnState::Guard;
    loop {
        match state {
            TurnState::Done(result) => return result,
            _ => state = state.transition(agent, &mut turn_ctx).await,
        }
    }
}

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

/// Output of B-segment [`prepare_turn`].
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

pub(crate) const TOOL_RESULT_EMPTY_CONTINUATION_USER_MESSAGE: &str = "[System: Tool results are available above. Use them to answer the user's original request now. Do not send a transition or acknowledgement only. If the results are insufficient, state that clearly.]";
pub(crate) const TOOL_RESULT_EMPTY_FAILURE_MESSAGE: &str =
    "工具已经返回结果，但模型没有生成最终答复。请重试一次。";

pub(crate) fn last_non_system_message_is_tool_result(messages: &[Message]) -> bool {
    messages
        .iter()
        .rev()
        .find(|m| m.role != MessageRole::System)
        .is_some_and(|m| m.role == MessageRole::Tool)
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
        let prepared = prepare_turn(
            &agent,
            &RunConversationParams {
                user_message: "current".into(),
                conversation_history: vec![Message::user("prior")],
                task_id: Some("task-x".into()),
                stream_callback: None,
                persist_user_message: None,
                tools: None,
                persist_session: false,
            },
        )
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
        hydrate_memory_nudge_counters_from_history(&agent, &history);
        let counters = &agent.state.lock().expect("lock").evolution_counters;
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
        hydrate_memory_nudge_counters_from_history(&agent, &history);
        assert_eq!(
            agent
                .state
                .lock()
                .expect("lock")
                .evolution_counters
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
