//! Local terminal backend – executes commands on the same host.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::ChildStdin;
use tokio::process::Command as TokioCommand;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use hermes_config::{TerminalConfig, TerminalHomeMode};
use hermes_core::{subprocess::CommandNoWindowExt, AgentError, CommandOutput, TerminalBackend};

const PROCESS_OUTPUT_WINDOW_CHARS: usize = 200_000;
const PROCESS_PREVIEW_CHARS: usize = 1_000;
const PROCESS_WAIT_OUTPUT_CHARS: usize = 2_000;
const PROCESS_LOG_DEFAULT_LINES: usize = 200;
const FOREGROUND_DRAIN_GRACE_MS: u64 = 120;

static NEXT_PROCESS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct ProcessSession {
    id: String,
    command: String,
    pid: Option<u32>,
    started_at: u64,
    status: Arc<Mutex<ProcessStatus>>,
    output: Arc<Mutex<String>>,
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
}

#[derive(Clone, Default)]
struct ProcessStatus {
    exited: bool,
    exit_code: Option<i32>,
}

/// A [`TerminalBackend`] that runs commands on the local machine.
pub struct LocalBackend {
    /// Default timeout in seconds for command execution.
    default_timeout: u64,
    /// Maximum output size in bytes before truncation.
    max_output_size: usize,
    /// Explicit shell init files for local command prelude.
    shell_init_files: Vec<String>,
    /// Auto-source common shell startup files when no explicit list is set.
    auto_source_bashrc: bool,
    /// HOME policy for subprocesses.
    home_mode: TerminalHomeMode,
    /// Env-var names allowed through subprocess secret sanitizers.
    env_passthrough: Vec<String>,
    /// Background processes tracked by session id for lifecycle operations.
    background_processes: Arc<Mutex<HashMap<String, ProcessSession>>>,
}

impl LocalBackend {
    /// Create a new local backend with the given defaults.
    pub fn new(default_timeout: u64, max_output_size: usize) -> Self {
        let (shell_init_files, auto_source_bashrc, home_mode, env_passthrough) =
            terminal_config_from_env();
        Self::new_with_shell_init_and_home_mode(
            default_timeout,
            max_output_size,
            shell_init_files,
            auto_source_bashrc,
            home_mode,
            env_passthrough,
        )
    }

    /// Create a local backend directly from terminal configuration.
    pub fn from_terminal_config(config: &TerminalConfig) -> Self {
        Self::new_with_shell_init_and_home_mode(
            config.timeout,
            config.max_output_size,
            config.shell_init_files.clone(),
            config.auto_source_bashrc,
            config.home_mode,
            config.env_passthrough.clone(),
        )
    }

    pub fn new_with_shell_init(
        default_timeout: u64,
        max_output_size: usize,
        shell_init_files: Vec<String>,
        auto_source_bashrc: bool,
        env_passthrough: Vec<String>,
    ) -> Self {
        Self::new_with_shell_init_and_home_mode(
            default_timeout,
            max_output_size,
            shell_init_files,
            auto_source_bashrc,
            TerminalHomeMode::Auto,
            env_passthrough,
        )
    }

