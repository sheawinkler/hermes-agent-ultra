//! Local code execution backend for explicitly approved, uncontained host execution.
//!
//! This backend owns process lifetime and temporary files, but it does not claim
//! filesystem, network, or OS sandbox isolation. Callers must perform the same
//! policy and approval dispatch as terminal execution before invoking it.

use async_trait::async_trait;
use serde_json::json;
use std::fs::File;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tempfile::TempDir;
use tokio::process::Command as TokioCommand;

use crate::bounded_subprocess::{
    run_bounded_command_with_output_limit, BoundedCommandError, BoundedOutputDirectoryLimit,
};
use crate::tools::code_execution::CodeExecutionBackend;
use hermes_core::{
    subprocess::CommandNoWindowExt, subprocess_env::hermes_subprocess_env, ToolError,
};

const MAX_CODE_WORKSPACE_ENTRIES: usize = 2;
const MAX_CODE_WORKSPACE_BYTES: u64 = 1_048_576;

/// Code execution backend using local non-Python interpreters.
///
/// It is deliberately paired with [`ExecuteCodeHandler`](crate::tools::code_execution::ExecuteCodeHandler),
/// which applies the terminal-equivalent approval guard. This type manages a
/// bounded host process tree; it is not a sandbox.
pub struct LocalCodeExecutionBackend {
    default_timeout_secs: u64,
    temp_parent: Option<PathBuf>,
}

impl LocalCodeExecutionBackend {
    pub fn new(default_timeout_secs: u64) -> Self {
        Self {
            default_timeout_secs,
            temp_parent: None,
        }
    }

    #[cfg(test)]
    fn with_temp_parent(mut self, temp_parent: PathBuf) -> Self {
        self.temp_parent = Some(temp_parent);
        self
    }

    fn execution_workspace(&self) -> Result<TempDir, ToolError> {
        let mut builder = tempfile::Builder::new();
        builder.prefix("hermes_exec_");
        match self.temp_parent.as_ref() {
            Some(parent) => builder.tempdir_in(parent),
            None => builder.tempdir(),
        }
        .map_err(|error| ToolError::ExecutionFailed(format!("Failed to create code workspace: {error}")))
    }
}

impl Default for LocalCodeExecutionBackend {
    fn default() -> Self {
        Self::new(30)
    }
}

fn hermes_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_HOME")
        .or_else(|| std::env::var_os("HERMES_AGENT_ULTRA_HOME"))
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn real_home_dir() -> Option<PathBuf> {
    std::env::var_os("HERMES_REAL_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

fn code_subprocess_home() -> Result<Option<PathBuf>, ToolError> {
    match std::env::var("TERMINAL_HOME_MODE")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("profile") | Some("isolated") => {
            let hermes_home = hermes_home_dir().ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "TERMINAL_HOME_MODE=profile requires HERMES_HOME or HERMES_AGENT_ULTRA_HOME"
                        .into(),
                )
            })?;
            let home = hermes_home.join("home");
            std::fs::create_dir_all(&home).map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "Failed to create isolated code-execution HOME '{}': {error}",
                    home.display()
                ))
            })?;
            Ok(Some(home))
        }
        _ => Ok(real_home_dir()),
    }
}

fn apply_code_subprocess_environment(cmd: &mut TokioCommand) -> Result<(), ToolError> {
    // Code execution is never allowed to inherit the host process environment
    // opportunistically. In particular, terminal passthrough and force knobs
    // are not an escape hatch for this path.
    cmd.env_clear();
    cmd.envs(hermes_subprocess_env(false));
    if let Some(real_home) = real_home_dir() {
        cmd.env("HERMES_REAL_HOME", real_home);
    }
    if let Some(home) = code_subprocess_home()? {
        cmd.env("HOME", home);
    }
    Ok(())
}

