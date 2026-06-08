//! Gateway message handlers extracted from `main.rs` to avoid huge closure futures
//! blowing the Windows default thread stack during startup.

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hermes_agent::{
    split_messages_for_run_conversation, AgentCallbacks, RunConversationParams,
};
use hermes_core::{Message, MessageRole, StreamChunk, ToolSchema};
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_gateway::{
    Gateway, GatewayError, GatewayRuntimeContext, SessionTeardownContext, SessionTeardownHandler,
};
use serde_json::Value;
use tracing::{debug, warn};
use hermes_tools::tools::clarify::MAX_CHOICES;
use hermes_tools::ToolRegistry;

use hermes_cli::app::bridge_tool_registry;
use hermes_cli::platform_toolsets::{
    cross_platform_system_hint, resolve_platform_tool_schemas, tool_definition_summary,
};
use hermes_cli::tool_preview::{build_tool_preview_from_value, tool_emoji};

use crate::{
    extract_last_assistant_reply, get_or_build_gateway_cached_agent, resolve_model_for_gateway,
    truncate_hook_tool_result,     GatewayAgentCache,
};

fn gateway_conversation_reply(conv: &hermes_agent::ConversationResult) -> String {
    conv.final_response
        .as_ref()
        .filter(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| extract_last_assistant_reply(conv.messages()))
}

fn prepend_clarify_user_request_hint(
    user_message: &str,
    tool_schemas: &[ToolSchema],
    history: &mut Vec<Message>,
) {
    let has_clarify = tool_schemas.iter().any(|s| s.name == "clarify");
    if !has_clarify {
        return;
    }
    let lower = user_message.to_ascii_lowercase();
    if !lower.contains("clarify") && !user_message.contains("澄清") {
        return;
    }
    let duplicate = history.iter().any(|m| {
        m.content
            .as_deref()
            .map(|c| c.contains("user explicitly requested the `clarify` tool"))
            .unwrap_or(false)
    });
    if duplicate {
        return;
    }
    history.insert(
        0,
        Message::system(
            "[SYSTEM] The user explicitly requested the `clarify` tool. You MUST call `clarify` \
             with `question` and up to 4 `choices` in this turn before ending. Do not reply with \
             only an introduction.",
        ),
    );
}

fn prepend_cross_platform_hint(
    platform: &str,
    tool_schemas: &[ToolSchema],
    history: &mut Vec<Message>,
) {
    let names: Vec<String> = tool_schemas.iter().map(|s| s.name.clone()).collect();
    let Some(hint) = cross_platform_system_hint(platform, &names) else {
        return;
    };
    let duplicate = history.iter().any(|m| {
        m.content
            .as_deref()
            .map(|c| {
                c.contains("WeChat-class channel")
                    || c.contains("Feishu/Lark")
                    || c.contains("Interactive IM channel:")
            })
            .unwrap_or(false)
    });
    if !duplicate {
        history.insert(0, Message::system(hint));
    }
}

fn clarify_choices_from_args(args: &Value) -> Vec<String> {
    match args.get("choices") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s.trim().to_string()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .take(MAX_CHOICES)
            .collect(),
        _ => Vec::new(),
    }
}

fn clarify_choices_with_skip(mut choices: Vec<String>) -> Vec<String> {
    if !choices
        .iter()
        .any(|c| c.contains("Skip") || c.contains("跳过"))
    {
        choices.push("Skip / 跳过".to_string());
    }
    choices
}

fn format_clarify_prompt_for_chat(question: &str, choices: &[String]) -> String {
    if choices.is_empty() {
        return format!("{question}\n\n请直接回复。");
    }
    let mut lines = vec![question.to_string(), String::new()];
    for (i, choice) in choices.iter().enumerate() {
        lines.push(format!("{}. {choice}", i + 1));
    }
    lines.push(String::new());
    lines.push("请回复选项编号或文字。".to_string());
    lines.join("\n")
}