    pub fn new_with_shell_init_and_home_mode(
        default_timeout: u64,
        max_output_size: usize,
        shell_init_files: Vec<String>,
        auto_source_bashrc: bool,
        home_mode: TerminalHomeMode,
        env_passthrough: Vec<String>,
    ) -> Self {
        Self {
            default_timeout,
            max_output_size,
            shell_init_files,
            auto_source_bashrc,
            home_mode,
            env_passthrough,
            background_processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn next_process_session_id() -> String {
        format!(
            "proc_{:012x}",
            NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed)
        )
    }

    fn now_unix_ts() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn max_process_output_chars(&self) -> usize {
        self.max_output_size.max(PROCESS_OUTPUT_WINDOW_CHARS)
    }

    fn slice_to_tail_chars(input: &str, keep: usize) -> String {
        if input.len() <= keep {
            return input.to_string();
        }
        let mut start = input.len() - keep;
        while start < input.len() && !input.is_char_boundary(start) {
            start += 1;
        }
        input[start..].to_string()
    }

    fn append_output(output: &Arc<Mutex<String>>, text: &str, max_chars: usize) {
        let mut guard = output.lock().unwrap_or_else(|e| e.into_inner());
        guard.push_str(text);
        if guard.len() > max_chars {
            let trimmed = Self::slice_to_tail_chars(&guard, max_chars);
            *guard = trimmed;
        }
    }

    fn read_status_snapshot(session: &ProcessSession) -> ProcessStatus {
        session
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn read_output_snapshot(session: &ProcessSession) -> String {
        session
            .output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn process_summary(session: &ProcessSession) -> Value {
        let status = Self::read_status_snapshot(session);
        let output = Self::read_output_snapshot(session);
        let uptime = Self::now_unix_ts().saturating_sub(session.started_at);
        let mut summary = json!({
            "session_id": session.id.clone(),
            "command": session.command.clone(),
            "pid": session.pid,
            "started_at": session.started_at,
            "uptime_seconds": uptime,
            "status": if status.exited { "exited" } else { "running" },
            "output_preview": Self::slice_to_tail_chars(&output, PROCESS_PREVIEW_CHARS),
        });
        if status.exited {
            summary["exit_code"] = json!(status.exit_code);
        }
        summary
    }

    fn get_session(&self, session_id: &str) -> Option<ProcessSession> {
        self.background_processes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    async fn read_stream_to_buffer<R>(
        mut stream: R,
        output: Arc<Mutex<String>>,
        max_output_chars: usize,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut chunk = [0_u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => {
                    let text = String::from_utf8_lossy(&chunk[..read]).to_string();
                    Self::append_output(&output, &text, max_output_chars);
                }
                Err(err) => {
                    let note = format!("\n[stream read error: {err}]");
                    Self::append_output(&output, &note, max_output_chars);
                    break;
                }
            }
        }
    }

    fn append_bytes(output: &Arc<Mutex<Vec<u8>>>, bytes: &[u8], max_bytes: usize) {
        let mut guard = output.lock().unwrap_or_else(|e| e.into_inner());
        let remaining = max_bytes.saturating_sub(guard.len());
        if remaining > 0 {
            guard.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
    }

    async fn read_stream_to_bytes<R>(
        mut stream: R,
        output: Arc<Mutex<Vec<u8>>>,
        max_output_bytes: usize,
    ) where
        R: AsyncRead + Unpin,
    {
        let mut chunk = [0_u8; 4096];
        loop {
            match stream.read(&mut chunk).await {
                Ok(0) => break,
                Ok(read) => Self::append_bytes(&output, &chunk[..read], max_output_bytes),
                Err(err) => {
                    let note = format!("\n[stream read error: {err}]");
                    Self::append_bytes(&output, note.as_bytes(), max_output_bytes);
                    break;
                }
            }
        }
    }

    fn decode_output(bytes: &Arc<Mutex<Vec<u8>>>) -> String {
        let guard = bytes.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&guard).to_string()
    }

    async fn finish_or_abort_reader(mut handle: JoinHandle<()>) {
        if tokio::time::timeout(std::time::Duration::from_millis(50), &mut handle)
            .await
            .is_err()
        {
            // A shell-backgrounded grandchild may still hold the pipe open
            // after the parent shell exits. Abort the reader instead of
            // waiting for EOF from an unmanaged descendant.
            handle.abort();
            let _ = handle.await;
        }
    }

    async fn collect_foreground_child(
        &self,
        mut cmd: TokioCommand,
        timeout_secs: u64,
        stdin_payload: Option<Vec<u8>>,
        spawn_label: &str,
        timeout_label: &str,
    ) -> Result<CommandOutput, AgentError> {
        configure_foreground_process_group(&mut cmd);
        let child = cmd
            .spawn()
            .map_err(|e| AgentError::Io(format!("Failed to spawn {spawn_label}: {e}")))?;
        let mut child_guard = ForegroundChildGuard::new(child);
        if let Some(payload) = stdin_payload {
            if let Some(mut stdin) = child_guard.child_mut().stdin.take() {
                stdin
                    .write_all(&payload)
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to write stdin: {e}")))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to flush stdin: {e}")))?;
            }
        }

        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut stdout_task = child_guard.child_mut().stdout.take().map(|stream| {
            let stdout = stdout.clone();
            let max = self.max_output_size;
            tokio::spawn(async move {
                Self::read_stream_to_bytes(stream, stdout, max).await;
            })
        });
        let mut stderr_task = child_guard.child_mut().stderr.take().map(|stream| {
            let stderr = stderr.clone();
            let max = self.max_output_size;
            tokio::spawn(async move {
                Self::read_stream_to_bytes(stream, stderr, max).await;
            })
        });

        let wait_result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            child_guard.child_mut().wait(),
        )
        .await;

