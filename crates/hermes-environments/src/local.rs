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

use hermes_config::TerminalConfig;
use hermes_core::{AgentError, CommandOutput, TerminalBackend};

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
    /// Env-var names allowed through subprocess secret sanitizers.
    env_passthrough: Vec<String>,
    /// Background processes tracked by session id for lifecycle operations.
    background_processes: Arc<Mutex<HashMap<String, ProcessSession>>>,
}

impl LocalBackend {
    /// Create a new local backend with the given defaults.
    pub fn new(default_timeout: u64, max_output_size: usize) -> Self {
        let (shell_init_files, auto_source_bashrc, env_passthrough) = terminal_config_from_env();
        Self::new_with_shell_init(
            default_timeout,
            max_output_size,
            shell_init_files,
            auto_source_bashrc,
            env_passthrough,
        )
    }

    /// Create a local backend directly from terminal configuration.
    pub fn from_terminal_config(config: &TerminalConfig) -> Self {
        Self::new_with_shell_init(
            config.timeout,
            config.max_output_size,
            config.shell_init_files.clone(),
            config.auto_source_bashrc,
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
        Self {
            default_timeout,
            max_output_size,
            shell_init_files,
            auto_source_bashrc,
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

#[cfg(unix)]
fn configure_foreground_process_group(cmd: &mut TokioCommand) {
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_foreground_process_group(_cmd: &mut TokioCommand) {}

struct ForegroundChildGuard {
    child: Option<tokio::process::Child>,
    pid: Option<u32>,
}

impl ForegroundChildGuard {
    fn new(child: tokio::process::Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    fn child_mut(&mut self) -> &mut tokio::process::Child {
        self.child.as_mut().expect("foreground child present")
    }

    fn disarm(&mut self) {
        self.child.take();
    }
}

impl Drop for ForegroundChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        terminate_child_process_sync(self.pid);
        let _ = child.start_kill();
    }
}

#[cfg(unix)]
fn terminate_child_process_sync(pid: Option<u32>) {
    if let Some(pid) = pid {
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
        unsafe {
            libc::kill(pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn terminate_child_process_sync(_pid: Option<u32>) {}

#[cfg(unix)]
async fn terminate_child_process(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id() {
        let pgid = -(pid as i32);
        unsafe {
            libc::kill(pgid, libc::SIGTERM);
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) | Err(_) => unsafe {
                libc::kill(pgid, libc::SIGKILL);
            },
        }
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(not(unix))]
async fn terminate_child_process(child: &mut tokio::process::Child) {
    let _ = child.kill().await;
    let _ = child.wait().await;
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
        let path = PathBuf::from(input);
        if path.is_absolute() {
            return Ok(path);
        }
        if let Some(cwd) = std::env::var_os("TERMINAL_CWD") {
            if !cwd.is_empty() {
                return Ok(PathBuf::from(cwd).join(path));
            }
        }
        return Ok(path);
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
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_TOKEN",
    "AWS_BEARER_TOKEN_BEDROCK",
    "BROWSERBASE_PROJECT_ID",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "COHERE_API_KEY",
    "DAYTONA_API_KEY",
    "DEEPSEEK_API_KEY",
    "DISCORD_FREE_RESPONSE_CHANNELS",
    "DISCORD_HOME_CHANNEL",
    "DISCORD_HOME_CHANNEL_NAME",
    "DISCORD_REQUIRE_MENTION",
    "EMAIL_ADDRESS",
    "EMAIL_HOME_ADDRESS",
    "EMAIL_HOME_ADDRESS_NAME",
    "EMAIL_IMAP_HOST",
    "EMAIL_PASSWORD",
    "EMAIL_SMTP_HOST",
    "ELEVENLABS_API_KEY",
    "FIRECRAWL_API_KEY",
    "FIREWORKS_API_KEY",
    "GATEWAY_ALLOW_ALL_USERS",
    "GATEWAY_ALLOWED_USERS",
    "GH_TOKEN",
    "GITHUB_APP_ID",
    "GITHUB_APP_INSTALLATION_ID",
    "GITHUB_APP_PRIVATE_KEY_PATH",
    "GITHUB_TOKEN",
    "GLM_API_KEY",
    "GOOGLE_API_KEY",
    "GROQ_API_KEY",
    "HASS_TOKEN",
    "HASS_URL",
    "HELICONE_API_KEY",
    "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
    "HERMES_POLICY_ADMIN_TOKEN",
    "KIMI_API_KEY",
    "LLM_MODEL",
    "MINIMAX_API_KEY",
    "MINIMAX_CN_API_KEY",
    "MISTRAL_API_KEY",
    "MODAL_TOKEN_ID",
    "MODAL_TOKEN_SECRET",
    "NVIDIA_API_KEY",
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENROUTER_API_KEY",
    "PERPLEXITY_API_KEY",
    "SIGNAL_ACCOUNT",
    "SIGNAL_ALLOWED_USERS",
    "SIGNAL_GROUP_ALLOWED_USERS",
    "SIGNAL_HOME_CHANNEL",
    "SIGNAL_HOME_CHANNEL_NAME",
    "SIGNAL_HTTP_URL",
    "SIGNAL_IGNORE_STORIES",
    "SLACK_ALLOWED_USERS",
    "SLACK_APP_TOKEN",
    "SLACK_HOME_CHANNEL",
    "SLACK_HOME_CHANNEL_NAME",
    "TELEGRAM_BOT_TOKEN",
    "TELEGRAM_HOME_CHANNEL",
    "TELEGRAM_HOME_CHANNEL_NAME",
    "TOGETHER_API_KEY",
    "WHATSAPP_ALLOWED_USERS",
    "WHATSAPP_ENABLED",
    "WHATSAPP_MODE",
    "XAI_API_KEY",
    "ZAI_API_KEY",
    "Z_AI_API_KEY",
];

const SUBPROCESS_ENV_BLOCKLIST_PREFIXES: &[&str] = &[
    "TOOL_GATEWAY_",
    "HERMES_MANAGED_TOOL_GATEWAY_",
    "HERMES_GATEWAY_",
    "HERMES_HTTP_",
];

const SUBPROCESS_ENV_FORCE_PREFIX: &str = "_HERMES_FORCE_";
const SUBPROCESS_ENV_PASSTHROUGH_VAR: &str = "HERMES_SUBPROCESS_ENV_PASSTHROUGH";

const SANE_PATH_ENTRIES: &[&str] = &[
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
    "/opt/homebrew/bin",
    "/opt/homebrew/sbin",
];

fn should_strip_subprocess_env(key: &str) -> bool {
    SUBPROCESS_ENV_BLOCKLIST_EXACT.contains(&key)
        || SUBPROCESS_ENV_BLOCKLIST_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

fn normalize_env_passthrough_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_subprocess_env_passthrough(value: &str) -> BTreeSet<String> {
    value
        .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ':' | ';'))
        .filter_map(normalize_env_passthrough_name)
        .collect()
}

fn subprocess_env_passthrough_set(configured: &[String]) -> BTreeSet<String> {
    let mut values = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR)
        .ok()
        .map(|raw| parse_subprocess_env_passthrough(&raw))
        .unwrap_or_default();
    values.extend(
        configured
            .iter()
            .filter_map(|name| normalize_env_passthrough_name(name)),
    );
    values
}

fn is_subprocess_env_passthrough(key: &str, passthrough: &BTreeSet<String>) -> bool {
    passthrough.contains(key)
}

fn normalize_subprocess_path(path: Option<&str>) -> String {
    let Some(path) = path.filter(|value| !value.trim().is_empty()) else {
        return SANE_PATH_ENTRIES.join(":");
    };
    if std::env::split_paths(path).any(|entry| entry == std::path::Path::new("/usr/bin")) {
        return path.to_string();
    }

    let mut entries: Vec<String> = std::env::split_paths(path)
        .map(|entry| entry.to_string_lossy().to_string())
        .filter(|entry| !entry.is_empty())
        .collect();
    for sane in SANE_PATH_ENTRIES {
        if !entries.iter().any(|entry| entry == sane) {
            entries.push((*sane).to_string());
        }
    }
    entries.join(":")
}

fn shell_env_cleanup_snippet(configured_passthrough: &[String]) -> String {
    let mut snippet = String::new();
    let configured_passthrough = configured_passthrough
        .iter()
        .filter_map(|name| normalize_env_passthrough_name(name))
        .collect::<Vec<_>>()
        .join(" ");
    if !configured_passthrough.is_empty() {
        snippet.push_str(
            "HERMES_SUBPROCESS_ENV_PASSTHROUGH=\"${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} ",
        );
        snippet.push_str(&configured_passthrough);
        snippet.push_str("\"; ");
    }
    if !SUBPROCESS_ENV_BLOCKLIST_PREFIXES.is_empty() {
        snippet
            .push_str("for __hermes_env in $(env | sed 's/=.*//'); do case \"$__hermes_env\" in ");
        for (idx, prefix) in SUBPROCESS_ENV_BLOCKLIST_PREFIXES.iter().enumerate() {
            if idx > 0 {
                snippet.push('|');
            }
            snippet.push_str(prefix);
            snippet.push('*');
        }
        snippet.push_str(") case \" ${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} \" in *\" $__hermes_env \"*) ;; *) unset \"$__hermes_env\" ;; esac ;; esac; done; unset __hermes_env; ");
    }
    for key in SUBPROCESS_ENV_BLOCKLIST_EXACT {
        snippet.push_str("case \" ${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-} \" in *\" ");
        snippet.push_str(key);
        snippet.push_str(" \"*) ;; *) unset ");
        snippet.push_str(key);
        snippet.push_str(" ;; esac; ");
    }
    snippet.push_str("unset HERMES_SUBPROCESS_FORCE_TARGETS; ");
    snippet
}

fn parse_shell_init_list(value: &str) -> Vec<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.starts_with('[') {
        if let Ok(values) = serde_json::from_str::<Vec<String>>(trimmed) {
            return values
                .into_iter()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect();
        }
    }
    let delimiter = if trimmed.contains(',') { ',' } else { ':' };
    trimmed
        .split(delimiter)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn bool_env_or_default(name: &str, default: bool) -> bool {
    let Some(value) = std::env::var(name).ok() else {
        return default;
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn terminal_config_from_env() -> (Vec<String>, bool, Vec<String>) {
    let explicit = std::env::var("TERMINAL_SHELL_INIT_FILES")
        .ok()
        .map(|v| parse_shell_init_list(&v))
        .unwrap_or_default();
    let auto_source_bashrc = bool_env_or_default("TERMINAL_AUTO_SOURCE_BASHRC", true);
    let env_passthrough = std::env::var(SUBPROCESS_ENV_PASSTHROUGH_VAR)
        .ok()
        .map(|v| parse_subprocess_env_passthrough(&v).into_iter().collect())
        .unwrap_or_default();
    (explicit, auto_source_bashrc, env_passthrough)
}

fn expand_env_refs(input: &str) -> String {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        let Some(end) = after_open.find('}') else {
            output.push_str(&rest[start..]);
            return output;
        };
        let name = &after_open[..end];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            if let Ok(value) = std::env::var(name) {
                output.push_str(&value);
            }
        } else {
            output.push_str("${");
            output.push_str(name);
            output.push('}');
        }
        rest = &after_open[end + 1..];
    }
    output.push_str(rest);
    output
}

fn expand_shell_init_path(input: &str) -> PathBuf {
    let expanded = expand_env_refs(input.trim());
    if expanded == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(expanded));
    }
    if let Some(rest) = expanded.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(expanded)
}

fn resolve_existing_shell_init_files(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    paths.into_iter().filter(|p| p.is_file()).collect()
}

fn auto_shell_init_candidates(shell: &str) -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    match shell {
        "zsh" => [".zshenv", ".zprofile", ".zshrc", ".profile"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
        _ => [".profile", ".bash_profile", ".bashrc"]
            .into_iter()
            .map(|file| home.join(file))
            .collect(),
    }
}

fn resolve_shell_init_files_for_shell(
    shell: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
) -> Vec<PathBuf> {
    if !explicit_files.is_empty() {
        return resolve_existing_shell_init_files(
            explicit_files
                .iter()
                .map(|path| expand_shell_init_path(path.as_str())),
        );
    }
    if !auto_source_bashrc {
        return Vec::new();
    }
    resolve_existing_shell_init_files(auto_shell_init_candidates(shell))
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_source_prelude(files: &[PathBuf]) -> String {
    let mut prelude = String::new();
    if files.is_empty() {
        return prelude;
    }
    prelude.push_str("set +e; ");
    for file in files {
        let quoted = shell_single_quote(&file.to_string_lossy());
        prelude.push_str("[ -r ");
        prelude.push_str(&quoted);
        prelude.push_str(" ] && . ");
        prelude.push_str(&quoted);
        prelude.push_str(" || true; ");
    }
    prelude
}

fn scrub_subprocess_env(cmd: &mut TokioCommand, configured_passthrough: &[String]) {
    let passthrough = subprocess_env_passthrough_set(configured_passthrough);
    let mut forced = Vec::new();
    for (key, _) in std::env::vars() {
        if should_strip_subprocess_env(&key) && !is_subprocess_env_passthrough(&key, &passthrough) {
            cmd.env_remove(key);
        } else if let Some(target) = key.strip_prefix(SUBPROCESS_ENV_FORCE_PREFIX) {
            cmd.env_remove(&key);
            if !target.is_empty() && should_strip_subprocess_env(target) {
                if let Ok(value) = std::env::var(&key) {
                    forced.push((target.to_string(), value));
                }
            }
        }
    }
    for (target, value) in forced {
        cmd.env(target, value);
    }
    let forced_targets: Vec<String> = std::env::vars()
        .filter_map(|(key, _)| {
            key.strip_prefix(SUBPROCESS_ENV_FORCE_PREFIX)
                .filter(|target| !target.is_empty() && should_strip_subprocess_env(target))
                .map(ToString::to_string)
        })
        .collect();
    if forced_targets.is_empty() {
        cmd.env_remove("HERMES_SUBPROCESS_FORCE_TARGETS");
    } else {
        cmd.env("HERMES_SUBPROCESS_FORCE_TARGETS", forced_targets.join(" "));
    }
    if passthrough.is_empty() {
        cmd.env_remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
    } else {
        cmd.env(
            SUBPROCESS_ENV_PASSTHROUGH_VAR,
            passthrough.into_iter().collect::<Vec<_>>().join(" "),
        );
    }

    let normalized_path = normalize_subprocess_path(std::env::var("PATH").ok().as_deref());
    cmd.env("PATH", normalized_path);
}

fn with_login_profile_sources(
    command: &str,
    explicit_files: &[String],
    auto_source_bashrc: bool,
    env_passthrough: &[String],
) -> String {
    #[cfg(unix)]
    {
        let cleanup = shell_env_cleanup_snippet(env_passthrough);
        let bash_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "bash",
            explicit_files,
            auto_source_bashrc,
        ));
        let zsh_prelude = shell_source_prelude(&resolve_shell_init_files_for_shell(
            "zsh",
            explicit_files,
            auto_source_bashrc,
        ));
        let bash_command = shell_single_quote(&format!("{bash_prelude}{cleanup}{command}"));
        let zsh_command = shell_single_quote(&format!("{zsh_prelude}{cleanup}{command}"));
        let preferred_shell = std::env::var("SHELL")
            .ok()
            .and_then(|raw| {
                std::path::Path::new(raw.trim())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.to_ascii_lowercase())
            })
            .filter(|name| matches!(name.as_str(), "bash" | "zsh"));
        let preferred_branch = match preferred_shell.as_deref() {
            Some("zsh") => {
                format!("if command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; fi; ")
            }
            Some("bash") => format!(
                "if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; fi; "
            ),
            _ => String::new(),
        };
        format!(
            "{preferred_branch}if command -v bash >/dev/null 2>&1; then exec bash -lc {bash_command}; \
elif command -v zsh >/dev/null 2>&1; then exec zsh -lc {zsh_command}; \
else printf '%s\n' \"Hermes could not find bash or zsh in PATH. Run 'exec zsh -l' or set your default shell where env vars are available.\" >&2; exit 127; fi"
        )
    }
    #[cfg(not(unix))]
    {
        let _ = explicit_files;
        let _ = auto_source_bashrc;
        let _ = env_passthrough;
        command.to_string()
    }
}

fn rewrite_compound_background(command: &str) -> String {
    let mut out = String::with_capacity(command.len());
    for line in command.split_inclusive('\n') {
        let (body, newline) = line
            .strip_suffix('\n')
            .map(|body| (body, "\n"))
            .unwrap_or((line, ""));
        out.push_str(&rewrite_compound_background_line(body));
        out.push_str(newline);
    }
    out
}

fn rewrite_compound_background_line(line: &str) -> String {
    if line.trim_start().starts_with('#') {
        return line.to_string();
    }
    let Some(amp_idx) = trailing_background_ampersand(line) else {
        return line.to_string();
    };
    let Some(op) = last_top_level_chain_operator(line, amp_idx) else {
        return line.to_string();
    };

    let mut tail_start = op.end;
    while tail_start < amp_idx {
        let Some(ch) = line[tail_start..amp_idx].chars().next() else {
            break;
        };
        if !ch.is_whitespace() {
            break;
        }
        tail_start += ch.len_utf8();
    }
    if line[tail_start..amp_idx].trim().is_empty() {
        return line.to_string();
    }

    let mut rewritten = String::with_capacity(line.len() + 4);
    rewritten.push_str(&line[..tail_start]);
    rewritten.push_str("{ ");
    rewritten.push_str(&line[tail_start..=amp_idx]);
    rewritten.push_str(" }");
    rewritten.push_str(&line[amp_idx + 1..]);
    rewritten
}

#[derive(Clone, Copy)]
struct ChainOperator {
    end: usize,
}

fn trailing_background_ampersand(line: &str) -> Option<usize> {
    let mut idx = line.len();
    while idx > 0 {
        let (prev, ch) = line[..idx].char_indices().next_back()?;
        if !ch.is_whitespace() {
            idx = prev;
            break;
        }
        idx = prev;
    }
    if line[idx..].chars().next()? != '&' || is_escaped(line, idx) {
        return None;
    }
    if idx > 0 && line[..idx].ends_with('&') {
        return None;
    }
    Some(idx)
}

fn is_escaped(input: &str, idx: usize) -> bool {
    let mut count = 0usize;
    let mut pos = idx;
    while pos > 0 {
        let Some((prev, ch)) = input[..pos].char_indices().next_back() else {
            break;
        };
        if ch != '\\' {
            break;
        }
        count += 1;
        pos = prev;
    }
    count % 2 == 1
}

fn last_top_level_chain_operator(line: &str, stop: usize) -> Option<ChainOperator> {
    let mut last = None;
    let mut single = false;
    let mut double = false;
    let mut paren_depth = 0usize;
    let mut command_sub_depth = 0usize;
    let mut iter = line[..stop].char_indices().peekable();

    while let Some((idx, ch)) = iter.next() {
        if is_escaped(line, idx) {
            continue;
        }
        if single {
            if ch == '\'' {
                single = false;
            }
            continue;
        }
        if double {
            if ch == '"' {
                double = false;
                continue;
            }
            if ch == '$' && iter.peek().is_some_and(|(_, next)| *next == '(') {
                command_sub_depth += 1;
                iter.next();
            }
            continue;
        }

        match ch {
            '\'' => single = true,
            '"' => double = true,
            '$' if iter.peek().is_some_and(|(_, next)| *next == '(') => {
                command_sub_depth += 1;
                iter.next();
            }
            '(' if command_sub_depth == 0 => paren_depth += 1,
            ')' if command_sub_depth > 0 => command_sub_depth -= 1,
            ')' if paren_depth > 0 => paren_depth -= 1,
            ';' if paren_depth == 0 && command_sub_depth == 0 => last = None,
            '|' if paren_depth == 0 && command_sub_depth == 0 => {
                if iter.peek().is_some_and(|(_, next)| *next == '|') {
                    iter.next();
                    last = Some(ChainOperator { end: idx + 2 });
                } else {
                    last = None;
                }
            }
            '&' if paren_depth == 0
                && command_sub_depth == 0
                && iter.peek().is_some_and(|(_, next)| *next == '&') =>
            {
                iter.next();
                last = Some(ChainOperator { end: idx + 2 });
            }
            _ => {}
        }
    }
    last
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
        let rewritten_command = rewrite_compound_background(command);
        let command_with_profiles = with_login_profile_sources(
            &rewritten_command,
            &self.shell_init_files,
            self.auto_source_bashrc,
            &self.env_passthrough,
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

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }

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
        let command_with_profiles = with_login_profile_sources(
            &rewritten_command,
            &self.shell_init_files,
            self.auto_source_bashrc,
            &self.env_passthrough,
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

                if let Some(dir) = workdir {
                    pty_cmd.current_dir(resolve_path(dir)?);
                }

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
        if let Some(dir) = workdir {
            cmd.current_dir(resolve_path(dir)?);
        }

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
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use tempfile::tempdir;

    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    #[cfg(unix)]
    fn pids_for_marker(marker: &str) -> Vec<u32> {
        let output = std::process::Command::new("ps")
            .args(["-axo", "pid=,command="])
            .output()
            .expect("ps output");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| line.contains(marker))
            .filter_map(|line| line.split_whitespace().next()?.parse::<u32>().ok())
            .collect()
    }

    #[cfg(unix)]
    async fn wait_for_marker(marker: &str, present: bool, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            let found = !pids_for_marker(marker).is_empty();
            if found == present {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let found = !pids_for_marker(marker).is_empty();
        found == present
    }

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
                Some(v) => std::env::set_var(self.key, v),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_with_login_profile_sources_prepends_profile_loads() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        for file in [".profile", ".bash_profile", ".bashrc", ".zshrc"] {
            std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
        }
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
        let wrapped = with_login_profile_sources("echo hi", &[], true, &[]);
        assert!(wrapped.contains("command -v bash"));
        assert!(wrapped.contains("exec bash -lc"));
        assert!(wrapped.contains(".bash_profile"));
        assert!(wrapped.contains(".bashrc"));
        assert!(wrapped.contains("command -v zsh"));
        assert!(wrapped.contains("exec zsh -lc"));
        assert!(wrapped.contains(".zshrc"));
        assert!(wrapped.contains("echo hi"));
    }

    #[cfg(unix)]
    #[test]
    fn test_with_login_profile_sources_prefers_user_shell_when_supported() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _shell = EnvGuard::set("SHELL", "/bin/zsh");
        let wrapped = with_login_profile_sources("echo hi", &[], true, &[]);
        let preferred = "if command -v zsh >/dev/null 2>&1; then exec zsh -lc";
        let fallback = "if command -v bash >/dev/null 2>&1; then exec bash -lc";
        let preferred_idx = wrapped.find(preferred).expect("preferred zsh branch");
        let fallback_idx = wrapped.find(fallback).expect("fallback bash branch");
        assert!(
            preferred_idx < fallback_idx,
            "preferred shell branch should come before fallback"
        );
    }

    #[test]
    fn test_resolve_shell_init_files_auto_profile_before_bashrc() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        for file in [".profile", ".bash_profile", ".bashrc"] {
            std::fs::write(td.path().join(file), "export HERMES_TEST=1\n").unwrap();
        }
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

        let resolved = resolve_shell_init_files_for_shell("bash", &[], true);
        let names = resolved
            .iter()
            .filter_map(|p| p.file_name().and_then(|name| name.to_str()))
            .collect::<Vec<_>>();
        assert_eq!(names, [".profile", ".bash_profile", ".bashrc"]);
    }

