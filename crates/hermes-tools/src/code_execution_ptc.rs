//! Local execute_code PTC: RPC server + child Python process (UDS on Unix, TCP on Windows).

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
#[cfg(unix)]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::process::Command as TokioCommand;
#[cfg(unix)]
use tracing::warn;

use crate::code_execution_env::SANDBOX_ALLOWED_TOOLS;
use crate::code_execution_stubs::{generate_hermes_tools_module, RpcTransport};
use crate::code_execution_env::scrub_child_env;
use crate::dispatch;
use crate::ToolRegistry;
use hermes_core::{FunctionCall, ToolCall, ToolError};

pub const DEFAULT_PTC_TIMEOUT_SECS: u64 = 300;
pub const DEFAULT_MAX_TOOL_CALLS: usize = 50;
pub const MAX_STDOUT_BYTES: usize = 50_000;
pub const MAX_STDERR_BYTES: usize = 10_000;

#[derive(Debug, Clone)]
pub struct PtcConfig {
    pub timeout_secs: u64,
    pub max_tool_calls: usize,
}

impl Default for PtcConfig {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_PTC_TIMEOUT_SECS,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
        }
    }
}

enum RpcServer {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener, PathBuf),
}

impl RpcServer {
    fn bind() -> Result<(Self, String), ToolError> {
        if cfg!(windows) {
            return Self::bind_tcp();
        }
        #[cfg(unix)]
        {
            match Self::bind_unix() {
                Ok(v) => return Ok(v),
                Err(e) => warn!("Unix RPC bind failed ({e}); using loopback TCP"),
            }
        }
        Self::bind_tcp()
    }

    fn bind_tcp() -> Result<(Self, String), ToolError> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| {
            ToolError::ExecutionFailed(format!("RPC tcp bind failed: {e}"))
        })?;
        let port = listener.local_addr().map_err(|e| {
            ToolError::ExecutionFailed(format!("RPC tcp local_addr: {e}"))
        })?;
        Ok((
            RpcServer::Tcp(listener),
            format!("tcp://127.0.0.1:{}", port.port()),
        ))
    }

    #[cfg(unix)]
    fn bind_unix() -> Result<(Self, String), ToolError> {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;
        let path = std::env::temp_dir().join(format!("hermes_rpc_{}.sock", uuid::Uuid::new_v4()));
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path).map_err(|e| {
            ToolError::ExecutionFailed(format!("RPC unix bind failed: {e}"))
        })?;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let env_value = path.display().to_string();
        Ok((RpcServer::Unix(listener, path), env_value))
    }

    fn run(
        self,
        registry: Arc<ToolRegistry>,
        allowed: BTreeSet<String>,
        max_calls: usize,
        stop: Arc<Mutex<bool>>,
        rt: tokio::runtime::Handle,
    ) -> Result<(), String> {
        match self {
            RpcServer::Tcp(listener) => accept_tcp_loop(listener, registry, allowed, max_calls, stop, rt),
            #[cfg(unix)]
            RpcServer::Unix(listener, path) => {
                let result = accept_unix_loop(listener, registry, allowed, max_calls, stop, rt);
                let _ = std::fs::remove_file(path);
                result
            }
        }
    }
}

fn sandbox_tool_names(registry: &ToolRegistry) -> Vec<String> {
    let registered: BTreeSet<String> = registry
        .list_tools()
        .into_iter()
        .map(|e| e.name)
        .collect();
    SANDBOX_ALLOWED_TOOLS
        .iter()
        .filter(|t| registered.iter().any(|r| r == *t))
        .map(|s| (*s).to_string())
        .collect()
}

fn strip_terminal_blocked_params(args: &mut Value) {
    const BLOCKED: &[&str] = &["background", "session_id", "detach"];
    let Some(obj) = args.as_object_mut() else {
        return;
    };
    for key in BLOCKED {
        obj.remove(*key);
    }
}