        match wait_result {
            Ok(Ok(status)) => {
                tokio::time::sleep(std::time::Duration::from_millis(FOREGROUND_DRAIN_GRACE_MS))
                    .await;
                if let Some(handle) = stdout_task.take() {
                    Self::finish_or_abort_reader(handle).await;
                }
                if let Some(handle) = stderr_task.take() {
                    Self::finish_or_abort_reader(handle).await;
                }
                child_guard.disarm();
                Ok(CommandOutput {
                    exit_code: status.code().unwrap_or(-1),
                    stdout: Self::decode_output(&stdout),
                    stderr: Self::decode_output(&stderr),
                })
            }
            Ok(Err(e)) => Err(AgentError::Io(format!(
                "Failed to wait for {spawn_label}: {e}"
            ))),
            Err(_) => {
                terminate_child_process(child_guard.child_mut()).await;
                child_guard.disarm();
                if let Some(handle) = stdout_task.take() {
                    Self::finish_or_abort_reader(handle).await;
                }
                if let Some(handle) = stderr_task.take() {
                    Self::finish_or_abort_reader(handle).await;
                }
                Err(AgentError::Timeout(format!(
                    "{timeout_label} timed out after {timeout_secs} seconds"
                )))
            }
        }
    }

    fn terminate_pid(pid: u32) -> Result<(), AgentError> {
        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .suppress_windows_console()
                .status()
                .map_err(|e| AgentError::Io(format!("Failed to invoke kill for pid {pid}: {e}")))?;
            if status.success() {
                Ok(())
            } else {
                Err(AgentError::Io(format!(
                    "Failed to terminate pid {pid} (exit status: {status})"
                )))
            }
        }
        #[cfg(not(unix))]
        {
            let status = std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .suppress_windows_console()
                .status()
                .map_err(|e| {
                    AgentError::Io(format!("Failed to invoke taskkill for pid {pid}: {e}"))
                })?;
            if status.success() {
                Ok(())
            } else {
                Err(AgentError::Io(format!(
                    "Failed to terminate pid {pid} (exit status: {status})"
                )))
            }
        }
    }
}

include!("local/env_shell.rs");
impl Default for LocalBackend {
    fn default() -> Self {
        Self::new(120, 1_048_576)
    }
}

#[async_trait]
impl TerminalBackend for LocalBackend {
    async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        let timeout_secs = timeout.unwrap_or(self.default_timeout);
        let rewritten_command = rewrite_compound_background(command);
        let subprocess_home = subprocess_home_for_mode(self.home_mode);
        let command_with_profiles = with_login_profile_sources(
            &rewritten_command,
            &self.shell_init_files,
            self.auto_source_bashrc,
            &self.env_passthrough,
            subprocess_home.as_deref(),
        );

        if pty && !background {
            // PTY mode: allocate a pseudo-terminal for interactive commands.
            // Uses `script` command (available on macOS/Linux) to wrap the
            // command in a PTY, which makes programs behave as if connected
            // to a real terminal (enables colors, line editing, etc.).
            #[cfg(unix)]
            {
                let mut pty_cmd = TokioCommand::new("script");
                pty_cmd
                    .arg("-q") // quiet mode
                    .arg("/dev/null") // discard typescript file
                    .arg("-c") // command to execute
                    .arg(&command_with_profiles)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .stdin(Stdio::null());
                scrub_subprocess_env(&mut pty_cmd, &self.env_passthrough);
                apply_subprocess_home_policy(&mut pty_cmd, subprocess_home.as_ref());

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }
                pty_cmd.suppress_windows_console();

                return self
                    .collect_foreground_child(
                        pty_cmd,
                        timeout_secs,
                        None,
                        "PTY command",
                        "PTY command",
                    )
                    .await;
            }

