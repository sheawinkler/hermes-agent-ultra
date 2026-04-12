//! Delegation tool: spawn sub-agents with isolated contexts

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// DelegationBackend trait
// ---------------------------------------------------------------------------

/// Backend for task delegation operations.
#[async_trait]
pub trait DelegationBackend: Send + Sync {
    /// Delegate a task to a sub-agent.
    async fn delegate(
        &self,
        task: &str,
        context: Option<&str>,
        toolset: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// DelegateTaskHandler
// ---------------------------------------------------------------------------

/// Tool for delegating tasks to sub-agents.
pub struct DelegateTaskHandler {
    backend: Arc<dyn DelegationBackend>,
}

impl DelegateTaskHandler {
    pub fn new(backend: Arc<dyn DelegationBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for DelegateTaskHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let task = params.get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'task' parameter".into()))?;

        let context = params.get("context").and_then(|v| v.as_str());
        let toolset = params.get("toolset").and_then(|v| v.as_str());
        let model = params.get("model").and_then(|v| v.as_str());

        self.backend.delegate(task, context, toolset, model).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("task".into(), json!({
            "type": "string",
            "description": "The task description for the sub-agent"
        }));
        props.insert("context".into(), json!({
            "type": "string",
            "description": "Additional context or instructions for the sub-agent"
        }));
        props.insert("toolset".into(), json!({
            "type": "string",
            "description": "Toolset name to assign to the sub-agent (e.g. 'web', 'terminal')"
        }));
        props.insert("model".into(), json!({
            "type": "string",
            "description": "Model to use for the sub-agent (default: same as parent)"
        }));

        tool_schema(
            "delegate_task",
            "Delegate a task to a sub-agent with an isolated context. The sub-agent will work independently and return results.",
            JsonSchema::object(props, vec!["task".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockDelegationBackend;
    #[async_trait]
    impl DelegationBackend for MockDelegationBackend {
        async fn delegate(&self, task: &str, _context: Option<&str>, _toolset: Option<&str>, _model: Option<&str>) -> Result<String, ToolError> {
            Ok(format!("Delegated task: {}", task))
        }
    }

    #[tokio::test]
    async fn test_delegate_task_schema() {
        let handler = DelegateTaskHandler::new(Arc::new(MockDelegationBackend));
        assert_eq!(handler.schema().name, "delegate_task");
    }

    #[tokio::test]
    async fn test_delegate_task_execute() {
        let handler = DelegateTaskHandler::new(Arc::new(MockDelegationBackend));
        let result = handler.execute(json!({"task": "Research AI trends"})).await.unwrap();
        assert!(result.contains("Research AI trends"));
    }
}