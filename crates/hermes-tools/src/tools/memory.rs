//! Memory tool: add, replace, remove operations on persistent memory

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// MemoryBackend trait (extends MemoryProvider for memory-specific ops)
// ---------------------------------------------------------------------------

/// Backend for persistent memory operations (MEMORY.md and USER.md).
#[async_trait]
pub trait MemoryBackend: Send + Sync {
    /// Add a memory entry.
    async fn add(&self, target: &str, content: &str) -> Result<String, ToolError>;
    /// Replace a memory entry.
    async fn replace(
        &self,
        target: &str,
        old_text: &str,
        new_content: &str,
    ) -> Result<String, ToolError>;
    /// Remove a memory entry.
    async fn remove(&self, target: &str, old_text: &str) -> Result<String, ToolError>;
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
        let target = params
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'target' parameter".into()))?;
        if target != "memory" && target != "user" {
            return Err(ToolError::InvalidParams(
                "Invalid 'target' parameter. Use 'memory' or 'user'.".into(),
            ));
        }

        match action {
            "add" => {
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'content' parameter".into())
                    })?;
                self.backend.add(target, content).await
            }
            "replace" => {
                let old_text =
                    params
                        .get("old_text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ToolError::InvalidParams("Missing 'old_text' parameter".into())
                        })?;
                let content = params
                    .get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("Missing 'content' parameter".into())
                    })?;
                self.backend.replace(target, old_text, content).await
            }
            "remove" => {
                let old_text =
                    params
                        .get("old_text")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ToolError::InvalidParams("Missing 'old_text' parameter".into())
                        })?;
                self.backend.remove(target, old_text).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use 'add', 'replace', or 'remove'.",
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
                "description": "Action to perform.",
                "enum": ["add", "replace", "remove"]
            }),
        );
        props.insert(
            "target".into(),
            json!({
                "type": "string",
                "description": "Which memory store to update.",
                "enum": ["memory", "user"]
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Entry content (required for add and replace)."
            }),
        );
        props.insert(
            "old_text".into(),
            json!({
                "type": "string",
                "description": "Short unique substring identifying entry to replace or remove."
            }),
        );

        tool_schema(
            "memory",
            "Save durable information to persistent memory across sessions. Use target='user' for user preferences and target='memory' for environment/workflow notes. Do not store temporary task progress.",
            JsonSchema::object(props, vec!["action".into(), "target".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockMemoryBackend;
    #[async_trait]
    impl MemoryBackend for MockMemoryBackend {
        async fn add(&self, target: &str, content: &str) -> Result<String, ToolError> {
            Ok(format!("Added: {} => {}", target, content))
        }
        async fn replace(
            &self,
            target: &str,
            old_text: &str,
            new_content: &str,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Replaced: {} => {} -> {}",
                target, old_text, new_content
            ))
        }
        async fn remove(&self, target: &str, old_text: &str) -> Result<String, ToolError> {
            Ok(format!("Removed: {} => {}", target, old_text))
        }
    }

    #[tokio::test]
    async fn test_memory_add() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        let result = handler
            .execute(json!({"action": "add", "target": "memory", "content": "User prefers concise answers"}))
            .await
            .unwrap();
        assert!(result.contains("Added"));
    }

    #[tokio::test]
    async fn test_memory_replace() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        let result = handler
            .execute(
                json!({"action":"replace","target":"user","old_text":"prefers concise","content":"Prefers concise Chinese answers"}),
            )
            .await
            .unwrap();
        assert!(result.contains("Replaced"));
    }

    #[tokio::test]
    async fn test_memory_schema() {
        let handler = MemoryHandler::new(Arc::new(MockMemoryBackend));
        assert_eq!(handler.schema().name, "memory");
    }
}