/// Push clarify question + numbered choices to the active IM chat.
fn spawn_gateway_clarify_prompt(
    gateway: Arc<Gateway>,
    platform: String,
    chat_id: String,
    args: &Value,
) {
    let Some(question) = args
        .get("question")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
    else {
        warn!("gateway clarify: skipped outbound prompt — empty question");
        return;
    };
    let choices = clarify_choices_with_skip(clarify_choices_from_args(args));
    let text = format_clarify_prompt_for_chat(question, &choices);
    debug!(
        platform = %platform,
        chat_id = %chat_id,
        choice_count = choices.len(),
        text_chars = text.chars().count(),
        "gateway clarify: sending prompt to chat"
    );
    tokio::spawn(async move {
        match gateway.send_message(&platform, &chat_id, &text, None).await {
            Ok(()) => debug!(
                platform = %platform,
                chat_id = %chat_id,
                "gateway clarify: prompt sent to chat"
            ),
            Err(e) => warn!(
                platform = %platform,
                chat_id = %chat_id,
                error = %e,
                "gateway clarify: failed to send prompt to chat"
            ),
        }
    });
}

fn clarify_async_mode_enabled() -> bool {
    matches!(
        std::env::var("HERMES_CLARIFY_ASYNC")
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on")
    )
}

/// If a clarify request is queued from a prior async turn, treat this inbound
/// user message as the answer and continue with a normal agent turn.
async fn gateway_consume_pending_clarify_answer(
    clarify: &ClarifyDispatcher,
    messages: &Arc<Vec<Message>>,
    ctx: &GatewayRuntimeContext,
    streaming: bool,
) {
    if !clarify_async_mode_enabled() {
        return;
    }
    let Some(pending) = clarify.take_next().await else {
        return;
    };
    let answer = messages
        .iter()
        .rev()
        .find_map(|m| {
            (m.role == MessageRole::User)
                .then(|| m.content.clone())
                .flatten()
        })
        .unwrap_or_default();
    debug!(
        platform = %ctx.platform,
        session_key = %ctx.session_key,
        chat_id = %ctx.chat_id,
        streaming,
        clarification_id = %pending.id,
        question = %pending.question,
        choice_count = pending.choices.len(),
        answer_len = answer.len(),
        answer_preview = %truncate_hook_tool_result(&answer),
        "gateway clarify: consuming pending answer; continuing normal agent turn"
    );
    let clarification_id = pending.id.clone();
    if let Err(e) = pending.respond(clarify, &answer).await {
        warn!(
            platform = %ctx.platform,
            session_key = %ctx.session_key,
            clarification_id = %clarification_id,
            error = %e,
            "gateway clarify: failed to record answer for pending request"
        );
    }
}

#[derive(Clone)]
pub(crate) struct GatewayHandlerDeps {
    pub config: Arc<hermes_config::GatewayConfig>,
    pub runtime_tools: Arc<ToolRegistry>,
    pub gateway_for_review: Arc<Gateway>,
    pub clarify: ClarifyDispatcher,
    pub gateway_agent_cache: GatewayAgentCache,
}

