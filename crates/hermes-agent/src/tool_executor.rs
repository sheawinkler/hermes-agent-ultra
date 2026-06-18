//! Tool batch dispatch (parity with run_agent handle_function_call).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use futures::future::join_all;
use hermes_core::{Message, ToolCall, ToolError, ToolResult};
use serde_json::Value;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::Instrument as _;
use tracing::debug_span;

use crate::agent_loop::{
    AgentLoop, inject_runtime_tool_params, is_contextlattice_shell_invocation,
    looks_like_tool_error_output,
};

impl AgentLoop {
    pub(crate) async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        tool_concurrency: usize,
        contextlattice_connect_intent: bool,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<hermes_core::ToolErrorRecord>,
        mut checkpoint_mgr: Option<&mut hermes_tools::CheckpointManager>,
        current_user_task: Option<String>,
    ) -> Vec<ToolResult> {
        let batch_start = Instant::now();
        let mut join_set = JoinSet::new();
        let tool_concurrency = if hermes_tools::should_parallelize_tool_batch(tool_calls) {
            tool_concurrency.max(1)
        } else {
            1
        };
        let sem = Arc::new(Semaphore::new(tool_concurrency));
        let mut results = Vec::with_capacity(tool_calls.len());
        let max_delegate_depth = crate::tool_executor::resolve_max_delegate_depth(self);
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();
        let async_tool_dispatch = self.async_tool_dispatch.clone();
        let active_task_id = self.current_task_id();
        let mut dedupe_search_seen: HashMap<String, String> = HashMap::new();
        let mut dedupe_search_dups: Vec<(String, String)> = Vec::new();
        let plan_phase = self.plan_phase();

        // Run orchestrated `delegate_task` calls concurrently — each future is already
        // spawned internally by the orchestrator, so join_all drives them in parallel
        // without putting a non-Send AgentLoop future into a Send-bound JoinSet.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
            let mut pending_ids: Vec<String> = Vec::new();
            let mut pending_futs = Vec::new();
            for tc in tool_calls {
                if tc.function.name != "delegate_task" {
                    continue;
                }
                if current_delegate_depth >= max_delegate_depth {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                let parsed: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            format!(
                                "Invalid JSON params for tool 'delegate_task': {}. \
                                 Please retry with valid JSON.",
                                e
                            ),
                        ));
                        continue;
                    }
                };
                let task = parsed
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if task.trim().is_empty() {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        "delegate_task requires non-empty 'task' string.",
                    ));
                    continue;
                }
                let req = crate::sub_agent_orchestrator::SubAgentRequest {
                    task,
                    context: parsed
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    toolset: parsed
                        .get("toolset")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    model: parsed
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    child_depth: current_delegate_depth + 1,
                    max_depth: max_delegate_depth,
                    parent_budget_remaining_usd,
                    inherited_tool_schemas: Vec::new(),
                };
                pending_ids.push(tc.id.clone());
                pending_futs.push(orch.execute(req));
            }
            if !pending_futs.is_empty() {
                let outputs = join_all(pending_futs).await;
                for (id, output) in pending_ids.into_iter().zip(outputs) {
                    orchestrated.push(ToolResult::ok(&id, output));
                }
            }
        }

        for tc in tool_calls {
            // Skip `delegate_task` when an orchestrator already handled it.
            if orchestrator.is_some() && tc.function.name == "delegate_task" {
                continue;
            }
            if tc.function.name == "search_files" {
                if let Some(original_id) = dedupe_search_seen.get(&tc.function.arguments).cloned() {
                    dedupe_search_dups.push((tc.id.clone(), original_id.clone()));
                    tracing::debug!(
                        tool = "search_files",
                        duplicate_tool_call_id = %tc.id,
                        original_tool_call_id = %original_id,
                        "agent tool call deduplicated"
                    );
                    continue;
                }
                dedupe_search_seen.insert(tc.function.arguments.clone(), tc.id.clone());
            }
            if let Some(ref mut mgr) = checkpoint_mgr {
                if let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) {
                    if matches!(tc.function.name.as_str(), "write_file" | "patch") {
                        if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                            let _ = mgr.ensure_checkpoint(Path::new(path), "pre-tool");
                        }
                    } else if tc.function.name == "terminal" {
                        if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                            if hermes_tools::is_destructive_command(cmd) {
                                let cwd = args
                                    .get("cwd")
                                    .and_then(|v| v.as_str())
                                    .map(Path::new)
                                    .unwrap_or_else(|| Path::new("."));
                                let _ = mgr.ensure_checkpoint(cwd, "pre-terminal");
                            }
                        }
                    }
                }
            }
            if contextlattice_connect_intent
                && tc.function.name == "terminal"
                && is_contextlattice_shell_invocation(&tc.function.arguments)
            {
                let msg = "ContextLattice integration requests must use `contextlattice_search` / `contextlattice_context_pack`, not shell command `contextlattice`. Retry by calling `contextlattice_search` first with a scoped query.".to_string();
                tool_errors.push(hermes_core::ToolErrorRecord {
                    tool_name: tc.function.name.clone(),
                    error: msg.clone(),
                    turn,
                });
                results.push(ToolResult::err(&tc.id, msg));
                continue;
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            // Parse JSON once here; move Value into spawned task instead
            // of cloning the raw JSON string and re-parsing inside the task.
            let mut params: Value = match serde_json::from_str(&tc.function.arguments) {
                Ok(v) => v,
                Err(e) => {
                    let error_msg = format!(
                        "Invalid JSON params for tool '{}': {}. Please check your parameters and retry with valid JSON.",
                        tool_name, e
                    );
                    results.push(ToolResult::err(&tool_call_id, error_msg));
                    continue;
                }
            };
            inject_runtime_tool_params(
                &tool_name,
                &mut params,
                active_task_id.as_deref(),
                current_user_task.as_deref(),
            );
            if !hermes_tools::plan_allows_tool(plan_phase, &tool_name, &params) {
                let block = hermes_tools::plan_block_payload(&tool_name);
                let msg = serde_json::from_str::<Value>(&block)
                    .ok()
                    .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                    .unwrap_or(block);
                results.push(ToolResult::err(&tool_call_id, msg));
                continue;
            }
            let deps = hermes_config::deps_for_tool(&tool_name);
            if !deps.is_empty()
                && deps
                    .iter()
                    .any(|dep| !hermes_config::dep_is_available(*dep))
            {
                let status_cb = self.callbacks.status_callback.clone();
                let notify = Arc::new(move |msg: String| {
                    if let Some(cb) = &status_cb {
                        cb("dep_install", &msg);
                    }
                });
                if !hermes_config::await_tool_deps(&tool_name, notify).await {
                    let missing = hermes_config::dep_gate::missing_dep_labels(deps);
                    results.push(ToolResult::err(
                        &tool_call_id,
                        format!("运行时依赖安装未完成 ({missing})，无法执行 `{tool_name}`"),
                    ));
                    continue;
                }
            }
            if tool_name == "delegate_task" {
                if current_delegate_depth >= max_delegate_depth {
                    results.push(ToolResult::err(
                        &tool_call_id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                if let Some(obj) = params.as_object_mut() {
                    obj.insert(
                        "child_depth".to_string(),
                        Value::from(current_delegate_depth + 1),
                    );
                    obj.insert("max_depth".to_string(), Value::from(max_delegate_depth));
                    if let Some(remaining) = parent_budget_remaining_usd {
                        obj.insert(
                            "parent_budget_remaining_usd".to_string(),
                            Value::from(remaining),
                        );
                    }
                }
            }
            let registry = self.tool_registry.clone();
            let async_tool_dispatch = async_tool_dispatch.clone();

            let _permit = Arc::clone(&sem)
                .acquire_owned()
                .await
                .expect("semaphore closed");
            let tool_span = debug_span!("tool_call", tool = %tool_name, id = %tool_call_id);
            join_set.spawn(async move {
                let _permit = _permit; // held for task lifetime; dropped on completion
                let started = Instant::now();
                let dispatch_result = if let Some(dispatch) = async_tool_dispatch.as_ref() {
                    tracing::debug!(tool = %tool_name, "agent tool call start (async dispatch)");
                    dispatch(tool_name.clone(), params).await
                } else {
                    match registry.get(&tool_name) {
                        Some(entry) => {
                            tracing::debug!(tool = %tool_name, "agent tool call start");
                            let handler = Arc::clone(&entry.handler);
                            match tokio::task::spawn_blocking(move || handler(params)).await {
                                Ok(result) => result,
                                Err(e) => Err(ToolError::ExecutionFailed(format!(
                                    "Tool blocking task join failed: {e}"
                                ))),
                            }
                        }
                        None => {
                            let available = registry.names().join(", ");
                            let error_msg = format!(
                                "Unknown tool '{}'. Available tools: [{}]",
                                tool_name, available
                            );
                            return ToolResult::err(&tool_call_id, error_msg);
                        }
                    }
                };

                match dispatch_result {
                    Ok(output) if async_tool_dispatch.is_some() => {
                        match tool_result_from_dispatch_output(output) {
                            Ok(output) => {
                                tracing::debug!(
                                    tool = %tool_name,
                                    elapsed_ms = started.elapsed().as_millis() as u64,
                                    output_chars = output.chars().count(),
                                    "agent tool call finished"
                                );
                                if looks_like_tool_error_output(&output) {
                                    ToolResult::err(&tool_call_id, output)
                                } else {
                                    ToolResult::ok(&tool_call_id, output)
                                }
                            }
                            Err(e) => {
                                tracing::debug!(
                                    tool = %tool_name,
                                    elapsed_ms = started.elapsed().as_millis() as u64,
                                    error = %e,
                                    "agent tool call failed"
                                );
                                ToolResult::err(&tool_call_id, e.to_string())
                            }
                        }
                    }
                    Ok(output) => {
                        tracing::debug!(
                            tool = %tool_name,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            output_chars = output.chars().count(),
                            "agent tool call finished"
                        );
                        if looks_like_tool_error_output(&output) {
                            ToolResult::err(&tool_call_id, output)
                        } else {
                            ToolResult::ok(&tool_call_id, output)
                        }
                    }
                    Err(e) => {
                        tracing::debug!(
                            tool = %tool_name,
                            elapsed_ms = started.elapsed().as_millis() as u64,
                            error = %e,
                            "agent tool call failed"
                        );
                        ToolResult::err(&tool_call_id, e.to_string())
                    }
                }
            }.instrument(tool_span));
        }

        for tool_result in orchestrated {
            if tool_result.is_error {
                let tc = tool_calls
                    .iter()
                    .find(|tc| tc.id == tool_result.tool_call_id);
                if let Some(tc) = tc {
                    tool_errors.push(hermes_core::ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: tool_result.content.clone(),
                        turn,
                    });
                }
            }
            results.push(tool_result);
        }
        while let Some(result) = join_set.join_next().await {
            match result {
                Ok(tool_result) => {
                    if tool_result.is_error {
                        // Record the error but we still add the result to context
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(hermes_core::ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    results.push(tool_result);
                }
                Err(e) => {
                    tracing::error!("Task join error: {}", e);
                }
            }
        }
        if !dedupe_search_dups.is_empty() {
            let mut by_id: HashMap<String, ToolResult> = HashMap::new();
            for result in &results {
                by_id.insert(result.tool_call_id.clone(), result.clone());
            }
            for (dup_id, original_id) in dedupe_search_dups {
                if let Some(original) = by_id.get(&original_id) {
                    results.push(ToolResult {
                        tool_call_id: dup_id,
                        content: original.content.clone(),
                        is_error: original.is_error,
                    });
                }
            }
        }

        tracing::debug!(
            turn,
            tool_count = tool_calls.len(),
            elapsed_ms = batch_start.elapsed().as_millis() as u64,
            "tool batch complete"
        );
        results
    }
}

// ---------------------------------------------------------------------------
// Tool helper functions (extracted from `impl AgentLoop` in agent_loop.rs)
// ---------------------------------------------------------------------------

pub(crate) fn tool_result_from_dispatch_output(
    output: String,
) -> Result<String, hermes_core::ToolError> {
    if let Ok(value) = serde_json::from_str::<Value>(&output) {
        if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
            return Err(hermes_core::ToolError::ExecutionFailed(err.to_string()));
        }
    }
    Ok(output)
}

/// Remove duplicate tool calls that share the same function name and arguments.
pub(crate) fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for tc in calls {
        let key = format!("{}:{}", tc.function.name, tc.function.arguments);
        if seen.insert(key) {
            deduped.push(tc.clone());
        } else {
            tracing::warn!("Deduplicated tool call: {}", tc.function.name);
        }
    }
    deduped
}

