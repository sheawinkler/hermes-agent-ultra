//! Local terminal backend – executes commands on the same host.

use std::collections::HashMap;
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

use hermes_core::{AgentError, CommandOutput, TerminalBackend};

const PROCESS_OUTPUT_WINDOW_CHARS: usize = 200_000;
const PROCESS_PREVIEW_CHARS: usize = 1_000;
const PROCESS_WAIT_OUTPUT_CHARS: usize = 2_000;
const PROCESS_LOG_DEFAULT_LINES: usize = 200;

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
    /// Background processes tracked by session id for lifecycle operations.
    background_processes: Arc<Mutex<HashMap<String, ProcessSession>>>,
}

impl LocalBackend {
    /// Create a new local backend with the given defaults.
    pub fn new(default_timeout: u64, max_output_size: usize) -> Self {
        Self {
            default_timeout,
            max_output_size,
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

    fn terminate_pid(pid: u32) -> Result<(), AgentError> {
        #[cfg(unix)]
        {
            let status = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
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

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn current_username() -> Option<String> {
    std::env::var("USER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("LOGNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("USERNAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

fn is_valid_unix_username(username: &str) -> bool {
    !username.is_empty()
        && username
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

#[cfg(unix)]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in passwd.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let user = parts.next()?;
        let _passwd = parts.next()?;
        let _uid = parts.next()?;
        let _gid = parts.next()?;
        let _gecos = parts.next()?;
        let home = parts.next()?;
        if user == username && !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    None
}

#[cfg(not(unix))]
fn lookup_home_for_username(username: &str) -> Option<PathBuf> {
    if current_username().as_deref() == Some(username) {
        return home_dir();
    }
    None
}

fn resolve_path(input: &str) -> Result<PathBuf, AgentError> {
    if !input.starts_with('~') {
        return Ok(PathBuf::from(input));
    }

    let rest = &input[1..];
    if rest.is_empty() {
        return home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()));
    }

    if rest.starts_with('/') {
        let home = home_dir().ok_or_else(|| AgentError::Io("Failed to resolve home dir".into()))?;
        let suffix = rest.trim_start_matches('/');
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    // ~username or ~username/path. For security, only allow traditional
    // username characters so malicious payloads cannot pass through.
    let (username, suffix) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    if !is_valid_unix_username(username) {
        return Ok(PathBuf::from(input));
    }

    if let Some(home) = lookup_home_for_username(username) {
        return Ok(if suffix.is_empty() {
            home
        } else {
            home.join(suffix)
        });
    }

    Ok(PathBuf::from(input))
}

const SUBPROCESS_ENV_BLOCKLIST_EXACT: &[&str] = &[
    "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
    "HERMES_POLICY_ADMIN_TOKEN",
];

const SUBPROCESS_ENV_BLOCKLIST_PREFIXES: &[&str] = &[
    "TOOL_GATEWAY_",
    "HERMES_MANAGED_TOOL_GATEWAY_",
    "HERMES_GATEWAY_",
    "HERMES_HTTP_",
];

fn should_strip_subprocess_env(key: &str) -> bool {
    SUBPROCESS_ENV_BLOCKLIST_EXACT.contains(&key)
        || SUBPROCESS_ENV_BLOCKLIST_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

fn scrub_subprocess_env(cmd: &mut TokioCommand) {
    for (key, _) in std::env::vars() {
        if should_strip_subprocess_env(&key) {
            cmd.env_remove(key);
        }
    }
}

fn with_login_profile_sources(command: &str) -> String {
    #[cfg(unix)]
    {
        format!(
            "[ -f \"$HOME/.profile\" ] && . \"$HOME/.profile\"; [ -f \"$HOME/.bash_profile\" ] && . \"$HOME/.bash_profile\"; {command}"
        )
    }
    #[cfg(not(unix))]
    {
        command.to_string()
    }
}

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
        let command_with_profiles = with_login_profile_sources(command);

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
                scrub_subprocess_env(&mut pty_cmd);

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }

                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
                        let output = pty_cmd.output().await.map_err(|e| {
                            AgentError::Io(format!("Failed to spawn PTY command: {}", e))
                        })?;
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        Ok(CommandOutput {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout: if stdout.len() > self.max_output_size {
                                stdout[..self.max_output_size].to_string()
                            } else {
                                stdout
                            },
                            stderr: if stderr.len() > self.max_output_size {
                                stderr[..self.max_output_size].to_string()
                            } else {
                                stderr
                            },
                        })
                    })
                    .await;

                return match result {
                    Ok(Ok(output)) => Ok(output),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(AgentError::Timeout(format!(
                        "PTY command timed out after {} seconds",
                        timeout_secs
                    ))),
                };
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
        scrub_subprocess_env(&mut cmd);

        if let Some(dir) = workdir {
            cmd.current_dir(resolve_path(dir)?);
        }

        if background {
            // In background mode, keep stdin pipe open for process(write/submit).
            cmd.stdin(Stdio::piped());
        } else if pty {
            cmd.stdin(Stdio::null());
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let mut child = cmd
                .spawn()
                .map_err(|e| AgentError::Io(format!("Failed to spawn command: {}", e)))?;

            // In background mode, track the process session and return immediately.
            if background {
                let session_id = Self::next_process_session_id();
                let pid = child.id();
                let output = Arc::new(Mutex::new(String::new()));
                let status = Arc::new(Mutex::new(ProcessStatus::default()));
                let stdin = Arc::new(AsyncMutex::new(child.stdin.take()));
                let max_output_chars = self.max_process_output_chars();
                let session = ProcessSession {
                    id: session_id.clone(),
                    command: command.to_string(),
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
                return Ok(CommandOutput {
                    exit_code: 0,
                    stdout: started.to_string(),
                    stderr: String::new(),
                });
            }

            let output = child
                .wait_with_output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to wait for command: {}", e)))?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

            // Truncate output if it exceeds max_output_size
            let stdout = if stdout.len() > self.max_output_size {
                tracing::warn!(
                    "Command output exceeded max size ({} bytes), truncating",
                    stdout.len()
                );
                stdout[..self.max_output_size].to_string()
            } else {
                stdout
            };

            let stderr = if stderr.len() > self.max_output_size {
                tracing::warn!(
                    "Command stderr exceeded max size ({} bytes), truncating",
                    stderr.len()
                );
                stderr[..self.max_output_size].to_string()
            } else {
                stderr
            };

            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout,
                stderr,
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
        let command_with_profiles = with_login_profile_sources(command);

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
                scrub_subprocess_env(&mut pty_cmd);

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }

                let result =
                    tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
                        let mut child = pty_cmd.spawn().map_err(|e| {
                            AgentError::Io(format!("Failed to spawn PTY command: {}", e))
                        })?;
                        if let Some(mut stdin) = child.stdin.take() {
                            stdin.write_all(stdin_owned.as_bytes()).await.map_err(|e| {
                                AgentError::Io(format!("Failed to write PTY stdin: {e}"))
                            })?;
                            stdin.flush().await.map_err(|e| {
                                AgentError::Io(format!("Failed to flush PTY stdin: {e}"))
                            })?;
                        }

                        let output = child.wait_with_output().await.map_err(|e| {
                            AgentError::Io(format!("Failed to wait for PTY command: {}", e))
                        })?;
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        Ok(CommandOutput {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout: if stdout.len() > self.max_output_size {
                                stdout[..self.max_output_size].to_string()
                            } else {
                                stdout
                            },
                            stderr: if stderr.len() > self.max_output_size {
                                stderr[..self.max_output_size].to_string()
                            } else {
                                stderr
                            },
                        })
                    })
                    .await;

                return match result {
                    Ok(Ok(output)) => Ok(output),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(AgentError::Timeout(format!(
                        "PTY command timed out after {} seconds",
                        timeout_secs
                    ))),
                };
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
        scrub_subprocess_env(&mut cmd);
        if let Some(dir) = workdir {
            cmd.current_dir(resolve_path(dir)?);
        }

        let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), async {
            let mut child = cmd
                .spawn()
                .map_err(|e| AgentError::Io(format!("Failed to spawn command: {}", e)))?;
            if let Some(mut stdin) = child.stdin.take() {
                stdin
                    .write_all(stdin_owned.as_bytes())
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to write stdin: {e}")))?;
                stdin
                    .flush()
                    .await
                    .map_err(|e| AgentError::Io(format!("Failed to flush stdin: {e}")))?;
            }

            let output = child
                .wait_with_output()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to wait for command: {}", e)))?;
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(CommandOutput {
                exit_code: output.status.code().unwrap_or(-1),
                stdout: if stdout.len() > self.max_output_size {
                    stdout[..self.max_output_size].to_string()
                } else {
                    stdout
                },
                stderr: if stderr.len() > self.max_output_size {
                    stderr[..self.max_output_size].to_string()
                } else {
                    stderr
                },
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
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use tempfile::tempdir;

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
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_with_login_profile_sources_prepends_profile_loads() {
        let wrapped = with_login_profile_sources("echo hi");
        assert!(wrapped.contains(". \"$HOME/.profile\""));
        assert!(wrapped.contains(". \"$HOME/.bash_profile\""));
        assert!(wrapped.ends_with("echo hi"));
    }

    #[cfg(not(unix))]
    #[test]
    fn test_with_login_profile_sources_is_passthrough_off_unix() {
        let wrapped = with_login_profile_sources("echo hi");
        assert_eq!(wrapped, "echo hi");
    }

    #[tokio::test]
    async fn test_execute_command_echo() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("echo hello", None, None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("hello"));
    }

    #[tokio::test]
    async fn test_execute_command_with_stdin_data() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command_with_stdin("cat", None, None, false, false, Some("hello stdin"))
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("hello stdin"));
    }

    #[tokio::test]
    async fn test_execute_command_with_workdir() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("pwd", None, Some("/tmp"), false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.trim().contains("/tmp"));
    }

    #[tokio::test]
    async fn test_execute_command_timeout() {
        let backend = LocalBackend::new(1, 1_048_576);
        let result = backend
            .execute_command("sleep 30", None, None, false, false)
            .await;
        assert!(result.is_err());
        match result {
            Err(AgentError::Timeout(_)) => {}
            _ => panic!("Expected timeout error"),
        }
    }

    #[tokio::test]
    async fn test_execute_command_failure() {
        let backend = LocalBackend::default();
        let output = backend
            .execute_command("exit 42", None, None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 42);
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let dir = std::env::temp_dir().join("hermes_test_write_read");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("test_file.txt");
        let path_str = path.to_string_lossy().to_string();

        let backend = LocalBackend::default();

        backend
            .write_file(&path_str, "hello\nworld\nfoo\nbar")
            .await
            .unwrap();
        let content = backend.read_file(&path_str, None, None).await.unwrap();
        assert_eq!(content, "hello\nworld\nfoo\nbar");

        // Test with offset
        let content = backend.read_file(&path_str, Some(1), None).await.unwrap();
        assert_eq!(content, "world\nfoo\nbar");

        // Test with offset and limit
        let content = backend
            .read_file(&path_str, Some(1), Some(2))
            .await
            .unwrap();
        assert_eq!(content, "world\nfoo");

        // Cleanup
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[tokio::test]
    async fn test_file_exists() {
        let backend = LocalBackend::default();

        // A path that should exist
        assert!(backend.file_exists("/tmp").await.unwrap());

        // A path that should not exist
        assert!(!backend
            .file_exists("/tmp/hermes_nonexistent_test_file_xyz")
            .await
            .unwrap());
    }

    #[test]
    fn test_resolve_path_rejects_tilde_injection() {
        let malicious = "~; echo PWNED > /tmp/hermes_local_backend_injection";
        let resolved = resolve_path(malicious).unwrap();
        assert_eq!(resolved, Path::new(malicious));
        assert!(!Path::new("/tmp/hermes_local_backend_injection").exists());
    }

    #[test]
    fn test_resolve_path_expands_tilde_username_with_suffix() {
        let Some(user) = current_username() else {
            return;
        };
        let Some(home) = home_dir() else {
            return;
        };

        let resolved = resolve_path(&format!("~{user}/workspace/file.txt")).unwrap();
        assert!(resolved.starts_with(&home));
        assert!(resolved.ends_with("workspace/file.txt"));
    }

    #[tokio::test]
    async fn test_write_file_expands_tilde_home() {
        let td = tempdir().unwrap();
        let _home_guard = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
        let backend = LocalBackend::default();
        let file = "~/nested/path/test.txt";

        backend.write_file(file, "ok").await.unwrap();
        let expanded = td.path().join("nested/path/test.txt");
        let content = std::fs::read_to_string(&expanded).unwrap();
        assert_eq!(content, "ok");
    }

    #[tokio::test]
    async fn test_execute_command_strips_gateway_env_vars() {
        let _token_guard = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "should-not-leak");
        let _managed_guard = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let _http_guard = EnvGuard::set("HERMES_HTTP_API_KEY", "secret-http-key");
        let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
        let backend = LocalBackend::default();

        let output = backend
            .execute_command(
                "printf '%s|%s|%s|%s' \"${TOOL_GATEWAY_USER_TOKEN:-}\" \"${HERMES_ENABLE_NOUS_MANAGED_TOOLS:-}\" \"${HERMES_HTTP_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
                None,
                None,
                false,
                false,
            )
            .await
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "|||ok");
    }