pub(crate) async fn gateway_handle_message_non_streaming(
    messages: Arc<Vec<Message>>,
    ctx: GatewayRuntimeContext,
    deps: GatewayHandlerDeps,
) -> Result<String, GatewayError> {
    let GatewayHandlerDeps {
        config,
        runtime_tools,
        gateway_for_review,
        clarify,
        gateway_agent_cache,
    } = deps;

    gateway_consume_pending_clarify_answer(&clarify, &messages, &ctx, false).await;
    let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
    let _effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), &ctx);
    let tool_schemas = resolve_platform_tool_schemas(config.as_ref(), &ctx.platform, &runtime_tools);
    let tool_defs = tool_definition_summary(&tool_schemas);
    gateway_for_review
        .emit_hook_event(
            "agent:tool_definitions",
            serde_json::json!({
                "platform": ctx.platform,
                "chat_id": ctx.chat_id,
                "user_id": ctx.user_id,
                "session_id": ctx.session_key,
                "streaming": false,
                "tools": tool_defs
            }),
        )
        .await;
    let platform_for_review = ctx.platform.clone();
    let chat_for_review = ctx.chat_id.clone();
    let deferred_queue = ctx.deferred_post_delivery_messages.clone();
    let deferred_released = ctx.deferred_post_delivery_released.clone();
    let gateway_for_review_cb = gateway_for_review.clone();
    let review_cb = Arc::new(move |text: &str| {
        if let (Some(queue), Some(released)) = (deferred_queue.as_ref(), deferred_released.as_ref())
        {
            if !released.load(Ordering::Acquire) {
                if let Ok(mut guard) = queue.lock() {
                    guard.push(text.to_string());
                    return;
                }
            }
        }
        let gw = gateway_for_review_cb.clone();
        let platform = platform_for_review.clone();
        let chat_id = chat_for_review.clone();
        let msg = text.to_string();
        tokio::spawn(async move {
            let _ = gw.send_message(&platform, &chat_id, &msg, None).await;
        });
    });
    let status_cb = hermes_cli::gateway_inbound_wiring::make_gateway_status_callback(
        gateway_for_review.clone(),
        ctx.platform.clone(),
        ctx.chat_id.clone(),
        ctx.user_id.clone(),
        ctx.session_key.clone(),
    );
    let on_thinking = hermes_cli::gateway_inbound_wiring::make_gateway_on_thinking_callback(
        gateway_for_review.clone(),
        ctx.platform.clone(),
        ctx.chat_id.clone(),
        None,
    );
    let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let tool_events_for_start = tool_events.clone();
    let gateway_for_clarify = gateway_for_review.clone();
    let platform_for_clarify = ctx.platform.clone();
    let chat_for_clarify = ctx.chat_id.clone();
    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
            if name == "clarify" {
                debug!(
                    question = args.get("question").and_then(|v| v.as_str()),
                    choices = ?args.get("choices"),
                    "gateway clarify: tool call started (non-streaming)"
                );
                spawn_gateway_clarify_prompt(
                    gateway_for_clarify.clone(),
                    platform_for_clarify.clone(),
                    chat_for_clarify.clone(),
                    args,
                );
            }
            let preview = build_tool_preview_from_value(name, args, 60).unwrap_or_default();
            let mut event = serde_json::json!({
                "phase": "start",
                "name": name,
                "emoji": tool_emoji(name)
            });
            if !preview.is_empty() {
                event["preview"] = serde_json::json!(preview);
            }
            if let Ok(mut guard) = tool_events_for_start.lock() {
                guard.push(event);
            }
        });
    let tool_events_for_complete = tool_events.clone();
    let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
        Box::new(move |name: &str, result: &str| {
            if name == "clarify" {
                debug!(
                    result_preview = %truncate_hook_tool_result(result),
                    "gateway clarify: tool call completed (non-streaming)"
                );
            }
            if let Ok(mut guard) = tool_events_for_complete.lock() {
                guard.push(serde_json::json!({
                    "phase": "complete",
                    "name": name,
                    "emoji": tool_emoji(name),
                    "result": truncate_hook_tool_result(result)
                }));
            }
        });
    let tool_events_for_step = tool_events.clone();
    let gateway_for_step_hook = gateway_for_review.clone();
    let platform_for_step_hook = ctx.platform.clone();
    let user_for_step_hook = ctx.user_id.clone();
    let session_for_step_hook = ctx.session_key.clone();
    let on_step_complete: Box<dyn Fn(u32) + Send + Sync> = Box::new(move |iteration: u32| {
        let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
            std::mem::take(&mut *guard)
        } else {
            Vec::new()
        };
        let tool_names: Vec<String> = tools
            .iter()
            .filter_map(|v| {
                v.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        let gw_hook = gateway_for_step_hook.clone();
        let platform = platform_for_step_hook.clone();
        let user_id = user_for_step_hook.clone();
        let session_id = session_for_step_hook.clone();
        tokio::spawn(async move {
            gw_hook
                .emit_hook_event(
                    "agent:step",
                    serde_json::json!({
                        "platform": platform,
                        "user_id": user_id,
                        "session_id": session_id,
                        "iteration": iteration,
                        "tool_names": tool_names,
                        "tools": tools
                    }),
                )
                .await;
        });
    });
    let callbacks = AgentCallbacks {
        background_review_callback: Some(review_cb),
        status_callback: Some(status_cb),
        on_thinking: Some(on_thinking),
        on_tool_start: Some(on_tool_start),
        on_tool_complete: Some(on_tool_complete),
        on_step_complete: Some(on_step_complete),
        ..Default::default()
    };
    let agent = get_or_build_gateway_cached_agent(
        &gateway_agent_cache,
        config.as_ref(),
        &ctx,
        agent_tools,
        runtime_tools.clone(),
    )
    .await;
    let (history, user_message) = split_messages_for_run_conversation(&messages).ok_or_else(|| {
        GatewayError::Platform("session has no user message for run_conversation".into())
    })?;
    let mut history = history;
    prepend_cross_platform_hint(&ctx.platform, &tool_schemas, &mut history);
    prepend_clarify_user_request_hint(&user_message, &tool_schemas, &mut history);
    let task_id = Some(ctx.session_key.clone());
    let mut agent = agent.lock().await;
    agent.callbacks = Arc::new(callbacks);
    let conv = agent
        .run_conversation(RunConversationParams {
            user_message,
            conversation_history: history,
            task_id,
            stream_callback: None,
            persist_user_message: None,
            tools: Some(tool_schemas),
            persist_session: true,
        })
        .await
        .map_err(|e| GatewayError::Platform(e.to_string()))?;
    let usage_display = agent.session_usage_display();
    let session_key = ctx.session_key.clone();
    drop(agent);
    gateway_for_review
        .sync_session_token_usage(&session_key, usage_display)
        .await;
    Ok(gateway_conversation_reply(&conv))
}

