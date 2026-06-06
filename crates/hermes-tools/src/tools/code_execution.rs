//! Code execution tool: run local non-Python snippets through hermes_tools RPC.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// CodeExecutionBackend trait
// ---------------------------------------------------------------------------

/// Backend for code execution operations.
#[async_trait]
pub trait CodeExecutionBackend: Send + Sync {
    /// Execute code and return the output.
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// ExecuteCodeHandler
// ---------------------------------------------------------------------------

/// Tool for executing code in a sandboxed environment.
pub struct ExecuteCodeHandler {
    backend: Arc<dyn CodeExecutionBackend>,
}

impl ExecuteCodeHandler {
    pub fn new(backend: Arc<dyn CodeExecutionBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ExecuteCodeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let code = params
            .get("code")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'code' parameter".into()))?;

        let language = params
            .get("language")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParams(
                    "Missing 'language' parameter; Python is not a default in Hermes Agent Ultra"
                        .into(),
                )
            })?;
        let timeout = params.get("timeout").and_then(|v| v.as_u64());

        self.backend.execute(code, Some(language), timeout).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "code".into(),
            json!({
                "type": "string",
                "description": "The code to execute"
            }),
        );
        props.insert(
            "language".into(),
            json!({
                "type": "string",
                "description": "Programming language. Python execution is disabled in Hermes Agent Ultra's Rust-only runtime.",
                "enum": ["javascript", "typescript", "bash", "sh"]
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Execution timeout in seconds (default: 30)",
                "default": 30
            }),
        );

        tool_schema(
            "execute_code",
            "Execute code in a sandboxed environment. Supports JavaScript, TypeScript, and shell snippets; Python execution is disabled in Hermes Agent Ultra's Rust-only runtime.",
            JsonSchema::object(props, vec!["code".into(), "language".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCodeBackend;
    #[async_trait]
    impl CodeExecutionBackend for MockCodeBackend {
        async fn execute(
            &self,
            code: &str,
            _language: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, ToolError> {
            Ok(format!("Output of: {}", code))
        }
    }

    #[tokio::test]
    async fn test_execute_code_schema() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "execute_code");
        let rendered = serde_json::to_string(&schema.parameters).expect("schema json");
        assert!(!rendered.contains("\"python\""));
        assert!(!rendered.contains("web_search"));
        assert!(!rendered.contains("web_extract"));
        assert!(!rendered.contains("read_file"));
        assert!(rendered.contains("\"language\""));
    }

    #[tokio::test]
    async fn test_execute_code() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let result = handler
            .execute(json!({"code": "console.log(1+1)", "language": "javascript"}))
            .await
            .unwrap();
        assert!(result.contains("console.log(1+1)"));
    }

    #[tokio::test]
    async fn test_execute_code_requires_language() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let err = handler
            .execute(json!({"code": "print(1+1)"}))
            .await
            .expect_err("language is required");
        assert!(err.to_string().contains("Missing 'language' parameter"));
    }
}
