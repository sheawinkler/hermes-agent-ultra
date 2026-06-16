//! Real code execution backend: run snippets via tokio::process::Command.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use crate::tools::code_execution::CodeExecutionBackend;
use hermes_core::ToolError;

/// Code execution backend using local non-Python interpreters.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs,
        }
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

fn hermes_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HERMES_HOME")
        .or_else(|| std::env::var_os("HERMES_AGENT_ULTRA_HOME"))
        .map(std::path::PathBuf::from)
}

fn real_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HERMES_REAL_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(std::path::PathBuf::from))
}

fn code_subprocess_home() -> Option<std::path::PathBuf> {
    match std::env::var("TERMINAL_HOME_MODE")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("profile") | Some("isolated") => {
            let home = hermes_home_dir()?.join("home");
            if std::fs::create_dir_all(&home).is_ok() {
                Some(home)
            } else {
                real_home_dir()
            }
        }
        _ => real_home_dir(),
    }
}

fn apply_code_subprocess_home(cmd: &mut TokioCommand) {
    if let Some(real_home) = real_home_dir() {
        cmd.env("HERMES_REAL_HOME", real_home);
    }
    if let Some(home) = code_subprocess_home() {
        cmd.env("HOME", home);
    }
}

#[async_trait]
impl CodeExecutionBackend for LocalCodeExecutionBackend {
    async fn execute(
        &self,
        code: &str,
        language: Option<&str>,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        let Some(lang) = language else {
            return Err(ToolError::InvalidParams(
                "Missing language parameter; Hermes Agent Ultra does not default to Python in the Rust-only runtime".into(),
            ));
        };
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);

        let (interpreter, flag) = match lang {
            "python" | "python3" => {
                return Err(ToolError::InvalidParams(
                    "Python execution is disabled in Hermes Agent Ultra's Rust-only runtime".into(),
                ))
            }
            "javascript" | "js" | "node" => ("node", "-e"),
            "typescript" | "ts" => ("npx", ""),
            "bash" | "sh" => ("bash", "-c"),
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {}",
                    other
                )))
            }
        };

        let mut cmd = TokioCommand::new(interpreter);
        if lang == "typescript" {
            // For TypeScript, write to temp file and run with ts-node
            let tmp = std::env::temp_dir().join(format!("hermes_exec_{}.ts", uuid::Uuid::new_v4()));
            tokio::fs::write(&tmp, code).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to write temp file: {}", e))
            })?;
            cmd = TokioCommand::new("npx");
            cmd.arg("ts-node").arg(tmp.to_str().unwrap_or(""));
        } else {
            cmd.arg(flag).arg(code);
        }

        apply_code_subprocess_home(&mut cmd);
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

    #[tokio::test]
    async fn missing_language_does_not_default_to_python() {
        let backend = LocalCodeExecutionBackend::default();
        let err = backend
            .execute("print(1)", None, Some(1))
            .await
            .expect_err("missing language should fail");
        assert!(err
            .to_string()
            .contains("does not default to Python in the Rust-only runtime"));
    }

    #[tokio::test]
    async fn python_execution_is_disabled() {
        let backend = LocalCodeExecutionBackend::default();
        let err = backend
            .execute("print(1)", Some("python"), Some(1))
            .await
            .expect_err("python should fail");
        assert!(err.to_string().contains("Python execution is disabled"));
    }

    #[tokio::test]
    async fn profile_home_mode_sets_home_for_shell_snippets() {
        let real = tempfile::tempdir().expect("real home");
        let hermes = tempfile::tempdir().expect("hermes home");
        let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());
        let _real_home = EnvGuard::set("HERMES_REAL_HOME", real.path().to_string_lossy().as_ref());
        let _hermes_home = EnvGuard::set("HERMES_HOME", hermes.path().to_string_lossy().as_ref());
        let _mode = EnvGuard::set("TERMINAL_HOME_MODE", "profile");
        let backend = LocalCodeExecutionBackend::default();

        let raw = backend
            .execute(
                "printf '%s|%s' \"$HOME\" \"$HERMES_REAL_HOME\"",
                Some("bash"),
                Some(5),
            )
            .await
            .expect("bash should run");
        let payload: serde_json::Value = serde_json::from_str(&raw).expect("json payload");
        assert_eq!(
            payload["stdout"].as_str().unwrap(),
            format!(
                "{}|{}",
                hermes.path().join("home").display(),
                real.path().display()
            )
        );
    }

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }
}