/// Agent-layer POI / memory flush before gateway session reset, idle expiry, or shutdown.
pub fn make_gateway_session_teardown_handler(
    deps: GatewayHandlerDeps,
) -> SessionTeardownHandler {
    Arc::new(move |ctx| {
        let deps = deps.clone();
        Box::pin(async move { gateway_run_session_teardown(ctx, deps).await })
    })
}

async fn gateway_run_session_teardown(ctx: SessionTeardownContext, deps: GatewayHandlerDeps) {
    let gateway_ctx = GatewayRuntimeContext {
        session_key: ctx.session_key.clone(),
        session_id: ctx.session_id.clone(),
        platform: ctx.platform.clone(),
        chat_id: ctx.chat_id.clone(),
        user_id: ctx.user_id.clone(),
        model: ctx.model.clone(),
        provider: ctx.provider.clone(),
        personality: ctx.personality.clone(),
        home: ctx.home.clone(),
        ..Default::default()
    };
    let agent_tools = Arc::new(bridge_tool_registry(&deps.runtime_tools));
    let agent = get_or_build_gateway_cached_agent(
        &deps.gateway_agent_cache,
        deps.config.as_ref(),
        &gateway_ctx,
        agent_tools,
        deps.runtime_tools.clone(),
    )
    .await;
    let agent = agent.lock().await;
    let interrupted = ctx.reason == "shutdown";
    agent.session_end_hooks(
        ctx.messages.as_ref(),
        false,
        interrupted,
        0,
        true,
    );
}

