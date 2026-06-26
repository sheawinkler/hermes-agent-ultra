//! Process registry — spawn, manage, and monitor background processes.
//!
//! This module provides the `process_registry` tool that the LLM invokes to:
//! - **spawn**: Start a background process with stdout/stderr capture
//! - **list**: List all tracked processes with status
//! - **status**: Get detailed status of a specific process
//! - **output**: Retrieve captured stdout/stderr (ring buffer, default 10MB)
//! - **stop**: Gracefully terminate a process (SIGTERM → timeout → SIGKILL)
//! - **restart**: Stop then re-spawn a process
//!
//! Architecture:
//! ```text
//!   LLM → process_registry tool → ProcessManager
//!                                    ↓
//!                              tokio::process::Command
//!                              ↓              ↓
//!                          stdout ring    stderr ring
//! ```
//!
//! Processes are tracked in-memory with optional persistence to
//! `~/.hermes/processes.json` for recovery after restart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

// ---------------------------------------------------------------------------
// Ring buffer for captured output
// ---------------------------------------------------------------------------

/// Fixed-capacity ring buffer for process output capture.
#[derive(Debug, Clone)]
struct RingBuffer {
    data: Vec<u8>,
    capacity: usize,
    /// Write position (wraps around).
    write_pos: usize,
    /// Total bytes written (may exceed capacity).
    total_written: usize,
}

impl RingBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            data: vec![0u8; capacity],
            capacity,
            write_pos: 0,
            total_written: 0,
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.data[self.write_pos] = b;
            self.write_pos = (self.write_pos + 1) % self.capacity;
            self.total_written += 1;
        }
    }

    /// Read the buffer contents in order (oldest to newest).
    fn read_all(&self) -> Vec<u8> {
        if self.total_written <= self.capacity {
            self.data[..self.total_written].to_vec()
        } else {
            // Buffer has wrapped: read from write_pos to end, then start to write_pos
            let mut result = Vec::with_capacity(self.capacity);
            result.extend_from_slice(&self.data[self.write_pos..]);
            result.extend_from_slice(&self.data[..self.write_pos]);
            result
        }
    }

    /// Total bytes ever written (including overwritten).
    fn total_bytes(&self) -> usize {
        self.total_written
    }

    /// Read as UTF-8 string, replacing invalid sequences.
    fn read_string(&self) -> String {
        String::from_utf8_lossy(&self.read_all()).to_string()
    }

    /// Read the last N bytes.
    fn tail(&self, n: usize) -> String {
        let all = self.read_all();
        let start = all.len().saturating_sub(n);
        String::from_utf8_lossy(&all[start..]).to_string()
    }
}

// ---------------------------------------------------------------------------
// Process entry
// ---------------------------------------------------------------------------

/// Status of a managed process.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProcessStatus {
    Running,
    Stopped,
    Failed,
    /// Process exited normally.
    Exited,
}

/// Metadata for a managed process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub name: String,
    pub pid: u32,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    pub status: ProcessStatus,
    pub exit_code: Option<i32>,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub stopped_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Internal state for a running process.
struct ManagedProcess {
    info: ProcessInfo,
    child: Option<tokio::process::Child>,
    stdout_buf: Arc<Mutex<RingBuffer>>,
    stderr_buf: Arc<Mutex<RingBuffer>>,
}

// ---------------------------------------------------------------------------
// ProcessManager
// ---------------------------------------------------------------------------

/// Default ring buffer capacity: 10 MB.
const DEFAULT_BUFFER_CAPACITY: usize = 10 * 1024 * 1024;

/// Manages background processes with output capture.
pub struct ProcessManager {
    processes: Arc<Mutex<HashMap<String, ManagedProcess>>>,
    buffer_capacity: usize,
}

