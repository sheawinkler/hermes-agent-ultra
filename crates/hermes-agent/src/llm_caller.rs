//! LLM interaction with providers — message building, streaming collection,
//! finish reason handling, and finalization signals.
//!
//! Extracted from `impl AgentLoop` in `agent_loop.rs` to reduce the God struct.
//! All functions take `agent: &AgentLoop` instead of `&self`.

use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use serde_json::Value;

use hermes_core::{
    AgentError, LlmResponse, Message, StreamChunk, ToolCall, ToolSchema, UsageStats,
};

use crate::agent_config::FinalizationSignals;
use crate::agent_config::{is_stream_not_supported_error, is_transient_stream_error};
use crate::agent_loop::{AgentLoop, ApiMode, StreamCollectOutcome, TurnRuntimeRoute};
use crate::agent_runtime_helpers;
use crate::context::ContextManager;
use crate::message_sanitization::should_treat_stop_as_truncated;
use crate::message_sanitization::{
    build_partial_stream_stub_response, format_partial_stream_tool_call_warning,
    partial_stream_dropped_tool_names, partial_stream_tool_calls_in_flight,
};

// ---------------------------------------------------------------------------
// Message building
// ---------------------------------------------------------------------------

pub(crate) fn build_turn_api_messages(
    agent: &AgentLoop,
    ctx: &mut ContextManager,
) -> Arc<[Message]> {
    let _span = tracing::debug_span!(
        "build_turn_api_messages",
        msg_count = ctx.len(),
        total_chars = ctx.total_chars(),
    )
    .entered();
    prepare_ctx_for_api_call(agent, ctx);
    let key = api_messages_cache_key(agent, ctx);
    if let Ok(state) = agent.state.lock() {
        if let Some((cached_key, arc)) = state.turn_api_messages_cache.as_ref() {
            if *cached_key == key {
                // tracing::debug!(cache = "hit", msg_count = arc.len());
                return Arc::clone(arc);
            }
        }
    }
    // tracing::debug!(cache = "miss");

    let cfg = agent.config();
    let prefetch = agent
        .state
        .lock()
        .map(|state| state.turn_ext_prefetch_cache.clone())
        .unwrap_or_default();
    let ephemeral = cfg
        .ephemeral_system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let force_strip_images = !agent
        .vision_supported
        .load(std::sync::atomic::Ordering::Acquire);
    let provider = cfg.provider.as_deref().unwrap_or("");
    let base_url = crate::runtime_provider::resolve_runtime_base_url(agent, provider, None)
        .unwrap_or_default();
    let messages = crate::api_messages::assemble_api_messages_from_ctx(
        ctx.get_messages(),
        &prefetch,
        ephemeral,
        crate::runtime_provider::active_model(agent).as_str(),
        cfg.cache_ttl.as_str(),
        agent
            .use_prompt_caching
            .load(std::sync::atomic::Ordering::Relaxed),
        agent
            .use_native_cache_layout
            .load(std::sync::atomic::Ordering::Relaxed),
        force_strip_images,
    );
    let messages = agent_runtime_helpers::prepare_wire_messages_for_api(
        messages,
        provider,
        crate::runtime_provider::active_model(agent).as_str(),
        base_url.as_str(),
    );
    let arc: Arc<[Message]> = messages.into();

    if let Ok(mut state) = agent.state.lock() {
        state.turn_api_messages_cache = Some((key, Arc::clone(&arc)));
    }
    arc
}

fn api_messages_cache_key(
    agent: &AgentLoop,
    ctx: &ContextManager,
) -> crate::api_messages::ApiMessagesCacheKey {
    let prefetch = agent
        .state
        .lock()
        .map(|state| state.turn_ext_prefetch_cache.clone())
        .unwrap_or_default();
    let cfg = agent.config();
    crate::api_messages::ApiMessagesCacheKey {
        message_count: ctx.len(),
        total_chars: ctx.total_chars(),
        prefetch_len: prefetch.len(),
        prefetch_hash: crate::api_messages::hash_str(&prefetch),
        ephemeral_len: cfg
            .ephemeral_system_prompt
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::len)
            .unwrap_or(0),
        model_hash: crate::api_messages::hash_str(&crate::runtime_provider::active_model(agent)),
        use_prompt_caching: agent
            .use_prompt_caching
            .load(std::sync::atomic::Ordering::Relaxed),
        use_native_cache_layout: agent
            .use_native_cache_layout
            .load(std::sync::atomic::Ordering::Relaxed),
        cache_ttl_hash: crate::api_messages::hash_str(&cfg.cache_ttl),
    }
}