pub(crate) async fn gateway_handle_message_streaming(
    messages: Arc<Vec<Message>>,
    ctx: GatewayRuntimeContext,
    on_chunk: Arc<dyn Fn(String) + Send + Sync>,
    deps: GatewayHandlerDeps,
) -> Result<String, GatewayError> {
    let GatewayHandlerDeps {
        config,
        runtime_tools,
        gateway_for_review,
        clarify,
        gateway_agent_cache,
    } = deps;

    gateway_consume_pending_clarify_answer(&clarify, &messages, &ctx, true).await;
    let agent_tools = Arc::new(bridge_tool_registry(&runtime_tools));
    let _effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), &ctx);
    let tool_schemas = resolve_platform_tool_schemas(config.as_ref(), &ctx.platform, &runtime_tools);
    let tool_defs = tool_definition_summary(&tool_schemas);
    gateway_for_review
        .emit_hook_event(
            "agent:tool_definitions",
            serde_json::json!({
                "platform": ctx.platform,
                "chat_id": ctx.chat_id,
                "user_id": ctx.user_id,
                "session_id": ctx.session_key,
                "streaming": true,
                "tools": tool_defs
            }),
        )
        .await;
    let platform_for_review = ctx.platform.clone();
    let chat_for_review = ctx.chat_id.clone();
    let deferred_queue = ctx.deferred_post_delivery_messages.clone();
    let deferred_released = ctx.deferred_post_delivery_released.clone();
    let gateway_for_review_cb = gateway_for_review.clone();
    let review_cb = Arc::new(move |text: &str| {
        if let (Some(queue), Some(released)) = (deferred_queue.as_ref(), deferred_released.as_ref())
        {
            if !released.load(Ordering::Acquire) {
                if let Ok(mut guard) = queue.lock() {
                    guard.push(text.to_string());
                    return;
                }
            }
        }
        let gw = gateway_for_review_cb.clone();
        let platform = platform_for_review.clone();
        let chat_id = chat_for_review.clone();
        let msg = text.to_string();
        tokio::spawn(async move {
            let _ = gw.send_message(&platform, &chat_id, &msg, None).await;
        });
    });
    let status_cb = hermes_cli::gateway_inbound_wiring::make_gateway_status_callback(
        gateway_for_review.clone(),
        ctx.platform.clone(),
        ctx.chat_id.clone(),
        ctx.user_id.clone(),
        ctx.session_key.clone(),
    );
    let on_thinking = hermes_cli::gateway_inbound_wiring::make_gateway_on_thinking_callback(
        gateway_for_review.clone(),
        ctx.platform.clone(),
        ctx.chat_id.clone(),
        None,
    );
    let tool_events = Arc::new(Mutex::new(Vec::<serde_json::Value>::new()));
    let tool_events_for_start = tool_events.clone();
    let gateway_for_clarify = gateway_for_review.clone();
    let platform_for_clarify = ctx.platform.clone();
    let chat_for_clarify = ctx.chat_id.clone();
    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
            if name == "clarify" {
                debug!(
                    question = args.get("question").and_then(|v| v.as_str()),
                    choices = ?args.get("choices"),
                    "gateway clarify: tool call started (streaming)"
                );
                spawn_gateway_clarify_prompt(
                    gateway_for_clarify.clone(),
                    platform_for_clarify.clone(),
                    chat_for_clarify.clone(),
                    args,
                );
            }
            let preview = build_tool_preview_from_value(name, args, 60).unwrap_or_default();
            let mut event = serde_json::json!({
                "phase": "start",
                "name": name,
                "emoji": tool_emoji(name)
            });
            if !preview.is_empty() {
                event["preview"] = serde_json::json!(preview);
            }
            if let Ok(mut guard) = tool_events_for_start.lock() {
                guard.push(event);
            }
        });
    let tool_events_for_complete = tool_events.clone();
    let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
        Box::new(move |name: &str, result: &str| {
            if name == "clarify" {
                debug!(
                    result_preview = %truncate_hook_tool_result(result),
                    "gateway clarify: tool call completed (streaming)"
                );
            }
            if let Ok(mut guard) = tool_events_for_complete.lock() {
                guard.push(serde_json::json!({
                    "phase": "complete",
                    "name": name,
                    "emoji": tool_emoji(name),
                    "result": truncate_hook_tool_result(result)
                }));
            }
        });
    let tool_events_for_step = tool_events.clone();
    let gateway_for_step_hook = gateway_for_review.clone();
    let platform_for_step_hook = ctx.platform.clone();
    let user_for_step_hook = ctx.user_id.clone();
    let session_for_step_hook = ctx.session_key.clone();
    let on_step_complete: Box<dyn Fn(u32) + Send + Sync> = Box::new(move |iteration: u32| {
        let tools = if let Ok(mut guard) = tool_events_for_step.lock() {
            std::mem::take(&mut *guard)
        } else {
            Vec::new()
        };
        let tool_names: Vec<String> = tools
            .iter()
            .filter_map(|v| {
                v.get("name")
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string())
            })
            .collect();
        let gw_hook = gateway_for_step_hook.clone();
        let platform = platform_for_step_hook.clone();
        let user_id = user_for_step_hook.clone();
        let session_id = session_for_step_hook.clone();
        tokio::spawn(async move {
            gw_hook
                .emit_hook_event(
                    "agent:step",
                    serde_json::json!({
                        "platform": platform,
                        "user_id": user_id,
                        "session_id": session_id,
                        "iteration": iteration,
                        "tool_names": tool_names,
                        "tools": tools
                    }),
                )
                .await;
        });
    });
    let callbacks = AgentCallbacks {
        background_review_callback: Some(review_cb),
        status_callback: Some(status_cb),
        on_thinking: Some(on_thinking),
        on_tool_start: Some(on_tool_start),
        on_tool_complete: Some(on_tool_complete),
        on_step_complete: Some(on_step_complete),
        ..Default::default()
    };
    let agent = get_or_build_gateway_cached_agent(
        &gateway_agent_cache,
        config.as_ref(),
        &ctx,
        agent_tools,
        runtime_tools.clone(),
    )
    .await;
    let emit = on_chunk.clone();
    let ui_state = Arc::new(Mutex::new((false, false)));
    let ui_state_cb = ui_state.clone();
    let stream_cb: Box<dyn Fn(StreamChunk) + Send + Sync> = Box::new(move |chunk: StreamChunk| {
        if let Some(delta) = chunk.delta {
            if let Some(extra) = delta.extra.as_ref() {
                if let Some(control) = extra.get("control").and_then(|v| v.as_str()) {
                    if control == "mute_post_response" {
                        let enabled = extra
                            .get("enabled")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if let Ok(mut st) = ui_state_cb.lock() {
                            st.0 = enabled;
                        }
                    } else if control == "stream_break" {
                        if let Ok(mut st) = ui_state_cb.lock() {
                            st.1 = true;
                        }
                    }
                }
            }
            if let Some(text) = delta.content {
                if let Ok(mut st) = ui_state_cb.lock() {
                    if st.0 {
                        return;
                    }
                    if st.1 {
                        emit("\n\n".to_string());
                        st.1 = false;
                    }
                }
                emit(text);
            }
        }
    });

    let (history, user_message) = split_messages_for_run_conversation(&messages).ok_or_else(|| {
        GatewayError::Platform("session has no user message for run_conversation".into())
    })?;
    let mut history = history;
    prepend_cross_platform_hint(&ctx.platform, &tool_schemas, &mut history);
    prepend_clarify_user_request_hint(&user_message, &tool_schemas, &mut history);
    let task_id = Some(ctx.session_key.clone());
    let mut agent = agent.lock().await;
    agent.callbacks = Arc::new(callbacks);
    let conv = agent
        .run_conversation(RunConversationParams {
            user_message,
            conversation_history: history,
            task_id,
            stream_callback: Some(stream_cb),
            persist_user_message: None,
            tools: Some(tool_schemas),
            persist_session: true,
        })
        .await
        .map_err(|e| GatewayError::Platform(e.to_string()))?;
    let usage_display = agent.session_usage_display();
    let session_key = ctx.session_key.clone();
    drop(agent);
    gateway_for_review
        .sync_session_token_usage(&session_key, usage_display)
        .await;
    Ok(gateway_conversation_reply(&conv))
}