impl ProcessManager {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            buffer_capacity: DEFAULT_BUFFER_CAPACITY,
        }
    }

    pub fn with_buffer_capacity(mut self, capacity: usize) -> Self {
        self.buffer_capacity = capacity;
        self
    }

    /// Spawn a new background process.
    pub async fn spawn(
        &self,
        name: &str,
        command: &str,
        args: &[&str],
        working_dir: Option<&str>,
        env_vars: Option<&HashMap<String, String>>,
    ) -> Result<ProcessInfo, ToolError> {
        let mut procs = self.processes.lock().await;

        // Check if a process with this name is already running
        if let Some(existing) = procs.get(name) {
            if existing.info.status == ProcessStatus::Running {
                return Err(ToolError::ExecutionFailed(format!(
                    "Process '{}' is already running (PID {})",
                    name, existing.info.pid
                )));
            }
        }

        let mut cmd = Command::new(command);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if let Some(dir) = working_dir {
            cmd.current_dir(dir);
        }

        if let Some(env) = env_vars {
            for (k, v) in env {
                cmd.env(k, v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to spawn '{}': {}", command, e))
        })?;

        let pid = child.id().unwrap_or(0);

        let stdout_buf = Arc::new(Mutex::new(RingBuffer::new(self.buffer_capacity)));
        let stderr_buf = Arc::new(Mutex::new(RingBuffer::new(self.buffer_capacity)));

        // Spawn tasks to capture stdout and stderr
        if let Some(stdout) = child.stdout.take() {
            let buf = stdout_buf.clone();
            tokio::spawn(async move {
                capture_output(stdout, buf).await;
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let buf = stderr_buf.clone();
            tokio::spawn(async move {
                capture_output(stderr, buf).await;
            });
        }

        let info = ProcessInfo {
            name: name.to_string(),
            pid,
            command: command.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            working_dir: working_dir.map(|s| s.to_string()),
            status: ProcessStatus::Running,
            exit_code: None,
            started_at: chrono::Utc::now(),
            stopped_at: None,
        };

        let managed = ManagedProcess {
            info: info.clone(),
            child: Some(child),
            stdout_buf,
            stderr_buf,
        };

        procs.insert(name.to_string(), managed);
        tracing::info!(name = name, pid = pid, command = command, "Process spawned");

        Ok(info)
    }

    /// List all tracked processes (refreshes status).
    pub async fn list(&self) -> Vec<ProcessInfo> {
        let mut procs = self.processes.lock().await;
        let mut infos = Vec::new();

        for proc in procs.values_mut() {
            refresh_status(proc).await;
            infos.push(proc.info.clone());
        }

        infos
    }

    /// Get status of a specific process.
    pub async fn status(&self, name: &str) -> Option<ProcessInfo> {
        let mut procs = self.processes.lock().await;
        if let Some(proc) = procs.get_mut(name) {
            refresh_status(proc).await;
            Some(proc.info.clone())
        } else {
            None
        }
    }

    /// Get captured output for a process.
    pub async fn output(
        &self,
        name: &str,
        stream: &str,
        tail_bytes: Option<usize>,
    ) -> Result<String, ToolError> {
        let procs = self.processes.lock().await;
        let proc = procs
            .get(name)
            .ok_or_else(|| ToolError::ExecutionFailed(format!("No process named '{name}'")))?;

        let buf = match stream {
            "stderr" => &proc.stderr_buf,
            _ => &proc.stdout_buf,
        };

        let buf = buf.lock().await;
        Ok(match tail_bytes {
            Some(n) => buf.tail(n),
            None => buf.read_string(),
        })
    }

    /// Gracefully stop a process: SIGTERM → wait → SIGKILL.
    pub async fn stop(&self, name: &str, timeout: Duration) -> Result<ProcessInfo, ToolError> {
        let mut procs = self.processes.lock().await;
        let proc = procs
            .get_mut(name)
            .ok_or_else(|| ToolError::ExecutionFailed(format!("No process named '{name}'")))?;

        if proc.info.status != ProcessStatus::Running {
            return Ok(proc.info.clone());
        }

        if let Some(ref mut child) = proc.child {
            // Try graceful shutdown first
            #[cfg(unix)]
            {
                // Send SIGTERM via kill command (avoids libc dependency)
                if let Some(pid) = child.id() {
                    let _ = std::process::Command::new("kill")
                        .args(["-TERM", &pid.to_string()])
                        .output();
                }
            }
            #[cfg(not(unix))]
            {
                let _ = child.start_kill();
            }

            // Wait for graceful shutdown
            match tokio::time::timeout(timeout, child.wait()).await {
                Ok(Ok(status)) => {
                    proc.info.exit_code = status.code();
                    proc.info.status = ProcessStatus::Stopped;
                }
                Ok(Err(e)) => {
                    tracing::warn!(name = name, error = %e, "Error waiting for process");
                    proc.info.status = ProcessStatus::Failed;
                }
                Err(_) => {
                    // Timeout: force kill
                    tracing::warn!(
                        name = name,
                        "Process did not stop gracefully, sending SIGKILL"
                    );
                    let _ = child.kill().await;
                    if let Ok(status) = child.wait().await {
                        proc.info.exit_code = status.code();
                    }
                    proc.info.status = ProcessStatus::Stopped;
                }
            }
        }

        proc.info.stopped_at = Some(chrono::Utc::now());
        proc.child = None;

        tracing::info!(name = name, "Process stopped");
        Ok(proc.info.clone())
    }

    /// Remove a stopped process from the registry.
    pub async fn remove(&self, name: &str) -> bool {
        let mut procs = self.processes.lock().await;
        if let Some(proc) = procs.get(name) {
            if proc.info.status == ProcessStatus::Running {
                return false; // Don't remove running processes
            }
        }
        procs.remove(name).is_some()
    }
}

impl Default for ProcessManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Refresh the status of a managed process by checking if it's still alive.
async fn refresh_status(proc: &mut ManagedProcess) {
    if proc.info.status != ProcessStatus::Running {
        return;
    }
    if let Some(ref mut child) = proc.child {
        match child.try_wait() {
            Ok(Some(status)) => {
                proc.info.exit_code = status.code();
                proc.info.status = if status.success() {
                    ProcessStatus::Exited
                } else {
                    ProcessStatus::Failed
                };
                proc.info.stopped_at = Some(chrono::Utc::now());
            }
            Ok(None) => {
                // Still running
            }
            Err(_) => {
                proc.info.status = ProcessStatus::Failed;
            }
        }
    } else {
        proc.info.status = ProcessStatus::Stopped;
    }
}

/// Background task to capture output from a reader into a ring buffer.
async fn capture_output<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    buf: Arc<Mutex<RingBuffer>>,
) {
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                let mut buf = buf.lock().await;
                buf.write(&chunk[..n]);
            }
            Err(_) => break,
        }
    }
}