fn prepare_ctx_for_api_call(agent: &AgentLoop, ctx: &mut ContextManager) {
    let cfg = agent.config();
    let provider = cfg.provider.as_deref().unwrap_or("");
    let base_url = crate::runtime_provider::resolve_runtime_base_url(agent, provider, None)
        .unwrap_or_default();
    let api_mode = api_mode_as_hook_str(&cfg.api_mode);
    crate::runtime_provider::refresh_prompt_cache_policy(agent, provider, &base_url, api_mode);
    let session_id = cfg.session_id.as_deref();
    let (tool_repairs, seq_repairs) =
        agent_runtime_helpers::prepare_live_history_for_api(ctx.get_messages_mut(), session_id);
    if tool_repairs > 0 || seq_repairs > 0 {
        tracing::debug!(
            tool_call_arg_repairs = tool_repairs,
            message_sequence_repairs = seq_repairs,
            "pre-API live history repairs"
        );
        agent.invalidate_turn_api_messages_cache();
    }
    agent
        .pending_steer
        .drain_pre_api_into_messages(ctx.get_messages_mut());
    agent.interest_sync_user_messages(ctx.get_messages());
}

pub(crate) fn build_api_messages_legacy(
    agent: &AgentLoop,
    ctx: &mut ContextManager,
) -> Vec<Message> {
    prepare_ctx_for_api_call(agent, ctx);
    let mut messages = ctx.get_messages().to_vec();
    let prefetch = agent
        .state
        .lock()
        .map(|state| state.turn_ext_prefetch_cache.clone())
        .unwrap_or_default();
    crate::api_messages::apply_prefetch_to_last_user(&mut messages, &prefetch);
    if let Some(ephemeral) = agent
        .config()
        .ephemeral_system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        messages.push(Message::system(ephemeral));
    }
    let cfg = agent.config();
    if !messages.is_empty()
        && agent
            .use_prompt_caching
            .load(std::sync::atomic::Ordering::Relaxed)
    {
        crate::prompt_caching::apply_anthropic_cache_control_in_place(
            &mut messages,
            cfg.cache_ttl.as_str(),
            agent
                .use_native_cache_layout
                .load(std::sync::atomic::Ordering::Relaxed),
        );
    }
    crate::vision_message_prepare::strip_images_for_non_vision_model(
        &messages,
        crate::runtime_provider::active_model(agent).as_str(),
    )
}

/// Golden harness entry for `messages_for_api_call` (zero-copy migration oracle).
#[doc(hidden)]
pub fn oracle_messages_for_api_call(agent: &AgentLoop, ctx: &mut ContextManager) -> Vec<Message> {
    build_api_messages_legacy(agent, ctx)
}

#[doc(hidden)]
pub fn oracle_candidate_messages_for_api_call(
    agent: &AgentLoop,
    ctx: &mut ContextManager,
) -> Vec<Message> {
    build_turn_api_messages(agent, ctx).to_vec()
}

/// Set turn-scoped memory prefetch injected at API-call time (test harness only).
#[doc(hidden)]
pub fn oracle_set_turn_ext_prefetch_cache(agent: &AgentLoop, prefetch: impl Into<String>) {
    agent.set_turn_ext_prefetch_cache(prefetch.into());
}

// ---------------------------------------------------------------------------
// Streaming helpers
// ---------------------------------------------------------------------------

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

fn tool_call_arguments_look_truncated(tc: &ToolCall) -> bool {
    let trimmed = tc.function.arguments.trim();
    !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_err()
}

pub(crate) fn upgrade_finish_reason_for_truncated_tool_args(response: &mut LlmResponse) {
    let truncated = response
        .message
        .tool_calls
        .as_ref()
        .map(|calls| calls.iter().any(tool_call_arguments_look_truncated))
        .unwrap_or(false);
    if truncated {
        response.finish_reason = Some("length".to_string());
    }
}

/// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
pub(crate) async fn collect_stream_llm_response(
    agent: &AgentLoop,
    ctx: &mut ContextManager,
    tool_schemas: &[ToolSchema],
    route: Option<&TurnRuntimeRoute>,
    active_model: &str,
    max_tokens_override: Option<u32>,
    on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    api_call_count: &mut u32,
    mut stream_scrubber: Option<&mut crate::stream_scrubber::ThinkBlockScrubber>,
) -> Result<StreamCollectOutcome, AgentError> {
    let api_messages = build_turn_api_messages(agent, ctx);
    let (_, active_model_name) =
        crate::route_learning::extract_provider_and_model(agent, active_model);
    let (active_provider, _) =
        crate::route_learning::extract_provider_and_model(agent, active_model);
    let default_api_mode = crate::route_learning::primary_runtime_snapshot(agent)
        .api_mode
        .clone();
    let default_extra_body = extra_body_for_api_mode(agent, &default_api_mode);
    let effective_max_tokens = max_tokens_override.or(agent.config().max_tokens);
    let max_stream_retries = std::env::var("HERMES_STREAM_RETRIES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .map(|v| v.min(10))
        .unwrap_or(agent.config().stream_read_max_retries.min(10));

    let mut recovered_stream_text = String::new();

    'stream_attempt: for stream_attempt in 0..=max_stream_retries {
        *api_call_count = api_call_count.saturating_add(1);
        let hook_api_mode = route
            .and_then(|rt| rt.api_mode.as_ref())
            .unwrap_or(&default_api_mode);
        let hook_base_url = crate::runtime_provider::resolve_runtime_base_url(
            agent,
            active_provider.as_str(),
            route.and_then(|rt| rt.base_url.as_deref()),
        );
        crate::hooks::invoke_pre_api_request_hook(
            agent,
            *api_call_count,
            &api_messages,
            tool_schemas.len(),
            active_model,
            active_provider.as_str(),
            hook_base_url.as_deref(),
            hook_api_mode,
            effective_max_tokens,
        );
        let mut stream = if let Some(rt) = route {
            let (provider_name, model_name) =
                crate::route_learning::extract_provider_and_model(agent, active_model);
            let mode = rt.api_mode.as_ref().unwrap_or(&default_api_mode);
            let extra_body_for_call = extra_body_for_api_mode(agent, mode);
            let pool = crate::runtime_provider::credentials_pool_for_route(agent, rt);
            match crate::runtime_provider::build_runtime_provider(
                agent,
                rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                model_name,
                rt.base_url.as_deref(),
                rt.api_key_env.as_deref(),
                None,
                Some(mode),
                pool,
            ) {
                Ok(provider) => provider.chat_completion_stream(
                    &api_messages,
                    tool_schemas,
                    effective_max_tokens,
                    agent.config().temperature,
                    Some(model_name),
                    extra_body_for_call.as_ref(),
                ),
                Err(e) => {
                    tracing::warn!(
                        "Runtime route unavailable (reason={:?}) for stream, falling back to primary runtime: {}",
                        rt.routing_reason,
                        e
                    );
                    agent.llm_provider.chat_completion_stream(
                        &api_messages,
                        tool_schemas,
                        effective_max_tokens,
                        agent.config().temperature,
                        Some(
                            crate::route_learning::extract_provider_and_model(
                                agent,
                                crate::runtime_provider::active_model(agent).as_str(),
                            )
                            .1,
                        ),
                        default_extra_body.as_ref(),
                    )
                }
            }
        } else {
            agent.llm_provider.chat_completion_stream(
                &api_messages,
                tool_schemas,
                effective_max_tokens,
                agent.config().temperature,
                Some(active_model_name),
                default_extra_body.as_ref(),
            )
        };

        let t_stream_start = Instant::now();
        let mut ttft_ms: Option<u64> = None;
        let mut chunk_count: u64 = 0;

        let mut content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage: Option<UsageStats> = None;
        let mut finish_reason: Option<String> = None;
        let mut deltas_were_sent = false;

        while let Some(chunk_result) = stream.next().await {
            if agent.interrupt.take_interrupt_graceful().is_some() {
                let message =
                    assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);
                return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                    message,
                    usage: last_usage.clone(),
                    model: active_model.to_string(),
                    finish_reason: Some("interrupted".to_string()),
                    ..Default::default()
                }));
            }

            let chunk = match chunk_result {
                Ok(chunk) => chunk,
                Err(err) => {
                    let partial_tool_in_flight = partial_stream_tool_calls_in_flight(&tool_calls);
                    let should_retry_for_partial_tool = deltas_were_sent
                        && partial_tool_in_flight
                        && is_transient_stream_error(&err)
                        && stream_attempt < max_stream_retries;
                    let should_retry_before_deltas = !deltas_were_sent
                        && is_transient_stream_error(&err)
                        && stream_attempt < max_stream_retries;

                    if should_retry_for_partial_tool || should_retry_before_deltas {
                        let next_attempt = stream_attempt + 2;
                        let total_attempts = max_stream_retries + 1;
                        if should_retry_for_partial_tool {
                            on_chunk(StreamChunk {
                                delta: Some(hermes_core::StreamDelta {
                                    content: Some(
                                        "\n\n[connection dropped mid tool-call; reconnecting...]\n\n"
                                            .to_string(),
                                    ),
                                    tool_calls: None,
                                    extra: None,
                                }),
                                finish_reason: None,
                                usage: None,
                            });
                            crate::hooks::emit_status(
                                agent,
                                "lifecycle",
                                &format!(
                                    "Connection dropped mid tool-call; reconnecting (attempt {}/{})",
                                    next_attempt, total_attempts
                                ),
                            );
                            tracing::warn!(
                                "Streaming attempt {}/{} failed after partial tool-call data; retrying: {}",
                                stream_attempt + 1,
                                total_attempts,
                                err
                            );
                        } else {
                            tracing::warn!(
                                "Streaming attempt {}/{} failed before deltas; retrying: {}",
                                stream_attempt + 1,
                                total_attempts,
                                err
                            );
                        }
                        continue 'stream_attempt;
                    }
                    if deltas_were_sent || !recovered_stream_text.is_empty() {
                        return Ok(partial_stream_stub_outcome(
                            &recovered_stream_text,
                            &tool_calls,
                            last_usage.clone(),
                            active_model,
                            on_chunk,
                            &err,
                        ));
                    }
                    note_stream_not_supported(agent, &err);
                    return Err(err);
                }
            };

            if ttft_ms.is_none() {
                ttft_ms = Some(t_stream_start.elapsed().as_millis() as u64);
            }
            chunk_count += 1;

            if let Some(ref delta) = chunk.delta {
                if let Some(ref text) = delta.content {
                    deltas_were_sent = true;
                    let scrubbed = if let Some(scrubber) = stream_scrubber.as_deref_mut() {
                        scrubber.scrub(text)
                    } else {
                        text.clone()
                    };
                    content.push_str(&scrubbed);
                    recovered_stream_text.push_str(&scrubbed);
                    if let Some(ref cb) = agent.callbacks.on_stream_delta {
                        cb(&scrubbed);
                    }
                }
                if let Some(ref extra) = delta.extra {
                    if let Some(thinking) = extra.get("thinking").and_then(|v| v.as_str()) {
                        deltas_were_sent = true;
                        reasoning_content.push_str(thinking);
                        if let Some(ref cb) = agent.callbacks.on_thinking {
                            cb(thinking);
                        }
                    }
                }
                if let Some(ref tc_deltas) = delta.tool_calls {
                    for tcd in tc_deltas {
                        let idx = tcd.index as usize;
                        while tool_calls.len() <= idx {
                            tool_calls.push(ToolCall {
                                id: String::new(),
                                function: hermes_core::FunctionCall {
                                    name: String::new(),
                                    arguments: String::new(),
                                },
                                extra_content: None,
                            });
                        }
                        if let Some(ref id) = tcd.id {
                            tool_calls[idx].id = id.clone();
                        }
                        if let Some(ref fc) = tcd.function {
                            if let Some(ref name) = fc.name {
                                tool_calls[idx].function.name = name.clone();
                            }
                            if let Some(ref args) = fc.arguments {
                                tool_calls[idx].function.arguments.push_str(args);
                            }
                        }
                    }
                }
            }

            if let Some(ref usage) = chunk.usage {
                last_usage = Some(usage.clone());
            }
            if let Some(ref fr) = chunk.finish_reason {
                finish_reason = Some(fr.clone());
            }

            on_chunk(chunk);
        }

        if std::env::var("HERMES_TURN_PERF").map_or(false, |v| !v.is_empty()) {
            let gen_ms = t_stream_start.elapsed().as_millis() as u64;
            tracing::info!(
                target: "turn_perf",
                attempt = stream_attempt,
                provider = %active_provider,
                model = %active_model,
                ttft_ms = ttft_ms.unwrap_or(gen_ms),
                gen_ms,
                chunks = chunk_count,
                "stream timing"
            );
        }

        if tool_calls.iter().any(tool_call_arguments_look_truncated) {
            finish_reason = Some("length".to_string());
        }

        let message = assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);

        // Cache diagnostics: capture prefix shape, compare with
        // previous turn, log hit/miss breakdown.  Mirrors the
        // non-streaming path in call_llm_with_retry_inner.
        if let Ok(mut state) = agent.state.lock() {
            let prev = state.last_prefix_shape.clone();
            let s_hit = state.session_cache_hit;
            let s_miss = state.session_cache_miss;
            let rewrite_ver = state.compaction_count;
            let (new_shape, diag) = crate::cache_diagnostics::trace_turn(
                ctx.get_messages(),
                tool_schemas,
                rewrite_ver,
                last_usage.as_ref(),
                prev.as_ref(),
                s_hit,
                s_miss,
            );
            state.last_prefix_shape = Some(new_shape);
            state.session_cache_hit = diag.session_hit;
            state.session_cache_miss = diag.session_miss;
        }

        return Ok(StreamCollectOutcome::Complete(LlmResponse {
            message,
            usage: last_usage,
            model: active_model.to_string(),
            finish_reason,
            ..Default::default()
        }));
    }

    Err(AgentError::LlmApi(
        "streaming failed after retry budget exhausted".to_string(),
    ))
}