/// Try to repair an unknown tool name via case-insensitive or substring matching.
/// Returns `true` if the tool call was repaired.
pub(crate) fn repair_tool_call(agent: &AgentLoop, tc: &mut ToolCall) -> bool {
    if agent.tool_registry.get(&tc.function.name).is_some() {
        return false;
    }
    let names = agent.tool_registry.names();
    let Some(fixed) = crate::agent_runtime_helpers::repair_tool_name(&tc.function.name, &names)
    else {
        return false;
    };
    tracing::info!("Repaired tool call: '{}' → '{}'", tc.function.name, fixed);
    tc.function.name = fixed;
    true
}

/// Inject current session id into `session_search` calls when absent.
pub(crate) fn hydrate_session_search_args(agent: &AgentLoop, tc: &mut ToolCall) {
    if tc.function.name != "session_search" {
        return;
    }
    let Some(session_id) = agent
        .config()
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let session_id = session_id.as_str();
    if session_id.is_empty() {
        return;
    }

    let mut args: Value =
        serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| serde_json::json!({}));
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    let has_current = obj
        .get("current_session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some();
    if has_current {
        return;
    }
    obj.insert(
        "current_session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    if let Ok(updated) = serde_json::to_string(&args) {
        tc.function.arguments = updated;
    }
}

