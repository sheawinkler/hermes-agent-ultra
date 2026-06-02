//! Tool dispatch with parallel execution
//!
//! Dispatches a batch of tool calls through the registry using tokio::JoinSet
//! for parallel execution, with configurable concurrency and budget enforcement.

use std::sync::Arc;
use std::time::Instant;

use hermes_core::{BudgetConfig, ToolCall, ToolResult};
use tokio::task::JoinSet;

use crate::registry::ToolRegistry;
use crate::tools::tool_result_storage::{
    default_threshold_for_tool, enforce_turn_budget, maybe_persist_tool_result,
};

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
    let mut results: Vec<ToolResult> = Vec::new();

    let mut collect_join = |res: Result<DispatchedResult, tokio::task::JoinError>| match res {
        Ok(dr) => {
            let content = maybe_persist_tool_result(
                &dr.content,
                &dr.tool_name,
                &dr.tool_call_id,
                default_threshold_for_tool(&dr.tool_name, budget.max_result_size_chars),
            );
            results.push(ToolResult {
                tool_call_id: dr.tool_call_id,
                content,
                is_error: dr.is_error,
            });
        }
        Err(e) => {
            // Join error — treat as a tool error
            results.push(ToolResult::err(
                "unknown",
                format!("Task join error: {}", e),
            ));
        }
    };

    for call in calls {
        let registry = registry.clone();
        join_set.spawn(async move {
            let start = Instant::now();
            let result = registry
                .dispatch_async(
                    &call.function.name,
                    call.function
                        .arguments
                        .parse()
                        .unwrap_or(serde_json::Value::Null),
                )
                .await;
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

        // Cap pending tasks to the requested concurrency.
        if join_set.len() >= concurrency {
            if let Some(res) = join_set.join_next().await {
                collect_join(res);
            }
        }
    }

    while let Some(res) = join_set.join_next().await {
        collect_join(res);
    }

    enforce_turn_budget(&mut results, &budget);

    results
}

/// Dispatch a single tool call through the registry (convenience wrapper).
pub async fn dispatch_single(
    call: ToolCall,
    registry: Arc<ToolRegistry>,
    max_result_size_chars: usize,
) -> ToolResult {
    let result = registry
        .dispatch_async(
            &call.function.name,
            call.function
                .arguments
                .parse()
                .unwrap_or(serde_json::Value::Null),
        )
        .await;

    let is_error = result.starts_with(r#"{"error"#);

    let content = maybe_persist_tool_result(
        &result,
        &call.function.name,
        &call.id,
        default_threshold_for_tool(&call.function.name, max_result_size_chars),
    );

    ToolResult {
        tool_call_id: call.id,
        content,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use hermes_core::{tool_schema, FunctionCall, JsonSchema, ToolError, ToolHandler, ToolSchema};
    use serde_json::Value;

    use crate::tools::tool_result_storage::{PERSISTED_OUTPUT_TAG, STORAGE_ENV_LOCK};

    struct LargeHandler;

    #[async_trait]
    impl ToolHandler for LargeHandler {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            Ok(format!("large-start-{}", "x".repeat(5_000)))
        }

        fn schema(&self) -> ToolSchema {
            tool_schema("large", "Large test output", JsonSchema::new("object"))
        }
    }

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

    #[test]
    fn dispatch_single_persists_oversized_result() {
        let _guard = STORAGE_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_TOOL_RESULT_STORAGE_DIR", tmp.path());

        let registry = Arc::new(ToolRegistry::new());
        let handler = Arc::new(LargeHandler);
        registry.register(
            "large",
            "test",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            false,
            "Large output",
            "",
            None,
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime.block_on(async {
            dispatch_single(
                ToolCall {
                    id: "large_call".to_string(),
                    function: FunctionCall {
                        name: "large".to_string(),
                        arguments: "{}".to_string(),
                    },
                    extra_content: None,
                },
                registry,
                100,
            )
            .await
        });

        assert!(result.content.contains(PERSISTED_OUTPUT_TAG));
        assert!(result.content.contains("large-start"));
        let persisted =
            std::fs::read_to_string(tmp.path().join("large_call.txt")).expect("persisted result");
        assert!(persisted.contains("large-start-"));
        assert!(persisted.contains(&"x".repeat(5_000)));

        std::env::remove_var("HERMES_TOOL_RESULT_STORAGE_DIR");
    }
}