            #[cfg(not(unix))]
            {
                tracing::warn!("PTY mode is not supported on this platform; falling back to standard execution");
            }
        }

        let mut cmd = if pty {
            #[cfg(unix)]
            {
                let mut pty_cmd = TokioCommand::new("script");
                pty_cmd
                    .arg("-q")
                    .arg("/dev/null")
                    .arg("-c")
                    .arg(&command_with_profiles);
                pty_cmd
            }
            #[cfg(not(unix))]
            {
                tracing::warn!(
                    "PTY mode is not supported on this platform; using standard shell execution"
                );
                let mut shell_cmd = TokioCommand::new("sh");
                shell_cmd.arg("-c").arg(&command_with_profiles);
                shell_cmd
            }
        } else {
            let mut shell_cmd = TokioCommand::new("sh");
            shell_cmd.arg("-c").arg(&command_with_profiles);
            shell_cmd
        };
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        scrub_subprocess_env(&mut cmd, &self.env_passthrough);
        apply_subprocess_home_policy(&mut cmd, subprocess_home.as_ref());
        cmd.suppress_windows_console();

        if let Some(dir) = workdir {
            cmd.current_dir(resolve_path(dir)?);
        }

        if background {
            // In background mode, keep stdin pipe open for process(write/submit).
            cmd.stdin(Stdio::piped());
        } else if pty {
            cmd.stdin(Stdio::null());
        }

        if !background {
            return self
                .collect_foreground_child(cmd, timeout_secs, None, "command", "Command")
                .await;
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let mut child = cmd
                .spawn()
                .map_err(|e| AgentError::Io(format!("Failed to spawn command: {}", e)))?;

            let session_id = Self::next_process_session_id();
            let pid = child.id();
            let output = Arc::new(Mutex::new(String::new()));
            let status = Arc::new(Mutex::new(ProcessStatus::default()));
            let stdin = Arc::new(AsyncMutex::new(child.stdin.take()));
            let max_output_chars = self.max_process_output_chars();
            let session = ProcessSession {
                id: session_id.clone(),
                command: rewritten_command.clone(),
                pid,
                started_at: Self::now_unix_ts(),
                status: status.clone(),
                output: output.clone(),
                stdin,
            };
            self.background_processes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(session_id.clone(), session);

            if let Some(stdout) = child.stdout.take() {
                let output = output.clone();
                tokio::spawn(async move {
                    Self::read_stream_to_buffer(stdout, output, max_output_chars).await;
                });
            }
            if let Some(stderr) = child.stderr.take() {
                let output = output.clone();
                tokio::spawn(async move {
                    Self::read_stream_to_buffer(stderr, output, max_output_chars).await;
                });
            }

            tokio::spawn(async move {
                match child.wait().await {
                    Ok(exit) => {
                        let mut guard = status.lock().unwrap_or_else(|e| e.into_inner());
                        guard.exited = true;
                        guard.exit_code = exit.code();
                    }
                    Err(err) => {
                        let mut guard = status.lock().unwrap_or_else(|e| e.into_inner());
                        guard.exited = true;
                        guard.exit_code = Some(-1);
                        let note = format!("\n[wait error: {err}]");
                        Self::append_output(&output, &note, max_output_chars);
                    }
                }
            });

            let started = json!({
                "output": "Background process started",
                "session_id": session_id,
                "pid": pid,
                "exit_code": 0,
                "error": Value::Null
            });
            Ok(CommandOutput {
                exit_code: 0,
                stdout: started.to_string(),
                stderr: String::new(),
            })
        })
        .await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(AgentError::Timeout(format!(
                "Command timed out after {} seconds",
                timeout_secs
            ))),
        }
    }

    async fn execute_command_with_stdin(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
        stdin_data: Option<&str>,
    ) -> Result<CommandOutput, AgentError> {
        let Some(stdin_data) = stdin_data else {
            return self
                .execute_command(command, timeout, workdir, background, pty)
                .await;
        };
        let rewritten_command = rewrite_compound_background(command);
        let subprocess_home = subprocess_home_for_mode(self.home_mode);
        let command_with_profiles = with_login_profile_sources(
            &rewritten_command,
            &self.shell_init_files,
            self.auto_source_bashrc,
            &self.env_passthrough,
            subprocess_home.as_deref(),
        );

        if background {
            let started = self
                .execute_command(command, timeout, workdir, true, pty)
                .await?;
            let session_id = serde_json::from_str::<Value>(&started.stdout)
                .ok()
                .and_then(|v| {
                    v.get("session_id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                });
            if let Some(session_id) = session_id {
                let _ = self.write_process_stdin(&session_id, stdin_data).await?;
                let _ = self.close_process_stdin(&session_id).await?;
            }
            return Ok(started);
        }

        let timeout_secs = timeout.unwrap_or(self.default_timeout);
        let stdin_owned = stdin_data.to_string();
        if pty {
            #[cfg(unix)]
            {
                let mut pty_cmd = TokioCommand::new("script");
                pty_cmd
                    .arg("-q")
                    .arg("/dev/null")
                    .arg("-c")
                    .arg(&command_with_profiles)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .stdin(Stdio::piped());
                scrub_subprocess_env(&mut pty_cmd, &self.env_passthrough);
                apply_subprocess_home_policy(&mut pty_cmd, subprocess_home.as_ref());

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }
                pty_cmd.suppress_windows_console();

                return self
                    .collect_foreground_child(
                        pty_cmd,
                        timeout_secs,
                        Some(stdin_owned.as_bytes().to_vec()),
                        "PTY command",
                        "PTY command",
                    )
                    .await;
            }

            #[cfg(not(unix))]
            {
                tracing::warn!(
                    "PTY mode is not supported on this platform; using standard shell execution"
                );
            }
        }

        let mut cmd = TokioCommand::new("sh");
        cmd.arg("-c")
            .arg(&command_with_profiles)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::piped());
        scrub_subprocess_env(&mut cmd, &self.env_passthrough);
        apply_subprocess_home_policy(&mut cmd, subprocess_home.as_ref());
        if let Some(dir) = workdir {
            cmd.current_dir(resolve_path(dir)?);
        }
        cmd.suppress_windows_console();

        self.collect_foreground_child(
            cmd,
            timeout_secs,
            Some(stdin_owned.into_bytes()),
            "command",
            "Command",
        )
        .await
    }

    async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        let resolved = resolve_path(path)?;
        let content = tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| AgentError::Io(format!("Failed to read file '{}': {}", path, e)))?;

        let lines: Vec<&str> = content.lines().collect();

        // Apply offset (0-indexed line number to start from)
        let start = offset.unwrap_or(0) as usize;
        let start = start.min(lines.len());

        // Apply limit (max number of lines to return)
        let end = if let Some(lim) = limit {
            (start + lim as usize).min(lines.len())
        } else {
            lines.len()
        };

        let selected_lines = &lines[start..end];
        Ok(selected_lines.join("\n"))
    }

    async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        let resolved = resolve_path(path)?;

        // Ensure parent directory exists
        if let Some(parent) = resolved.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    AgentError::Io(format!(
                        "Failed to create parent directory for '{}': {}",
                        path, e
                    ))
                })?;
            }
        }

        tokio::fs::write(&resolved, content)
            .await
            .map_err(|e| AgentError::Io(format!("Failed to write file '{}': {}", path, e)))?;

        Ok(())
    }

    async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        let resolved = resolve_path(path)?;
        match tokio::fs::metadata(&resolved).await {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(AgentError::Io(format!(
                "Failed to check file existence '{}': {}",
                path, e
            ))),
        }
    }

    async fn list_processes(&self) -> Result<Value, AgentError> {
        let sessions = self
            .background_processes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect::<Vec<_>>();

        let mut entries = sessions
            .iter()
            .map(Self::process_summary)
            .collect::<Vec<Value>>();
        entries.sort_by_key(|v| {
            std::cmp::Reverse(v.get("started_at").and_then(Value::as_u64).unwrap_or(0))
        });

        Ok(json!({
            "status": "ok",
            "count": entries.len(),
            "sessions": entries
        }))
    }

    async fn poll_process(&self, session_id: &str) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };
        Ok(Self::process_summary(&session))
    }

    async fn read_process_log(
        &self,
        session_id: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };
        let status = Self::read_status_snapshot(&session);
        let output = Self::read_output_snapshot(&session);
        let lines = output.lines().collect::<Vec<_>>();
        let total_lines = lines.len();
        let limit = limit
            .map(|v| v as usize)
            .unwrap_or(PROCESS_LOG_DEFAULT_LINES)
            .max(1);
        let selected = if offset.unwrap_or(0) == 0 {
            lines
                .get(total_lines.saturating_sub(limit)..total_lines)
                .unwrap_or(&[])
                .to_vec()
        } else {
            let start = (offset.unwrap_or(0) as usize).min(total_lines);
            let end = (start + limit).min(total_lines);
            lines.get(start..end).unwrap_or(&[]).to_vec()
        };

        Ok(json!({
            "session_id": session.id.clone(),
            "status": if status.exited { "exited" } else { "running" },
            "output": selected.join("\n"),
            "total_lines": total_lines,
            "showing": format!("{} lines", selected.len())
        }))
    }

    async fn wait_process(
        &self,
        session_id: &str,
        timeout: Option<u64>,
    ) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };

        let max_timeout = self.default_timeout;
        let requested_timeout = timeout.unwrap_or(max_timeout);
        let effective_timeout = requested_timeout.min(max_timeout);
        let timeout_note = if requested_timeout > max_timeout {
            Some(format!(
                "Requested wait of {requested_timeout}s was clamped to configured limit of {max_timeout}s"
            ))
        } else {
            None
        };
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(effective_timeout);
        loop {
            let status = Self::read_status_snapshot(&session);
            let output = Self::read_output_snapshot(&session);
            if status.exited {
                let mut result = json!({
                    "status": "exited",
                    "exit_code": status.exit_code,
                    "output": Self::slice_to_tail_chars(&output, PROCESS_WAIT_OUTPUT_CHARS),
                });
                if let Some(note) = &timeout_note {
                    result["timeout_note"] = json!(note);
                }
                return Ok(result);
            }
            if std::time::Instant::now() >= deadline {
                let mut result = json!({
                    "status": "timeout",
                    "output": Self::slice_to_tail_chars(&output, PROCESS_PREVIEW_CHARS),
                });
                if let Some(note) = timeout_note.clone() {
                    result["timeout_note"] = json!(note);
                } else {
                    result["timeout_note"] = json!(format!(
                        "Waited {effective_timeout}s, process still running"
                    ));
                }
                return Ok(result);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
    }

    async fn kill_process(&self, session_id: &str) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };
        let status = Self::read_status_snapshot(&session);
        if status.exited {
            return Ok(json!({
                "status": "already_exited",
                "exit_code": status.exit_code
            }));
        }
        let pid = session.pid.ok_or_else(|| {
            AgentError::Io(format!(
                "Cannot terminate process {session_id}: missing pid"
            ))
        })?;
        Self::terminate_pid(pid)?;

        {
            let mut guard = session.status.lock().unwrap_or_else(|e| e.into_inner());
            guard.exited = true;
            guard.exit_code = Some(-15);
        }
        let _ = session.stdin.lock().await.take();
        Ok(json!({
            "status": "killed",
            "session_id": session.id.clone()
        }))
    }

    async fn write_process_stdin(&self, session_id: &str, data: &str) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };
        let status = Self::read_status_snapshot(&session);
        if status.exited {
            return Ok(json!({
                "status": "already_exited",
                "error": "Process has already finished"
            }));
        }
        let mut stdin_guard = session.stdin.lock().await;
        let Some(stdin) = stdin_guard.as_mut() else {
            return Ok(json!({
                "status": "error",
                "error": "Process stdin not available (stdin closed)"
            }));
        };
        stdin
            .write_all(data.as_bytes())
            .await
            .map_err(|e| AgentError::Io(format!("Failed writing stdin for {session_id}: {e}")))?;
        stdin
            .flush()
            .await
            .map_err(|e| AgentError::Io(format!("Failed flushing stdin for {session_id}: {e}")))?;
        Ok(json!({
            "status": "ok",
            "bytes_written": data.len()
        }))
    }

    async fn submit_process_stdin(
        &self,
        session_id: &str,
        data: &str,
    ) -> Result<Value, AgentError> {
        self.write_process_stdin(session_id, &format!("{data}\n"))
            .await
    }

    async fn close_process_stdin(&self, session_id: &str) -> Result<Value, AgentError> {
        let Some(session) = self.get_session(session_id) else {
            return Ok(json!({
                "status": "not_found",
                "error": format!("No process with ID {session_id}")
            }));
        };
        let mut stdin_guard = session.stdin.lock().await;
        if stdin_guard.take().is_some() {
            Ok(json!({
                "status": "ok",
                "closed": true
            }))
        } else {
            Ok(json!({
                "status": "ok",
                "closed": false
            }))
        }
    }
}

#[cfg(test)]
mod tests;
