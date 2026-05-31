use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

use hermes_core::ToolError;

use super::backend::{ActionResult, CaptureResult, ComputerUseBackend, UiElement};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MCP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct ActiveWindow {
    pid: Option<i64>,
    window_id: Option<i64>,
}

#[derive(Clone)]
pub struct CuaDriverBackend {
    active: Arc<Mutex<ActiveWindow>>,
}

#[derive(Debug, Clone)]
struct WindowInfo {
    app_name: String,
    pid: i64,
    window_id: i64,
}

#[derive(Debug)]
struct McpResponse {
    text: String,
    images: Vec<McpImage>,
    is_error: bool,
}

#[derive(Debug)]
struct McpImage {
    mime_type: Option<String>,
    data: String,
}

struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

impl CuaDriverBackend {
    pub fn new() -> Self {
        Self {
            active: Arc::new(Mutex::new(ActiveWindow::default())),
        }
    }

    fn set_active(&self, pid: i64, window_id: i64) -> Result<(), ToolError> {
        let mut guard = self
            .active
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("computer_use state lock poisoned".into()))?;
        guard.pid = Some(pid);
        guard.window_id = Some(window_id);
        Ok(())
    }

    fn active(&self) -> Result<(i64, i64), ToolError> {
        let guard = self
            .active
            .lock()
            .map_err(|_| ToolError::ExecutionFailed("computer_use state lock poisoned".into()))?;
        let pid = guard.pid.ok_or_else(|| {
            ToolError::ExecutionFailed("No active window. Call capture first.".into())
        })?;
        let window_id = guard.window_id.ok_or_else(|| {
            ToolError::ExecutionFailed("No active window id. Call capture first.".into())
        })?;
        Ok((pid, window_id))
    }

    async fn list_windows(&self) -> Result<Vec<WindowInfo>, ToolError> {
        let response =
            call_cua_driver_tool("list_windows", json!({"on_screen_only": true})).await?;
        parse_windows_from_text(&response.text)
    }
}

#[async_trait]
impl ComputerUseBackend for CuaDriverBackend {
    async fn capture(&self, mode: &str, app: Option<&str>) -> Result<CaptureResult, ToolError> {
        let mut windows = self.list_windows().await?;
        if windows.is_empty() {
            return Ok(CaptureResult {
                mode: mode.to_string(),
                image_b64: None,
                image_mime: None,
                app: String::new(),
                window_title: String::new(),
                elements: Vec::new(),
            });
        }
        if let Some(app_filter) = app {
            let needle = app_filter.to_ascii_lowercase();
            let filtered: Vec<_> = windows
                .iter()
                .filter(|w| w.app_name.to_ascii_lowercase().contains(&needle))
                .cloned()
                .collect();
            if !filtered.is_empty() {
                windows = filtered;
            }
        }
        let target = windows[0].clone();
        self.set_active(target.pid, target.window_id)?;

        if mode == "vision" {
            let response = call_cua_driver_tool(
                "screenshot",
                json!({"window_id": target.window_id, "format": "jpeg", "quality": 85}),
            )
            .await?;
            let image = response.images.first().map(|img| img.data.clone());
            let mime = response
                .images
                .first()
                .and_then(|img| img.mime_type.clone())
                .or_else(|| Some("image/jpeg".to_string()));
            return Ok(CaptureResult {
                mode: mode.to_string(),
                image_b64: image,
                image_mime: mime,
                app: target.app_name,
                window_title: String::new(),
                elements: Vec::new(),
            });
        }

        let response = call_cua_driver_tool(
            "get_window_state",
            json!({"pid": target.pid, "window_id": target.window_id}),
        )
        .await?;
        let elements = parse_elements_from_tree(&response.text);
        let window_title = extract_window_title(&response.text).unwrap_or_default();
        let image = response.images.first().map(|img| img.data.clone());
        let mime = response
            .images
            .first()
            .and_then(|img| img.mime_type.clone())
            .or_else(|| image.as_ref().map(|_| "image/jpeg".to_string()));

        Ok(CaptureResult {
            mode: mode.to_string(),
            image_b64: if mode == "ax" { None } else { image },
            image_mime: if mode == "ax" { None } else { mime },
            app: target.app_name,
            window_title,
            elements,
        })
    }

    async fn click(
        &self,
        element: Option<i64>,
        coordinate: Option<(i64, i64)>,
        button: &str,
        click_count: i64,
        modifiers: &[String],
    ) -> Result<ActionResult, ToolError> {
        let (pid, window_id) = self.active()?;
        let tool = if button == "right" {
            "right_click"
        } else if click_count >= 2 {
            "double_click"
        } else {
            "click"
        };
        let mut args = serde_json::Map::new();
        args.insert("pid".into(), json!(pid));
        if let Some(idx) = element {
            args.insert("element_index".into(), json!(idx));
            args.insert("window_id".into(), json!(window_id));
        } else if let Some((x, y)) = coordinate {
            args.insert("x".into(), json!(x));
            args.insert("y".into(), json!(y));
        } else {
            return Err(ToolError::InvalidParams(
                "click requires element or coordinate".into(),
            ));
        }
        if !modifiers.is_empty() {
            args.insert("modifier".into(), json!(modifiers));
        }
        map_action(tool, call_cua_driver_tool(tool, Value::Object(args)).await)
    }

