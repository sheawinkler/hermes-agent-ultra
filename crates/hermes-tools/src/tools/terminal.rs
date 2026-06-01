//! Terminal tools: terminal command execution and process management

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::OnceLock;

use hermes_core::{
    tool_schema, CommandOutput, JsonSchema, TerminalBackend, ToolError, ToolHandler, ToolSchema,
};

use crate::approval::{ApprovalDecision, ApprovalManager};

const DEFAULT_FOREGROUND_MAX_TIMEOUT_SECS: u64 = 600;

fn foreground_max_timeout_secs() -> u64 {
    std::env::var("TERMINAL_MAX_FOREGROUND_TIMEOUT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_FOREGROUND_MAX_TIMEOUT_SECS)
}

fn has_unquoted_trailing_background_operator(command: &str) -> bool {
    let mut idx = command.len();
    while idx > 0 {
        let Some((prev, ch)) = command[..idx].char_indices().next_back() else {
            return false;
        };
        if !ch.is_whitespace() {
            idx = prev;
            break;
        }
        idx = prev;
    }
    if !command[idx..].starts_with('&') {
        return false;
    }
    let mut slash_count = 0usize;
    let mut pos = idx;
    while pos > 0 {
        let Some((prev, ch)) = command[..pos].char_indices().next_back() else {
            break;
        };
        if ch != '\\' {
            break;
        }
        slash_count += 1;
        pos = prev;
    }
    slash_count.is_multiple_of(2)
}

fn foreground_background_wrapper_error(command: &str) -> Option<String> {
    let lower = command.trim().to_ascii_lowercase();
    let padded = format!(" {lower} ");
    if has_unquoted_trailing_background_operator(command) {
        return Some(
            "Foreground command uses '&' shell backgrounding. Use terminal(background=true) so Hermes can track lifecycle and output.".to_string(),
        );
    }
    if lower.starts_with("nohup ") || padded.contains(" nohup ") {
        return Some(
            "Foreground command uses nohup. Use terminal(background=true) instead of shell-level background wrappers.".to_string(),
        );
    }
    if lower.starts_with("setsid ") || padded.contains(" setsid ") || padded.contains(" disown ") {
        return Some(
            "Foreground command uses setsid/disown. Use terminal(background=true) so Hermes can manage the process.".to_string(),
        );
    }
    None
}

fn is_help_variant(command: &str) -> bool {
    command
        .split_ascii_whitespace()
        .any(|part| matches!(part, "--help" | "-h" | "help"))
}

fn long_lived_foreground_error(command: &str) -> Option<String> {
    if is_help_variant(command) {
        return None;
    }
    let lower = command.trim().to_ascii_lowercase();
    let padded = format!(" {lower} ");
    let long_lived = [
        " pnpm dev ",
        " npm run dev ",
        " yarn dev ",
        " bun dev ",
        " next dev ",
        " vite ",
        " webpack serve ",
        " cargo watch ",
        " python -m http.server ",
        " python3 -m http.server ",
        " uvicorn ",
        " gunicorn ",
    ]
    .iter()
    .any(|needle| padded.contains(needle));

    long_lived.then(|| {
        "This foreground command appears to start a long-lived server/watch process. Run it with background=true, then verify readiness with logs or a health check.".to_string()
    })
}

// ---------------------------------------------------------------------------
// TerminalHandler
// ---------------------------------------------------------------------------

/// Tool for executing terminal commands via an injected backend.
pub struct TerminalHandler {
    backend: std::sync::Arc<dyn TerminalBackend>,
    approval: ApprovalManager,
}

impl TerminalHandler {
    pub fn new(backend: std::sync::Arc<dyn TerminalBackend>) -> Self {
        Self {
            backend,
            approval: ApprovalManager::new(),
        }
    }
}

#[async_trait]
impl ToolHandler for TerminalHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let command = params
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'command' parameter".into()))?;

        let timeout = params.get("timeout").and_then(|v| v.as_u64());
        let background = params
            .get("background")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !background {
            if let Some(timeout) = timeout {
                let max_timeout = foreground_max_timeout_secs();
                if timeout > max_timeout {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Foreground timeout {timeout}s exceeds max {max_timeout}s. Use background=true with process polling for longer commands."
                    )));
                }
            }
            if let Some(error) = foreground_background_wrapper_error(command) {
                return Err(ToolError::ExecutionFailed(error));
            }
            if let Some(error) = long_lived_foreground_error(command) {
                return Err(ToolError::ExecutionFailed(error));
            }
        }

        match self.approval.check_approval_from_env(command, "local") {
            ApprovalDecision::Denied => {
                return Err(ToolError::ExecutionFailed(format!(
                    "Command denied by security policy: {}",
                    command
                )));
            }
            ApprovalDecision::RequiresConfirmation => {
                tracing::warn!(
                    command,
                    "command requires explicit confirmation; denying because no user approval was supplied"
                );
                return Err(ToolError::ExecutionFailed(format!(
                    "Command requires explicit user approval and was not executed: {command}. Do NOT retry, rephrase, or achieve the same outcome via a different command. Silence is not consent."
                )));
            }
            ApprovalDecision::Approved => {}
        }

        let workdir = params.get("workdir").and_then(|v| v.as_str());

        let pty = params.get("pty").and_then(|v| v.as_bool()).unwrap_or(false);
        let stdin_data = params.get("stdin_data").and_then(|v| v.as_str());

        let transformed_command = transform_sudo_command(command);

        match self
            .backend
            .execute_command_with_stdin(
                &transformed_command,
                timeout,
                workdir,
                background,
                pty,
                stdin_data,
            )
            .await
        {
            Ok(output) => Ok(format_command_output(&output)),
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "command".into(),
            json!({
                "type": "string",
                "description": "The command to execute"
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": format!(
                    "Timeout in seconds. Foreground commands above {}s are rejected; use background=true for longer jobs.",
                    foreground_max_timeout_secs()
                )
            }),
        );
        props.insert(
            "workdir".into(),
            json!({
                "type": "string",
                "description": "Working directory for the command"
            }),
        );
        props.insert(
            "background".into(),
            json!({
                "type": "boolean",
                "description": "Run command in background (default: false)",
                "default": false
            }),
        );
        props.insert(
            "pty".into(),
            json!({
                "type": "boolean",
                "description": "Run command in PTY mode for interactive programs (default: false)",
                "default": false
            }),
        );
        props.insert(
            "stdin_data".into(),
            json!({
                "type": "string",
                "description": "Optional data piped to command stdin. Use this for large payloads instead of embedding content directly in the command."
            }),
        );

        tool_schema(
            "terminal",
            "Execute a terminal command. Returns stdout, stderr, and exit code.",
            JsonSchema::object(props, vec!["command".into()]),
        )
    }
}

