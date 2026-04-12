//! Memory tool: add, replace, remove operations on persistent memory

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{
    tool_schema, AgentError, JsonSchema, MemoryProvider, ToolError, ToolHandler, ToolSchema,
};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// MemoryBackend trait (extends MemoryProvider for memory-specific ops)
// ---------------------------------------------------------------------------

/// Backend for persistent memory operations (MEMORY.md and USER.md).
#[async_trait]
pub trait MemoryBackend: Send + Sync {
    /// Add a memory entry.
    async fn add(&self, key: &str, value: &str) -> Result<String, ToolError>;
    /// Replace a memory entry.
    async fn replace(&self, key: &str, value: &str) -> Result<String, ToolError>;
    /// Remove a memory entry.
    async fn remove(&self, key: &str) -> Result<String, ToolError>;
    /// List all memory entries.
    async fn list(&self) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// MemoryHandler
// ---------------------------------------------------------------------------

/// Tool for managing persistent memory (MEMORY.md and USER.md).
pub struct MemoryHandler {
    backend: Arc<dyn MemoryBackend>,
}

impl MemoryHandler {
    pub fn new(backend: Arc<dyn MemoryBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for MemoryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "add" => {
                let key = params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
                let value = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'value' parameter".into()))?;
                self.backend.add(key, value).await
            }
            "replace" => {
                let key = params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
                let value = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'value' parameter".into()))?;
                self.backend.replace(key, value).await
            }
            "remove" => {
                let key = params
                    .get("key")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'key' parameter".into()))?;
                self.backend.remove(key).await
            }
            "list" => self.backend.list().await,
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'add', 'replace', 'remove', or 'list'.",
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
                "description": "Action to perform: add, replace, remove, or list",
                "enum": ["add", "replace", "remove", "list"]
            }),
        );
        props.insert(
            "key".into(),
            json!({
                "type": "string",
                "description": "Memory key (for add, replace, remove)"
            }),
        );
        props.insert(
            "value".into(),
            json!({
                "type": "string",
                "description": "Memory value (for add, replace)"
            }),
        );

        tool_schema(
            "memory",
            "Manage persistent memory entries stored in MEMORY.md and USER.md. Supports add, replace, remove, and list operations.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockMemoryBackend;
    #[async_trait]
    impl MemoryBackend for MockMemoryBackend {
        async fn add(&self, key: &str, value: &str) -> Result<String, ToolError> {
            Ok(format!("Added: {} = {}", key, value))
        }
        async fn replace(&self, key: &str, value: &str) -> Result<String, ToolError> {
            Ok(format!("Replaced: {} = {}", key, value))
        }
        async fn remove(&self, key: &str) -> Result<String, ToolError> {
            Ok(format!("Removed: {}", key))
        }
        async fn list(&self) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
    }

    #[tokio::test]
    async fn test_memory_add() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        let result = handler
            .execute(json!({"action": "add", "key": "name", "value": "Hermes"}))
            .await
            .unwrap();
        assert!(result.contains("Added"));
    }

    #[tokio::test]
    async fn test_memory_list() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_memory_schema() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        assert_eq!(handler.schema().name, "memory");
    }
}
