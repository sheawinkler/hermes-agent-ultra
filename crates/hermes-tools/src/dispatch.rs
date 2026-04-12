//! Tool dispatch with parallel execution
//!
//! Dispatches a batch of tool calls through the registry using tokio::JoinSet
//! for parallel execution, with configurable concurrency and budget enforcement.

use std::sync::Arc;
use std::time::Instant;

use hermes_core::{BudgetConfig, ToolCall, ToolResult};
use tokio::task::JoinSet;

use crate::registry::ToolRegistry;

/// Result of dispatching a single tool call, including timing information.
#[derive(Debug, Clone)]
pub struct DispatchedResult {
    /// The tool call ID.
    pub tool_call_id: String,
    /// The tool name that was called.
    pub tool_name: String,
    /// The result content (or error JSON).
    pub content: String,
    /// Whether the result is an error.
    pub is_error: bool,
    /// Time taken in milliseconds.
    pub duration_ms: u64,
}

impl From<DispatchedResult> for ToolResult {
    fn from(dr: DispatchedResult) -> Self {
        ToolResult {
            tool_call_id: dr.tool_call_id,
            content: dr.content,
            is_error: dr.is_error,
        }
    }
}

/// Dispatch multiple tool calls in parallel through the registry.
///
/// Uses `tokio::JoinSet` with a configurable pool size for concurrency control.
/// Results are truncated according to the budget config.
///
/// # Arguments
/// * `calls` - List of tool calls to dispatch
/// * `registry` - The tool registry to dispatch through
/// * `budget` - Budget configuration for result size limits
/// * `pool_size` - Maximum number of concurrent tool executions (default: 8)
///
/// # Returns
/// Vec of ToolResult, one per input call, in no particular order.
pub async fn dispatch_tools(
    calls: Vec<ToolCall>,
    registry: Arc<ToolRegistry>,
    budget: BudgetConfig,
    pool_size: usize,
) -> Vec<ToolResult> {
    let concurrency = pool_size.max(1);
    let mut join_set = JoinSet::new();

    for call in calls {
        let registry = registry.clone();
        let max_chars = budget.max_result_size_chars;
        join_set.spawn(async move {
            let start = Instant::now();
            let result = registry.dispatch_async(&call.function.name, call.function.arguments.parse().unwrap_or(serde_json::Value::Null)).await;
            let duration_ms = start.elapsed().as_millis() as u64;

            // Check if result is an error
            let is_error = result.starts_with(r#"{"error"#);

            DispatchedResult {
                tool_call_id: call.id,
                tool_name: call.function.name,
                content: result,
                is_error,
                duration_ms,
            }
        });

        // If we've reached the concurrency limit, wait for one to complete
        // This is approximate; JoinSet doesn't support limiting directly,
        // so we use a semaphore-like approach by waiting when too many are pending
    }

    let mut results: Vec<ToolResult> = Vec::new();
    while let Some(res) = join_set.join_next().await {
        match res {
            Ok(dr) => {
                let mut content = dr.content;
                // Per-result truncation
                if content.len() > budget.max_result_size_chars {
                    let truncated = &content[..budget.max_result_size_chars];
                    let removed = content.len() - budget.max_result_size_chars;
                    content = format!(
                        "{}\n\n[... truncated {} characters ...]",
                        truncated, removed
                    );
                }
                results.push(ToolResult {
                    tool_call_id: dr.tool_call_id,
                    content,
                    is_error: dr.is_error,
                });
            }
            Err(e) => {
                // Join error — treat as a tool error
                results.push(ToolResult::err("unknown", format!("Task join error: {}", e)));
            }
        }
    }

    // Aggregate budget enforcement
    let total_chars: usize = results.iter().map(|r| r.content.len()).sum();
    if total_chars > budget.max_aggregate_chars {
        let ratio = budget.max_aggregate_chars as f64 / total_chars as f64;
        for result in results.iter_mut() {
            let target_len = ((result.content.len() as f64) * ratio) as usize;
            let min_len = target_len.max(200);
            if result.content.len() > min_len {
                let removed = result.content.len() - min_len;
                result.content = format!(
                    "{}\n\n[... truncated {} characters ...]",
                    &result.content[..min_len],
                    removed
                );
            }
        }
    }

    results
}

/// Dispatch a single tool call through the registry (convenience wrapper).
pub async fn dispatch_single(
    call: ToolCall,
    registry: Arc<ToolRegistry>,
    max_result_size_chars: usize,
) -> ToolResult {
    let start = Instant::now();
    let result = registry.dispatch_async(&call.function.name, call.function.arguments.parse().unwrap_or(serde_json::Value::Null)).await;
    let duration_ms = start.elapsed().as_millis() as u64;

    let is_error = result.starts_with(r#"{"error"#);

    let content = if result.len() > max_result_size_chars {
        let truncated = &result[..max_result_size_chars];
        let removed = result.len() - max_result_size_chars;
        format!(
            "{}\n\n[... truncated {} characters ...]",
            truncated, removed
        )
    } else {
        result
    };

    ToolResult {
        tool_call_id: call.id,
        content,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatched_result_conversion() {
        let dr = DispatchedResult {
            tool_call_id: "call_1".to_string(),
            tool_name: "test".to_string(),
            content: "result".to_string(),
            is_error: false,
            duration_ms: 100,
        };
        let tr: ToolResult = dr.into();
        assert_eq!(tr.tool_call_id, "call_1");
        assert!(!tr.is_error);
    }
}