/// Format command output for display.
fn format_command_output(output: &CommandOutput) -> String {
    let mut result = String::new();
    if !output.stdout.is_empty() {
        result.push_str(&output.stdout);
    }
    if !output.stderr.is_empty() {
        if !result.is_empty() {
            result.push_str("\n--- STDERR ---\n");
        }
        result.push_str(&output.stderr);
    }
    if output.exit_code != 0 {
        result.push_str(&format!("\n[exit code: {}]", output.exit_code));
    }
    if result.is_empty() {
        result = format!("[exit code: {}]", output.exit_code);
    }
    result
}

fn sudo_word_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\bsudo\b").expect("valid sudo regex"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn transform_sudo_command_with_password(command: &str, sudo_password: Option<&str>) -> String {
    let Some(password) = sudo_password.filter(|v| !v.is_empty()) else {
        return command.to_string();
    };
    if !sudo_word_regex().is_match(command) {
        return command.to_string();
    }
    let replacement = format!("echo {} | sudo -S -p ''", shell_quote(password));
    sudo_word_regex()
        .replace_all(command, replacement.as_str())
        .into_owned()
}

fn transform_sudo_command(command: &str) -> String {
    let sudo_password = std::env::var("SUDO_PASSWORD").ok();
    transform_sudo_command_with_password(command, sudo_password.as_deref())
}