// ---------------------------------------------------------------------------
// Finish reason / finalization
// ---------------------------------------------------------------------------

fn finish_reason_requires_continuation(finish_reason: Option<&str>) -> bool {
    matches!(finish_reason, Some("length" | "pause_turn"))
}

/// Provider reported tool intent (`finish_reason=tool_calls`) but no executable tool calls arrived.
pub(crate) fn missing_tool_calls_finish_mismatch(
    finish_reason: Option<&str>,
    has_tool_calls: bool,
) -> bool {
    !has_tool_calls && matches!(finish_reason, Some("tool_calls"))
}

pub(crate) fn effective_finish_reason(
    agent: &AgentLoop,
    response: &LlmResponse,
    assistant: &Message,
    history_includes_tool: bool,
    route: Option<&TurnRuntimeRoute>,
) -> Option<String> {
    let finish_reason = response.finish_reason.as_deref();
    let active_model = crate::runtime_provider::active_model(agent);
    let (provider, model) =
        crate::route_learning::extract_provider_and_model(agent, active_model.as_str());
    let cfg = agent.config();
    let api_mode = route
        .and_then(|rt| rt.api_mode.as_ref())
        .unwrap_or(&cfg.api_mode);
    let base_url = crate::runtime_provider::resolve_runtime_base_url(
        agent,
        provider.as_str(),
        route.and_then(|rt| rt.base_url.as_deref()),
    );
    if should_treat_stop_as_truncated(
        finish_reason,
        assistant.content.as_deref(),
        history_includes_tool,
        api_mode_as_hook_str(api_mode),
        model,
        provider.as_str(),
        base_url.as_deref(),
    ) {
        crate::hooks::emit_status(
            agent,
            "lifecycle",
            "Treating suspicious Ollama/GLM stop response as truncated",
        );
        return Some("length".to_string());
    }
    response.finish_reason.clone()
}