    async fn scroll(
        &self,
        direction: &str,
        amount: i64,
        element: Option<i64>,
        coordinate: Option<(i64, i64)>,
        modifiers: &[String],
    ) -> Result<ActionResult, ToolError> {
        let (pid, window_id) = self.active()?;
        let mut args = serde_json::Map::new();
        args.insert("pid".into(), json!(pid));
        args.insert("direction".into(), json!(direction));
        args.insert("amount".into(), json!(amount.clamp(1, 50)));
        if let Some(idx) = element {
            args.insert("element_index".into(), json!(idx));
            args.insert("window_id".into(), json!(window_id));
        } else if let Some((x, y)) = coordinate {
            args.insert("x".into(), json!(x));
            args.insert("y".into(), json!(y));
        }
        if !modifiers.is_empty() {
            args.insert("modifier".into(), json!(modifiers));
        }
        map_action(
            "scroll",
            call_cua_driver_tool("scroll", Value::Object(args)).await,
        )
    }

    async fn type_text(&self, text: &str) -> Result<ActionResult, ToolError> {
        let (pid, _) = self.active()?;
        map_action(
            "type",
            call_cua_driver_tool("type_text_chars", json!({"pid": pid, "text": text})).await,
        )
    }

    async fn key(&self, keys: &str) -> Result<ActionResult, ToolError> {
        let (pid, _) = self.active()?;
        let (key, modifiers) = parse_key_combo(keys);
        let key = key.ok_or_else(|| ToolError::InvalidParams("invalid key combo".into()))?;
        if modifiers.is_empty() {
            map_action(
                "key",
                call_cua_driver_tool("press_key", json!({"pid": pid, "key": key})).await,
            )
        } else {
            let mut combo = modifiers;
            combo.push(key);
            map_action(
                "key",
                call_cua_driver_tool("hotkey", json!({"pid": pid, "keys": combo})).await,
            )
        }
    }

    async fn set_value(
        &self,
        value: &str,
        element: Option<i64>,
    ) -> Result<ActionResult, ToolError> {
        let (pid, window_id) = self.active()?;
        let element =
            element.ok_or_else(|| ToolError::InvalidParams("set_value requires element".into()))?;
        map_action(
            "set_value",
            call_cua_driver_tool(
                "set_value",
                json!({"pid": pid, "window_id": window_id, "element_index": element, "value": value}),
            )
            .await,
        )
    }

    async fn wait(&self, seconds: f64) -> Result<ActionResult, ToolError> {
        let clamped = seconds.clamp(0.0, 30.0);
        tokio::time::sleep(Duration::from_secs_f64(clamped)).await;
        Ok(ActionResult {
            ok: true,
            action: "wait".to_string(),
            message: format!("waited {clamped:.2}s"),
            meta: json!({}),
        })
    }

    async fn list_apps(&self) -> Result<Value, ToolError> {
        let response = call_cua_driver_tool("list_apps", json!({})).await?;
        let mut apps = Vec::new();
        let re = Regex::new(r"(.+?)\s+\(pid\s+(\d+)\)")
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        for line in response.text.lines() {
            if let Some(caps) = re.captures(line) {
                let name = caps.get(1).map(|m| m.as_str().trim()).unwrap_or_default();
                let pid = caps
                    .get(2)
                    .and_then(|m| m.as_str().parse::<i64>().ok())
                    .unwrap_or(0);
                apps.push(json!({"name": name, "pid": pid}));
            }
        }
        Ok(json!({"apps": apps, "count": apps.len()}))
    }

    async fn focus_app(&self, app: &str, _raise_window: bool) -> Result<ActionResult, ToolError> {
        let windows = self.list_windows().await?;
        let needle = app.to_ascii_lowercase();
        let target = windows
            .iter()
            .find(|w| w.app_name.to_ascii_lowercase().contains(&needle))
            .cloned()
            .or_else(|| windows.first().cloned())
            .ok_or_else(|| ToolError::ExecutionFailed("No window available".into()))?;
        self.set_active(target.pid, target.window_id)?;
        Ok(ActionResult {
            ok: true,
            action: "focus_app".to_string(),
            message: format!(
                "Targeted {} (pid {}, window {}) without raising window",
                target.app_name, target.pid, target.window_id
            ),
            meta: json!({"app": target.app_name, "pid": target.pid, "window_id": target.window_id}),
        })
    }
}

