//! Todo management tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// TodoBackend trait
// ---------------------------------------------------------------------------

/// Backend for todo/task list management.
#[async_trait]
pub trait TodoBackend: Send + Sync {
    /// Create a new todo item.
    async fn create(
        &self,
        title: &str,
        description: Option<&str>,
        priority: Option<&str>,
    ) -> Result<String, ToolError>;
    /// Update a todo item.
    async fn update(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
        status: Option<&str>,
        priority: Option<&str>,
    ) -> Result<String, ToolError>;
    /// List all todo items.
    async fn list(&self, status: Option<&str>) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// TodoHandler
// ---------------------------------------------------------------------------

/// Tool for managing a task/todo list.
pub struct TodoHandler {
    backend: Arc<dyn TodoBackend>,
}

impl TodoHandler {
    pub fn new(backend: Arc<dyn TodoBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for TodoHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let title = params
                    .get("title")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'title' parameter".into()))?;
                let description = params.get("description").and_then(|v| v.as_str());
                let priority = params.get("priority").and_then(|v| v.as_str());
                self.backend.create(title, description, priority).await
            }
            "update" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let title = params.get("title").and_then(|v| v.as_str());
                let description = params.get("description").and_then(|v| v.as_str());
                let status = params.get("status").and_then(|v| v.as_str());
                let priority = params.get("priority").and_then(|v| v.as_str());
                self.backend
                    .update(id, title, description, status, priority)
                    .await
            }
            "list" => {
                let status = params.get("status").and_then(|v| v.as_str());
                self.backend.list(status).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'create', 'update', or 'list'.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform: create, update, or list",
                "enum": ["create", "update", "list"]
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Todo item ID (for update)"
            }),
        );
        props.insert(
            "title".into(),
            json!({
                "type": "string",
                "description": "Todo item title (for create/update)"
            }),
        );
        props.insert(
            "description".into(),
            json!({
                "type": "string",
                "description": "Todo item description"
            }),
        );
        props.insert("status".into(), json!({
            "type": "string",
            "description": "Todo status: pending, in_progress, completed (for update/list filter)",
            "enum": ["pending", "in_progress", "completed"]
        }));
        props.insert(
            "priority".into(),
            json!({
                "type": "string",
                "description": "Priority level: low, medium, high",
                "enum": ["low", "medium", "high"]
            }),
        );

        tool_schema(
            "todo",
            "Manage task/todo items: create new tasks, update existing ones, or list tasks.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTodoBackend;
    #[async_trait]
    impl TodoBackend for MockTodoBackend {
        async fn create(
            &self,
            title: &str,
            _description: Option<&str>,
            _priority: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Created todo: {}", title))
        }
        async fn update(
            &self,
            id: &str,
            _title: Option<&str>,
            _description: Option<&str>,
            _status: Option<&str>,
            _priority: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Updated todo: {}", id))
        }
        async fn list(&self, _status: Option<&str>) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
    }

    #[tokio::test]
    async fn test_todo_create() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        let result = handler
            .execute(json!({"action": "create", "title": "Test task"}))
            .await
            .unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_todo_list() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_todo_schema() {
        let handler = TodoHandler::new(Arc::new(MockTodoBackend));
        assert_eq!(handler.schema().name, "todo");
    }
}