pub(crate) fn build_finalization_signals(
    _agent: &AgentLoop,
    task_hint: &str,
    messages: &[Message],
    message: &Message,
    finish_reason: Option<&str>,
) -> FinalizationSignals {
    let has_tool_calls = message.tool_calls.as_ref().map_or(false, |v| !v.is_empty());
    let has_visible_text = AgentLoop::assistant_visible_text(message);
    let has_visible_text_after_think =
        AgentLoop::assistant_visible_text_after_think_blocks(message);
    let has_reasoning = AgentLoop::assistant_has_reasoning(message);
    let continuation_required = finish_reason_requires_continuation(finish_reason)
        || missing_tool_calls_finish_mismatch(finish_reason, has_tool_calls);
    let ack_detected = !has_tool_calls
        && !continuation_required
        && agent_runtime_helpers::looks_like_codex_intermediate_ack(
            task_hint,
            message.content.as_deref().unwrap_or(""),
            messages,
        );

    FinalizationSignals {
        finish_reason: finish_reason.map(str::to_string),
        has_tool_calls,
        has_visible_text,
        has_visible_text_after_think,
        has_reasoning,
        continuation_required,
        ack_detected,
    }
}

/// DeepSeek / thinking models may return structured reasoning before visible content.
pub(crate) fn handle_reasoning_only_prefill(
    agent: &AgentLoop,
    message: &Message,
    attempt: u32,
    max_attempts: u32,
) {
    crate::hooks::emit_reasoning_from_message(agent, message);
    // tracing::debug!(
    //     "reasoning-only assistant response; prefill continuation ({}/{})",
    //     attempt,
    //     max_attempts
    // );
    let _ = (attempt, max_attempts);
}