    #[test]
    fn test_resolve_shell_init_files_explicit_list_wins_over_auto() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
        let custom = td.path().join("custom.sh");
        std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

        let resolved = resolve_shell_init_files_for_shell(
            "bash",
            &[custom.to_string_lossy().to_string()],
            true,
        );
        assert_eq!(resolved, [custom]);
    }

    #[test]
    fn test_resolve_shell_init_files_auto_source_off_suppresses_defaults() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        std::fs::write(td.path().join(".bashrc"), "export FROM_BASHRC=1\n").unwrap();
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());

        let resolved = resolve_shell_init_files_for_shell("bash", &[], false);
        assert!(resolved.is_empty());
    }

    #[test]
    fn test_resolve_shell_init_files_expands_home_and_env_vars() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        let rc_dir = td.path().join("rc");
        std::fs::create_dir_all(&rc_dir).unwrap();
        let custom = rc_dir.join("custom.sh");
        std::fs::write(&custom, "export FROM_CUSTOM=1\n").unwrap();
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
        let _custom_dir = EnvGuard::set("CUSTOM_RC_DIR", rc_dir.to_string_lossy().as_ref());

        let home_resolved =
            resolve_shell_init_files_for_shell("bash", &["~/rc/custom.sh".to_string()], false);
        let env_resolved = resolve_shell_init_files_for_shell(
            "bash",
            &["${CUSTOM_RC_DIR}/custom.sh".to_string()],
            false,
        );
        assert_eq!(home_resolved.as_slice(), std::slice::from_ref(&custom));
        assert_eq!(env_resolved, [custom]);
    }

    #[test]
    fn test_subprocess_env_passthrough_parses_configured_and_env_values() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _passthrough = EnvGuard::set(
            SUBPROCESS_ENV_PASSTHROUGH_VAR,
            "OPENAI_API_KEY,ANTHROPIC_TOKEN:BAD-NAME VALID_NAME",
        );
        let parsed = subprocess_env_passthrough_set(&["GOOGLE_API_KEY".to_string()]);
        assert!(parsed.contains("OPENAI_API_KEY"));
        assert!(parsed.contains("ANTHROPIC_TOKEN"));
        assert!(parsed.contains("VALID_NAME"));
        assert!(parsed.contains("GOOGLE_API_KEY"));
        assert!(!parsed.contains("BAD-NAME"));
    }

    #[cfg(unix)]
    #[test]
    fn test_shell_cleanup_keeps_configured_passthrough() {
        let wrapped = with_login_profile_sources(
            "echo hi",
            &[],
            true,
            &[
                "OPENAI_API_KEY".to_string(),
                "HERMES_GATEWAY_SECRET".to_string(),
            ],
        );
        assert!(wrapped.contains("HERMES_SUBPROCESS_ENV_PASSTHROUGH"));
        assert!(wrapped.contains("OPENAI_API_KEY"));
        assert!(wrapped.contains("HERMES_GATEWAY_SECRET"));
        assert!(wrapped.contains(
            "${HERMES_SUBPROCESS_FORCE_TARGETS:-} ${HERMES_SUBPROCESS_ENV_PASSTHROUGH:-}"
        ));
    }

    #[test]
    fn test_terminal_sanitizer_blocks_provider_env_by_default() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
        let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
        let backend =
            LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

        let output = block_on(backend.execute_command(
            "printf '%s' \"${OPENAI_API_KEY-unset}\"",
            Some(10),
            None,
            false,
            false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "unset");
    }

    #[test]
    fn test_terminal_config_passthrough_allows_blocklisted_env() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _api = EnvGuard::set("OPENAI_API_KEY", "secret-value");
        let _passthrough = EnvGuard::remove(SUBPROCESS_ENV_PASSTHROUGH_VAR);
        let backend = LocalBackend::new_with_shell_init(
            10,
            1_048_576,
            Vec::new(),
            true,
            vec!["OPENAI_API_KEY".to_string()],
        );

        let output = block_on(backend.execute_command(
            "printf '%s' \"$OPENAI_API_KEY\"",
            Some(10),
            None,
            false,
            false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "secret-value");
    }

    #[test]
    fn test_terminal_registry_env_passthrough_allows_prefix_blocked_env() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _gateway = EnvGuard::set("HERMES_GATEWAY_SECRET", "gateway-secret");
        let _passthrough = EnvGuard::set(SUBPROCESS_ENV_PASSTHROUGH_VAR, "HERMES_GATEWAY_SECRET");
        let backend =
            LocalBackend::new_with_shell_init(10, 1_048_576, Vec::new(), true, Vec::new());

        let output = block_on(backend.execute_command(
            "printf '%s' \"$HERMES_GATEWAY_SECRET\"",
            Some(10),
            None,
            false,
            false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "gateway-secret");
    }

    #[test]
    fn test_rewrite_compound_background_contract() {
        assert_eq!(rewrite_compound_background("A && B &"), "A && { B & }");
        assert_eq!(rewrite_compound_background("A || B &"), "A || { B & }");
        assert_eq!(
            rewrite_compound_background("A && B && C &"),
            "A && B && { C & }"
        );
        assert_eq!(
            rewrite_compound_background("cd /tmp && server &\nsleep 1"),
            "cd /tmp && { server & }\nsleep 1"
        );
        assert_eq!(rewrite_compound_background("sleep 5 &"), "sleep 5 &");
        assert_eq!(rewrite_compound_background("A && B | C &"), "A && B | C &");
        assert_eq!(
            rewrite_compound_background("A && B &>/dev/null &"),
            "A && { B &>/dev/null & }"
        );
        assert_eq!(
            rewrite_compound_background("echo 'A && B &'"),
            "echo 'A && B &'"
        );
        assert_eq!(
            rewrite_compound_background("   A && B &"),
            "   A && { B & }"
        );
        let once = rewrite_compound_background("A && B &");
        assert_eq!(rewrite_compound_background(&once), once);
    }

    #[cfg(not(unix))]
    #[test]
    fn test_with_login_profile_sources_is_passthrough_off_unix() {
        let wrapped = with_login_profile_sources("echo hi", &[], true, &[]);
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
    async fn test_execute_command_sources_explicit_shell_init_file() {
        let td = tempdir().unwrap();
        let init = td.path().join("custom-init.sh");
        std::fs::write(
            &init,
            "export HERMES_SHELL_INIT_PROBE=probe-ok\nexport PATH=\"/opt/shell-init-probe/bin:$PATH\"\n",
        )
        .unwrap();
        let backend = LocalBackend::new_with_shell_init(
            10,
            1_048_576,
            vec![init.to_string_lossy().to_string()],
            false,
            Vec::new(),
        );

        let output = backend
            .execute_command(
                "printf '%s|%s' \"$HERMES_SHELL_INIT_PROBE\" \"$PATH\"",
                Some(10),
                Some(td.path().to_string_lossy().as_ref()),
                false,
                false,
            )
            .await
            .unwrap();

        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("probe-ok"));
        assert!(output.stdout.contains("/opt/shell-init-probe/bin"));
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

    #[cfg(unix)]
    #[tokio::test]
    async fn test_foreground_child_group_is_killed_when_future_is_aborted() {
        let backend = Arc::new(LocalBackend::new(60, 1_048_576));
        let marker = format!("hermes_abort_guard_{}", std::process::id());
        let command = format!("python3 -c 'import time; time.sleep(60)' {marker}");
        let task = {
            let backend = backend.clone();
            tokio::spawn(async move {
                backend
                    .execute_command(&command, Some(60), None, false, false)
                    .await
            })
        };

        assert!(
            wait_for_marker(&marker, true, std::time::Duration::from_secs(5)).await,
            "test setup failed to observe marker process"
        );
        task.abort();
        let _ = task.await;
        assert!(
            wait_for_marker(&marker, false, std::time::Duration::from_secs(5)).await,
            "foreground process marker survived future cancellation: {:?}",
            pids_for_marker(&marker)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_plain_shell_background_child_does_not_hang_foreground_collection() {
        let backend = LocalBackend::new(10, 1_048_576);
        let marker = "hermes_bg_nohang_marker";
        let probe = "hermes_bg_nohang_probe";
        let command = format!("python3 -c 'import time; time.sleep(60)' {probe} & echo {marker}");
        let started = std::time::Instant::now();
        let output = backend
            .execute_command(&command, Some(5), None, false, false)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        let _ = std::process::Command::new("pkill")
            .args(["-f", probe])
            .status();

        assert!(
            elapsed < std::time::Duration::from_secs(3),
            "foreground collection hung for {elapsed:?}"
        );
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains(marker), "stdout={:?}", output.stdout);
    }

    #[tokio::test]
    async fn test_foreground_collection_preserves_multibyte_utf8_boundaries() {
        let backend = LocalBackend::new(10, 100_000);
        let command = "python3 -c 'import sys; sys.stdout.buffer.write(chr(0x65e5).encode(\"utf-8\") * 10000); sys.stdout.buffer.write(b\"\\n\")'";
        let output = backend
            .execute_command(command, Some(10), None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.matches('\u{65e5}').count(), 10_000);
        assert!(!output.stdout.contains("binary output detected"));
    }

    #[tokio::test]
    async fn test_foreground_collection_preserves_high_volume_line_output() {
        let backend = LocalBackend::new(10, 1_048_576);
        let output = backend
            .execute_command("seq 1 3000", Some(10), None, false, false)
            .await
            .unwrap();
        let lines = output.stdout.trim().split('\n').collect::<Vec<_>>();
        assert_eq!(output.exit_code, 0);
        assert_eq!(lines.len(), 3000);
        assert_eq!(lines.first().copied(), Some("1"));
        assert_eq!(lines.last().copied(), Some("3000"));
    }

    #[tokio::test]
    async fn test_foreground_collection_replaces_invalid_utf8() {
        let backend = LocalBackend::new(10, 1_048_576);
        let command = "python3 -c 'import sys; sys.stdout.buffer.write(b\"before \"); sys.stdout.buffer.write(b\"\\xff\\xfe\"); sys.stdout.buffer.write(b\" after\\n\")'";
        let output = backend
            .execute_command(command, Some(5), None, false, false)
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);
        assert!(output.stdout.contains("before"));
        assert!(output.stdout.contains("after"));
        assert!(output.stdout.contains('\u{fffd}'));
        assert!(!output.stdout.contains("binary output detected"));
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
    async fn test_relative_file_paths_use_terminal_cwd() {
        let td = tempdir().unwrap();
        let terminal_cwd = td.path().join("worktree");
        std::fs::create_dir_all(&terminal_cwd).unwrap();
        let _cwd_guard = EnvGuard::set("TERMINAL_CWD", terminal_cwd.to_string_lossy().as_ref());
        let backend = LocalBackend::default();

        backend
            .write_file("nested/file.txt", "from terminal cwd")
            .await
            .unwrap();

        let expected = terminal_cwd.join("nested/file.txt");
        assert_eq!(
            std::fs::read_to_string(&expected).unwrap(),
            "from terminal cwd"
        );
        assert_eq!(
            backend
                .read_file("nested/file.txt", None, None)
                .await
                .unwrap(),
            "from terminal cwd"
        );
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

    #[test]
    fn test_execute_command_strips_gateway_env_vars() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _token_guard = EnvGuard::set("TOOL_GATEWAY_USER_TOKEN", "should-not-leak");
        let _managed_guard = EnvGuard::set("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let _http_guard = EnvGuard::set("HERMES_HTTP_API_KEY", "secret-http-key");
        let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
        let backend = LocalBackend::default();

        let output = block_on(backend.execute_command(
                "printf '%s|%s|%s|%s' \"${TOOL_GATEWAY_USER_TOKEN:-}\" \"${HERMES_ENABLE_NOUS_MANAGED_TOOLS:-}\" \"${HERMES_HTTP_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
                None,
                None,
                false,
                false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "|||ok");
    }

    #[test]
    fn test_execute_command_strips_provider_tool_and_gateway_env_vars() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _openai_key = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
        let _openai_base = EnvGuard::set("OPENAI_BASE_URL", "http://localhost:8000/v1");
        let _bedrock_bearer = EnvGuard::set("AWS_BEARER_TOKEN_BEDROCK", "bedrock-secret");
        let _github = EnvGuard::set("GITHUB_TOKEN", "ghp-secret");
        let _modal = EnvGuard::set("MODAL_TOKEN_SECRET", "modal-secret");
        let _gateway = EnvGuard::set("GATEWAY_ALLOWED_USERS", "alice,bob");
        let _safe_guard = EnvGuard::set("SAFE_PASSTHRU_TEST", "ok");
        let backend = LocalBackend::default();

        let output = block_on(backend.execute_command(
                "printf '%s|%s|%s|%s|%s|%s|%s' \"${OPENAI_API_KEY:-}\" \"${OPENAI_BASE_URL:-}\" \"${AWS_BEARER_TOKEN_BEDROCK:-}\" \"${GITHUB_TOKEN:-}\" \"${MODAL_TOKEN_SECRET:-}\" \"${GATEWAY_ALLOWED_USERS:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
                None,
                None,
                false,
                false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "||||||ok");
    }

    #[test]
    fn test_execute_command_preserves_general_aws_credentials() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _access_key = EnvGuard::set("AWS_ACCESS_KEY_ID", "AKIAIOSFODNN7EXAMPLE");
        let _secret_key = EnvGuard::set("AWS_SECRET_ACCESS_KEY", "aws-secret");
        let _session = EnvGuard::set("AWS_SESSION_TOKEN", "aws-session");
        let backend = LocalBackend::default();

        let output = block_on(backend.execute_command(
                "printf '%s|%s|%s' \"${AWS_ACCESS_KEY_ID:-}\" \"${AWS_SECRET_ACCESS_KEY:-}\" \"${AWS_SESSION_TOKEN:-}\"",
                None,
                None,
                false,
                false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "AKIAIOSFODNN7EXAMPLE|aws-secret|aws-session");
        for var in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "AWS_DEFAULT_REGION",
            "AWS_REGION",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_CONFIG_FILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_ROLE_ARN",
        ] {
            assert!(
                !should_strip_subprocess_env(var),
                "{var} must not be in the Hermes subprocess blocklist"
            );
        }
    }

    #[test]
    fn test_execute_command_force_prefix_reinjects_blocked_var() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let _blocked = EnvGuard::set("OPENAI_API_KEY", "sk-should-not-leak");
        let _forced = EnvGuard::set("_HERMES_FORCE_OPENAI_API_KEY", "sk-explicit");
        let backend = LocalBackend::default();

        let output = block_on(backend.execute_command(
            "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${_HERMES_FORCE_OPENAI_API_KEY:-}\"",
            None,
            None,
            false,
            false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "sk-explicit|");
    }

    #[test]
    fn test_execute_command_cleans_profile_reintroduced_blocked_vars() {
        let _lock = ENV_TEST_LOCK.lock().expect("lock env");
        let td = tempdir().unwrap();
        std::fs::write(
            td.path().join(".profile"),
            "export OPENAI_API_KEY=from-profile\nexport SAFE_PASSTHRU_TEST=ok\n",
        )
        .unwrap();
        let _home = EnvGuard::set("HOME", td.path().to_string_lossy().as_ref());
        let backend = LocalBackend::default();

        let output = block_on(backend.execute_command(
            "printf '%s|%s' \"${OPENAI_API_KEY:-}\" \"${SAFE_PASSTHRU_TEST:-}\"",
            None,
            None,
            false,
            false,
        ))
        .unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout, "|ok");
    }

    #[test]
    fn test_subprocess_path_appends_homebrew_when_path_is_minimal() {
        let normalized = normalize_subprocess_path(Some("/some/custom/bin"));
        assert!(normalized.contains("/some/custom/bin"));
        assert!(normalized.contains("/usr/bin"));
        assert!(normalized.contains("/opt/homebrew/bin"));
        assert!(normalized.contains("/opt/homebrew/sbin"));
    }

    #[test]
    fn test_subprocess_path_preserves_full_path() {
        assert_eq!(
            normalize_subprocess_path(Some("/usr/bin:/bin")),
            "/usr/bin:/bin"
        );
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
            panic!("background process did not exit after closing stdin: wait={wait}, poll={poll}");
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
