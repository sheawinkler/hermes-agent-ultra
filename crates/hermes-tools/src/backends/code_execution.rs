//! Code execution backend: PTC sandbox (Python) or direct interpreter fallback.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio::process::Command as TokioCommand;

use crate::ToolRegistry;
use crate::code_execution_env::prepare_child_env;
use crate::code_execution_ptc::{PtcConfig, execute_python_ptc, ptc_enabled};
use crate::tools::code_execution::CodeExecutionBackend;
use crate::tools::env_passthrough::is_env_passthrough;
use hermes_core::ToolError;

/// Code execution backend using local interpreters and optional PTC RPC sandbox.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
    tool_registry: Option<Arc<ToolRegistry>>,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs,
            tool_registry: None,
        }
    }

    pub fn with_tool_registry(registry: Arc<ToolRegistry>) -> Self {
        Self {
            default_timeout_secs: 30,
            tool_registry: Some(registry),
        }
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

/// Resolve Python interpreter candidates (first existing wins at spawn time).
fn python_interpreter_candidates() -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    if let Ok(custom) = std::env::var("HERMES_PYTHON") {
        let t = custom.trim();
        if !t.is_empty() {
            out.push(vec![t.to_string()]);
        }
    }
    out.push(vec!["python3".into()]);
    out.push(vec!["python".into()]);
    if cfg!(windows) {
        out.push(vec!["py".into(), "-3".into()]);
    }
    out
}

async fn spawn_python(
    code: &str,
    timeout_secs: u64,
) -> Result<(i32, Vec<u8>, Vec<u8>, String), ToolError> {
    let candidates = python_interpreter_candidates();
    let mut tried: Vec<String> = Vec::new();
    let mut last_err: Option<String> = None;

    for argv0 in candidates {
        let program = &argv0[0];
        tried.push(if argv0.len() > 1 {
            argv0.join(" ")
        } else {
            program.clone()
        });
        let mut cmd = TokioCommand::new(program);
        for arg in &argv0[1..] {
            cmd.arg(arg);
        }
        cmd.arg("-c").arg(code);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let source_env: BTreeMap<String, String> = std::env::vars().collect();
        let child_env = prepare_child_env(&source_env, is_env_passthrough, cfg!(windows));
        cmd.env_clear().envs(child_env);

        let spawn_result = cmd.spawn();
        let child = match spawn_result {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(format!("Failed to spawn {}: {}", program, e));
                continue;
            }
        };

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let output = child.wait_with_output().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {}", e))
            })?;
            Ok::<_, ToolError>((
                output.status.code().unwrap_or(-1),
                output.stdout,
                output.stderr,
                program.clone(),
            ))
        })
        .await;

        return match result {
            Ok(Ok(tuple)) => Ok(tuple),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Timeout(format!(
                "Code execution timed out after {}s",
                timeout_secs
            ))),
        };
    }

    Err(ToolError::ExecutionFailed(format!(
        "Failed to spawn Python (tried: {}). Last error: {}",
        tried.join(", "),
        last_err.unwrap_or_else(|| "no interpreter found".into())
    )))
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        let lang = language.unwrap_or("python");
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);

        if matches!(lang, "python" | "python3") {
            if ptc_enabled() {
                if let Some(registry) = &self.tool_registry {
                    match execute_python_ptc(
                        code,
                        registry.clone(),
                        PtcConfig {
                            timeout_secs,
                            max_tool_calls: crate::code_execution_ptc::DEFAULT_MAX_TOOL_CALLS,
                        },
                    )
                    .await
                    {
                        Ok(out) => return Ok(out),
                        Err(e) => {
                            tracing::warn!(
                                "execute_code PTC failed ({e}); falling back to inline python -c"
                            );
                        }
                    }
                }
            }
            let (exit_code, stdout, stderr, interpreter) = spawn_python(code, timeout_secs).await?;
            let stdout_str = String::from_utf8_lossy(&stdout).to_string();
            let stderr_str = String::from_utf8_lossy(&stderr).to_string();
            return Ok(json!({
                "exit_code": exit_code,
                "stdout": stdout_str,
                "stderr": stderr_str,
                "language": lang,
                "interpreter": interpreter,
            })
            .to_string());
        }

        let (interpreter, flag) = match lang {
            "javascript" | "js" | "node" => ("node", "-e"),
            "typescript" | "ts" => ("npx", ""),
            "bash" | "sh" => ("bash", "-c"),
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {}",
                    other
                )));
            }
        };

        let mut cmd = TokioCommand::new(interpreter);
        if lang == "typescript" {
            let tmp = std::env::temp_dir().join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e))
            })?;
            cmd = TokioCommand::new("npx");
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let child = cmd.spawn().map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to spawn {}: {}", interpreter, e))
            })?;
            let output = child.wait_with_output().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to wait for process: {}", e))
            })?;
            Ok::<_, ToolError>((
                output.status.code().unwrap_or(-1),
                output.stdout,
                output.stderr,
            ))
        })
        .await;

        match result {
            Ok(Ok((exit_code, stdout, stderr))) => {
                let stdout_str = String::from_utf8_lossy(&stdout).to_string();
                let stderr_str = String::from_utf8_lossy(&stderr).to_string();
                Ok(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "language": lang,
                })
                .to_string())
            }
            Ok(Err(e)) => Err(e),
            Err(_) => Err(ToolError::Timeout(format!(
                "Code execution timed out after {}s",
                timeout_secs
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_candidates_include_hermes_python_and_fallbacks() {
        hermes_core::test_env::set_var("HERMES_PYTHON", "/custom/python");
        let cands = python_interpreter_candidates();
        assert_eq!(cands[0], vec!["/custom/python".to_string()]);
        assert!(cands.iter().any(|c| c == &vec!["python3".to_string()]));
        hermes_core::test_env::remove_var("HERMES_PYTHON");
    }
}
