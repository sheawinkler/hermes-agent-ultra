//! Code execution tool for policy-approved, explicitly uncontained host snippets.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

use crate::approval::{ApprovalDecision, ApprovalManager, CommandExecutionEnvironment};
use crate::credential_guard::CredentialGuard;

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

/// Tool for executing snippets on the local host after the terminal-equivalent
/// approval guard. It does not claim sandbox or container isolation.
pub struct ExecuteCodeHandler {
    backend: Arc<dyn CodeExecutionBackend>,
    approval: ApprovalManager,
    execution_environment: CommandExecutionEnvironment,
    skill_trust_guard: CredentialGuard,
}

impl ExecuteCodeHandler {
    pub fn new(backend: Arc<dyn CodeExecutionBackend>) -> Self {
        Self::with_execution_environment(
            backend,
            CommandExecutionEnvironment::from_configured_environment(),
        )
    }

    pub fn with_execution_environment(
        backend: Arc<dyn CodeExecutionBackend>,
        execution_environment: CommandExecutionEnvironment,
    ) -> Self {
        Self {
            backend,
            approval: ApprovalManager::new(),
            execution_environment,
            skill_trust_guard: CredentialGuard::new(),
        }
    }

    fn approval_command(language: &str, code: &str) -> Result<String, ToolError> {
        let interpreter = match language {
            "python" | "python3" => {
                return Err(ToolError::InvalidParams(
                    "Python execution is unsupported until a contained backend exists".into(),
                ));
            }
            "javascript" | "js" | "node" => "node -e",
            "typescript" | "ts" => "npx ts-node",
            "bash" | "sh" => "bash -c",
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {other}"
                )));
            }
        };
        Ok(format!("{interpreter} {code}"))
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
        self.skill_trust_guard.check_effect_text(code)?;
        let approval_command = Self::approval_command(language, code)?;
        match self.approval.check_approval_for_execution(
            &approval_command,
            self.execution_environment.clone(),
            None,
        ) {
            ApprovalDecision::Denied => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Code execution denied by security policy: {approval_command}"
                )));
            }
            ApprovalDecision::RequiresConfirmation => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Code execution requires explicit user approval and was not executed: {approval_command}. Do NOT retry, rephrase, or achieve the same outcome via a different command. Silence is not consent."
                )));
            }
            ApprovalDecision::Approved => {}
        }

        self.backend.execute(code, Some(language), timeout).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "code".into(),
            json!({
                "type": "string",
                "description": "Code to execute only after policy and terminal-equivalent approval"
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
            "Execute JavaScript, TypeScript, or shell snippets on the local host after policy and explicit approval checks. This tool manages process lifetime but does not provide OS sandbox isolation. Python execution remains disabled until a contained backend exists.",
            JsonSchema::object(props, vec!["code".into(), "language".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingCodeBackend {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl CodeExecutionBackend for CountingCodeBackend {
        async fn execute(
            &self,
            _code: &str,
            _language: Option<&str>,
            _timeout: Option<u64>,
        ) -> Result<String, ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("unexpected execution".to_string())
        }
    }

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
        assert!(!schema.description.contains("sandboxed"));
        assert!(schema.description.contains("does not provide OS sandbox isolation"));
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

    #[tokio::test]
    async fn denied_code_is_not_invoked() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = ExecuteCodeHandler::with_execution_environment(
            Arc::new(CountingCodeBackend {
                calls: calls.clone(),
            }),
            CommandExecutionEnvironment::host_sensitive("execute_code"),
        );

        let error = handler
            .execute(json!({"code": "rm -rf /", "language": "bash"}))
            .await
            .expect_err("destructive code must be denied before backend invocation");
        assert!(error.to_string().contains("denied by security policy"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn python_is_refused_without_backend_invocation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = ExecuteCodeHandler::new(Arc::new(CountingCodeBackend {
            calls: calls.clone(),
        }));

        let error = handler
            .execute(json!({"code": "print(1)", "language": "python"}))
            .await
            .expect_err("Python requires a contained backend");
        assert!(error.to_string().contains("unsupported until a contained backend"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