// ---------------------------------------------------------------------------
// Streaming transport
// ---------------------------------------------------------------------------

pub(crate) fn has_stream_consumers(agent: &AgentLoop, turn_stream_callback: bool) -> bool {
    turn_stream_callback || agent.callbacks.on_stream_delta.is_some()
}

fn route_blocks_llm_streaming(route: &TurnRuntimeRoute) -> bool {
    crate::agent_config::is_copilot_acp_transport(
        route.provider.as_deref().unwrap_or(""),
        route.base_url.as_deref().unwrap_or(""),
    )
}

pub(crate) fn provider_blocks_llm_streaming(agent: &AgentLoop) -> bool {
    let rt = crate::route_learning::primary_runtime_snapshot(agent);
    let cfg = agent.config();
    let prov = rt
        .provider
        .as_deref()
        .or(cfg.provider.as_deref())
        .unwrap_or("");
    let url = rt.base_url.as_deref().unwrap_or("");
    crate::agent_config::is_copilot_acp_transport(prov, url)
}

/// Python `_use_streaming` gate for the first LLM attempt in a turn (`inner_attempt == 0`).
pub(crate) fn use_streaming_llm_transport(
    agent: &AgentLoop,
    turn_stream_callback: bool,
    inner_attempt: u32,
    route: Option<&TurnRuntimeRoute>,
) -> bool {
    if inner_attempt > 0 {
        return false;
    }
    if agent
        .disable_streaming
        .load(std::sync::atomic::Ordering::Acquire)
    {
        return false;
    }
    if route.is_some_and(route_blocks_llm_streaming) || provider_blocks_llm_streaming(agent) {
        return false;
    }
    if !has_stream_consumers(agent, turn_stream_callback) {
        return !agent.llm_provider.prefers_non_streaming_transport();
    }
    true
}

pub(crate) fn session_disable_streaming(agent: &AgentLoop) {
    agent
        .disable_streaming
        .store(true, std::sync::atomic::Ordering::Release);
}

pub(crate) fn note_stream_not_supported(agent: &AgentLoop, err: &AgentError) {
    if !is_stream_not_supported_error(err) {
        return;
    }
    session_disable_streaming(agent);
    if !agent.config().quiet_mode {
        crate::hooks::emit_status(
            agent,
            "lifecycle",
            "Streaming is not supported for this model/provider. Switching to non-streaming. \
             Set display.streaming: false in config.yaml to skip this probe.",
        );
    }
    tracing::info!(error = %err, "streaming disabled for remainder of agent session");
}

// ---------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------

fn api_mode_as_hook_str(mode: &ApiMode) -> &'static str {
    match mode {
        ApiMode::ChatCompletions => "chat_completions",
        ApiMode::AnthropicMessages => "anthropic_messages",
        ApiMode::CodexResponses => "codex_responses",
        ApiMode::CodexAppServer => "codex_app_server",
        ApiMode::BedrockConverse => "bedrock_converse",
    }
}

pub(crate) fn extra_body_for_api_mode(agent: &AgentLoop, api_mode: &ApiMode) -> Option<Value> {
    let mut body = agent
        .config()
        .extra_body
        .clone()
        .unwrap_or_else(|| serde_json::json!({}));
    if !body.is_object() {
        return agent.config().extra_body.clone();
    }
    if !matches!(api_mode, ApiMode::CodexResponses) {
        if body.get("strict_tool_calls").is_none()
            && body.get("strict_api").is_none()
            && body.get("provider_strict").is_none()
        {
            body["strict_api"] = Value::Bool(true);
        }
    }
    let cfg = agent.config();
    let provider = cfg.provider.as_deref().unwrap_or("");
    if provider.eq_ignore_ascii_case("openrouter")
        || crate::runtime_provider::active_model(agent).contains("openrouter/")
    {
        if let Some(prefs) = agent.openrouter_provider_preferences() {
            body["provider"] = prefs;
        }
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tool_calls_finish_mismatch_detects_provider_tool_intent_gap() {
        assert!(missing_tool_calls_finish_mismatch(
            Some("tool_calls"),
            false
        ));
        assert!(!missing_tool_calls_finish_mismatch(
            Some("tool_calls"),
            true
        ));
        assert!(!missing_tool_calls_finish_mismatch(Some("stop"), false));
        assert!(!missing_tool_calls_finish_mismatch(None, false));
    }
}
