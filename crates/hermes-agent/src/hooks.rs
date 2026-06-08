//! Plugin hooks system — pre-LLM, post-LLM, session lifecycle, and status emission.
//!
//! Extracted from `impl AgentLoop` in `agent_loop.rs` to reduce the God struct.
//! All functions take `agent: &AgentLoop` instead of `&self`.

use std::path::PathBuf;

use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent_loop::{AgentLoop, ApiMode};
use hermes_core::{Message, ToolCall, ToolResult};

use crate::context::ContextManager;
use crate::plugins::{HookResult, HookType};
use crate::replay::truncate_hook_preview;

// ---------------------------------------------------------------------------
// Hook invocation
// ---------------------------------------------------------------------------

pub(crate) fn invoke_hook(agent: &AgentLoop, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
    if let Some(ref pm) = agent.plugin_manager {
        if let Ok(pm) = pm.lock() {
            return pm.invoke_hook(hook, ctx_val);
        }
    }
    Vec::new()
}

// ---------------------------------------------------------------------------
// Pre-LLM hook
// ---------------------------------------------------------------------------

pub(crate) fn inject_pre_llm_hook_into_user_message(
    agent: &AgentLoop,
    results: &[HookResult],
    ctx: &mut ContextManager,
) {
    let mut parts: Vec<String> = Vec::new();
    for r in results {
        if let HookResult::InjectContext(text) = r {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                parts.push(trimmed.to_string());
            }
        }
    }
    if parts.is_empty() {
        return;
    }
    let plugin_ctx = parts.join("\n\n");
    let messages = ctx.get_messages_mut();
    if let Some(idx) = messages
        .iter()
        .rposition(|m| m.role == hermes_core::MessageRole::User)
    {
        let msg = &mut messages[idx];
        let base = msg.content.clone().unwrap_or_default();
        let merged = if base.trim().is_empty() {
            plugin_ctx
        } else {
            format!("{base}\n\n{plugin_ctx}")
        };
        msg.content = Some(merged);
    }
}

/// Fire `pre_llm_call` once before the tool loop (Python `conversation_loop.py` ~652-686).
pub(crate) fn apply_pre_llm_call_hooks_once(
    agent: &AgentLoop,
    ctx: &mut ContextManager,
    user_message: &str,
    _session_id: &str,
) {
    let history: Vec<Message> = ctx
        .get_messages()
        .iter()
        .filter(|m| m.role != hermes_core::MessageRole::System)
        .cloned()
        .collect();
    let user_turns = history
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let hook_ctx = serde_json::json!({
        "session_id": agent.config().session_id,
        "user_message": user_message,
        "conversation_history": history,
        "is_first_turn": user_turns <= 1,
        "model": crate::runtime_provider::active_model(agent),
        "platform": agent.config().platform,
        "sender_id": serde_json::Value::Null,
        "turn": 0,
    });
    let results = invoke_hook(agent, HookType::PreLlmCall, &hook_ctx);
    inject_pre_llm_hook_into_user_message(agent, &results, ctx);
}

// ---------------------------------------------------------------------------
// Post-LLM hook context injection
// ---------------------------------------------------------------------------

pub(crate) fn inject_hook_context(
    agent: &AgentLoop,
    results: &[HookResult],
    ctx: &mut ContextManager,
) {
    for r in results {
        if let HookResult::InjectContext(text) = r {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(spill_path) = spill_hook_context_if_oversized(agent, trimmed) {
                let preview = truncate_hook_preview(trimmed, 720);
                let note = format!(
                    "Hook context was oversized and spilled to disk.\nspill_path={}\npreview:\n{}",
                    spill_path.display(),
                    preview
                );
                ctx.add_message(Message::system(note));
                continue;
            }
            ctx.add_message(Message::system(trimmed.to_string()));
        }
    }
}

pub(crate) fn apply_hook_output_transforms(results: &[HookResult], content: &mut Option<String>) {
    let mut current = content.clone().unwrap_or_default();
    let mut changed = false;
    for r in results {
        if let HookResult::TransformLlmOutput(next) = r {
            current = next.clone();
            changed = true;
        }
    }
    if changed {
        *content = Some(current);
    }
}

pub(crate) fn apply_transform_llm_output_hooks(agent: &AgentLoop, content: &mut Option<String>) {
    let hook_ctx = serde_json::json!({
        "content": content.clone().unwrap_or_default(),
    });
    let results = invoke_hook(agent, HookType::TransformLlmOutput, &hook_ctx);
    apply_hook_output_transforms(&results, content);
}

// ---------------------------------------------------------------------------
// Context spill
// ---------------------------------------------------------------------------

fn hook_context_spill_threshold_chars(agent: &AgentLoop) -> usize {
    std::env::var("HERMES_HOOK_CONTEXT_SPILL_CHARS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v >= 1024)
        .unwrap_or(12_000)
}

fn hook_context_spill_dir(agent: &AgentLoop) -> PathBuf {
    let hermes_home = agent
        .config()
        .hermes_home
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| Some(hermes_config::hermes_home()))
        .unwrap_or_else(hermes_config::hermes_home);
    hermes_home.join("hooks").join("spills")
}