    #[tokio::test]
    async fn test_background_process_lifecycle_with_stdin() {
        let backend = LocalBackend::new(10, 1_048_576);
        let started = backend
            .execute_command("cat", None, None, true, false)
            .await
            .unwrap();
        assert_eq!(started.exit_code, 0);
        let payload: Value = serde_json::from_str(&started.stdout).expect("valid start payload");
        let session_id = payload
            .get("session_id")
            .and_then(Value::as_str)
            .expect("session_id")
            .to_string();

        let write = backend
            .write_process_stdin(&session_id, "hello from stdin\n")
            .await
            .unwrap();
        assert_eq!(write["status"], "ok");

        let close = backend.close_process_stdin(&session_id).await.unwrap();
        assert_eq!(close["status"], "ok");

        let wait = backend.wait_process(&session_id, Some(20)).await.unwrap();
        if wait["status"] == "timeout" {
            let poll = backend.poll_process(&session_id).await.unwrap();
            let _ = backend.kill_process(&session_id).await;
            panic!(
                "background process did not exit after closing stdin: wait={wait}, poll={poll}"
            );
        }
        assert_eq!(wait["status"], "exited");
        assert!(wait["output"]
            .as_str()
            .unwrap_or_default()
            .contains("hello from stdin"));
    }

    #[tokio::test]
    async fn test_background_process_not_found_contract() {
        let backend = LocalBackend::default();
        let poll = backend.poll_process("proc_missing").await.unwrap();
        assert_eq!(poll["status"], "not_found");

        let log = backend
            .read_process_log("proc_missing", None, None)
            .await
            .unwrap();
        assert_eq!(log["status"], "not_found");

        let kill = backend.kill_process("proc_missing").await.unwrap();
        assert_eq!(kill["status"], "not_found");
    }
}