fn bounded_execution_error(error: BoundedCommandError, timeout_secs: u64) -> ToolError {
    match error {
        BoundedCommandError::TimedOut => ToolError::Timeout(format!(
            "Code execution timed out after {timeout_secs}s"
        )),
        other => ToolError::ExecutionFailed(format!("Code execution failed: {other:?}")),
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
        let Some(language) = language else {
            return Err(ToolError::InvalidParams(
                "Missing language parameter; Hermes Agent Ultra does not default to Python in the Rust-only runtime".into(),
            ));
        };
        let timeout_secs = timeout.unwrap_or(self.default_timeout_secs);
        let workspace = self.execution_workspace()?;
        let stdout_path = workspace.path().join("stdout");
        let stdout_file = File::create(&stdout_path).map_err(|error| {
            ToolError::ExecutionFailed(format!("Failed to create code stdout capture: {error}"))
        })?;

        let mut cmd = match language {
            "python" | "python3" => {
                return Err(ToolError::InvalidParams(
                    "Python execution is disabled until a contained backend exists".into(),
                ));
            }
            "javascript" | "js" | "node" => {
                let mut command = TokioCommand::new("node");
                command.arg("-e").arg(code);
                command
            }
            "typescript" | "ts" => {
                let source = workspace.path().join("snippet.ts");
                tokio::fs::write(&source, code).await.map_err(|error| {
                    ToolError::ExecutionFailed(format!("Failed to write TypeScript source: {error}"))
                })?;
                let mut command = TokioCommand::new("npx");
                command.arg("ts-node").arg(source);
                command
            }
            "bash" | "sh" => {
                let mut command = TokioCommand::new("bash");
                command.arg("-c").arg(code);
                command
            }
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "Unsupported language: {other}"
                )));
            }
        };

        apply_code_subprocess_environment(&mut cmd)?;
        cmd.current_dir(workspace.path())
            .stdout(Stdio::from(stdout_file))
            .stderr(Stdio::piped())
            .stdin(Stdio::null());
        cmd.suppress_windows_console();

        let output_limit = BoundedOutputDirectoryLimit::new(
            workspace.path().to_path_buf(),
            MAX_CODE_WORKSPACE_ENTRIES,
            MAX_CODE_WORKSPACE_BYTES,
        );
        let output = run_bounded_command_with_output_limit(
            cmd,
            Duration::from_secs(timeout_secs),
            true,
            output_limit,
        )
        .await
        .map_err(|error| bounded_execution_error(error, timeout_secs))?;
        let stdout = tokio::fs::read(&stdout_path).await.map_err(|error| {
            ToolError::ExecutionFailed(format!("Failed to read code stdout capture: {error}"))
        })?;

        Ok(json!({
            "exit_code": output.status.code().unwrap_or(-1),
            "stdout": String::from_utf8_lossy(&stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "language": language,
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;
    use std::ffi::OsString;

    struct EnvGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

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
    async fn python_execution_is_disabled_without_a_contained_backend() {
        let backend = LocalCodeExecutionBackend::default();
        let err = backend
            .execute("print(1)", Some("python"), Some(1))
            .await
            .expect_err("python should fail");
        assert!(err
            .to_string()
            .contains("Python execution is disabled until a contained backend exists"));
    }

    #[tokio::test]
    async fn code_execution_uses_closed_environment() {
        let _global_env = test_lock::lock();
        let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
        let backend = LocalCodeExecutionBackend::default();
        let raw = backend
            .execute("printf '%s' \"${OPENAI_API_KEY-unset}\"", Some("bash"), Some(5))
            .await
            .expect("bash should run");
        let payload: serde_json::Value = serde_json::from_str(&raw).expect("json payload");
        assert_eq!(payload["stdout"], "unset");
    }

    #[tokio::test]
    async fn profile_home_mode_never_falls_back_to_real_home() {
        let _global_env = test_lock::lock();
        let real = tempfile::tempdir().expect("real home");
        let _home = EnvGuard::set("HOME", real.path().to_string_lossy().as_ref());
        let _real_home = EnvGuard::set("HERMES_REAL_HOME", real.path().to_string_lossy().as_ref());
        let _hermes_home = EnvGuard::remove("HERMES_HOME");
        let _agent_home = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _mode = EnvGuard::set("TERMINAL_HOME_MODE", "isolated");
        let backend = LocalCodeExecutionBackend::default();

        let err = backend
            .execute("printf should-not-run", Some("bash"), Some(5))
            .await
            .expect_err("isolated profile requires a profile home");
        assert!(err
            .to_string()
            .contains("requires HERMES_HOME or HERMES_AGENT_ULTRA_HOME"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn typescript_workspace_is_removed_after_nonzero_exit() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().expect("workspace parent");
        let bin = parent.path().join("bin");
        std::fs::create_dir(&bin).expect("bin");
        let npx = bin.join("npx");
        std::fs::write(&npx, "#!/bin/sh\nexit 17\n").expect("fake npx");
        let mut permissions = std::fs::metadata(&npx).expect("npx metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&npx, permissions).expect("npx permissions");
        let original_path = std::env::var("PATH").unwrap_or_default();
        let _path = EnvGuard::set(
            "PATH",
            &format!("{}:{original_path}", bin.display()),
        );
        let backend = LocalCodeExecutionBackend::default().with_temp_parent(parent.path().into());

        let output = backend
            .execute("const value: number = 1;", Some("typescript"), Some(5))
            .await
            .expect("nonzero interpreter exit is reported as structured output");
        let payload: serde_json::Value =
            serde_json::from_str(&output).expect("code execution output JSON");
        assert_eq!(payload["exit_code"], 17);
        let leftovers = std::fs::read_dir(parent.path())
            .expect("workspace parent entries")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("hermes_exec_"))
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "temporary code workspaces leaked: {leftovers:?}");
    }
}
