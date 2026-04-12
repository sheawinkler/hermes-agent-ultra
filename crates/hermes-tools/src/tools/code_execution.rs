//! Code execution tool: run Python scripts with hermes_tools RPC

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
    /// Execute Python code and return the output.
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

/// Tool for executing code (primarily Python) in a sandboxed environment.
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

        let language = params.get("language").and_then(|v| v.as_str());
        let timeout = params.get("timeout").and_then(|v| v.as_u64());

        self.backend.execute(code, language, timeout).await
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
                "description": "Programming language (default: 'python')",
                "enum": ["python", "javascript", "typescript"],
                "default": "python"
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
            "Execute code in a sandboxed environment. Supports Python, JavaScript, and TypeScript.",
            JsonSchema::object(props, vec!["code".into()]),
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
        assert_eq!(handler.schema().name, "execute_code");
    }

    #[tokio::test]
    async fn test_execute_code() {
        let handler = ExecuteCodeHandler::new(Arc::new(MockCodeBackend));
        let result = handler
            .execute(json!({"code": "print(1+1)"}))
            .await
            .unwrap();
        assert!(result.contains("print(1+1)"));
    }
}
