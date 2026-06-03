//! JSON-RPC 2.0 client for `codex app-server` over stdio (Python `codex_app_server.py`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// Minimum tested codex CLI version (major, minor, patch).
pub const MIN_CODEX_VERSION: (u32, u32, u32) = (0, 125, 0);

#[derive(Debug, Clone)]
pub struct CodexAppServerError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl std::fmt::Display for CodexAppServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "codex app-server error {}: {}",
            self.code, self.message
        )
    }
}

impl std::error::Error for CodexAppServerError {}

struct Pending {
    tx: std::sync::mpsc::Sender<Value>,
    method: String,
}

/// Blocking JSON-RPC client — drive from async via [`tokio::task::spawn_blocking`].
pub struct CodexAppServerClient {
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    child: Arc<Mutex<Option<Child>>>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    notifications: Arc<Mutex<std::collections::VecDeque<Value>>>,
    server_requests: Arc<Mutex<std::collections::VecDeque<Value>>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
    closed: AtomicBool,
    initialized: AtomicBool,
}

impl CodexAppServerClient {
    pub fn spawn(
        codex_bin: &str,
        codex_home: Option<&str>,
        extra_args: &[String],
        env_extra: &HashMap<String, String>,
    ) -> Result<Self, String> {
        let mut cmd = Command::new(codex_bin);
        cmd.arg("app-server");
        cmd.args(extra_args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.env("RUST_LOG", "warn");
        if let Some(home) = codex_home.filter(|s| !s.is_empty()) {
            cmd.env("CODEX_HOME", home);
        }
        for (k, v) in env_extra {
            cmd.env(k, v);
        }
        if let Ok(kanban) = std::env::var("HERMES_KANBAN_TASK") {
            if !kanban.trim().is_empty() {
                let kanban_root = std::env::var("HERMES_KANBAN_DB")
                    .ok()
                    .map(|p| {
                        std::path::Path::new(&p)
                            .parent()
                            .map(|x| x.to_string_lossy().to_string())
                    })
                    .flatten()
                    .or_else(|| std::env::var("HERMES_KANBAN_ROOT").ok())
                    .unwrap_or_else(|| {
                        let base = std::env::var("HERMES_HOME")
                            .unwrap_or_else(|_| format!("{}/.hermes", dirs::home_dir().map(|p| p.display().to_string()).unwrap_or_else(|| ".".into())));
                        format!("{base}/kanban")
                    });
                cmd.arg("-c").arg(r#"sandbox_mode="workspace-write""#);
                cmd.arg("-c").arg(format!(
                    r#"sandbox_workspace_write.writable_roots=["{kanban_root}"]"#
                ));
                cmd.arg("-c")
                    .arg(r#"sandbox_workspace_write.network_access=false"#);
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("failed to spawn `{codex_bin} app-server`: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "codex app-server stdout unavailable".to_string())?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| "codex app-server stderr unavailable".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "codex app-server stdin unavailable".to_string())?;

        let client = Self {
            stdin: Arc::new(Mutex::new(Some(stdin))),
            child: Arc::new(Mutex::new(Some(child))),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            notifications: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            server_requests: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            stderr_lines: Arc::new(Mutex::new(Vec::new())),
            closed: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
        };

        let pending = client.pending.clone();
        let notifications = client.notifications.clone();
        let server_requests = client.server_requests.clone();
        let stderr_lines = client.stderr_lines.clone();
        std::thread::Builder::new()
            .name("codex-app-server-stdout".into())
            .spawn(move || read_stdout_loop(stdout, pending, notifications, server_requests, stderr_lines))
            .map_err(|e| format!("codex stdout reader thread: {e}"))?;

        let stderr_lines = client.stderr_lines.clone();
        std::thread::Builder::new()
            .name("codex-app-server-stderr".into())
            .spawn(move || read_stderr_loop(stderr, stderr_lines))
            .map_err(|e| format!("codex stderr reader thread: {e}"))?;

        Ok(client)
    }

    pub fn initialize(
        &self,
        client_name: &str,
        client_title: &str,
        client_version: &str,
        timeout: Duration,
    ) -> Result<Value, CodexAppServerError> {
        if self.initialized.load(Ordering::Acquire) {
            return Err(CodexAppServerError {
                code: -1,
                message: "already initialized".into(),
                data: None,
            });
        }
        let params = serde_json::json!({
            "clientInfo": {
                "name": client_name,
                "title": client_title,
                "version": client_version,
            },
            "capabilities": {},
        });
        let result = self.request("initialize", Some(params), timeout)?;
        self.notify("initialized", None).map_err(|e| CodexAppServerError {
            code: -1,
            message: e,
            data: None,
        })?;
        self.initialized.store(true, Ordering::Release);
        Ok(result)
    }

    pub fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value, CodexAppServerError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| codex_lock_err("pending"))?;
            pending.insert(
                id,
                Pending {
                    tx,
                    method: method.to_string(),
                },
            );
        }
        self.send(&serde_json::json!({
            "id": id,
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        }))
        .map_err(|e| CodexAppServerError {
            code: -1,
            message: e,
            data: None,
        })?;
        match rx.recv_timeout(timeout) {
            Ok(msg) => {
                if let Some(err) = msg.get("error") {
                    return Err(CodexAppServerError {
                        code: err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1),
                        message: err
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        data: err.get("data").cloned(),
                    });
                }
                Ok(msg.get("result").cloned().unwrap_or(Value::Null))
            }
            Err(_) => {
                let _ = self.pending.lock().map(|mut p| p.remove(&id));
                Err(CodexAppServerError {
                    code: -1,
                    message: format!(
                        "codex app-server method {method:?} timed out after {:?}",
                        timeout
                    ),
                    data: None,
                })
            }
        }
    }

    pub fn notify(&self, method: &str, params: Option<Value>) -> Result<(), String> {
        self.send(&serde_json::json!({
            "method": method,
            "params": params.unwrap_or(Value::Object(Default::default())),
        }))
    }

    pub fn respond(&self, request_id: &Value, result: Value) -> Result<(), String> {
        self.send(&serde_json::json!({
            "id": request_id,
            "result": result,
        }))
    }

    pub fn respond_error(
        &self,
        request_id: &Value,
        code: i64,
        message: &str,
        data: Option<Value>,
    ) -> Result<(), String> {
        let mut err = serde_json::json!({ "code": code, "message": message });
        if let Some(d) = data {
            err["data"] = d;
        }
        self.send(&serde_json::json!({ "id": request_id, "error": err }))
    }

    pub fn take_notification(&self, timeout: Duration) -> Option<Value> {
        if timeout.is_zero() {
            return self.notifications.lock().ok()?.pop_front();
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(n) = self.notifications.lock().ok()?.pop_front() {
                return Some(n);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn take_server_request(&self, timeout: Duration) -> Option<Value> {
        if timeout.is_zero() {
            return self.server_requests.lock().ok()?.pop_front();
        }
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(r) = self.server_requests.lock().ok()?.pop_front() {
                return Some(r);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn stderr_tail(&self, n: usize) -> Vec<String> {
        self.stderr_lines
            .lock()
            .map(|lines| lines.iter().rev().take(n).cloned().collect::<Vec<_>>().into_iter().rev().collect())
            .unwrap_or_default()
    }

    pub fn is_alive(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut g| g.as_mut().map(|c| c.try_wait().ok().flatten().is_none()))
            .unwrap_or(false)
    }

    pub fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut stdin) = self.stdin.lock() {
            *stdin = None;
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(mut c) = child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
    }

    fn send(&self, obj: &Value) -> Result<(), String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("codex app-server client is closed".into());
        }
        let line = serde_json::to_string(obj).map_err(|e| e.to_string())? + "\n";
        let mut guard = self.stdin.lock().map_err(|e| e.to_string())?;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| "codex app-server stdin not available".to_string())?;
        stdin
            .write_all(line.as_bytes())
            .map_err(|e| format!("codex stdin write failed: {e}"))?;
        stdin
            .flush()
            .map_err(|e| format!("codex stdin flush failed: {e}"))
    }
}

fn codex_lock_err(what: &str) -> CodexAppServerError {
    CodexAppServerError {
        code: -1,
        message: format!("codex client lock poisoned ({what})"),
        data: None,
    }
}

fn read_stdout_loop(
    stdout: std::process::ChildStdout,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    notifications: Arc<Mutex<std::collections::VecDeque<Value>>>,
    server_requests: Arc<Mutex<std::collections::VecDeque<Value>>>,
    stderr_lines: Arc<Mutex<Vec<String>>>,
) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                if let Ok(mut tail) = stderr_lines.lock() {
                    tail.push(format!("<non-json on stdout> {line:.200}"));
                }
                continue;
            }
        };
        dispatch_message(msg, &pending, &notifications, &server_requests);
    }
}

