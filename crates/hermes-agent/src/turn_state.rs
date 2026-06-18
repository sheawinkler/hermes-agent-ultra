//! Turn state machine for the agent loop.
//!
//! Replaces the flat `loop { ... }` in `run_with_message_prelude` with
//! an explicit state machine. Each state is a well-defined step with a
//! single transition to the next state — no hidden gotos, no multi-level
//! breaks.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use hermes_core::{
    AgentError, AgentResult, LlmResponse, Message, StreamChunk, ToolCall, ToolResult, ToolSchema,
    UsageStats,
};
use serde_json::Value;

use crate::agent_loop::{
    AgentLoop, LoopExit, OBJECTIVE_DEEP_AUDIT_MAX_RETRIES, OBJECTIVE_GUARD_MAX_RETRIES,
    ReplayRecorder, RepoReviewBudgetState, StreamCollectOutcome, ToolProgressWatchdog,
    TurnRuntimeRoute, apply_repo_review_discovery_budget_policy,
    apply_repo_review_tool_profile_narrowing, apply_web_tool_budget,
    detect_contextlattice_connect_intent, effective_max_turns, estimate_usage_cost_usd,
    extract_last_user_assistant, finalizer_action_execution_requires_retry,
    finalizer_claim_requires_evidence_retry, finalizer_output_quality_requires_retry,
    governor_for_turn, governor_runtime_state, governor_window_size, is_budgeted_web_tool,
    latest_user_content, merge_usage, objective_guard_policy, objective_guard_retry_prompt,
    objective_guard_satisfied, push_window_f64, push_window_u64,
    should_apply_turn_reliability_guard, should_trip_tool_loop_guard,
    update_repo_review_budget_state_from_results, web_tool_budget_max_consecutive_errors,
    web_tool_budget_user_notice,
};
use crate::budget;
use crate::compression::estimate_request_tokens_for_compression;
use crate::context::ContextManager;

use crate::file_mutation_tracker::FileMutationTracker;
use crate::governor::GovernorRuntimeState;
use crate::hooks;
use crate::iteration_budget::IterationBudget;
use crate::message_sanitization::{
    CLARIFY_TOOL_RETRY_MAX, CLARIFY_TOOL_RETRY_USER_MESSAGE, CODEX_CONTINUE_USER_MESSAGE,
    budget_pressure_text, clarify_tool_invocation_requires_retry, continuation_prompt_for_response,
    inject_budget_pressure_into_last_tool_result, strip_think_blocks_for_ack,
};
use crate::plugins::HookType;
use crate::route_learning;
use crate::runtime_provider;
use crate::stream_scrubber::ThinkBlockScrubber;
use crate::tool_guardrails::ToolGuardrailController;
use crate::web_research::WebResearchController;
use hermes_intelligence::auxiliary::AuxiliaryClient;

// ---------------------------------------------------------------------------
// TurnState
// ---------------------------------------------------------------------------

/// All possible states of the main agent turn loop.
#[derive(Debug, Clone)]
pub(crate) enum TurnState {
    /// Check interrupt signal and iteration budget.
    Guard,
    /// Memory prefetch + pre-LLM hooks + turn init.
    Prefetch,
    /// Smart model routing (cheap-vs-strong) + reliability guards.
    RouteSelection,
    /// Actually call the LLM.
    CallLlm,
    /// Process LLM output: extract tool calls vs text, continuation logic.
    ProcessLlmOutput,
    /// Execute tool calls in parallel.
    ExecuteTools,
    /// Post-tool processing: guard checks, objective guard, memory sync.
    PostTool,
    /// Loop is done — return the result.
    Done(Result<AgentResult, AgentError>),
}

impl TurnState {
    /// Single-step transition: execute current state and return the next state.
    pub(crate) async fn transition(self, agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
        match self {
            TurnState::Guard => turn_guard(agent, tc).await,
            TurnState::Prefetch => turn_prefetch(agent, tc).await,
            TurnState::RouteSelection => turn_route(agent, tc).await,
            TurnState::CallLlm => turn_call_llm(agent, tc).await,
            TurnState::ProcessLlmOutput => turn_process_output(agent, tc).await,
            TurnState::ExecuteTools => turn_execute_tools(agent, tc).await,
            TurnState::PostTool => turn_post_tool(agent, tc).await,
            TurnState::Done(result) => TurnState::Done(result),
        }
    }
}

// ---------------------------------------------------------------------------
// MessageAnalysisCache — per-turn token estimate cache
// ---------------------------------------------------------------------------

/// Caches the per-turn `estimate_request_tokens_for_compression` result so it
/// is only recomputed when `ContextManager::message_generation` changes.
#[derive(Default)]
pub(crate) struct MessageAnalysisCache {
    /// Generation value when the cache was last populated.
    pub(crate) cached_generation: u64,
    /// Approximate request token count from the last analysis, or `None`
    /// if the cache has never been filled or was explicitly invalidated.
    pub(crate) approx_request_tokens: Option<u32>,
}

impl MessageAnalysisCache {
    /// Return a cached value if `current_generation` matches, otherwise `None`.
    pub(crate) fn get(&self, current_generation: u64) -> Option<u32> {
        if self.approx_request_tokens.is_some() && self.cached_generation == current_generation {
            self.approx_request_tokens
        } else {
            None
        }
    }

    /// Store a fresh estimate together with the generation it was computed for.
    pub(crate) fn set(&mut self, generation: u64, tokens: u32) {
        self.cached_generation = generation;
        self.approx_request_tokens = Some(tokens);
    }
}

// ---------------------------------------------------------------------------
// TurnContext — all loop-local mutable state
// ---------------------------------------------------------------------------

/// Holds all per-session mutable state that was previously scattered as local
/// variables inside `run_with_message_prelude`.
pub(crate) struct TurnContext {
    // --- Inter-state data (carried between state functions) ---
    pub last_llm_response: Option<LlmResponse>,
    pub turn_usage_acc: Option<UsageStats>,
    pub _api_elapsed_ms: u64,
    pub active_model_str: String,
    pub turn_runtime_route: Option<TurnRuntimeRoute>,
    pub tool_calls_to_execute: Option<Vec<ToolCall>>,
    pub assistant_msg: Option<Message>,
    pub response: Option<LlmResponse>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_results: Option<Vec<ToolResult>>,
    pub _turn_tool_error_count: u32,
    pub tool_elapsed: u64,
    pub turn_governor_runtime: Option<GovernorRuntimeState>,
    // --- Context ---
    pub ctx: ContextManager,
    pub system_content: String,
    pub tool_errors: Vec<hermes_core::ToolErrorRecord>,
    pub session_id: String,
    pub session_started_hooks_fired: bool,
    pub prefill_range: Option<std::ops::Range<usize>>,
    pub persist_user_idx: Option<usize>,
    pub replay: ReplayRecorder,
    pub task_hint: String,

    // --- Streaming ---
    pub ui_streaming: bool,
    pub stream_mute: Option<Arc<AtomicBool>>,
    pub stream_needs_break: Option<Arc<AtomicBool>>,
    pub stream_chunk_sink: Box<dyn Fn(StreamChunk) + Send + Sync>,

    // --- Turn counters & retries ---
    pub total_turns: u32,
    pub api_call_count: u32,
    pub accumulated_usage: Option<UsageStats>,
    pub session_cost_usd: f64,
    pub cost_warned: bool,
    pub codex_ack_continuations: u32,
    pub continuation_retries: u32,
    pub continuation_trigger_count: u32,
    pub ack_trigger_count: u32,
    pub premature_finalize_suspected_count: u32,
    pub invalid_tool_retries: u32,
    pub invalid_json_retries: u32,
    pub truncated_tool_call_retries: u32,
    pub objective_guard_retries: u32,
    pub finalizer_evidence_retries: u32,
    pub finalizer_output_quality_retries: u32,
    pub finalizer_action_execution_retries: u32,
    pub clarify_tool_retries: u32,
    pub last_content_with_tools: Option<String>,
    pub web_tool_calls_used: u32,
    pub web_search_calls_used: u32,
    pub web_tool_consecutive_error_turns: u32,
    pub web_finalize_hint_injected: bool,
    pub web_active_hint_injected: bool,
    pub review_memory_at_end: bool,
    pub first_user: String,

    // --- Governor state ---
    pub governor_llm_latency_window: VecDeque<u64>,
    pub governor_tool_error_window: VecDeque<f64>,
    pub governor_consecutive_error_turns: u32,
    pub governor_window_limit: usize,

    // --- Runtime route ---
    pub forced_runtime_route: Option<TurnRuntimeRoute>,

    // --- Model & tool state ---
    pub tool_schemas: Arc<[ToolSchema]>,
    pub active_tool_schemas: Arc<[ToolSchema]>,

    // --- Checkpoint ---
    pub last_checkpoint_messages: Option<Vec<Message>>,

    // --- File mutation tracker ---
    pub file_mutation: FileMutationTracker,

    // --- Guardrails ---
    pub tool_guardrails: ToolGuardrailController,

    // --- Iteration budget ---
    pub iteration_budget: IterationBudget,

    // --- Streaming scrubber ---
    pub stream_scrubber: Option<ThinkBlockScrubber>,

    // --- Web research ---
    pub web_research_ctrl: Option<WebResearchController>,
    pub web_auxiliary: Option<Arc<AuxiliaryClient>>,

    // --- Listed-equity tool ordering (analyze_stock before web) ---
    pub equity_research_gate: crate::equity_research_gate::EquityResearchGate,

    // --- Checkpoint manager ---
    pub checkpoint_mgr: hermes_tools::CheckpointManager,

    // --- Misc ---
    pub repo_review_budget_state: RepoReviewBudgetState,

    // --- Context pressure ---
    pub context_pressure_warned_at: f64,
    pub context_pressure_last_warn_at: Option<Instant>,
    pub context_pressure_last_warn_percent: f64,