fn coerce_string_param(params: &Value, key: &str) -> Option<String> {
    match params.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Bool(b)) => Some(b.to_string()),
        Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn process_id_param(params: &Value) -> Result<String, ToolError> {
    coerce_string_param(params, "session_id")
        .or_else(|| coerce_string_param(params, "pid"))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            ToolError::InvalidParams("Missing 'session_id' parameter (or deprecated 'pid')".into())
        })
}

fn process_data_param_with_alias(params: &Value, alias: &str) -> Result<String, ToolError> {
    coerce_string_param(params, "data")
        .or_else(|| coerce_string_param(params, alias))
        .ok_or_else(|| {
            ToolError::InvalidParams(format!("Missing 'data' parameter (or alias '{}')", alias))
        })
}

fn process_data_param(params: &Value) -> Result<String, ToolError> {
    process_data_param_with_alias(params, "input")
}

// ---------------------------------------------------------------------------
// ProcessHandler
// ---------------------------------------------------------------------------

/// Backend trait for process management operations.
#[async_trait]
pub trait ProcessBackend: Send + Sync {
    /// List all background processes.
    async fn list_processes(&self) -> Result<String, ToolError>;
    /// Poll a process for output (non-blocking).
    async fn poll_process(&self, session_id: &str) -> Result<String, ToolError>;
    /// Read process output logs.
    async fn read_process_log(
        &self,
        session_id: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, ToolError>;
    /// Wait for a process to complete and return its full output.
    async fn wait_process(
        &self,
        session_id: &str,
        timeout: Option<u64>,
    ) -> Result<String, ToolError>;
    /// Kill a background process.
    async fn kill_process(&self, session_id: &str) -> Result<String, ToolError>;
    /// Write stdin to a running process.
    async fn write_stdin(&self, session_id: &str, data: &str) -> Result<String, ToolError>;
    /// Submit input to a process and get output.
    async fn submit_process(&self, session_id: &str, input: &str) -> Result<String, ToolError>;
    /// Close stdin of a process.
    async fn close_process(&self, session_id: &str) -> Result<String, ToolError>;
}

/// Tool for managing background processes.
pub struct ProcessHandler {
    backend: std::sync::Arc<dyn ProcessBackend>,
}

impl ProcessHandler {
    pub fn new(backend: std::sync::Arc<dyn ProcessBackend>) -> Self {
        Self { backend }
    }
}

/// Adapter that forwards process operations through `TerminalBackend`.
pub struct TerminalProcessBackendAdapter {
    backend: std::sync::Arc<dyn TerminalBackend>,
}

impl TerminalProcessBackendAdapter {
    pub fn new(backend: std::sync::Arc<dyn TerminalBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ProcessBackend for TerminalProcessBackendAdapter {
    async fn list_processes(&self) -> Result<String, ToolError> {
        self.backend
            .list_processes()
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn poll_process(&self, session_id: &str) -> Result<String, ToolError> {
        self.backend
            .poll_process(session_id)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn read_process_log(
        &self,
        session_id: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, ToolError> {
        self.backend
            .read_process_log(session_id, offset, limit)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn wait_process(
        &self,
        session_id: &str,
        timeout: Option<u64>,
    ) -> Result<String, ToolError> {
        self.backend
            .wait_process(session_id, timeout)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn kill_process(&self, session_id: &str) -> Result<String, ToolError> {
        self.backend
            .kill_process(session_id)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn write_stdin(&self, session_id: &str, data: &str) -> Result<String, ToolError> {
        self.backend
            .write_process_stdin(session_id, data)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn submit_process(&self, session_id: &str, input: &str) -> Result<String, ToolError> {
        self.backend
            .submit_process_stdin(session_id, input)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    async fn close_process(&self, session_id: &str) -> Result<String, ToolError> {
        self.backend
            .close_process_stdin(session_id)
            .await
            .map(|v| v.to_string())
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }
}

#[async_trait]
impl ToolHandler for ProcessHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "list" => self.backend.list_processes().await,
            "poll" => {
                let session_id = process_id_param(&params)?;
                self.backend.poll_process(&session_id).await
            }
            "log" => {
                let session_id = process_id_param(&params)?;
                let offset = params.get("offset").and_then(|v| v.as_u64());
                let limit = params.get("limit").and_then(|v| v.as_u64());
                self.backend
                    .read_process_log(&session_id, offset, limit)
                    .await
            }
            "wait" => {
                let session_id = process_id_param(&params)?;
                let timeout = params.get("timeout").and_then(|v| v.as_u64());
                self.backend.wait_process(&session_id, timeout).await
            }
            "kill" => {
                let session_id = process_id_param(&params)?;
                self.backend.kill_process(&session_id).await
            }
            "write" => {
                let session_id = process_id_param(&params)?;
                let data = process_data_param(&params)?;
                self.backend.write_stdin(&session_id, &data).await
            }
            "submit" => {
                let session_id = process_id_param(&params)?;
                let input = process_data_param_with_alias(&params, "input")?;
                self.backend.submit_process(&session_id, &input).await
            }
            "close" => {
                let session_id = process_id_param(&params)?;
                self.backend.close_process(&session_id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: {}",
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
                "description": "Action to perform: list, poll, log, wait, kill, write, submit, close",
                "enum": ["list", "poll", "log", "wait", "kill", "write", "submit", "close"]
            }),
        );
        props.insert(
            "session_id".into(),
            json!({
                "type": "string",
                "description": "Process session identifier returned from terminal(background=true)"
            }),
        );
        props.insert(
            "pid".into(),
            json!({
                "type": "string",
                "description": "Deprecated alias for session_id"
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Timeout in seconds for 'wait' action"
            }),
        );
        props.insert(
            "offset".into(),
            json!({
                "type": "integer",
                "description": "Starting line offset for 'log' action"
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "description": "Maximum lines to return for 'log' action"
            }),
        );
        props.insert(
            "data".into(),
            json!({
                "type": "string",
                "description": "Data to write/submit to process stdin (for 'write' and 'submit')"
            }),
        );
        props.insert(
            "input".into(),
            json!({
                "type": "string",
                "description": "Alias for data when using 'submit'"
            }),
        );

        tool_schema(
            "process",
            "Manage background process sessions: list, poll, read logs, wait, kill, write/submit stdin, or close stdin.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::TEST_ENV_LOCK;
    use hermes_core::AgentError;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, old }
        }

        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                std::env::set_var(self.key, old);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    struct MockBackend;
    #[async_trait]
    impl TerminalBackend for MockBackend {
        async fn execute_command(
            &self,
            cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _bg: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: format!("output of: {}", cmd),
                stderr: String::new(),
            })
        }
        async fn execute_command_with_stdin(
            &self,
            cmd: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
            stdin_data: Option<&str>,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: format!("output of: {} / stdin={}", cmd, stdin_data.unwrap_or("")),
                stderr: String::new(),
            })
        }
        async fn read_file(
            &self,
            _path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            Ok(String::new())
        }
        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }
        async fn file_exists(&self, _path: &str) -> Result<bool, AgentError> {
            Ok(true)
        }
    }

    struct MockProcessBackend;

    #[async_trait]
    impl ProcessBackend for MockProcessBackend {
        async fn list_processes(&self) -> Result<String, ToolError> {
            Ok(json!({"status":"ok","count":1}).to_string())
        }

        async fn poll_process(&self, session_id: &str) -> Result<String, ToolError> {
            Ok(json!({"status":"running","session_id":session_id}).to_string())
        }

        async fn read_process_log(
            &self,
            session_id: &str,
            offset: Option<u64>,
            limit: Option<u64>,
        ) -> Result<String, ToolError> {
            Ok(json!({
                "status":"running",
                "session_id": session_id,
                "offset": offset,
                "limit": limit
            })
            .to_string())
        }

        async fn wait_process(
            &self,
            session_id: &str,
            timeout: Option<u64>,
        ) -> Result<String, ToolError> {
            Ok(json!({"status":"exited","session_id":session_id,"timeout":timeout}).to_string())
        }

        async fn kill_process(&self, session_id: &str) -> Result<String, ToolError> {
            Ok(json!({"status":"killed","session_id":session_id}).to_string())
        }

        async fn write_stdin(&self, session_id: &str, data: &str) -> Result<String, ToolError> {
            Ok(json!({"status":"ok","session_id":session_id,"data":data}).to_string())
        }

        async fn submit_process(&self, session_id: &str, input: &str) -> Result<String, ToolError> {
            Ok(json!({"status":"ok","session_id":session_id,"input":input}).to_string())
        }

        async fn close_process(&self, session_id: &str) -> Result<String, ToolError> {
            Ok(json!({"status":"ok","session_id":session_id,"closed":true}).to_string())
        }
    }

    #[tokio::test]
    async fn test_terminal_handler_schema() {
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "terminal");
        let timeout_desc = schema
            .parameters
            .properties
            .as_ref()
            .expect("properties")
            .get("timeout")
            .and_then(Value::as_object)
            .and_then(|obj| obj.get("description"))
            .and_then(Value::as_str)
            .expect("timeout description");
        assert!(timeout_desc.contains("600s"));
        assert!(timeout_desc.contains("background=true"));
    }

    #[test]
    fn test_terminal_handler_execute() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let result = block_on(handler.execute(json!({"command": "echo hello"}))).unwrap();
        assert!(result.contains("echo hello"));
    }