/// Cap concurrent delegate_task calls based on config.
pub(crate) fn cap_delegates(agent: &AgentLoop, tool_calls: &mut Vec<ToolCall>) {
    fn delegation_spawning_paused() -> bool {
        std::env::var("HERMES_DELEGATION_PAUSED")
            .ok()
            .map(|raw| {
                matches!(
                    raw.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }
    if delegation_spawning_paused() {
        let delegate_count = tool_calls
            .iter()
            .filter(|tc| tc.function.name == "delegate_task")
            .count();
        if delegate_count > 0 {
            tracing::warn!(
                "Dropping {} delegate_task call(s): delegation spawning is paused",
                delegate_count
            );
            tool_calls.retain(|tc| tc.function.name != "delegate_task");
        }
        return;
    }
    let delegate_count = tool_calls
        .iter()
        .filter(|tc| tc.function.name == "delegate_task")
        .count() as u32;
    if delegate_count > agent.config().max_concurrent_delegates {
        tracing::warn!(
            "Capping delegate_task calls from {} to {}",
            delegate_count,
            agent.config().max_concurrent_delegates
        );
        let mut kept_delegates = 0u32;
        tool_calls.retain(|tc| {
            if tc.function.name == "delegate_task" {
                if kept_delegates < agent.config().max_concurrent_delegates {
                    kept_delegates += 1;
                    true
                } else {
                    false
                }
            } else {
                true
            }
        });
    }
}

/// Execute a batch of tool calls in parallel using a JoinSet.
pub(crate) fn resolve_max_delegate_depth(agent: &AgentLoop) -> u32 {
    fn normalize_delegate_depth(value: u32) -> u32 {
        value.max(1)
    }
    fn parse_delegate_depth(raw: &str) -> Option<u32> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        trimmed.parse().ok().map(normalize_delegate_depth)
    }
    std::env::var("HERMES_MAX_DELEGATE_DEPTH")
        .ok()
        .and_then(|v| parse_delegate_depth(&v))
        .unwrap_or_else(|| normalize_delegate_depth(agent.config().max_delegate_depth))
}

pub(crate) fn coerce_textual_tool_calls(mut m: Message) -> (Message, Vec<ToolCall>, bool) {
    let declared = m.tool_calls.clone().unwrap_or_default();
    if !declared.is_empty() {
        return (m, declared, false);
    }
    let Some(content) = m.content.as_deref() else {
        return (m, Vec::new(), false);
    };
    let (plain_text, parsed_calls) = hermes_core::separate_text_and_calls(content);
    if parsed_calls.is_empty() {
        return (m, Vec::new(), false);
    }
    m.tool_calls = Some(parsed_calls.clone());
    let trimmed = plain_text.trim();
    m.content = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    };
    (m, parsed_calls, true)
}

fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
    if tc.function.name != "memory" {
        return None;
    }
    let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_lowercase();
    if action != "add" && action != "replace" && action != "remove" {
        return None;
    }
    let target = args
        .get("target")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("memory")
        .to_string();
    let content = if action == "remove" {
        args.get("old_text")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    } else {
        args.get("content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_string()
    };
    Some((action, target, content))
}

fn delegation_event_from_tool_result(
    tc: &ToolCall,
    result: &hermes_core::ToolResult,
) -> Option<(String, String)> {
    if tc.function.name != "delegate_task" || result.is_error {
        return None;
    }
    let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
    let task = args
        .get("task")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();

    let sub_agent_id = serde_json::from_str::<Value>(&result.content)
        .ok()
        .and_then(|v| {
            v.get("sub_agent_id")
                .and_then(|id| id.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_default();

    Some((task, sub_agent_id))
}

pub(crate) fn notify_memory_writes(
    agent: &AgentLoop,
    tool_calls: &[ToolCall],
    results: &[hermes_core::ToolResult],
) {
    if agent.config().skip_memory {
        return;
    }
    let Some(ref mm) = agent.memory_manager else {
        return;
    };
    let Ok(mut mm) = mm.lock() else {
        return;
    };
    for result in results {
        if result.is_error {
            continue;
        }
        let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
            continue;
        };
        let Some((action, target, content)) = memory_write_event_from_tool_call(tc) else {
            continue;
        };
        mm.on_memory_write(&action, &target, &content);
    }
}

pub(crate) fn notify_delegations(
    agent: &AgentLoop,
    tool_calls: &[ToolCall],
    results: &[hermes_core::ToolResult],
) {
    if agent.config().skip_memory {
        return;
    }
    let Some(ref mm) = agent.memory_manager else {
        return;
    };
    let Ok(mm) = mm.lock() else {
        return;
    };
    for result in results {
        let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
            continue;
        };
        let Some((task, sub_agent_id)) = delegation_event_from_tool_result(tc, result) else {
            continue;
        };
        mm.on_delegation(&task, &sub_agent_id);
    }
}

pub(crate) fn memory_on_turn_start(agent: &AgentLoop, turn: u32, message: &str) {
    if let Some(ref mm) = agent.memory_manager {
        if let Ok(mut mm) = mm.lock() {
            mm.on_turn_start(turn, message);
        }
    }
}

pub(crate) fn memory_system_prompt(agent: &AgentLoop) -> String {
    if agent.config().skip_memory {
        return String::new();
    }
    if let Some(ref mm) = agent.memory_manager {
        if let Ok(mm) = mm.lock() {
            return mm.build_system_prompt();
        }
    }
    String::new()
}

pub(crate) fn memory_pre_compress_note(agent: &AgentLoop, messages: &[Message]) -> Option<String> {
    if agent.config().skip_memory {
        return None;
    }
    let Some(ref mm) = agent.memory_manager else {
        return None;
    };
    let Ok(mm) = mm.lock() else {
        return None;
    };
    let as_values: Vec<Value> = messages
        .iter()
        .filter_map(|m| serde_json::to_value(m).ok())
        .collect();
    let note = mm.on_pre_compress(&as_values);
    if note.trim().is_empty() {
        None
    } else {
        Some(note)
    }
}