fn dispatch_message(
    msg: Value,
    pending: &Mutex<HashMap<u64, Pending>>,
    notifications: &Mutex<std::collections::VecDeque<Value>>,
    server_requests: &Mutex<std::collections::VecDeque<Value>>,
) {
    let has_id = msg.get("id").is_some();
    let has_result = msg.get("result").is_some() || msg.get("error").is_some();
    let has_method = msg.get("method").is_some();

    if has_id && has_result {
        if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
            if let Ok(mut map) = pending.lock() {
                if let Some(p) = map.remove(&id) {
                    let _ = p.tx.send(msg);
                }
            }
        }
        return;
    }
    if has_id && has_method {
        if let Ok(mut q) = server_requests.lock() {
            q.push_back(msg);
        }
        return;
    }
    if has_method {
        if let Ok(mut q) = notifications.lock() {
            q.push_back(msg);
        }
    }
}

fn read_stderr_loop(stderr: std::process::ChildStderr, stderr_lines: Arc<Mutex<Vec<String>>>) {
    let reader = BufReader::new(stderr);
    for line in reader.lines().flatten() {
        if let Ok(mut tail) = stderr_lines.lock() {
            tail.push(line);
            if tail.len() > 500 {
                let drain = tail.len() - 500;
                tail.drain(0..drain);
            }
        }
    }
}