pub(crate) fn spill_hook_context_if_oversized(agent: &AgentLoop, text: &str) -> Option<PathBuf> {
    if text.len() < hook_context_spill_threshold_chars(agent) {
        return None;
    }
    let dir = hook_context_spill_dir(agent);
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hex::encode(hasher.finalize());
    let stamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("hook_context_{}_{}.txt", stamp, &digest[..16]));
    if std::fs::write(&path, text).is_ok() {
        Some(path)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Pre-API request hook
// ---------------------------------------------------------------------------

pub(crate) fn invoke_pre_api_request_hook(
    agent: &AgentLoop,
    api_call_count: u32,
    api_messages: &[Message],
    tool_count: usize,
    model: &str,
    provider: &str,
    base_url: Option<&str>,
    api_mode: &ApiMode,
    max_tokens: Option<u32>,
) {
    let request_messages: Vec<Value> = api_messages
        .iter()
        .map(|m| {
            serde_json::json!({
                "role": m.role,
                "content": m.content,
            })
        })
        .collect();

    let hook_ctx = serde_json::json!({
        "api_call_count": api_call_count,
        "request_messages": request_messages,
        "tool_count": tool_count,
        "model": model,
        "provider": provider,
        "base_url": base_url,
        "api_mode": api_mode_as_hook_str(api_mode),
        "max_tokens": max_tokens,
    });
    let _ = invoke_hook(agent, HookType::PreApiRequest, &hook_ctx);
}

pub(crate) fn api_mode_as_hook_str(mode: &ApiMode) -> &'static str {
    match mode {
        ApiMode::ChatCompletions => "chat_completions",
        ApiMode::AnthropicMessages => "anthropic_messages",
        ApiMode::CodexResponses => "codex_responses",
        ApiMode::CodexAppServer => "codex_app_server",
        ApiMode::BedrockConverse => "bedrock_converse",
    }
}

// ---------------------------------------------------------------------------
// Session / turn lifecycle hooks
// ---------------------------------------------------------------------------

pub(crate) fn turn_end_plugin_hooks(
    agent: &AgentLoop,
    messages: &[Message],
    completed: bool,
    interrupted: bool,
    total_turns: u32,
    session_started_hooks_fired: bool,
) {
    let _ = messages;
    plugin_on_session_end(
        agent,
        completed,
        interrupted,
        total_turns,
        session_started_hooks_fired,
    );
}

pub fn session_end_hooks(
    agent: &AgentLoop,
    messages: &[Message],
    completed: bool,
    interrupted: bool,
    total_turns: u32,
    session_started_hooks_fired: bool,
) {
    agent.memory_on_session_end(messages);
    plugin_on_session_end(
        agent,
        completed,
        interrupted,
        total_turns,
        session_started_hooks_fired,
    );
}

pub(crate) fn plugin_on_session_end(
    agent: &AgentLoop,
    completed: bool,
    interrupted: bool,
    total_turns: u32,
    session_started_hooks_fired: bool,
) {
    let hook_ctx = serde_json::json!({
        "session_id": agent.config().session_id.as_deref().unwrap_or(""),
        "completed": completed,
        "finished_naturally": completed,
        "interrupted": interrupted,
        "turns": total_turns,
        "model": crate::runtime_provider::active_model(agent),
        "platform": agent.config().platform.as_deref().unwrap_or(""),
        "session_started_hooks_fired": session_started_hooks_fired,
    });
    let _results = invoke_hook(agent, HookType::OnSessionEnd, &hook_ctx);
}

// ---------------------------------------------------------------------------
// Status emission
// ---------------------------------------------------------------------------

pub(crate) fn emit_status(agent: &AgentLoop, event_type: &str, message: &str) {
    if agent.config().quiet_mode {
        return;
    }
    if let Some(cb) = agent.callbacks.status_callback.as_ref() {
        cb(event_type, message);
    }
}

// ---------------------------------------------------------------------------
// Tool failure notices
// ---------------------------------------------------------------------------

pub(crate) fn emit_tool_failure_notices(
    agent: &AgentLoop,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
) {
    for tc in tool_calls {
        let Some(result) = results
            .iter()
            .find(|r| r.tool_call_id == tc.id && r.is_error)
        else {
            continue;
        };
        if let Some(msg) =
            crate::agent_loop::summarize_tool_failure_for_user(&tc.function.name, &result.content)
        {
            emit_status(agent, "tool_failure", &msg);
        }
    }
}

// ---------------------------------------------------------------------------
// Thinking delta
// ---------------------------------------------------------------------------

pub(crate) fn emit_thinking_delta(agent: &AgentLoop, text: &str) {
    if text.trim().is_empty() {
        return;
    }
    if let Some(cb) = agent.callbacks.on_thinking.as_ref() {
        cb(text);
    }
}

pub(crate) fn emit_reasoning_from_message(agent: &AgentLoop, message: &Message) {
    if let Some(reasoning) = message.reasoning_content.as_deref() {
        emit_thinking_delta(agent, reasoning);
    }
}