fn handle_rpc_line(
    line: &str,
    allowed: &BTreeSet<String>,
    counter: &mut usize,
    max_calls: usize,
    registry: &ToolRegistry,
    rt: &tokio::runtime::Handle,
) -> String {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("Invalid RPC request: {e}")}).to_string(),
    };
    let tool_name = request.get("tool").and_then(|v| v.as_str()).unwrap_or("");
    if !allowed.contains(tool_name) {
        let available = allowed.iter().cloned().collect::<Vec<_>>().join(", ");
        return json!({
            "error": format!(
                "Tool '{tool_name}' is not available in execute_code. Available: {available}"
            )
        })
        .to_string();
    }
    if *counter >= max_calls {
        return json!({
            "error": format!(
                "Tool call limit reached ({max_calls}). No more tool calls allowed in this execution."
            )
        })
        .to_string();
    }
    *counter += 1;
    let mut args = request.get("args").cloned().unwrap_or(json!({}));
    if tool_name == "terminal" {
        strip_terminal_blocked_params(&mut args);
    }
    let call = ToolCall {
        id: format!("ptc_{}", *counter),
        function: FunctionCall {
            name: tool_name.to_string(),
            arguments: args.to_string(),
        },
        extra_content: None,
    };
    let result = rt.block_on(async {
        dispatch::dispatch_single(call, Arc::new(registry.clone()), 50_000).await
    });
    if result.is_error {
        json!({"error": result.content}).to_string()
    } else {
        result.content
    }
}

trait RpcConn: Read + Write + Send {
    fn set_timeouts(&self) -> std::io::Result<()>;
}

impl RpcConn for TcpStream {
    fn set_timeouts(&self) -> std::io::Result<()> {
        self.set_read_timeout(Some(Duration::from_secs(300)))?;
        self.set_write_timeout(Some(Duration::from_secs(300)))?;
        Ok(())
    }
}

#[cfg(unix)]
impl RpcConn for std::os::unix::net::UnixStream {
    fn set_timeouts(&self) -> std::io::Result<()> {
        self.set_read_timeout(Some(Duration::from_secs(300)))?;
        self.set_write_timeout(Some(Duration::from_secs(300)))?;
        Ok(())
    }
}

fn rpc_read_loop<C: RpcConn>(
    mut stream: C,
    registry: Arc<ToolRegistry>,
    allowed: BTreeSet<String>,
    max_calls: usize,
    stop: Arc<Mutex<bool>>,
    rt: tokio::runtime::Handle,
) -> Result<(), String> {
    stream.set_timeouts().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    let mut read_buf = [0u8; 65536];
    let mut call_count = 0usize;
    loop {
        if *stop.lock().unwrap() {
            break;
        }
        match stream.read(&mut read_buf) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&read_buf[..n]);
                while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                    let line_bytes: Vec<u8> = buf.drain(..=pos).collect();
                    let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let resp =
                        handle_rpc_line(&line, &allowed, &mut call_count, max_calls, &registry, &rt);
                    stream
                        .write_all(format!("{resp}\n").as_bytes())
                        .map_err(|e| e.to_string())?;
                    stream.flush().map_err(|e| e.to_string())?;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn accept_tcp_loop(
    listener: TcpListener,
    registry: Arc<ToolRegistry>,
    allowed: BTreeSet<String>,
    max_calls: usize,
    stop: Arc<Mutex<bool>>,
    rt: tokio::runtime::Handle,
) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let (stream, _) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= Duration::from_secs(10) {
                    return Err("RPC accept timeout".into());
                }
                if *stop.lock().unwrap() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    rpc_read_loop(stream, registry, allowed, max_calls, stop, rt)
}