pub fn parse_codex_version(output: &str) -> Option<(u32, u32, u32)> {
    let re = regex::Regex::new(r"(\d+)\.(\d+)\.(\d+)").ok()?;
    let caps = re.captures(output)?;
    Some((
        caps.get(1)?.as_str().parse().ok()?,
        caps.get(2)?.as_str().parse().ok()?,
        caps.get(3)?.as_str().parse().ok()?,
    ))
}

pub fn check_codex_binary(codex_bin: &str) -> (bool, String) {
    let output = std::process::Command::new(codex_bin)
        .arg("--version")
        .output();
    match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
            false,
            format!(
                "codex CLI not found at {codex_bin:?}. Install with: npm i -g @openai/codex"
            ),
        ),
        Err(e) => (false, format!("codex --version failed: {e}")),
        Ok(out) if !out.status.success() => (
            false,
            format!(
                "codex --version exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        ),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            match parse_codex_version(&text) {
                Some(v) if v < MIN_CODEX_VERSION => (
                    false,
                    format!(
                        "codex {}.{}.{} is older than required {}.{}.{}",
                        v.0,
                        v.1,
                        v.2,
                        MIN_CODEX_VERSION.0,
                        MIN_CODEX_VERSION.1,
                        MIN_CODEX_VERSION.2
                    ),
                ),
                Some(v) => (true, format!("{}.{}.{}", v.0, v.1, v.2)),
                None => (false, format!("could not parse codex version from: {text:?}")),
            }
        }
    }
}
