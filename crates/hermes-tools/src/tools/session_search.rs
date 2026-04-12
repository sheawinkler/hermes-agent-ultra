//! Session search tool: search past conversations using FTS5

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// SessionSearchBackend trait
// ---------------------------------------------------------------------------

/// Backend for searching past conversation sessions.
#[async_trait]
pub trait SessionSearchBackend: Send + Sync {
    /// Search past conversations using FTS5 full-text search.
    async fn search(&self, query: &str, limit: usize) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// SessionSearchHandler
// ---------------------------------------------------------------------------

/// Tool for searching past conversation sessions using FTS5.
pub struct SessionSearchHandler {
    backend: Arc<dyn SessionSearchBackend>,
}

impl SessionSearchHandler {
    pub fn new(backend: Arc<dyn SessionSearchBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for SessionSearchHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params.get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'query' parameter".into()))?;

        let limit = params.get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10) as usize;

        self.backend.search(query, limit).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("query".into(), json!({
            "type": "string",
            "description": "Search query for finding past conversations"
        }));
        props.insert("limit".into(), json!({
            "type": "integer",
            "description": "Maximum number of results to return (default: 10)",
            "default": 10
        }));

        tool_schema(
            "session_search",
            "Search past conversation sessions using full-text search. Returns relevant conversation snippets.",
            JsonSchema::object(props, vec!["query".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockSessionSearchBackend;
    #[async_trait]
    impl SessionSearchBackend for MockSessionSearchBackend {
        async fn search(&self, query: &str, limit: usize) -> Result<String, ToolError> {
            Ok(format!("Found {} results for '{}' (limit: {})", 0, query, limit))
        }
    }

    #[tokio::test]
    async fn test_session_search_schema() {
        let handler = SessionSearchHandler::new(Arc::new(MockSessionSearchBackend));
        assert_eq!(handler.schema().name, "session_search");
    }

    #[tokio::test]
    async fn test_session_search_execute() {
        let handler = SessionSearchHandler::new(Arc::new(MockSessionSearchBackend));
        let result = handler.execute(json!({"query": "rust async"})).await.unwrap();
        assert!(result.contains("rust async"));
    }
}