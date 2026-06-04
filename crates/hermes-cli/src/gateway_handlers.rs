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
            .map(|c| c.contains("WeChat-class channel") || c.contains("Feishu/Lark"))
            .unwrap_or(false)
    });
    if !duplicate {
        history.insert(0, Message::system(hint));
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

    if let Some(pending) = clarify.take_next().await {
        let answer = messages
            .iter()
            .rev()
            .find_map(|m| {
                (m.role == MessageRole::User)
                    .then(|| m.content.clone())
                    .flatten()
            })
            .unwrap_or_default();
        let _ = pending.respond(&clarify, answer).await;
        return Ok("Clarification received. Continuing task execution...".to_string());
    }
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
    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
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
    Ok(conv
        .final_response
        .clone()
        .unwrap_or_else(|| extract_last_assistant_reply(conv.messages())))
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

    if let Some(pending) = clarify.take_next().await {
        let answer = messages
            .iter()
            .rev()
            .find_map(|m| {
                (m.role == MessageRole::User)
                    .then(|| m.content.clone())
                    .flatten()
            })
            .unwrap_or_default();
        let _ = pending.respond(&clarify, answer).await;
        return Ok("Clarification received. Continuing task execution...".to_string());
    }
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
    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
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
    Ok(conv
        .final_response
        .clone()
        .unwrap_or_else(|| extract_last_assistant_reply(conv.messages())))
}