// ---------------------------------------------------------------------------
// ProcessRegistryHandler — tool the LLM invokes
// ---------------------------------------------------------------------------

/// Tool handler for the process registry.
#[derive(Clone)]
pub struct ProcessRegistryHandler {
    manager: Arc<ProcessManager>,
}

impl ProcessRegistryHandler {
    pub fn new(manager: Arc<ProcessManager>) -> Self {
        Self { manager }
    }
}

impl Default for ProcessRegistryHandler {
    fn default() -> Self {
        Self::new(Arc::new(ProcessManager::new()))
    }
}

#[async_trait]
impl ToolHandler for ProcessRegistryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");

        match action {
            "spawn" | "start" => {
                let command = params
                    .get("command")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'command'".into()))?;

                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(command);

                let args: Vec<&str> = params
                    .get("args")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                let working_dir = params.get("working_dir").and_then(|v| v.as_str());

                let env_vars: Option<HashMap<String, String>> =
                    params.get("env").and_then(|v| v.as_object()).map(|obj| {
                        obj.iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                            .collect()
                    });

                let info = self
                    .manager
                    .spawn(name, command, &args, working_dir, env_vars.as_ref())
                    .await?;

                Ok(json!({
                    "status": "spawned",
                    "name": info.name,
                    "pid": info.pid,
                    "command": info.command,
                    "args": info.args,
                })
                .to_string())
            }

            "list" => {
                let processes = self.manager.list().await;
                let entries: Vec<Value> = processes
                    .iter()
                    .map(|p| {
                        json!({
                            "name": p.name,
                            "pid": p.pid,
                            "command": p.command,
                            "status": format!("{:?}", p.status).to_lowercase(),
                            "exit_code": p.exit_code,
                            "started_at": p.started_at.to_rfc3339(),
                        })
                    })
                    .collect();
                Ok(json!({"processes": entries, "count": entries.len()}).to_string())
            }

            "status" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name'".into()))?;

                match self.manager.status(name).await {
                    Some(info) => Ok(json!({
                        "name": info.name,
                        "pid": info.pid,
                        "command": info.command,
                        "status": format!("{:?}", info.status).to_lowercase(),
                        "exit_code": info.exit_code,
                        "started_at": info.started_at.to_rfc3339(),
                        "stopped_at": info.stopped_at.map(|t| t.to_rfc3339()),
                    })
                    .to_string()),
                    None => Err(ToolError::ExecutionFailed(format!(
                        "No process named '{name}'"
                    ))),
                }
            }

            "output" | "logs" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name'".into()))?;

                let stream = params
                    .get("stream")
                    .and_then(|v| v.as_str())
                    .unwrap_or("stdout");

                let tail = params
                    .get("tail")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let output = self.manager.output(name, stream, tail).await?;

                Ok(json!({
                    "name": name,
                    "stream": stream,
                    "output": output,
                    "length": output.len(),
                })
                .to_string())
            }

            "stop" | "kill" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name'".into()))?;

                let timeout_secs = params.get("timeout").and_then(|v| v.as_u64()).unwrap_or(5);

                let info = self
                    .manager
                    .stop(name, Duration::from_secs(timeout_secs))
                    .await?;

                Ok(json!({
                    "status": "stopped",
                    "name": info.name,
                    "pid": info.pid,
                    "exit_code": info.exit_code,
                })
                .to_string())
            }

            "restart" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name'".into()))?;

                // Get the original command info before stopping
                let original = self.manager.status(name).await.ok_or_else(|| {
                    ToolError::ExecutionFailed(format!("No process named '{name}'"))
                })?;

                // Stop the process
                self.manager.stop(name, Duration::from_secs(5)).await?;

                // Remove the old entry
                self.manager.remove(name).await;

                // Re-spawn with the same command
                let args_refs: Vec<&str> = original.args.iter().map(|s| s.as_str()).collect();
                let info = self
                    .manager
                    .spawn(
                        name,
                        &original.command,
                        &args_refs,
                        original.working_dir.as_deref(),
                        None,
                    )
                    .await?;

                Ok(json!({
                    "status": "restarted",
                    "name": info.name,
                    "pid": info.pid,
                    "command": info.command,
                })
                .to_string())
            }

            "remove" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name'".into()))?;

                if self.manager.remove(name).await {
                    Ok(json!({"status": "removed", "name": name}).to_string())
                } else {
                    Err(ToolError::ExecutionFailed(format!(
                        "Cannot remove '{}': still running or not found",
                        name
                    )))
                }
            }

            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{other}'. Use: spawn, list, status, output, stop, restart, remove"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["spawn", "list", "status", "output", "stop", "restart", "remove"],
                "default": "list"
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Process name (identifier for tracking)"
            }),
        );
        props.insert(
            "command".into(),
            json!({
                "type": "string",
                "description": "Command to execute (required for 'spawn')"
            }),
        );
        props.insert(
            "args".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Command arguments"
            }),
        );
        props.insert(
            "working_dir".into(),
            json!({
                "type": "string",
                "description": "Working directory for the process"
            }),
        );
        props.insert(
            "env".into(),
            json!({
                "type": "object",
                "description": "Environment variables to set"
            }),
        );
        props.insert(
            "stream".into(),
            json!({
                "type": "string",
                "description": "Output stream to read: 'stdout' or 'stderr'",
                "enum": ["stdout", "stderr"],
                "default": "stdout"
            }),
        );
        props.insert(
            "tail".into(),
            json!({
                "type": "integer",
                "description": "Number of bytes to read from the end of the output buffer"
            }),
        );
        props.insert(
            "timeout".into(),
            json!({
                "type": "integer",
                "description": "Timeout in seconds for graceful stop (default: 5)",
                "default": 5
            }),
        );
        tool_schema(
            "process_registry",
            "Manage background processes: spawn with stdout/stderr capture, list running processes, \
             read output logs, gracefully stop (SIGTERM→SIGKILL), and restart. \
             Output is captured in a 10MB ring buffer per stream.",
            JsonSchema::object(props, vec![]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Print `message` and exit (no shell required on Windows).
    fn test_print_cmd(message: &str) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            (
                "cmd".into(),
                vec!["/C".into(), "echo".into(), message.into()],
            )
        }
        #[cfg(not(windows))]
        {
            ("echo".into(), vec![message.into()])
        }
    }

    /// Block for roughly `seconds` (stop / duplicate-name tests).
    fn test_sleep_cmd(seconds: u32) -> (String, Vec<String>) {
        #[cfg(windows)]
        {
            let n = (seconds + 1).to_string();
            (
                "cmd".into(),
                vec![
                    "/C".into(),
                    "ping".into(),
                    "127.0.0.1".into(),
                    "-n".into(),
                    n,
                ],
            )
        }
        #[cfg(not(windows))]
        {
            ("sleep".into(), vec![seconds.to_string()])
        }
    }

    fn arg_refs(args: &[String]) -> Vec<&str> {
        args.iter().map(String::as_str).collect()
    }

    // -- Ring buffer tests ---------------------------------------------------

    #[test]
    fn ring_buffer_basic_write_read() {
        let mut buf = RingBuffer::new(16);
        buf.write(b"hello");
        assert_eq!(buf.read_string(), "hello");
        assert_eq!(buf.total_bytes(), 5);
    }

    #[test]
    fn ring_buffer_wrap_around() {
        let mut buf = RingBuffer::new(8);
        buf.write(b"12345678"); // fills exactly
        buf.write(b"AB"); // wraps: overwrites positions 0,1
        let s = buf.read_string();
        assert_eq!(s, "345678AB");
    }

    #[test]
    fn ring_buffer_tail() {
        let mut buf = RingBuffer::new(32);
        buf.write(b"hello world this is a test");
        let tail = buf.tail(4);
        assert_eq!(tail, "test");
    }

    #[test]
    fn ring_buffer_empty() {
        let buf = RingBuffer::new(16);
        assert_eq!(buf.read_string(), "");
        assert_eq!(buf.total_bytes(), 0);
    }

    // -- ProcessManager tests ------------------------------------------------

    #[tokio::test]
    async fn spawn_and_list() {
        let mgr = ProcessManager::new().with_buffer_capacity(1024);
        let (cmd, args) = test_print_cmd("hello");
        let info = mgr
            .spawn("test-echo", &cmd, &arg_refs(&args), None, None)
            .await
            .unwrap();
        assert_eq!(info.name, "test-echo");
        assert_eq!(info.command, cmd);
        assert!(info.pid > 0);

        // Give it a moment to finish
        tokio::time::sleep(Duration::from_millis(100)).await;

        let list = mgr.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "test-echo");
    }

    #[tokio::test]
    async fn spawn_captures_stdout() {
        let mgr = ProcessManager::new().with_buffer_capacity(1024);
        let (cmd, args) = test_print_cmd("captured output");
        mgr.spawn("echo-test", &cmd, &arg_refs(&args), None, None)
            .await
            .unwrap();

        // Wait for process to finish and output to be captured
        tokio::time::sleep(Duration::from_millis(200)).await;

        let output = mgr.output("echo-test", "stdout", None).await.unwrap();
        assert!(output.contains("captured output"));
    }

    #[tokio::test]
    async fn spawn_duplicate_name_fails() {
        let mgr = ProcessManager::new().with_buffer_capacity(1024);
        let (sleep_cmd, sleep_args) = test_sleep_cmd(3);
        mgr.spawn("dup", &sleep_cmd, &arg_refs(&sleep_args), None, None)
            .await
            .unwrap();

        let (echo_cmd, echo_args) = test_print_cmd("hi");
        let err = mgr
            .spawn("dup", &echo_cmd, &arg_refs(&echo_args), None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("already running"));

        // Cleanup
        mgr.stop("dup", Duration::from_millis(100)).await.ok();
    }

    #[tokio::test]
    async fn stop_process() {
        let mgr = ProcessManager::new().with_buffer_capacity(1024);
        let (cmd, args) = test_sleep_cmd(60);
        mgr.spawn("sleeper", &cmd, &arg_refs(&args), None, None)
            .await
            .unwrap();

        let info = mgr.stop("sleeper", Duration::from_secs(2)).await.unwrap();
        assert_eq!(info.status, ProcessStatus::Stopped);
        assert!(info.stopped_at.is_some());
    }

    #[tokio::test]
    async fn status_nonexistent() {
        let mgr = ProcessManager::new();
        assert!(mgr.status("nope").await.is_none());
    }

    #[tokio::test]
    async fn output_nonexistent() {
        let mgr = ProcessManager::new();
        let err = mgr.output("nope", "stdout", None).await.unwrap_err();
        assert!(err.to_string().contains("No process"));
    }

    #[tokio::test]
    async fn remove_stopped_process() {
        let mgr = ProcessManager::new().with_buffer_capacity(1024);
        let (cmd, args) = test_print_cmd("bye");
        mgr.spawn("rm-test", &cmd, &arg_refs(&args), None, None)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Process should have exited
        let list = mgr.list().await;
        assert!(
            list[0].status == ProcessStatus::Exited || list[0].status == ProcessStatus::Running
        );

        // If still running, stop it first
        if list[0].status == ProcessStatus::Running {
            mgr.stop("rm-test", Duration::from_secs(1)).await.ok();
        }

        assert!(mgr.remove("rm-test").await);
        assert!(mgr.list().await.is_empty());
    }

    // -- Handler tests -------------------------------------------------------

    #[tokio::test]
    async fn handler_spawn_and_list() {
        let handler = ProcessRegistryHandler::default();
        let (cmd, args) = test_print_cmd("handler test");

        let result = handler
            .execute(json!({
                "action": "spawn",
                "name": "h-echo",
                "command": cmd,
                "args": args
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["status"], "spawned");
        assert_eq!(v["name"], "h-echo");

        tokio::time::sleep(Duration::from_millis(100)).await;

        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["count"], 1);
    }

    #[tokio::test]
    async fn handler_output() {
        let handler = ProcessRegistryHandler::default();
        let (cmd, args) = test_print_cmd("output test");

        handler
            .execute(json!({
                "action": "spawn",
                "name": "h-out",
                "command": cmd,
                "args": args
            }))
            .await
            .unwrap();

        tokio::time::sleep(Duration::from_millis(200)).await;

        let result = handler
            .execute(json!({
                "action": "output",
                "name": "h-out"
            }))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert!(v["output"].as_str().unwrap().contains("output test"));
    }

    #[tokio::test]
    async fn handler_invalid_action() {
        let handler = ProcessRegistryHandler::default();
        let err = handler
            .execute(json!({"action": "bogus"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unknown action"));
    }

    #[tokio::test]
    async fn handler_spawn_missing_command() {
        let handler = ProcessRegistryHandler::default();
        let err = handler
            .execute(json!({"action": "spawn"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Missing 'command'"));
    }

    #[tokio::test]
    async fn handler_schema() {
        let handler = ProcessRegistryHandler::default();
        let schema = handler.schema();
        assert_eq!(schema.name, "process_registry");
        assert!(schema.description.contains("background processes"));
    }
}
