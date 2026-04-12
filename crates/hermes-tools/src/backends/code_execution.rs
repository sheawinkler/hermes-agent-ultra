//! Real code execution backend: run scripts via tokio::process::Command.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use hermes_core::ToolError;
use crate::tools::code_execution::CodeExecutionBackend;

/// Code execution backend using local interpreters.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self { default_timeout_secs }
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    async fn execute(&self, code: &str, language: Option<&str>, timeout: Option<u64>) -> Result<String, ToolError> {
        let lang = language.unwrap_or("python");
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);

        let (interpreter, flag) = match lang {
            "python" | "python3" => ("python3", "-c"),
            "javascript" | "js" | "node" => ("node", "-e"),
            "typescript" | "ts" => ("npx", ""),
            "bash" | "sh" => ("bash", "-c"),
            other => return Err(ToolError::InvalidParams(format!("Unsupported language: {}", other))),
        };

        let mut cmd = TokioCommand::new(interpreter);
        if lang == "typescript" {
            // For TypeScript, write to temp file and run with ts-node
            let tmp = std::env::temp_dir().join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await
                .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e)))?;
            cmd = TokioCommand::new("npx");
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                let child = cmd.spawn()
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to spawn {}: {}", interpreter, e)))?;
                let output = child.wait_with_output().await
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to wait for process: {}", e)))?;
                Ok::<_, ToolError>((output.status.code().unwrap_or(-1), output.stdout, output.stderr))
            },
        ).await;

        match result {
            Ok(Ok((exit_code, stdout, stderr))) => {
                let stdout_str = String::from_utf8_lossy(&stdout).to_string();
                let stderr_str = String::from_utf8_lossy(&stderr).to_string();
                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "language": lang,
                }).to_string())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Timeout(format!("Code execution timed out after {}s", timeout_secs))),
        }
    }
}