pub fn cua_driver_binary_available() -> bool {
    command_exists("cua-driver")
}

fn command_exists(program: &str) -> bool {
    if Path::new(program).exists() {
        return true;
    }
    let checker = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };
    std::process::Command::new(checker)
        .arg(program)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn map_action(
    action: &str,
    resp: Result<McpResponse, ToolError>,
) -> Result<ActionResult, ToolError> {
    let resp = resp?;
    Ok(ActionResult {
        ok: !resp.is_error,
        action: action.to_string(),
        message: if resp.text.is_empty() {
            action.to_string()
        } else {
            resp.text
        },
        meta: json!({}),
    })
}

fn parse_key_combo(keys: &str) -> (Option<String>, Vec<String>) {
    let mut key = None;
    let mut modifiers = Vec::new();
    for token in keys.split(['+', '-']) {
        let raw = token.trim().to_ascii_lowercase();
        if raw.is_empty() {
            continue;
        }
        let normalized = match raw.as_str() {
            "command" => "cmd".to_string(),
            "control" => "ctrl".to_string(),
            "alt" => "option".to_string(),
            x => x.to_string(),
        };
        if matches!(
            normalized.as_str(),
            "cmd" | "ctrl" | "option" | "shift" | "fn"
        ) {
            modifiers.push(normalized);
        } else {
            key = Some(normalized);
        }
    }
    (key, modifiers)
}

fn parse_elements_from_tree(text: &str) -> Vec<UiElement> {
    let mut out = Vec::new();
    let Ok(re) = Regex::new(r#"^\s*-\s+\[(\d+)\]\s+(\w+)(?:\s+"([^"]*)")?"#) else {
        return out;
    };
    for cap in re.captures_iter(text) {
        out.push(UiElement {
            index: cap
                .get(1)
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(0),
            role: cap
                .get(2)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
            label: cap
                .get(3)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .to_string(),
        });
    }
    out
}

fn parse_windows_from_text(text: &str) -> Result<Vec<WindowInfo>, ToolError> {
    let re = Regex::new(r#"^-\s+(.+?)\s+\(pid\s+(\d+)\)\s+.*\[window_id:\s+(\d+)\]"#)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        out.push(WindowInfo {
            app_name: cap
                .get(1)
                .map(|m| m.as_str())
                .unwrap_or_default()
                .trim()
                .to_string(),
            pid: cap
                .get(2)
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(0),
            window_id: cap
                .get(3)
                .and_then(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(0),
        });
    }
    Ok(out)
}

fn extract_window_title(text: &str) -> Option<String> {
    let re = Regex::new(r#"AXWindow\s+"([^"]+)""#).ok()?;
    let cap = re.captures(text)?;
    Some(cap.get(1)?.as_str().to_string())
}

async fn call_cua_driver_tool(name: &str, arguments: Value) -> Result<McpResponse, ToolError> {
    if !cua_driver_binary_available() {
        return Err(ToolError::ExecutionFailed(
            "cua-driver not found. Install with `hermes computer-use install`.".to_string(),
        ));
    }
    let mut guard = mcp_session_mutex()
        .lock()
        .await;
    ensure_mcp_session(&mut guard).await?;
    let session = guard
        .as_mut()
        .ok_or_else(|| ToolError::ExecutionFailed("cua-driver mcp session unavailable".into()))?;

    match run_tool_call(session, name, arguments.clone()).await {
        Ok(resp) => Ok(resp),
        Err(first_err) => {
            tracing::warn!(
                error = %first_err,
                tool = %name,
                "cua-driver session error, restarting once"
            );
            *guard = None;
            ensure_mcp_session(&mut guard).await?;
            let session = guard.as_mut().ok_or_else(|| {
                ToolError::ExecutionFailed("cua-driver mcp session unavailable after restart".into())
            })?;
            run_tool_call(session, name, arguments).await
        }
    }
}

fn mcp_session_mutex() -> &'static AsyncMutex<Option<McpSession>> {
    static SESSION: OnceLock<AsyncMutex<Option<McpSession>>> = OnceLock::new();
    SESSION.get_or_init(|| AsyncMutex::new(None))
}

async fn ensure_mcp_session(slot: &mut Option<McpSession>) -> Result<(), ToolError> {
    let needs_new = match slot.as_mut() {
        Some(existing) => match existing.child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        },
        None => true,
    };
    if !needs_new {
        return Ok(());
    }
    *slot = Some(start_mcp_session().await?);
    Ok(())
}