    #[test]
    fn test_terminal_handler_rejects_foreground_timeout_above_cap() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _max = EnvGuard::remove("TERMINAL_MAX_FOREGROUND_TIMEOUT");
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));

        let err = block_on(handler.execute(json!({"command": "echo hello", "timeout": 9999})))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("9999s"));
        assert!(msg.contains("600s"));
        assert!(msg.contains("background=true"));
    }

    #[test]
    fn test_terminal_handler_allows_background_timeout_above_cap() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _max = EnvGuard::remove("TERMINAL_MAX_FOREGROUND_TIMEOUT");
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));

        let result = block_on(handler.execute(json!({
            "command": "python server.py",
            "background": true,
            "timeout": 9999
        })))
        .unwrap();
        assert!(result.contains("python server.py"));
    }

    #[test]
    fn test_terminal_handler_rejects_shell_background_wrappers_in_foreground() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));

        let err = block_on(handler.execute(json!({
            "command": "nohup pnpm dev > /tmp/hermes-server.log 2>&1 &"
        })))
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("background=true"));
        assert!(msg.to_ascii_lowercase().contains("background"));
    }

    #[test]
    fn test_terminal_handler_rejects_long_lived_server_foreground() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));

        let err = block_on(handler.execute(json!({"command": "pnpm dev"}))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.to_ascii_lowercase().contains("long-lived"));
        assert!(msg.contains("background=true"));
    }

    #[test]
    fn test_terminal_handler_allows_help_variant_for_server_command() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));

        let result = block_on(handler.execute(json!({"command": "pnpm dev --help"}))).unwrap();
        assert!(result.contains("pnpm dev --help"));
    }

    #[test]
    fn test_terminal_handler_execute_with_stdin_data() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let result =
            block_on(handler.execute(json!({"command": "cat", "stdin_data": "abc123"}))).unwrap();
        assert!(result.contains("stdin=abc123"));
    }

    #[test]
    fn test_terminal_handler_denies_confirmation_without_consent() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let err = block_on(handler.execute(json!({"command": "sudo apt update"}))).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Do NOT retry"));
        assert!(msg.contains("rephrase"));
        assert!(msg.contains("same outcome"));
        assert!(msg.contains("Silence is not consent"));
    }

    #[test]
    fn test_terminal_handler_yolo_bypasses_recoverable_confirmation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::set("HERMES_YOLO_MODE", "1");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let result =
            block_on(handler.execute(json!({"command": "rm -rf /tmp/hermes-safe-test"}))).unwrap();
        assert!(result.contains("rm -rf /tmp/hermes-safe-test"));
    }

    #[test]
    fn test_terminal_handler_yolo_does_not_bypass_hardline() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::set("HERMES_YOLO_MODE", "1");
        let _session = EnvGuard::remove("HERMES_SESSION_KEY");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let err = block_on(handler.execute(json!({"command": "rm -rf /"}))).unwrap_err();
        assert!(err.to_string().contains("denied by security policy"));
    }

    #[test]
    fn test_terminal_handler_session_yolo_bypasses_recoverable_confirmation() {
        let _lock = TEST_ENV_LOCK.lock().unwrap();
        let _yolo = EnvGuard::remove("HERMES_YOLO_MODE");
        let _session = EnvGuard::set("HERMES_SESSION_KEY", "terminal-session-yolo");
        let _sudo = EnvGuard::remove("SUDO_PASSWORD");
        crate::approval::clear_session("terminal-session-yolo");
        crate::approval::enable_session_yolo("terminal-session-yolo");

        let handler = TerminalHandler::new(std::sync::Arc::new(MockBackend));
        let result =
            block_on(handler.execute(json!({"command": "rm -rf /tmp/hermes-safe-test"}))).unwrap();
        assert!(result.contains("rm -rf /tmp/hermes-safe-test"));

        crate::approval::clear_session("terminal-session-yolo");
    }

    #[tokio::test]
    async fn test_process_handler_uses_session_id_and_log_action() {
        let handler = ProcessHandler::new(std::sync::Arc::new(MockProcessBackend));
        let poll_result = handler
            .execute(json!({"action":"poll","session_id":"proc_123"}))
            .await
            .unwrap();
        assert!(poll_result.contains("\"session_id\":\"proc_123\""));

        let log_result = handler
            .execute(json!({"action":"log","session_id":"proc_123","offset":10,"limit":50}))
            .await
            .unwrap();
        assert!(log_result.contains("\"offset\":10"));
        assert!(log_result.contains("\"limit\":50"));
    }

    #[tokio::test]
    async fn test_process_handler_coerces_non_string_args() {
        let handler = ProcessHandler::new(std::sync::Arc::new(MockProcessBackend));
        let write_result = handler
            .execute(json!({"action":"write","session_id":123,"data":456}))
            .await
            .unwrap();
        assert!(write_result.contains("\"session_id\":\"123\""));
        assert!(write_result.contains("\"data\":\"456\""));
    }

    #[test]
    fn test_transform_sudo_command_quotes_password() {
        let transformed =
            transform_sudo_command_with_password("sudo apt install curl", Some("pa'ss$(whoami)"));
        assert!(transformed.contains("echo 'pa'\"'\"'ss$(whoami)' | sudo -S -p ''"));
        assert!(transformed.ends_with(" apt install curl"));
    }

    #[test]
    fn test_transform_sudo_command_without_password_is_unchanged() {
        let transformed = transform_sudo_command_with_password("sudo apt update", None);
        assert_eq!(transformed, "sudo apt update");
    }

    #[test]
    fn test_transform_sudo_command_with_non_sudo_is_unchanged() {
        let transformed =
            transform_sudo_command_with_password("echo hello", Some("secret-password"));
        assert_eq!(transformed, "echo hello");
    }

    #[test]
    fn test_format_command_output() {
        let output = CommandOutput {
            exit_code: 0,
            stdout: "hello".to_string(),
            stderr: String::new(),
        };
        assert_eq!(format_command_output(&output), "hello");

        let output_with_stderr = CommandOutput {
            exit_code: 1,
            stdout: "out".to_string(),
            stderr: "err".to_string(),
        };
        let formatted = format_command_output(&output_with_stderr);
        assert!(formatted.contains("out"));
        assert!(formatted.contains("err"));
        assert!(formatted.contains("exit code: 1"));
    }
}