    // --- Token estimate cache ---
    pub(crate) token_analysis: MessageAnalysisCache,
}

impl TurnContext {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ctx: ContextManager,
        system_content: String,
        tool_schemas: Arc<[ToolSchema]>,
        ui_streaming: bool,
        stream_mute: Option<Arc<AtomicBool>>,
        stream_needs_break: Option<Arc<AtomicBool>>,
        stream_chunk_sink: Box<dyn Fn(StreamChunk) + Send + Sync>,
        session_id: String,
        session_started_hooks_fired: bool,
        prefill_range: Option<std::ops::Range<usize>>,
        persist_user_idx: Option<usize>,
        replay: ReplayRecorder,
        task_hint: String,
        first_user: String,
        review_memory_at_end: bool,
        budget_cap: u32,
        checkpoints_enabled: bool,
        hermes_home: Option<String>,
        web_research_ctrl: Option<WebResearchController>,
        web_auxiliary: Option<Arc<AuxiliaryClient>>,
        stream_scrubber: Option<ThinkBlockScrubber>,
    ) -> Self {
        let governor_window_limit = governor_window_size();
        let equity_research_gate =
            crate::equity_research_gate::EquityResearchGate::from_tool_schemas(
                tool_schemas.as_ref(),
            );
        TurnContext {
            ctx,
            system_content,
            last_llm_response: None,
            turn_usage_acc: None,
            _api_elapsed_ms: 0,
            active_model_str: String::new(),
            turn_runtime_route: None,
            tool_calls_to_execute: None,
            assistant_msg: None,
            response: None,
            tool_calls: None,
            tool_results: None,
            _turn_tool_error_count: 0,
            tool_elapsed: 0,
            turn_governor_runtime: None,
            tool_errors: Vec::new(),
            session_id,
            session_started_hooks_fired,
            prefill_range,
            persist_user_idx,
            replay,
            task_hint,
            first_user,
            review_memory_at_end,
            ui_streaming,
            stream_mute,
            stream_needs_break,
            stream_chunk_sink,
            total_turns: 0,
            api_call_count: 0,
            accumulated_usage: None,
            session_cost_usd: 0.0,
            cost_warned: false,
            codex_ack_continuations: 0,
            continuation_retries: 0,
            continuation_trigger_count: 0,
            ack_trigger_count: 0,
            premature_finalize_suspected_count: 0,
            invalid_tool_retries: 0,
            invalid_json_retries: 0,
            truncated_tool_call_retries: 0,
            objective_guard_retries: 0,
            finalizer_evidence_retries: 0,
            finalizer_output_quality_retries: 0,
            finalizer_action_execution_retries: 0,
            clarify_tool_retries: 0,
            last_content_with_tools: None,
            web_tool_calls_used: 0,
            web_search_calls_used: 0,
            web_tool_consecutive_error_turns: 0,
            web_finalize_hint_injected: false,
            web_active_hint_injected: false,
            governor_llm_latency_window: VecDeque::new(),
            governor_tool_error_window: VecDeque::new(),
            governor_consecutive_error_turns: 0,
            governor_window_limit,
            forced_runtime_route: None,
            tool_schemas: tool_schemas.clone(),
            active_tool_schemas: tool_schemas,
            last_checkpoint_messages: None,
            file_mutation: FileMutationTracker::new(checkpoints_enabled),
            tool_guardrails: ToolGuardrailController::new(),
            iteration_budget: IterationBudget::new(budget_cap),
            stream_scrubber,
            web_research_ctrl,
            web_auxiliary,
            equity_research_gate,
            checkpoint_mgr: hermes_tools::CheckpointManager::new(
                checkpoints_enabled,
                hermes_home.as_deref().map(Path::new),
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            ),
            repo_review_budget_state: RepoReviewBudgetState::default(),
            context_pressure_warned_at: 0.0,
            context_pressure_last_warn_at: None,
            context_pressure_last_warn_percent: 0.0,
            token_analysis: MessageAnalysisCache::default(),
        }
    }
}

// ===========================================================================
// State functions
// ===========================================================================

// ---------------------------------------------------------------------------
// Guard — check interrupt, max turns, iteration budget
// ---------------------------------------------------------------------------

async fn turn_guard(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    if let Some(scrubber) = tc.stream_scrubber.as_mut() {
        scrubber.reset();
    }
    if agent.interrupt.take_interrupt_graceful().is_some() {
        return TurnState::Done(Ok(agent.graceful_interrupt_result(
            &tc.ctx,
            tc.total_turns,
            std::mem::take(&mut tc.tool_errors),
            tc.accumulated_usage.take(),
            tc.session_cost_usd,
            tc.session_started_hooks_fired,
            tc.persist_user_idx,
            tc.prefill_range.clone(),
            tc.api_call_count,
        )));
    }

    let max_turns_limit = effective_max_turns(agent.config().max_turns);
    if let Some(max_turns) = max_turns_limit {
        if tc.iteration_budget.exhausted() || tc.total_turns >= max_turns {
            tracing::warn!(
                "Max turns ({}) exceeded, requesting final summary",
                max_turns
            );
            let summary_msg = agent.handle_max_iterations(&mut tc.ctx).await;
            let summary_msg = match summary_msg {
                Ok(msg) => msg,
                Err(e) => return TurnState::Done(Err(e)),
            };
            if let Some(msg) = summary_msg {
                tc.ctx.add_message(msg);
            }
            crate::hooks::turn_end_plugin_hooks(
                agent,
                tc.ctx.get_messages(),
                false,
                false,
                tc.total_turns,
                tc.session_started_hooks_fired,
            );
            tc.replay.record(
                "session_end",
                serde_json::json!({
                    "reason": "max_turns",
                    "total_turns": tc.total_turns,
                    "session_cost_usd": tc.session_cost_usd,
                }),
            );
            return TurnState::Done(Ok(agent.seal_loop_result(
                &tc.ctx,
                tc.persist_user_idx,
                tc.prefill_range.clone(),
                LoopExit::base(
                    "max_iterations_reached",
                    tc.api_call_count,
                    false,
                    false,
                    false,
                    false,
                ),
                tc.total_turns,
                std::mem::take(&mut tc.tool_errors),
                tc.accumulated_usage.take(),
                tc.session_cost_usd,
                tc.session_started_hooks_fired,
            )));
        }
    }

    TurnState::Prefetch
}

// ---------------------------------------------------------------------------
// Prefetch — increment turn counter, OAuth refresh, memory sync, checkpoint
// ---------------------------------------------------------------------------

async fn turn_prefetch(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    tc.total_turns = tc.total_turns.saturating_add(1);
    agent.invalidate_turn_api_messages_cache();
    tc.checkpoint_mgr.new_turn();
    tc.iteration_budget.consume();
    // tracing::debug!("Agent turn {}", tc.total_turns);

    // Housekeeping-only turns enable mute_post_response for the *current* turn's
    // pre-tool narration. Reset each turn so the next LLM stream (especially the
    // final natural-language reply) is delivered to gateway native streaming.
    if let Some(mute) = tc.stream_mute.as_ref() {
        mute.store(false, Ordering::Release);
    }

    // Refresh oauth-backed runtime credentials before routing/provider selection.
    crate::runtime_provider::refresh_oauth_store_tokens_if_needed(agent).await;

    // Skill nudge counter
    if agent.config().skill_creation_nudge_interval > 0
        && agent
            .tool_registry
            .names()
            .iter()
            .any(|n| n == "skill_manage")
    {
        if let Ok(mut state) = agent.state.lock() {
            state.evolution_counters.iters_since_skill =
                state.evolution_counters.iters_since_skill.saturating_add(1);
        }
    }

    if agent.config().checkpoint_interval_turns > 0
        && (tc.total_turns - 1) % agent.config().checkpoint_interval_turns == 0
    {
        tc.last_checkpoint_messages = Some(tc.ctx.get_messages().to_vec());
    }

    if let Some(hint) = crate::prompt_builder::plan_mode_turn_hint(
        agent.plan_phase(),
        agent.pending_plan().as_deref(),
    ) {
        tc.ctx.add_message(Message::system(hint));
    }

    // Memory sync at flush interval
    if tc.total_turns % agent.config().memory_flush_interval == 0 && tc.total_turns > 0 {
        let msgs = tc.ctx.get_messages();
        let (u, a) = extract_last_user_assistant(msgs);
        agent.memory_sync(&u, &a, &tc.session_id);
    }

    TurnState::RouteSelection
}

// ---------------------------------------------------------------------------
// RouteSelection — smart model routing, reliability guards
// ---------------------------------------------------------------------------