async fn start_mcp_session() -> Result<McpSession, ToolError> {
    let mut child = Command::new("cua-driver")
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn cua-driver mcp: {e}")))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| ToolError::ExecutionFailed("failed to open cua-driver stdin".into()))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::ExecutionFailed("failed to open cua-driver stdout".into()))?;

    send_mcp_request(
        &mut stdin,
        json!({
            "jsonrpc":"2.0",
            "id":1,
            "method":"initialize",
            "params":{"protocolVersion": MCP_PROTOCOL_VERSION,"capabilities":{},"clientInfo":{"name":"hermes-tools","version":"0.1.0"}}
        }),
    )
    .await?;
    let _ = read_mcp_message_for_id(&mut stdout, 1).await?;

    send_mcp_request(
        &mut stdin,
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
    )
    .await?;
    Ok(McpSession {
        child,
        stdin,
        stdout,
    })
}

async fn run_tool_call(
    session: &mut McpSession,
    name: &str,
    arguments: Value,
) -> Result<McpResponse, ToolError> {
    send_mcp_request(
        &mut session.stdin,
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await?;
    let response = read_mcp_message_for_id(&mut session.stdout, 2).await?;
    parse_mcp_response(response)
}

fn parse_mcp_response(response: Value) -> Result<McpResponse, ToolError> {
    let result = response
        .get("result")
        .ok_or_else(|| ToolError::ExecutionFailed(format!("mcp tools/call missing result: {response}")))?;
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut text = String::new();
    let mut images = Vec::new();
    if let Some(parts) = result.get("content").and_then(|v| v.as_array()) {
        for part in parts {
            match part
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
            {
                "text" => {
                    if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(t);
                    }
                }
                "image" => {
                    if let Some(data) = part.get("data").and_then(|v| v.as_str()) {
                        images.push(McpImage {
                            mime_type: part
                                .get("mimeType")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            data: data.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(McpResponse {
        text,
        images,
        is_error,
    })
}

async fn send_mcp_request(
    stdin: &mut tokio::process::ChildStdin,
    payload: Value,
) -> Result<(), ToolError> {
    let body = serde_json::to_vec(&payload)
        .map_err(|e| ToolError::ExecutionFailed(format!("serialize mcp request: {e}")))?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    timeout(MCP_TIMEOUT, stdin.write_all(header.as_bytes()))
        .await
        .map_err(|_| ToolError::Timeout("write mcp header timeout".into()))?
        .map_err(|e| ToolError::ExecutionFailed(format!("write mcp header: {e}")))?;
    timeout(MCP_TIMEOUT, stdin.write_all(&body))
        .await
        .map_err(|_| ToolError::Timeout("write mcp body timeout".into()))?
        .map_err(|e| ToolError::ExecutionFailed(format!("write mcp body: {e}")))?;
    timeout(MCP_TIMEOUT, stdin.flush())
        .await
        .map_err(|_| ToolError::Timeout("flush mcp stdin timeout".into()))?
        .map_err(|e| ToolError::ExecutionFailed(format!("flush mcp stdin: {e}")))?;
    Ok(())
}

async fn read_mcp_message_for_id(
    stdout: &mut tokio::process::ChildStdout,
    id: i64,
) -> Result<Value, ToolError> {
    loop {
        let value = read_one_mcp_message(stdout).await?;
        if value.get("id").and_then(|v| v.as_i64()) == Some(id) {
            return Ok(value);
        }
    }
}

async fn read_one_mcp_message(stdout: &mut ChildStdout) -> Result<Value, ToolError> {
    let mut header = Vec::new();
    loop {
        let mut b = [0u8; 1];
        timeout(MCP_TIMEOUT, stdout.read_exact(&mut b))
            .await
            .map_err(|_| ToolError::Timeout("read mcp header timeout".into()))?
            .map_err(|e| ToolError::ExecutionFailed(format!("read mcp header: {e}")))?;
        header.push(b[0]);
        if header.ends_with(b"\r\n\r\n") {
            break;
        }
        if header.len() > 8192 {
            return Err(ToolError::ExecutionFailed("mcp header too large".into()));
        }
    }
    let header_text = String::from_utf8_lossy(&header);
    let mut content_length: Option<usize> = None;
    for line in header_text.lines() {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("content-length:") {
            content_length = line
                .split(':')
                .nth(1)
                .and_then(|v| v.trim().parse::<usize>().ok());
        }
    }
    let len = content_length.ok_or_else(|| {
        ToolError::ExecutionFailed("missing Content-Length in mcp response".into())
    })?;
    let mut body = vec![0u8; len];
    timeout(MCP_TIMEOUT, stdout.read_exact(&mut body))
        .await
        .map_err(|_| ToolError::Timeout("read mcp body timeout".into()))?
        .map_err(|e| ToolError::ExecutionFailed(format!("read mcp body: {e}")))?;
    serde_json::from_slice::<Value>(&body)
        .map_err(|e| ToolError::ExecutionFailed(format!("parse mcp json: {e}")))
}