#[cfg(unix)]
fn accept_unix_loop(
    listener: std::os::unix::net::UnixListener,
    registry: Arc<ToolRegistry>,
    allowed: BTreeSet<String>,
    max_calls: usize,
    stop: Arc<Mutex<bool>>,
    rt: tokio::runtime::Handle,
) -> Result<(), String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let (stream, _) = loop {
        match listener.accept() {
            Ok(pair) => break pair,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if start.elapsed() >= Duration::from_secs(10) {
                    return Err("RPC accept timeout".into());
                }
                if *stop.lock().unwrap() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
    rpc_read_loop(stream, registry, allowed, max_calls, stop, rt)
}

fn build_child_env(endpoint: &str, sandbox_dir: &std::path::Path) -> BTreeMap<String, String> {
    let source: BTreeMap<String, String> = std::env::vars().collect();
    let mut child = scrub_child_env(&source, |_| false, cfg!(windows));
    child.insert("HERMES_RPC_SOCKET".into(), endpoint.to_string());
    child.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    child.insert("PYTHONIOENCODING".into(), "utf-8".into());
    child.insert("PYTHONUTF8".into(), "1".into());
    let mut pp = vec![sandbox_dir.display().to_string()];
    if let Ok(existing) = std::env::var("PYTHONPATH") {
        if !existing.is_empty() {
            pp.push(existing);
        }
    }
    child.insert("PYTHONPATH".into(), pp.join(std::path::MAIN_SEPARATOR_STR));
    child
}

fn python_candidates() -> Vec<Vec<String>> {
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

async fn spawn_child(
    script: &std::path::Path,
    sandbox_dir: &std::path::Path,
    child_env: &BTreeMap<String, String>,
) -> Result<(tokio::process::Child, String), ToolError> {
    for argv0 in python_candidates() {
        let program = &argv0[0];
        let mut cmd = TokioCommand::new(program);
        for arg in &argv0[1..] {
            cmd.arg(arg);
        }
        cmd.arg(script)
            .current_dir(sandbox_dir)
            .env_clear()
            .envs(child_env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        match cmd.spawn() {
            Ok(child) => return Ok((child, program.clone())),
            Err(_) => continue,
        }
    }
    Err(ToolError::ExecutionFailed(
        "Failed to spawn Python for PTC sandbox".into(),
    ))
}

fn cap_output(bytes: Vec<u8>, max: usize) -> String {
    if bytes.len() <= max {
        return String::from_utf8_lossy(&bytes).to_string();
    }
    let truncated = &bytes[..max];
    format!(
        "{}\n\n[... truncated {} bytes ...]",
        String::from_utf8_lossy(truncated),
        bytes.len() - max
    )
}

/// Run user Python `code` in a local PTC sandbox with tool RPC to `registry`.
pub async fn execute_python_ptc(
    code: &str,
    registry: Arc<ToolRegistry>,
    config: PtcConfig,
) -> Result<String, ToolError> {
    let sandbox_tools = sandbox_tool_names(&registry);
    if sandbox_tools.is_empty() {
        return Err(ToolError::ExecutionFailed(
            "No sandbox tools available in registry for execute_code PTC".into(),
        ));
    }

    let tmp = TempDir::new().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    let sandbox_dir = tmp.path();
    let transport = if cfg!(windows) {
        RpcTransport::Tcp
    } else {
        RpcTransport::Uds
    };
    let tools_src = generate_hermes_tools_module(&sandbox_tools, transport);
    std::fs::write(sandbox_dir.join("hermes_tools.py"), tools_src)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    std::fs::write(sandbox_dir.join("script.py"), code)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

    let (server, rpc_env) = RpcServer::bind()?;
    let allowed: BTreeSet<String> = sandbox_tools.iter().cloned().collect();
    let stop = Arc::new(Mutex::new(false));
    let stop_rpc = stop.clone();
    let registry_rpc = registry.clone();
    let rt = tokio::runtime::Handle::current();
    let max_calls = config.max_tool_calls;
    let rpc_thread = std::thread::Builder::new()
        .name("hermes-ptc-rpc".into())
        .spawn(move || server.run(registry_rpc, allowed, max_calls, stop_rpc, rt))
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn RPC thread: {e}")))?;

    let child_env = build_child_env(&rpc_env, sandbox_dir);
    let script_path = sandbox_dir.join("script.py");
    let (child, interpreter) = spawn_child(&script_path, sandbox_dir, &child_env).await?;
    let child_pid = child.id();

    let timeout = Duration::from_secs(config.timeout_secs);
    let result = tokio::time::timeout(timeout, child.wait_with_output()).await;
    *stop.lock().unwrap() = true;
    let _ = rpc_thread.join();

    match result {
        Ok(Ok(output)) => {
            let exit_code = output.status.code().unwrap_or(-1);
            Ok(json!({
                "exit_code": exit_code,
                "stdout": cap_output(output.stdout, MAX_STDOUT_BYTES),
                "stderr": cap_output(output.stderr, MAX_STDERR_BYTES),
                "language": "python",
                "interpreter": interpreter,
                "ptc": true,
                "rpc_endpoint": rpc_env,
            })
            .to_string())
        }
        Ok(Err(e)) => Err(ToolError::ExecutionFailed(format!("PTC child wait: {e}"))),
        Err(_) => {
            kill_child_process(child_pid);
            Err(ToolError::Timeout(format!(
                "PTC execution timed out after {}s",
                config.timeout_secs
            )))
        }
    }
}

fn kill_child_process(pid: Option<u32>) {
    let Some(pid) = pid else {
        return;
    };
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F", "/T"])
            .status();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid.to_string()])
            .status();
    }
}

pub fn ptc_enabled() -> bool {
    match std::env::var("HERMES_EXECUTE_CODE_PTC") {
        Ok(v) => {
            let v = v.trim().to_ascii_lowercase();
            !matches!(v.as_str(), "0" | "false" | "no" | "off")
        }
        Err(_) => true,
    }
}