async fn turn_route(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    let turn_runtime_route = tc
        .forced_runtime_route
        .clone()
        .or_else(|| route_learning::resolve_smart_runtime_route(agent, tc.ctx.get_messages()));
    let turn_default_model = runtime_provider::active_model(agent);
    let active_model = turn_runtime_route
        .as_ref()
        .map(|r| r.model.as_str())
        .unwrap_or(turn_default_model.as_str());
    let turn_governor_runtime = governor_runtime_state(
        &tc.governor_llm_latency_window,
        &tc.governor_tool_error_window,
        tc.governor_consecutive_error_turns,
    );
    let llm_governor = governor_for_turn(&agent.config(), &tc.ctx, 0, Some(&turn_governor_runtime));

    if let Some(ref ctrl) = tc.web_research_ctrl {
        tc.active_tool_schemas = Arc::from(ctrl.filter_tool_schemas(tc.tool_schemas.as_ref()));
        if !tc.web_active_hint_injected {
            if let Some(hint) = ctrl.active_research_system_hint() {
                tc.ctx.add_message(Message::system(hint));
                tc.web_active_hint_injected = true;
            }
        }
        if !tc.web_finalize_hint_injected {
            if let Some(hint) = ctrl.finalization_system_hint() {
                tc.ctx.add_message(Message::system(hint));
                tc.web_finalize_hint_injected = true;
            }
        }
    }

    let approx_request_tokens = {
        let msg_gen = tc.ctx.message_generation;
        tc.token_analysis.get(msg_gen).unwrap_or_else(|| {
            let tokens = estimate_request_tokens_for_compression(
                tc.ctx.get_messages(),
                &tc.system_content,
                tc.active_tool_schemas.as_ref(),
            ) as u32;
            tc.token_analysis.set(msg_gen, tokens);
            tokens
        })
    };
    let rt_snap = route_learning::primary_runtime_snapshot(agent);
    if let Some(err) = crate::message_sanitization::ollama_context_limit_error(
        agent.config().ollama_num_ctx,
        !tc.active_tool_schemas.is_empty(),
        approx_request_tokens,
        active_model,
        rt_snap.provider.as_deref().unwrap_or("unknown"),
        rt_snap.base_url.as_deref().unwrap_or("unknown"),
        tc.active_tool_schemas.len(),
        agent.config().session_id.as_deref(),
    ) {
        tc.ctx.add_message(Message::assistant(err));
        tc.iteration_budget.refund(1);
        tc.total_turns = tc.total_turns.saturating_sub(1);
        return TurnState::Done(Ok(agent.seal_loop_result(
            &tc.ctx,
            tc.persist_user_idx,
            tc.prefill_range.clone(),
            LoopExit::base(
                "ollama_runtime_context_too_small",
                tc.api_call_count,
                true,
                false,
                false,
                false,
            ),
            tc.total_turns,
            std::mem::take(&mut tc.tool_errors),
            tc.accumulated_usage.take(),
            tc.session_cost_usd,
            tc.session_started_hooks_fired,
        )));
    }

    if tc.forced_runtime_route.is_none()
        && should_apply_turn_reliability_guard(
            &turn_governor_runtime,
            &llm_governor,
            tc.governor_llm_latency_window.len(),
        )
    {
        if let Some(model) = route_learning::resolve_reliability_degrade_model(
            agent,
            active_model,
            turn_runtime_route.as_ref(),
        ) {
            tracing::info!(
                turn = tc.total_turns,
                model = %model,
                consecutive_error_turns = turn_governor_runtime.consecutive_error_turns,
                avg_llm_latency_ms = ?turn_governor_runtime.avg_llm_latency_ms,
                "reliability guard switching runtime route after degradation"
            );
            tc.forced_runtime_route = Some(route_learning::turn_route_reliability_guard(
                agent,
                model.clone(),
            ));
            tc.ctx.add_message(Message::system(format!(
                "Reliability guard: runtime degradation detected. Switching next turns to `{}`.",
                model
            )));
        }
    }
    // tracing::debug!(
    //     turn = tc.total_turns,
    //     model = active_model,
    //     governor_pressure = llm_governor.pressure,
    //     governor_max_tokens = ?llm_governor.max_tokens,
    //     governor_avg_latency_ms = ?turn_governor_runtime.avg_llm_latency_ms,
    //     governor_avg_tool_error_rate = turn_governor_runtime.avg_tool_error_rate,
    //     governor_consecutive_error_turns = turn_governor_runtime.consecutive_error_turns,
    //     "turn governor snapshot"
    // );
    tc.replay.record(
        "turn_start",
        serde_json::json!({
            "turn": tc.total_turns,
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
    TurnState::CallLlm
}

// ---------------------------------------------------------------------------
// CallLlm — call the LLM with semantic empty/thinking recovery
// ---------------------------------------------------------------------------

async fn turn_call_llm(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    let turn_runtime_route = tc
        .forced_runtime_route
        .clone()
        .or_else(|| route_learning::resolve_smart_runtime_route(agent, tc.ctx.get_messages()));
    let turn_default_model = runtime_provider::active_model(agent);
    let active_model = turn_runtime_route
        .as_ref()
        .map(|r| r.model.as_str())
        .unwrap_or(turn_default_model.as_str());
    let turn_governor_runtime = governor_runtime_state(
        &tc.governor_llm_latency_window,
        &tc.governor_tool_error_window,
        tc.governor_consecutive_error_turns,
    );
    let llm_governor = governor_for_turn(&agent.config(), &tc.ctx, 0, Some(&turn_governor_runtime));

    // --- Streaming first attempt + semantic empty/thinking recovery ---
    let api_start = Instant::now();
    let mut inner_empty = 0u32;
    let mut tool_result_empty_continuation_requested = false;
    let mut inner_thinking = 0u32;
    let mut turn_usage_acc: Option<UsageStats> = None;
    let mut inner_attempt: u32 = 0;

    let mut response = loop {
        if agent.interrupt.take_interrupt_graceful().is_some() {
            return TurnState::Done(Ok(agent.graceful_interrupt_result(
                &tc.ctx,
                tc.total_turns,
                std::mem::take(&mut tc.tool_errors),
                tc.accumulated_usage.take(),
                tc.session_cost_usd,
                tc.session_started_hooks_fired,
                tc.persist_user_idx,
                tc.prefill_range.clone(),
                tc.api_call_count,
            )));
        }
        let r = if crate::llm_caller::use_streaming_llm_transport(
            agent,
            tc.ui_streaming,
            inner_attempt,
            turn_runtime_route.as_ref(),
        ) {
            match crate::llm_caller::collect_stream_llm_response(
                agent,
                &mut tc.ctx,
                &tc.active_tool_schemas,
                turn_runtime_route.as_ref(),
                active_model,
                llm_governor.max_tokens,
                tc.stream_chunk_sink.as_ref(),
                &mut tc.api_call_count,
                tc.stream_scrubber.as_mut(),
            )
            .await
            {
                Ok(StreamCollectOutcome::Complete(resp)) => resp,
                Ok(StreamCollectOutcome::Interrupted(partial)) => {
                    if let Some(ref u) = partial.usage {
                        agent.record_api_usage(u);
                        tc.accumulated_usage = Some(merge_usage(tc.accumulated_usage.take(), u));
                        if let Some(cost) =
                            estimate_usage_cost_usd(u, partial.model.as_str(), &agent.config())
                        {
                            tc.session_cost_usd += cost;
                        }
                    }
                    tc.ctx.add_message(partial.message);
                    return TurnState::Done(Ok(agent.graceful_interrupt_result(
                        &tc.ctx,
                        tc.total_turns,
                        std::mem::take(&mut tc.tool_errors),
                        tc.accumulated_usage.take(),
                        tc.session_cost_usd,
                        tc.session_started_hooks_fired,
                        tc.persist_user_idx,
                        tc.prefill_range.clone(),
                        tc.api_call_count,
                    )));
                }
                Err(e) => {
                    let api_elapsed = api_start.elapsed().as_millis() as u64;
                    route_learning::update_route_learning(
                        agent,
                        turn_runtime_route.as_ref(),
                        Some(active_model),
                        api_elapsed,
                        false,
                    );
                    return TurnState::Done(Err(e.into()));
                }
            }
        } else {
            match agent
                .call_llm_with_retry(
                    &mut tc.ctx,
                    &tc.active_tool_schemas,
                    turn_runtime_route.as_ref(),
                    llm_governor.max_tokens,
                    &mut tc.api_call_count,
                )
                .await
            {
                Ok(r) => r,
                Err(AgentError::Interrupted { .. }) => {
                    return TurnState::Done(Ok(agent.graceful_interrupt_result(
                        &tc.ctx,
                        tc.total_turns,
                        std::mem::take(&mut tc.tool_errors),
                        tc.accumulated_usage.take(),
                        tc.session_cost_usd,
                        tc.session_started_hooks_fired,
                        tc.persist_user_idx,
                        tc.prefill_range.clone(),
                        tc.api_call_count,
                    )));
                }
                Err(e) => {
                    let api_elapsed = api_start.elapsed().as_millis() as u64;
                    route_learning::update_route_learning(
                        agent,
                        turn_runtime_route.as_ref(),
                        Some(active_model),
                        api_elapsed,
                        false,
                    );
                    return TurnState::Done(Err(e.into()));
                }
            }
        };
        inner_attempt = inner_attempt.saturating_add(1);

        if let Some(ref u) = r.usage {
            agent.record_api_usage(u);
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
        if AgentLoop::assistant_visible_text(&r.message) {
            break r;
        }
        if AgentLoop::assistant_has_reasoning(&r.message)
            && inner_thinking < agent.config().thinking_prefill_max_retries
        {
            inner_thinking += 1;
            crate::llm_caller::handle_reasoning_only_prefill(
                agent,
                &r.message,
                inner_thinking,
                agent.config().thinking_prefill_max_retries,
            );
            tc.ctx.add_message(r.message.clone());
            continue;
        }
        let empty_without_reasoning = !AgentLoop::assistant_has_reasoning(&r.message);
        let awaiting_tool_result_final = tool_result_empty_continuation_requested
            || crate::conversation_loop::last_non_system_message_is_tool_result(
                tc.ctx.get_messages(),
            );
        if empty_without_reasoning && awaiting_tool_result_final {
            if !tool_result_empty_continuation_requested {
                tool_result_empty_continuation_requested = true;
                hooks::emit_status(
                    agent,
                    "lifecycle",
                    "Tool result received but assistant returned empty stop; requesting final answer.",
                );
                tc.ctx.add_message(Message::user(
                    crate::conversation_loop::TOOL_RESULT_EMPTY_CONTINUATION_USER_MESSAGE,
                ));
                continue;
            }
            let mut response = r;
            response.message.content =
                Some(crate::conversation_loop::TOOL_RESULT_EMPTY_FAILURE_MESSAGE.to_string());
            response.finish_reason = Some("empty_after_tool_result".to_string());
            break response;
        }
        // Accept explicit stop/end-turn responses even when assistant text is empty.
        if empty_without_reasoning && r.finish_reason.as_deref() == Some("stop") {
            break r;
        }
        if empty_without_reasoning && inner_empty < agent.config().empty_content_max_retries {
            inner_empty += 1;
            tracing::warn!(
                "empty assistant response (stream path) - retrying ({}/{})",
                inner_empty,
                agent.config().empty_content_max_retries
            );
            hooks::emit_status(
                agent,
                "lifecycle",
                &format!(
                    "Empty assistant response - retrying ({}/{})",
                    inner_empty,
                    agent.config().empty_content_max_retries
                ),
            );
            continue;
        }
        break r;
    };

    crate::llm_caller::upgrade_finish_reason_for_truncated_tool_args(&mut response);
    let _api_elapsed_ms = api_start.elapsed().as_millis() as u64;
    route_learning::update_route_learning(
        agent,
        turn_runtime_route.as_ref(),
        Some(response.model.as_str()),
        _api_elapsed_ms,
        true,
    );
    push_window_u64(
        &mut tc.governor_llm_latency_window,
        _api_elapsed_ms,
        tc.governor_window_limit,
    );

    tc.replay.record(
        "llm_response",
        serde_json::json!({
            "turn": tc.total_turns,
            "model": response.model,
            "finish_reason": response.finish_reason,
            "api_time_ms": _api_elapsed_ms,
            "tool_call_count": response.message.tool_calls.as_ref().map(|v| v.len()).unwrap_or(0),
            "has_visible_text": AgentLoop::assistant_visible_text(&response.message),
            "route_learning": route_learning::route_learning_snapshot(agent,
                turn_runtime_route.as_ref(),
                Some(response.model.as_str()),
            ),
        }),
    );

    if std::env::var("HERMES_TURN_PERF").map_or(false, |v| !v.is_empty()) {
        tracing::info!(
            target: "turn_perf",
            turn = tc.total_turns,
            provider = turn_runtime_route
                .as_ref()
                .and_then(|r| r.provider.as_deref())
                .unwrap_or("primary"),
            model = %response.model,
            api_ms = _api_elapsed_ms,
            tool_calls = response.message.tool_calls.as_ref().map_or(0, |v| v.len()),
            finish_reason = ?response.finish_reason,
            "llm turn timing"
        );
    }

    // Store response in context for next state
    tc.last_llm_response = Some(response);
    tc.turn_usage_acc = turn_usage_acc;
    tc._api_elapsed_ms = _api_elapsed_ms;
    tc.turn_runtime_route = turn_runtime_route.clone();
    tc.active_model_str = active_model.to_string();

    TurnState::ProcessLlmOutput
}

// ---------------------------------------------------------------------------
// ProcessLlmOutput — extract tool calls, finalization gate
// ---------------------------------------------------------------------------

async fn turn_process_output(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    let mut response = tc.last_llm_response.take().expect("response available");
    let turn_runtime_route = tc.turn_runtime_route.take();
    let _api_elapsed_ms = tc._api_elapsed_ms;
    let turn_usage_acc = tc.turn_usage_acc.take();

    // --- Post-LLM hook ---
    let post_ctx = serde_json::json!({
        "turn": tc.total_turns,
        "api_time_ms": _api_elapsed_ms,
        "has_tool_calls": response.message.tool_calls.as_ref().map_or(false, |tc| !tc.is_empty()),
    });
    let post_results = hooks::invoke_hook(agent, HookType::PostLlmCall, &post_ctx);
    hooks::inject_hook_context(agent, &post_results, &mut tc.ctx);
    hooks::apply_hook_output_transforms(&post_results, &mut response.message.content);
    hooks::apply_transform_llm_output_hooks(agent, &mut response.message.content);

    // Accumulate usage (merged across semantic-retried sub-calls)
    if let Some(ref usage) = turn_usage_acc {
        tc.accumulated_usage = Some(merge_usage(tc.accumulated_usage.take(), usage));
        if let Some(cost) = estimate_usage_cost_usd(usage, response.model.as_str(), &agent.config())
        {
            tc.session_cost_usd += cost;
        }
    }

    // Cost guard
    if let Some(limit) = agent.config().max_cost_usd {
        if !tc.cost_warned
            && tc.session_cost_usd >= limit * agent.config().cost_guard_degrade_at_ratio
        {
            tc.cost_warned = true;
            if tc.forced_runtime_route.is_none() {
                if let Some(model) = route_learning::resolve_cost_degrade_model(agent) {
                    tc.forced_runtime_route =
                        Some(route_learning::turn_route_cost_guard(agent, model.clone()));
                    tc.ctx.add_message(Message::system(format!(
                        "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                        tc.session_cost_usd, limit, model
                    )));
                } else {
                    tc.ctx.add_message(Message::system(format!(
                        "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                        tc.session_cost_usd, limit
                    )));
                }
            }
        }
        if tc.session_cost_usd >= limit {
            tc.ctx.add_message(Message::system(format!(
                "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                tc.session_cost_usd, limit
            )));
            crate::hooks::turn_end_plugin_hooks(
                agent,
                tc.ctx.get_messages(),
                false,
                false,
                tc.total_turns,
                tc.session_started_hooks_fired,
            );
            tc.replay.record(
                "session_end",
                serde_json::json!({
                    "reason": "cost_guard",
                    "total_turns": tc.total_turns,
                    "session_cost_usd": tc.session_cost_usd,
                }),
            );
            return TurnState::Done(Ok(agent.seal_loop_result(
                &tc.ctx,
                tc.persist_user_idx,
                tc.prefill_range.clone(),
                LoopExit::base(
                    "max_iterations_reached",
                    tc.api_call_count,
                    false,
                    false,
                    false,
                    false,
                ),
                tc.total_turns,
                std::mem::take(&mut tc.tool_errors),
                tc.accumulated_usage.take(),
                tc.session_cost_usd,
                tc.session_started_hooks_fired,
            )));
        }
    }

    let history_includes_tool = tc
        .ctx
        .get_messages()
        .iter()
        .any(|m| m.role == hermes_core::MessageRole::Tool);
    let (assistant_msg, parsed_tool_calls, parsed_textual_tool_calls) =
        crate::tool_executor::coerce_textual_tool_calls(response.message.clone());
    if parsed_textual_tool_calls {
        hooks::emit_status(
            agent,
            "lifecycle",
            "Parsed textual tool-call markup from assistant output; executing parsed calls.",
        );
    }
    tc.ctx.add_message(assistant_msg.clone());

    if assistant_msg
        .tool_calls
        .as_ref()
        .map_or(false, |v| !v.is_empty())
        && AgentLoop::assistant_visible_text_after_think_blocks(&assistant_msg)
    {
        tc.last_content_with_tools = assistant_msg
            .content
            .as_deref()
            .map(strip_think_blocks_for_ack)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }

    // Truncated tool call retry
    if response.finish_reason.as_deref() == Some("length")
        && assistant_msg
            .tool_calls
            .as_ref()
            .map_or(false, |calls| !calls.is_empty())
        && tc.truncated_tool_call_retries < agent.config().truncated_tool_call_max_retries
    {
        tc.truncated_tool_call_retries = tc.truncated_tool_call_retries.saturating_add(1);
        hooks::emit_status(
            agent,
            "lifecycle",
            &format!(
                "Truncated tool arguments - retrying ({}/{})",
                tc.truncated_tool_call_retries,
                agent.config().truncated_tool_call_max_retries
            ),
        );
        let _ = tc.ctx.get_messages_mut().pop();
        return TurnState::CallLlm;
    }
    tc.truncated_tool_call_retries = 0;

    if let Some(ref cb) = agent.callbacks.on_step_complete {
        cb(tc.total_turns);
    }

    // If no tool calls, the agent is done
    let tool_calls: Vec<ToolCall> = parsed_tool_calls
        .into_iter()
        .filter(|tc| !tc.function.name.is_empty())
        .collect();

    if tool_calls.is_empty() {
        let effective_finish_reason = crate::llm_caller::effective_finish_reason(
            agent,
            &response,
            &assistant_msg,
            history_includes_tool,
            turn_runtime_route.as_ref(),
        );
        let finalization_signals = crate::llm_caller::build_finalization_signals(
            agent,
            &tc.task_hint,
            tc.ctx.get_messages(),
            &assistant_msg,
            effective_finish_reason.as_deref(),
        );
        // tracing::debug!(
        //     turn = tc.total_turns,
        //     finish_reason = ?finalization_signals.finish_reason,
        //     has_tool_calls = finalization_signals.has_tool_calls,
        //     has_visible_text = finalization_signals.has_visible_text,
        //     has_visible_text_after_think = finalization_signals.has_visible_text_after_think,
        //     has_reasoning = finalization_signals.has_reasoning,
        //     continuation_required = finalization_signals.continuation_required,
        //     ack_detected = finalization_signals.ack_detected,
        //     final_gate_passed = finalization_signals.final_gate_passed(),
        //     "finalization gate evaluation (stream)"
        // );
        tc.replay.record(
            "final_gate",
            serde_json::json!({
                "turn": tc.total_turns,
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
            if tc.continuation_retries < agent.config().continuation_max_retries {
                tc.continuation_retries = tc.continuation_retries.saturating_add(1);
                tc.continuation_trigger_count = tc.continuation_trigger_count.saturating_add(1);
                hooks::emit_status(
                    agent,
                    "lifecycle",
                    &format!(
                        "Assistant response incomplete ({:?}) - requesting continuation ({}/{})",
                        response.finish_reason,
                        tc.continuation_retries,
                        agent.config().continuation_max_retries
                    ),
                );
                tc.ctx
                    .add_message(Message::user(&continuation_prompt_for_response(&response)));
                return TurnState::CallLlm;
            }
            tc.premature_finalize_suspected_count =
                tc.premature_finalize_suspected_count.saturating_add(1);
            hooks::emit_status(
                agent,
                "lifecycle",
                &format!(
                    "Continuation retries exhausted ({}) - finalizing with best effort output",
                    agent.config().continuation_max_retries
                ),
            );
        } else {
            tc.continuation_retries = 0;
        }

        // Clarify tool retry
        if clarify_tool_invocation_requires_retry(
            &tc.task_hint,
            tc.active_tool_schemas.iter().any(|s| s.name == "clarify"),
            tc.clarify_tool_retries,
        ) {
            tc.clarify_tool_retries = tc.clarify_tool_retries.saturating_add(1);
            tracing::info!(
                retry = tc.clarify_tool_retries,
                max = CLARIFY_TOOL_RETRY_MAX,
                "clarify tool not invoked for user-requested clarify; retrying"
            );
            tc.ctx
                .add_message(Message::user(CLARIFY_TOOL_RETRY_USER_MESSAGE));
            return TurnState::CallLlm;
        }

        // Ack detection
        if finalization_signals.ack_detected {
            if !tc.tool_schemas.is_empty()
                && tc.codex_ack_continuations < agent.config().ack_continuation_max_retries
            {
                tc.codex_ack_continuations = tc.codex_ack_continuations.saturating_add(1);
                tc.ack_trigger_count = tc.ack_trigger_count.saturating_add(1);
                hooks::emit_status(
                    agent,
                    "lifecycle",
                    &format!(
                        "Detected intermediate ack - requesting continuation ({}/{})",
                        tc.codex_ack_continuations,
                        agent.config().ack_continuation_max_retries
                    ),
                );
                tc.ctx
                    .add_message(Message::user(CODEX_CONTINUE_USER_MESSAGE));
                return TurnState::CallLlm;
            }
            tc.premature_finalize_suspected_count =
                tc.premature_finalize_suspected_count.saturating_add(1);
        }

        // Fallback to last content with tools
        if !AgentLoop::assistant_visible_text_after_think_blocks(&assistant_msg) {
            if let Some(fallback) = tc.last_content_with_tools.take() {
                if let Some(last) = tc.ctx.get_messages_mut().last_mut() {
                    if last.role == hermes_core::MessageRole::Assistant {
                        last.content = Some(fallback);
                    }
                }
            }
        }

        // Finalizer evidence retry
        if finalizer_claim_requires_evidence_retry(
            tc.ctx.get_messages(),
            assistant_msg.content.as_deref().unwrap_or_default(),
            tc.finalizer_evidence_retries,
        ) {
            tc.finalizer_evidence_retries = tc.finalizer_evidence_retries.saturating_add(1);
            tc.ctx.add_message(Message::system(
                "[SYSTEM] Finalizer evidence contract: include explicit evidence lines and confidence calibration.\n\
                 Required format:\n\
                 - confidence=<high|medium|low>\n\
                 - file=<absolute-or-repo-path>\n\
                 - cmd=<verification command or exact probe>\n\
                 If evidence is missing, state `objective_state=unproven` and blockers.",
            ));
            tc.ctx.add_message(Message::user(
                "Re-issue the final response with explicit evidence + confidence now.",
            ));
            return TurnState::CallLlm;
        }

        // Finalizer output quality retry
        if finalizer_output_quality_requires_retry(
            assistant_msg.content.as_deref().unwrap_or_default(),
            tc.finalizer_output_quality_retries,
        ) {
            tc.finalizer_output_quality_retries =
                tc.finalizer_output_quality_retries.saturating_add(1);
            hooks::emit_status(
                agent,
                "lifecycle",
                "Detected templated/duplicated output; forcing concrete unique rewrite.",
            );
            tc.ctx.add_message(Message::system(
                "[SYSTEM] Output quality contract: do not use placeholders or template filler.\n\
                 Requirements:\n\
                 - no unresolved placeholders (`[URL](URL)`, `(URL)`, `pack of authors`, `<insert...>`)\n\
                 - no repeated list items or duplicated paragraphs\n\
                 - provide concrete, unique, user-relevant items only; if unknown, mark as `UNPROVEN` instead of fabricating.",
            ));
            tc.ctx.add_message(Message::user(
                "Re-issue the response now with concrete unique items and zero placeholders.",
            ));
            return TurnState::CallLlm;
        }

        // Finalizer action execution retry
        if finalizer_action_execution_requires_retry(
            tc.ctx.get_messages(),
            assistant_msg.content.as_deref().unwrap_or_default(),
            tc.finalizer_action_execution_retries,
        ) {
            tc.finalizer_action_execution_retries =
                tc.finalizer_action_execution_retries.saturating_add(1);
            hooks::emit_status(
                agent,
                "lifecycle",
                "Detected intent narration without execution evidence; forcing action run.",
            );
            tc.ctx.add_message(Message::system(
                "[SYSTEM] Execution contract: this request requires concrete execution now.\n\
                 Requirements:\n\
                 - run the relevant tool calls in this turn (do not only describe intent)\n\
                 - if blocked, output `BLOCKED:` with exact command/tool error and next probe\n\
                 - include at least one evidence line (`cmd=...` or `file=...`) in the final response.",
            ));
            tc.ctx.add_message(Message::user(
                "Execute now. Do not narrate intent; return concrete evidence or explicit BLOCKED state.",
            ));
            return TurnState::CallLlm;
        }
        tc.finalizer_evidence_retries = 0;
        tc.finalizer_output_quality_retries = 0;
        tc.finalizer_action_execution_retries = 0;

        // Objective guard
        let (objective_guard_active, requires_analytics, deep_audit_required) =
            objective_guard_policy(tc.ctx.get_messages());
        if objective_guard_active {
            let assistant_text = assistant_msg.content.as_deref().unwrap_or_default();
            let max_guard_retries = if deep_audit_required {
                OBJECTIVE_DEEP_AUDIT_MAX_RETRIES
            } else {
                OBJECTIVE_GUARD_MAX_RETRIES
            };
            if !objective_guard_satisfied(assistant_text, requires_analytics, deep_audit_required)
                && tc.objective_guard_retries < max_guard_retries
            {
                tc.objective_guard_retries = tc.objective_guard_retries.saturating_add(1);
                tc.ctx
                    .add_message(Message::system(objective_guard_retry_prompt(
                        requires_analytics,
                        deep_audit_required,
                    )));
                tc.ctx.add_message(Message::user(
                    "Re-issue the final response with required verified sections now.",
                ));
                return TurnState::CallLlm;
            }
        }

        if agent.plan_phase() == hermes_tools::PlanPhase::Planning {
            if AgentLoop::assistant_visible_text_after_think_blocks(&assistant_msg) {
                let plan_text = assistant_msg.content.clone().unwrap_or_default();
                agent.set_pending_plan(Some(plan_text.clone()));
                agent.set_plan_phase(hermes_tools::PlanPhase::AwaitingApproval);
                hooks::emit_status(
                    agent,
                    "lifecycle",
                    "Plan submitted; awaiting user approval (/plan-mode approve).",
                );
                return TurnState::Done(Ok(agent.seal_loop_result(
                    &tc.ctx,
                    tc.persist_user_idx,
                    tc.prefill_range.clone(),
                    LoopExit {
                        turn_exit_reason: "plan_awaiting_approval",
                        api_calls: tc.api_call_count,
                        failed: false,
                        partial: false,
                        finished_naturally: false,
                        interrupted: false,
                        plan_pending: Some(plan_text),
                        plan_phase: Some(
                            hermes_tools::PlanPhase::AwaitingApproval
                                .as_str()
                                .to_string(),
                        ),
                    },
                    tc.total_turns,
                    std::mem::take(&mut tc.tool_errors),
                    tc.accumulated_usage.take(),
                    tc.session_cost_usd,
                    tc.session_started_hooks_fired,
                )));
            }
        }

        if agent.plan_phase() == hermes_tools::PlanPhase::Executing {
            agent.set_plan_phase(hermes_tools::PlanPhase::Off);
            agent.set_pending_plan(None);
        }

        // tracing::debug!("No tool calls in response, finishing naturally");
        if tc.file_mutation.has_failures() {
            let footer = tc.file_mutation.format_advisory_footer();
            for msg in tc.ctx.get_messages_mut().iter_mut().rev() {
                if matches!(msg.role, hermes_core::MessageRole::Assistant) {
                    if let Some(content) = msg.content.as_mut() {
                        content.push_str(&footer);
                    }
                    break;
                }
            }
        }
        if let Err(err) = agent.append_objective_runtime_ledger(
            tc.ctx.get_messages(),
            assistant_msg.content.as_deref().unwrap_or_default(),
            tc.total_turns,
        ) {
            hooks::emit_status(
                agent,
                "lifecycle",
                &format!("Objective runtime ledger append skipped: {}", err),
            );
        }

        // Final memory sync
        let (u, a) = extract_last_user_assistant(tc.ctx.get_messages());
        agent.memory_sync(&u, &a, &tc.session_id);
        agent.spawn_background_review(
            tc.total_turns,
            &tc.ctx,
            tc.review_memory_at_end,
            Some(tc.session_id.as_str()),
        );
        crate::hooks::turn_end_plugin_hooks(
            agent,
            tc.ctx.get_messages(),
            true,
            false,
            tc.total_turns,
            tc.session_started_hooks_fired,
        );
        tc.replay.record(
            "session_end",
            serde_json::json!({
                "reason": "finished_naturally",
                "total_turns": tc.total_turns,
                "session_cost_usd": tc.session_cost_usd,
                "continuation_trigger_count": tc.continuation_trigger_count,
                "ack_trigger_count": tc.ack_trigger_count,
                "premature_finalize_suspected_count": tc.premature_finalize_suspected_count,
            }),
        );
        if tc
            .stream_mute
            .as_ref()
            .is_some_and(|m| m.swap(false, Ordering::AcqRel))
        {
            crate::conversation_loop::emit_stream_chunk(
                Some(tc.stream_chunk_sink.as_ref()),
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
        return TurnState::Done(Ok(agent.seal_loop_result(
            &tc.ctx,
            tc.persist_user_idx,
            tc.prefill_range.clone(),
            LoopExit::base(
                "text_response",
                tc.api_call_count,
                false,
                false,
                true,
                false,
            ),
            tc.total_turns,
            std::mem::take(&mut tc.tool_errors),
            tc.accumulated_usage.take(),
            tc.session_cost_usd,
            tc.session_started_hooks_fired,
        )));
    }

    // Tool calls present — store for next state
    tc.codex_ack_continuations = 0;
    tc.tool_calls_to_execute = Some(tool_calls);
    tc.assistant_msg = Some(assistant_msg);
    tc.response = Some(response);
    tc.turn_runtime_route = turn_runtime_route;

    TurnState::ExecuteTools
}

// ---------------------------------------------------------------------------
// ExecuteTools — execute tool calls in parallel, handle web budget, rollback
// ---------------------------------------------------------------------------

async fn turn_execute_tools(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    let mut tool_calls = tc
        .tool_calls_to_execute
        .take()
        .expect("tool calls available");
    let assistant_msg = tc.assistant_msg.take().expect("assistant msg available");
    let _response = tc.response.take().expect("response available");
    let _turn_runtime_route = tc.turn_runtime_route.take();
    let turn_governor_runtime = governor_runtime_state(
        &tc.governor_llm_latency_window,
        &tc.governor_tool_error_window,
        tc.governor_consecutive_error_turns,
    );

    // Deduplicate tool calls
    tool_calls = crate::tool_executor::deduplicate_tool_calls(&tool_calls);
    for tc_ in &mut tool_calls {
        crate::tool_executor::repair_tool_call(agent, tc_);
        crate::tool_executor::hydrate_session_search_args(agent, tc_);
    }
    let mut guarded_tool_results = agent.guard_session_search_without_query(&mut tool_calls);
    if !guarded_tool_results.is_empty() {
        tracing::info!(
            blocked = guarded_tool_results.len(),
            "session_search query guard blocked tool call(s)"
        );
    }

    if let Some(note) =
        apply_repo_review_tool_profile_narrowing(&mut tool_calls, tc.ctx.get_messages())
    {
        hooks::emit_status(
            agent,
            "lifecycle",
            "Applied repo-review tool profile narrowing.",
        );
        tc.ctx.add_message(Message::system(note));
    }
    if let Some(note) = apply_repo_review_discovery_budget_policy(
        &mut tool_calls,
        tc.ctx.get_messages(),
        &mut tc.repo_review_budget_state,
    ) {
        hooks::emit_status(
            agent,
            "lifecycle",
            "Applied repo-review discovery budget policy.",
        );
        tc.ctx.add_message(Message::system(note));
    }

    if tool_calls.is_empty() {
        if !guarded_tool_results.is_empty() {
            for result in guarded_tool_results.drain(..) {
                tc.ctx
                    .add_message(Message::tool_result(&result.tool_call_id, &result.content));
            }
            return TurnState::CallLlm;
        }
        tc.ctx.add_message(Message::system(
            "[SYSTEM] Tool profile/budget policy filtered this turn's calls. Propose refined, scoped code-inspection calls next.",
        ));
        return TurnState::CallLlm;
    }

    let all_housekeeping = tool_calls.iter().all(|tc_| {
        matches!(
            tc_.function.name.as_str(),
            "memory" | "todo" | "skill_manage" | "session_search"
        )
    });
    let should_mute_post =
        all_housekeeping && AgentLoop::assistant_visible_text_after_think_blocks(&assistant_msg);
    let was_muted = tc
        .stream_mute
        .as_ref()
        .map(|m| m.swap(should_mute_post, Ordering::AcqRel))
        .unwrap_or(false);
    if was_muted != should_mute_post {
        crate::conversation_loop::emit_stream_chunk(
            Some(tc.stream_chunk_sink.as_ref()),
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

    // Invalid tool call detection
    let invalid_tool_calls: Vec<String> = tool_calls
        .iter()
        .filter(|tc_| agent.tool_registry.get(&tc_.function.name).is_none())
        .map(|tc_| tc_.function.name.clone())
        .collect();
    if !invalid_tool_calls.is_empty() {
        tc.invalid_tool_retries = tc.invalid_tool_retries.saturating_add(1);
        hooks::emit_status(
            agent,
            "lifecycle",
            &format!(
                "Invalid tool call detected - retrying ({}/{})",
                tc.invalid_tool_retries,
                agent.config().invalid_tool_call_max_retries
            ),
        );
        let available = agent.tool_registry.names().join(", ");
        if tc.invalid_tool_retries >= agent.config().invalid_tool_call_max_retries {
            hooks::emit_status(
                agent,
                "lifecycle",
                &format!(
                    "Max invalid tool retries reached ({})",
                    agent.config().invalid_tool_call_max_retries
                ),
            );
            tc.ctx.add_message(Message::system(format!(
                "Max invalid tool retries reached ({}). Last invalid tool: {}",
                agent.config().invalid_tool_call_max_retries,
                invalid_tool_calls[0]
            )));
            crate::hooks::turn_end_plugin_hooks(
                agent,
                tc.ctx.get_messages(),
                false,
                false,
                tc.total_turns,
                tc.session_started_hooks_fired,
            );
            return TurnState::Done(Ok(agent.seal_loop_result(
                &tc.ctx,
                tc.persist_user_idx,
                tc.prefill_range.clone(),
                LoopExit::base(
                    "invalid_tool_calls",
                    tc.api_call_count,
                    false,
                    true,
                    false,
                    false,
                ),
                tc.total_turns,
                std::mem::take(&mut tc.tool_errors),
                tc.accumulated_usage.take(),
                tc.session_cost_usd,
                tc.session_started_hooks_fired,
            )));
        }
        for tc_ in &tool_calls {
            let content = if agent.tool_registry.get(&tc_.function.name).is_none() {
                format!(
                    "Tool '{}' does not exist. Available tools: {}",
                    tc_.function.name, available
                )
            } else {
                "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
            };
            tc.ctx
                .add_message(Message::tool_result(tc_.id.clone(), content));
        }
        return TurnState::CallLlm;
    }
    tc.invalid_tool_retries = 0;

    // Invalid JSON arguments
    let mut invalid_json_args: Vec<(String, String)> = Vec::new();
    for tc_ in &mut tool_calls {
        if let Err(e) = crate::agent_runtime_helpers::normalize_tool_call_arguments(tc_) {
            invalid_json_args.push((tc_.function.name.clone(), e));
        }
    }
    if !invalid_json_args.is_empty() {
        tc.invalid_json_retries = tc.invalid_json_retries.saturating_add(1);
        if tc.invalid_json_retries < agent.config().invalid_tool_json_max_retries {
            hooks::emit_status(
                agent,
                "lifecycle",
                &format!(
                    "Invalid tool JSON arguments - retrying ({}/{})",
                    tc.invalid_json_retries,
                    agent.config().invalid_tool_json_max_retries
                ),
            );
            let _ = tc.ctx.get_messages_mut().pop();
            return TurnState::CallLlm;
        }
        hooks::emit_status(
            agent,
            "lifecycle",
            &format!(
                "Max invalid JSON retries reached ({}); returning tool errors",
                agent.config().invalid_tool_json_max_retries
            ),
        );
        tc.invalid_json_retries = 0;
        for tc_ in &tool_calls {
            let content = if let Some((_, err)) = invalid_json_args
                .iter()
                .find(|(name, _)| name == &tc_.function.name)
            {
                format!(
                    "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                    err
                )
            } else {
                "Skipped: other tool call in this response had invalid JSON.".to_string()
            };
            tc.ctx
                .add_message(Message::tool_result(tc_.id.clone(), content));
        }
        return TurnState::CallLlm;
    }
    tc.invalid_json_retries = 0;

    for tc_ in &tool_calls {
        if let Ok(mut state) = agent.state.lock() {
            match tc_.function.name.as_str() {
                "memory" => state.evolution_counters.turns_since_memory = 0,
                "skill_manage" => state.evolution_counters.iters_since_skill = 0,
                _ => {}
            }
        }
    }

    // Cap concurrent delegate_task calls
    crate::tool_executor::cap_delegates(agent, &mut tool_calls);

    let equity_deferred_results = tc.equity_research_gate.gate_tool_calls(&mut tool_calls);
    if !equity_deferred_results.is_empty() {
        tracing::info!(
            blocked = equity_deferred_results.len(),
            "equity research gate deferred web tool call(s)"
        );
    }

    // Web research
    let deferred_web_budget_results = if let Some(ref mut ctrl) = tc.web_research_ctrl {
        ctrl.ensure_plan_on_first_web(tc.web_auxiliary.as_ref(), &tc.first_user, &tool_calls)
            .await;
        let (blocked, notices) = ctrl
            .gate_web_batch(
                tc.web_auxiliary.as_ref(),
                tc.ctx.get_messages(),
                &mut tool_calls,
                tc.total_turns,
            )
            .await;
        for notice in notices {
            hooks::emit_status(agent, "tool_failure", &notice);
        }
        blocked
    } else {
        let blocked = apply_web_tool_budget(
            &mut tool_calls,
            tc.web_tool_calls_used,
            tc.web_search_calls_used,
            tc.web_tool_consecutive_error_turns,
            tc.total_turns,
        );
        if !blocked.is_empty() {
            let blocked_by_errors =
                tc.web_tool_consecutive_error_turns >= web_tool_budget_max_consecutive_errors();
            for (tool_name, _) in &blocked {
                hooks::emit_status(
                    agent,
                    "tool_failure",
                    &web_tool_budget_user_notice(tool_name, blocked_by_errors),
                );
            }
        }
        blocked
    };

    let contextlattice_connect_intent = detect_contextlattice_connect_intent(tc.ctx.get_messages());
    if tool_calls.is_empty() {
        for result in equity_deferred_results {
            tc.ctx
                .add_message(Message::tool_result(&result.tool_call_id, &result.content));
        }
        for (_, result) in deferred_web_budget_results {
            tc.ctx
                .add_message(Message::tool_result(&result.tool_call_id, &result.content));
        }
        return TurnState::CallLlm;
    }

    // Pre-parse tool args once; reused by hooks, guardrails, and file mutation.
    let tool_args: Vec<Value> = tool_calls
        .iter()
        .map(|tc_| serde_json::from_str(&tc_.function.arguments).unwrap_or(Value::Null))
        .collect();

    for (tc_, args) in tool_calls.iter().zip(tool_args.iter()) {
        let tc_ctx = serde_json::json!({"tool": &tc_.function.name, "turn": tc.total_turns});
        hooks::invoke_hook(agent, HookType::PreToolCall, &tc_ctx);
        if let Some(ref cb) = agent.callbacks.on_tool_start {
            cb(&tc_.function.name, args);
        }
    }

    // Interrupt check before tool execution
    if agent.interrupt.take_interrupt_graceful().is_some() {
        return TurnState::Done(Ok(agent.graceful_interrupt_result(
            &tc.ctx,
            tc.total_turns,
            std::mem::take(&mut tc.tool_errors),
            tc.accumulated_usage.take(),
            tc.session_cost_usd,
            tc.session_started_hooks_fired,
            tc.persist_user_idx,
            tc.prefill_range.clone(),
            tc.api_call_count,
        )));
    }

    let _tool_start = Instant::now();
    let _tool_governor = governor_for_turn(
        &agent.config(),
        &tc.ctx,
        tool_calls.len(),
        Some(&turn_governor_runtime),
    );
    let _parent_budget_remaining_usd = agent
        .config()
        .max_cost_usd
        .map(|limit| (limit - tc.session_cost_usd).max(0.0));

    // Tool guardrails (reuses pre-parsed tool_args)
    for (tc_, args) in tool_calls.iter().zip(tool_args.iter()) {
        match tc.tool_guardrails.before_call(&tc_.function.name, args) {
            crate::tool_guardrails::GuardrailDecision::Halt(reason) => {
                tc.ctx.add_message(Message::assistant(format!(
                    "[Tool guardrail halt] {reason}"
                )));
                return TurnState::Done(Ok(agent.seal_loop_result(
                    &tc.ctx,
                    tc.persist_user_idx,
                    tc.prefill_range.clone(),
                    LoopExit::base(
                        "guardrail_halt",
                        tc.api_call_count,
                        false,
                        false,
                        true,
                        false,
                    ),
                    tc.total_turns,
                    std::mem::take(&mut tc.tool_errors),
                    tc.accumulated_usage.take(),
                    tc.session_cost_usd,
                    tc.session_started_hooks_fired,
                )));
            }
            crate::tool_guardrails::GuardrailDecision::Block(reason) => {
                tracing::warn!(tool = %tc_.function.name, %reason, "tool guardrail block");
            }
            crate::tool_guardrails::GuardrailDecision::Allow => {}
        }
    }

    let tool_start = Instant::now();
    let tool_progress_names: Vec<String> = tool_calls
        .iter()
        .map(|tc_| tc_.function.name.clone())
        .collect();
    let _tool_progress = ToolProgressWatchdog::start(
        agent.callbacks.status_callback.clone(),
        tc.total_turns,
        tool_progress_names,
    );
    let mut results = agent
        .execute_tool_calls(
            &tool_calls,
            tc.total_turns,
            governor_for_turn(
                &agent.config(),
                &tc.ctx,
                tool_calls.len(),
                Some(&turn_governor_runtime),
            )
            .tool_concurrency,
            contextlattice_connect_intent,
            agent
                .config()
                .max_cost_usd
                .map(|limit| (limit - tc.session_cost_usd).max(0.0)),
            &mut tc.tool_errors,
            Some(&mut tc.checkpoint_mgr),
            latest_user_content(tc.ctx.get_messages()).map(str::to_string),
        )
        .await;

    if let Some(ref mut ctrl) = tc.web_research_ctrl {
        if ctrl.record_results(&tool_calls, &results) {
            tc.active_tool_schemas = Arc::from(ctrl.filter_tool_schemas(tc.tool_schemas.as_ref()));
        }
    }
    tc.equity_research_gate
        .record_tool_batch(&tool_calls, &results);
    if !equity_deferred_results.is_empty() {
        results.extend(equity_deferred_results);
    }
    if !deferred_web_budget_results.is_empty() {
        results.extend(
            deferred_web_budget_results
                .into_iter()
                .map(|(_, result)| result),
        );
    }
    if !guarded_tool_results.is_empty() {
        results.extend(guarded_tool_results);
    }

    let tool_elapsed = tool_start.elapsed().as_millis() as u64;
    let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;

    let mut web_turn_calls: u32 = 0;
    let mut web_turn_errors: u32 = 0;
    for tc_ in &tool_calls {
        if !is_budgeted_web_tool(&tc_.function.name) {
            continue;
        }
        web_turn_calls = web_turn_calls.saturating_add(1);
        if results
            .iter()
            .any(|r| r.tool_call_id == tc_.id && r.is_error)
        {
            web_turn_errors = web_turn_errors.saturating_add(1);
        }
    }
    if web_turn_calls > 0 {
        tc.web_tool_calls_used = tc.web_tool_calls_used.saturating_add(web_turn_calls);
        if web_turn_errors == web_turn_calls {
            tc.web_tool_consecutive_error_turns =
                tc.web_tool_consecutive_error_turns.saturating_add(1);
        } else {
            tc.web_tool_consecutive_error_turns = 0;
        }
    }
    for tc_ in &tool_calls {
        if tc_.function.name == "web_search" {
            tc.web_search_calls_used = tc.web_search_calls_used.saturating_add(1);
        }
    }

    tracing::info!(
        turn = tc.total_turns,
        tool_count = tool_calls.len(),
        result_count = results.len(),
        errors = turn_tool_error_count,
        elapsed_ms = tool_elapsed,
        streaming = true,
        "agent tool batch finished"
    );
    if std::env::var("HERMES_TURN_PERF").map_or(false, |v| !v.is_empty()) {
        let tool_names: Vec<&str> = tool_calls
            .iter()
            .map(|t| t.function.name.as_str())
            .collect();
        tracing::info!(
            target: "turn_perf",
            turn = tc.total_turns,
            tools = ?tool_names,
            tool_elapsed_ms = tool_elapsed,
            errors = turn_tool_error_count,
            "tool batch timing"
        );
    }
    hooks::emit_tool_failure_notices(agent, &tool_calls, &results);

    let turn_tool_error_rate = if results.is_empty() {
        0.0
    } else {
        turn_tool_error_count as f64 / results.len() as f64
    };
    push_window_f64(
        &mut tc.governor_tool_error_window,
        turn_tool_error_rate,
        tc.governor_window_limit,
    );
    if turn_tool_error_count > 0 {
        tc.governor_consecutive_error_turns = tc.governor_consecutive_error_turns.saturating_add(1);
    } else {
        tc.governor_consecutive_error_turns = 0;
    }

    tc.replay.record(
        "tool_batch",
        serde_json::json!({
            "turn": tc.total_turns,
            "tool_count": tool_calls.len(),
            "tool_concurrency": governor_for_turn(
                &agent.config(),
                &tc.ctx,
                tool_calls.len(),
                Some(&turn_governor_runtime),
            )
            .tool_concurrency,
            "errors": turn_tool_error_count,
            "error_rate": turn_tool_error_rate,
        }),
    );

    update_repo_review_budget_state_from_results(
        &mut tc.repo_review_budget_state,
        tc.ctx.get_messages(),
        &results,
    );

    // Checkpoint rollback
    if agent.config().rollback_on_tool_error_threshold > 0
        && turn_tool_error_count >= agent.config().rollback_on_tool_error_threshold
    {
        if let Some(snapshot) = tc.last_checkpoint_messages.clone() {
            *tc.ctx.get_messages_mut() = snapshot;
            let _ = tc.checkpoint_mgr.restore_latest();
            tc.ctx.add_message(Message::system(format!(
                "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                turn_tool_error_count
            )));
            return TurnState::CallLlm;
        }
    }

    // Store results for PostTool
    tc.tool_calls = Some(tool_calls);
    tc.tool_results = Some(results);
    tc._turn_tool_error_count = turn_tool_error_count;
    tc.tool_elapsed = tool_elapsed;
    tc.turn_governor_runtime = Some(turn_governor_runtime);

    TurnState::PostTool
}

// ---------------------------------------------------------------------------
// PostTool — post-tool processing, budget enforcement, context pressure,
//            tool loop guard, compression
// ---------------------------------------------------------------------------

async fn turn_post_tool(agent: &AgentLoop, tc: &mut TurnContext) -> TurnState {
    let tool_calls = tc.tool_calls.take().expect("tool calls available");
    let mut results = tc.tool_results.take().expect("tool results available");
    let turn_tool_error_count = tc._turn_tool_error_count;
    let _tool_elapsed = tc.tool_elapsed;
    let _turn_governor_runtime = tc.turn_governor_runtime.take().unwrap_or_else(|| {
        governor_runtime_state(
            &tc.governor_llm_latency_window,
            &tc.governor_tool_error_window,
            tc.governor_consecutive_error_turns,
        )
    });

    // Post-tool hooks
    for res in &results {
        let Some(tc_) = tool_calls.iter().find(|tc_| tc_.id == res.tool_call_id) else {
            continue;
        };
        let tc_ctx = serde_json::json!({"tool": &tc_.function.name, "is_error": res.is_error, "turn": tc.total_turns});
        hooks::invoke_hook(agent, HookType::PostToolCall, &tc_ctx);
        if let Some(ref cb) = agent.callbacks.on_tool_complete {
            cb(&tc_.function.name, &res.content);
        }
    }

    for (tc_, res) in tool_calls.iter().zip(results.iter()) {
        let args: Value = serde_json::from_str(&tc_.function.arguments).unwrap_or(Value::Null);
        tc.tool_guardrails
            .after_call(&tc_.function.name, res.is_error, &res.content);
        tc.file_mutation
            .record_tool_result(&tc_.function.name, &args, &res.content, res.is_error);
    }

    crate::tool_executor::notify_memory_writes(agent, &tool_calls, &results);
    crate::tool_executor::notify_delegations(agent, &tool_calls, &results);

    // Enforce budget
    budget::enforce_budget(&mut results, &agent.config().budget);

    if !results.is_empty() {
        let w = budget_pressure_text(
            tc.total_turns,
            agent.config().max_turns,
            agent.config().budget_caution_threshold,
            agent.config().budget_warning_threshold,
            agent.config().budget_pressure_enabled,
        );
        if let Some(ref text) = w {
            tracing::info!("{}", text);
        }
        inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
    }
    let lsp_note = agent.lsp_context_note(&tool_calls, &results);

    let execute_code_refund = !tool_calls.is_empty()
        && tool_calls
            .iter()
            .all(|tc_| tc_.function.name == "execute_code")
        && !results.iter().any(|r| r.is_error);

    let num_tool_msgs = results.len();
    for result in results {
        tc.replay.record(
            "tool_result",
            serde_json::json!({
                "turn": tc.total_turns,
                "tool_call_id": result.tool_call_id,
                "is_error": result.is_error,
                "content_preview": result.content.chars().take(240).collect::<String>(),
            }),
        );
        tc.ctx
            .add_message(Message::tool_result(&result.tool_call_id, &result.content));
    }
    agent
        .pending_steer
        .apply_to_tool_results(tc.ctx.get_messages_mut(), num_tool_msgs);
    if let Some(note) = lsp_note {
        tc.ctx.add_message(Message::system(note));
    }

    // Tool loop guard
    if should_trip_tool_loop_guard(
        tc.governor_consecutive_error_turns,
        tool_calls.len(),
        turn_tool_error_count,
    ) {
        let guard_message = format!(
            "Tool-loop guard tripped after {} consecutive error turn(s); latest turn failed {}/{} tool call(s).",
            tc.governor_consecutive_error_turns,
            turn_tool_error_count,
            tool_calls.len()
        );
        hooks::emit_status(agent, "lifecycle", &guard_message);
        tc.replay.record(
            "tool_loop_guard",
            serde_json::json!({
                "turn": tc.total_turns,
                "consecutive_error_turns": tc.governor_consecutive_error_turns,
                "failed_calls": turn_tool_error_count,
                "total_calls": tool_calls.len(),
            }),
        );
        match agent
            .handle_tool_loop_guard_summary(
                &mut tc.ctx,
                tc.governor_consecutive_error_turns,
                turn_tool_error_count,
                tool_calls.len(),
            )
            .await
        {
            Ok(Some(summary)) => {
                tc.ctx.add_message(summary);
            }
            Ok(None) => {}
            Err(e) => return TurnState::Done(Err(e.into())),
        }
        if tc
            .stream_mute
            .as_ref()
            .is_some_and(|m| m.swap(false, Ordering::AcqRel))
        {
            crate::conversation_loop::emit_stream_chunk(
                Some(tc.stream_chunk_sink.as_ref()),
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
        crate::hooks::turn_end_plugin_hooks(
            agent,
            tc.ctx.get_messages(),
            false,
            false,
            tc.total_turns,
            tc.session_started_hooks_fired,
        );
        return TurnState::Done(Ok(agent.enrich_turn_telemetry(
            agent.seal_loop_result(
                &tc.ctx,
                tc.persist_user_idx,
                tc.prefill_range.clone(),
                LoopExit::base(
                    "tool_loop_guard",
                    tc.api_call_count,
                    false,
                    false,
                    false,
                    false,
                ),
                tc.total_turns,
                std::mem::take(&mut tc.tool_errors),
                tc.accumulated_usage.take(),
                tc.session_cost_usd,
                tc.session_started_hooks_fired,
            ),
            Some(&tc.tool_guardrails),
        )));
    }

    // execute_code refund
    if execute_code_refund {
        tc.iteration_budget.refund(1);
        tc.total_turns = tc.total_turns.saturating_sub(1);
    }

    // Stream break
    if let Some(brk) = tc.stream_needs_break.as_ref() {
        brk.store(true, Ordering::Release);
    }
    crate::conversation_loop::emit_stream_chunk(
        Some(tc.stream_chunk_sink.as_ref()),
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
    agent.emit_background_review_metrics(tc.total_turns, &tc.ctx);

    // Context pressure
    let total_chars = tc.ctx.total_chars();
    let threshold = ((tc.ctx.max_context_chars().max(1) as f64) * 0.8) as usize;
    if threshold > 0 {
        let progress = total_chars as f64 / threshold as f64;
        let tier = if progress >= 0.95 {
            0.95
        } else if progress >= 0.85 {
            0.85
        } else {
            0.0
        };
        if AgentLoop::should_emit_context_pressure_warning(
            progress,
            tier,
            &mut tc.context_pressure_warned_at,
            &mut tc.context_pressure_last_warn_at,
            &mut tc.context_pressure_last_warn_percent,
        ) {
            tracing::warn!(
                "Context pressure {:.0}% of compaction threshold ({} / {})",
                progress * 100.0,
                total_chars,
                threshold
            );
        }
    }

    agent.auto_compress_if_over_threshold(&mut tc.ctx).await;

    TurnState::Guard
}

// ---------------------------------------------------------------------------
// Transition table (Phase A — compile-time documentation + test coverage)
// ---------------------------------------------------------------------------

/// State identifier without the `Done` payload — used in the transition table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum TurnStateId {
    Guard,
    Prefetch,
    RouteSelection,
    CallLlm,
    ProcessLlmOutput,
    ExecuteTools,
    PostTool,
    Done,
}

impl TurnStateId {
    pub(crate) fn from_state(s: &TurnState) -> Self {
        match s {
            TurnState::Guard => Self::Guard,
            TurnState::Prefetch => Self::Prefetch,
            TurnState::RouteSelection => Self::RouteSelection,
            TurnState::CallLlm => Self::CallLlm,
            TurnState::ProcessLlmOutput => Self::ProcessLlmOutput,
            TurnState::ExecuteTools => Self::ExecuteTools,
            TurnState::PostTool => Self::PostTool,
            TurnState::Done(_) => Self::Done,
        }
    }
}

/// Exhaustive list of all legal (from, to) transitions observed in the state
/// functions above.  A unit test below asserts that every reachable pair is
/// covered here.
pub(crate) const VALID_TRANSITIONS: &[(TurnStateId, TurnStateId)] = &[
    // Guard exits
    (TurnStateId::Guard, TurnStateId::Prefetch),
    (TurnStateId::Guard, TurnStateId::Done),
    // Prefetch exits
    (TurnStateId::Prefetch, TurnStateId::RouteSelection),
    (TurnStateId::Prefetch, TurnStateId::Done),
    // RouteSelection exits
    (TurnStateId::RouteSelection, TurnStateId::CallLlm),
    (TurnStateId::RouteSelection, TurnStateId::Done),
    // CallLlm exits
    (TurnStateId::CallLlm, TurnStateId::ProcessLlmOutput),
    (TurnStateId::CallLlm, TurnStateId::Done),
    // ProcessLlmOutput exits
    (TurnStateId::ProcessLlmOutput, TurnStateId::ExecuteTools),
    (TurnStateId::ProcessLlmOutput, TurnStateId::CallLlm),
    (TurnStateId::ProcessLlmOutput, TurnStateId::Done),
    // ExecuteTools exits
    (TurnStateId::ExecuteTools, TurnStateId::PostTool),
    (TurnStateId::ExecuteTools, TurnStateId::CallLlm),
    (TurnStateId::ExecuteTools, TurnStateId::Done),
    // PostTool exits
    (TurnStateId::PostTool, TurnStateId::Guard),
    (TurnStateId::PostTool, TurnStateId::CallLlm),
    (TurnStateId::PostTool, TurnStateId::Done),
    // Done is a sink
    (TurnStateId::Done, TurnStateId::Done),
];

pub(crate) fn is_valid_transition(from: TurnStateId, to: TurnStateId) -> bool {
    VALID_TRANSITIONS.iter().any(|&(f, t)| f == from && t == to)
}

#[cfg(test)]
mod transitions_tests {
    use super::*;

    #[test]
    fn transition_table_is_exhaustive_for_all_states() {
        let all_states = [
            TurnStateId::Guard,
            TurnStateId::Prefetch,
            TurnStateId::RouteSelection,
            TurnStateId::CallLlm,
            TurnStateId::ProcessLlmOutput,
            TurnStateId::ExecuteTools,
            TurnStateId::PostTool,
            TurnStateId::Done,
        ];
        for state in all_states {
            let has_exit = VALID_TRANSITIONS.iter().any(|&(from, _)| from == state);
            assert!(
                has_exit,
                "State {state:?} has no transitions in VALID_TRANSITIONS"
            );
        }
    }

    #[test]
    fn guard_can_reach_prefetch_and_done() {
        assert!(is_valid_transition(
            TurnStateId::Guard,
            TurnStateId::Prefetch
        ));
        assert!(is_valid_transition(TurnStateId::Guard, TurnStateId::Done));
        assert!(!is_valid_transition(
            TurnStateId::Guard,
            TurnStateId::CallLlm
        ));
    }

    #[test]
    fn post_tool_loops_back_to_guard() {
        assert!(is_valid_transition(
            TurnStateId::PostTool,
            TurnStateId::Guard
        ));
    }

    #[test]
    fn done_is_a_sink_state() {
        assert!(is_valid_transition(TurnStateId::Done, TurnStateId::Done));
        assert!(!is_valid_transition(TurnStateId::Done, TurnStateId::Guard));
    }
}
