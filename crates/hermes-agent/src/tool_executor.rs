//! Tool batch dispatch (parity with run_agent handle_function_call).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use hermes_core::{ToolCall, ToolError, ToolResult};
use tokio::task::JoinSet;
use serde_json::Value;

use crate::agent_loop::{
    inject_runtime_tool_params, is_contextlattice_shell_invocation, looks_like_tool_error_output,
    AgentLoop,
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
        let mut join_set = JoinSet::new();
        let tool_concurrency = if hermes_tools::should_parallelize_tool_batch(tool_calls) {
            tool_concurrency.max(1)
        } else {
            1
        };
        let mut results = Vec::with_capacity(tool_calls.len());
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();
        let async_tool_dispatch = self.async_tool_dispatch.clone();
        let active_task_id = self.current_task_id();
        let mut dedupe_search_seen: HashMap<String, String> = HashMap::new();
        let mut dedupe_search_dups: Vec<(String, String)> = Vec::new();

        // Run orchestrated `delegate_task` calls sequentially in the caller's
        // task - this keeps the inner AgentLoop future out of the Send-bound
        // JoinSet and preserves the requested concurrency cap which is already
        // applied upstream via `cap_delegates`.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
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
                // Orchestrator internally runs the child on its own
                // `tokio::spawn` task, which erases the child future and breaks
                // async recursion between parent / child `execute_tool_calls`.
                let output = orch.execute(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
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
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            let async_tool_dispatch = async_tool_dispatch.clone();
            let max_delegate_depth = max_delegate_depth;
            let current_delegate_depth = current_delegate_depth;
            let parent_budget_remaining_usd = parent_budget_remaining_usd;
            let active_task_id = active_task_id.clone();
            let current_user_task = current_user_task.clone();

            join_set.spawn(async move {
                let started = Instant::now();
                let dispatch_result = if let Some(dispatch) = async_tool_dispatch.as_ref() {
                    tracing::debug!(tool = %tool_name, "agent tool call start (async dispatch)");
                    let mut params: Value = match serde_json::from_str(&raw_args) {
                        Ok(v) => v,
                        Err(e) => {
                            let error_msg = format!(
                                "Invalid JSON params for tool '{}': {}. \
                                 Please check your parameters and retry with valid JSON.",
                                tool_name, e
                            );
                            return ToolResult::err(&tool_call_id, error_msg);
                        }
                    };
                    inject_runtime_tool_params(
                        &tool_name,
                        &mut params,
                        active_task_id.as_deref(),
                        current_user_task.as_deref(),
                    );
                    if tool_name == "delegate_task" {
                        if current_delegate_depth >= max_delegate_depth {
                            return ToolResult::err(
                                &tool_call_id,
                                format!(
                                    "Delegation depth limit reached ({}/{}).",
                                    current_delegate_depth, max_delegate_depth
                                ),
                            );
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
                    dispatch(tool_name.clone(), params).await
                } else {
                    match registry.get(&tool_name) {
                        Some(entry) => {
                            tracing::debug!(tool = %tool_name, "agent tool call start");
                            let mut params: Value = match serde_json::from_str(&raw_args) {
                                Ok(v) => v,
                                Err(e) => {
                                    let error_msg = format!(
                                        "Invalid JSON params for tool '{}': {}. \
                                         Please check your parameters and retry with valid JSON.",
                                        tool_name, e
                                    );
                                    return ToolResult::err(&tool_call_id, error_msg);
                                }
                            };
                            inject_runtime_tool_params(
                                &tool_name,
                                &mut params,
                                active_task_id.as_deref(),
                                current_user_task.as_deref(),
                            );
                            if tool_name == "delegate_task" {
                                if current_delegate_depth >= max_delegate_depth {
                                    return ToolResult::err(
                                        &tool_call_id,
                                        format!(
                                            "Delegation depth limit reached ({}/{}).",
                                            current_delegate_depth, max_delegate_depth
                                        ),
                                    );
                                }
                                if let Some(obj) = params.as_object_mut() {
                                    obj.insert(
                                        "child_depth".to_string(),
                                        Value::from(current_delegate_depth + 1),
                                    );
                                    obj.insert(
                                        "max_depth".to_string(),
                                        Value::from(max_delegate_depth),
                                    );
                                    if let Some(remaining) = parent_budget_remaining_usd {
                                        obj.insert(
                                            "parent_budget_remaining_usd".to_string(),
                                            Value::from(remaining),
                                        );
                                    }
                                }
                            }
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
                        match Self::tool_result_from_dispatch_output(output) {
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
            });
            if join_set.len() >= tool_concurrency {
                if let Some(result) = join_set.join_next().await {
                    match result {
                        Ok(tool_result) => {
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
                        Err(e) => {
                            tracing::error!("Task join error: {}", e);
                        }
                    }
                }
            }
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

        results
    }

}
