use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use regex::Regex;
use serde_json::{Value, json};
use tokio::fs;
use tokio::process::Command;
use uuid::Uuid;

use hermes_core::{ToolError, ToolHandler, ToolSchema};

use super::backend::{ActionResult, CaptureResult, ComputerUseBackend, unsupported_action};
use super::cua_backend::{CuaDriverBackend, cua_driver_binary_available};
use super::schema::computer_use_schema;

const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";

#[derive(Clone, Default)]
struct FallbackCaptureBackend;

#[async_trait]
impl ComputerUseBackend for FallbackCaptureBackend {
    async fn capture(&self, mode: &str, _app: Option<&str>) -> Result<CaptureResult, ToolError> {
        if mode == "ax" {
            return Ok(CaptureResult {
                mode: mode.to_string(),
                image_b64: None,
                image_mime: None,
                app: String::new(),
                window_title: String::new(),
                elements: Vec::new(),
            });
        }
        let path = std::env::temp_dir().join(format!("hermes-computer-use-{}.png", Uuid::new_v4()));
        capture_desktop_to_path(&path).await?;
        let bytes = fs::read(&path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("read screenshot file: {e}")))?;
        let _ = fs::remove_file(&path).await;
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        Ok(CaptureResult {
            mode: mode.to_string(),
            image_b64: Some(b64),
            image_mime: Some("image/png".to_string()),
            app: String::new(),
            window_title: String::new(),
            elements: Vec::new(),
        })
    }

    async fn click(
        &self,
        _element: Option<i64>,
        _coordinate: Option<(i64, i64)>,
        _button: &str,
        _click_count: i64,
        _modifiers: &[String],
    ) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("click"))
    }

    async fn scroll(
        &self,
        _direction: &str,
        _amount: i64,
        _element: Option<i64>,
        _coordinate: Option<(i64, i64)>,
        _modifiers: &[String],
    ) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("scroll"))
    }

    async fn type_text(&self, _text: &str) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("type"))
    }

    async fn key(&self, _keys: &str) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("key"))
    }

    async fn set_value(
        &self,
        _value: &str,
        _element: Option<i64>,
    ) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("set_value"))
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
        Ok(json!({"apps": [], "count": 0}))
    }

    async fn focus_app(&self, _app: &str, _raise_window: bool) -> Result<ActionResult, ToolError> {
        Ok(unsupported_action("focus_app"))
    }
}

pub struct ComputerUseHandler {
    backend: Arc<dyn ComputerUseBackend>,
}

impl ComputerUseHandler {
    pub fn with_default_backend() -> Self {
        let backend: Arc<dyn ComputerUseBackend> = if cua_driver_binary_available() {
            Arc::new(CuaDriverBackend::new())
        } else {
            Arc::new(FallbackCaptureBackend)
        };
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ComputerUseHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("missing `action`".into()))?;

        if action == "type" {
            let text = params
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if let Some(pat) = blocked_type_pattern(text) {
                return Ok(
                    json!({"error": format!("blocked pattern in type text: {pat}")}).to_string(),
                );
            }
        }
        if action == "key" {
            let keys = params
                .get("keys")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if let Some(combo) = blocked_key_combo(keys) {
                return Ok(json!({"error": format!("blocked key combo: {:?}", combo)}).to_string());
            }
        }

        let capture_after = params
            .get("capture_after")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let out = match action.as_str() {
            "capture" => {
                let mode = params
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("som")
                    .to_ascii_lowercase();
                let capture = self
                    .backend
                    .capture(&mode, params.get("app").and_then(|v| v.as_str()))
                    .await?;
                capture_to_output(&capture)?
            }
            "capture_to_file" => {
                let mode = params
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("som")
                    .to_ascii_lowercase();
                let capture = self
                    .backend
                    .capture(&mode, params.get("app").and_then(|v| v.as_str()))
                    .await?;
                capture_to_file_output(&capture).await?
            }
            "wait" => action_to_json(
                &self
                    .backend
                    .wait(params.get("seconds").and_then(|v| v.as_f64()).unwrap_or(1.0))
                    .await?,
            ),
            "list_apps" => self.backend.list_apps().await?.to_string(),
            "focus_app" => {
                let app = params
                    .get("app")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("focus_app requires `app`".into()))?;
                let res = self
                    .backend
                    .focus_app(
                        app,
                        params
                            .get("raise_window")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    )
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            "click" | "double_click" | "right_click" | "middle_click" => {
                let element = params.get("element").and_then(|v| v.as_i64());
                let coordinate = parse_xy(params.get("coordinate"))?;
                let button = match action.as_str() {
                    "right_click" => "right",
                    "middle_click" => "middle",
                    _ => params.get("button").and_then(|v| v.as_str()).unwrap_or("left"),
                };
                let click_count = if action == "double_click" { 2 } else { 1 };
                let res = self
                    .backend
                    .click(
                        element,
                        coordinate,
                        button,
                        click_count,
                        &parse_modifiers(&params),
                    )
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            "drag" => {
                json!({"ok": false, "action":"drag", "message":"drag is not supported by current backend"})
                    .to_string()
            }
            "scroll" => {
                let res = self
                    .backend
                    .scroll(
                        params
                            .get("direction")
                            .and_then(|v| v.as_str())
                            .unwrap_or("down"),
                        params.get("amount").and_then(|v| v.as_i64()).unwrap_or(3),
                        params.get("element").and_then(|v| v.as_i64()),
                        parse_xy(params.get("coordinate"))?,
                        &parse_modifiers(&params),
                    )
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            "type" => {
                let res = self
                    .backend
                    .type_text(
                        params
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default(),
                    )
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            "key" => {
                let res = self
                    .backend
                    .key(
                        params
                            .get("keys")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default(),
                    )
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            "set_value" => {
                let value = params
                    .get("value")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("set_value requires `value`".into()))?;
                let res = self
                    .backend
                    .set_value(value, params.get("element").and_then(|v| v.as_i64()))
                    .await?;
                maybe_capture_after(self.backend.as_ref(), &res, capture_after).await?
            }
            _ => json!({"error": format!("unknown action {action}")}).to_string(),
        };
        Ok(out)
    }

    fn schema(&self) -> ToolSchema {
        computer_use_schema()
    }
}

pub fn check_computer_use_requirements() -> bool {
    if cua_driver_binary_available() {
        return true;
    }
    if cfg!(target_os = "macos") {
        return command_exists("screencapture");
    }
    if cfg!(target_os = "windows") {
        return command_exists("powershell") || command_exists("pwsh");
    }
    if cfg!(target_os = "linux") {
        return command_exists("grim")
            || command_exists("gnome-screenshot")
            || command_exists("scrot");
    }
    false
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

fn blocked_type_pattern(text: &str) -> Option<&'static str> {
    let patterns = [
        r"curl\s+[^|]*\|\s*bash",
        r"curl\s+[^|]*\|\s*sh",
        r"wget\s+[^|]*\|\s*bash",
        r"\bsudo\s+rm\s+-[rf]",
        r"\brm\s+-rf\s+/\s*$",
        r":\s*\(\)\s*\{\s*:\|:\s*&\s*\}",
    ];
    for pattern in patterns {
        if let Ok(re) = Regex::new(pattern) {
            if re.is_match(text) {
                return Some(pattern);
            }
        }
    }
    None
}

fn blocked_key_combo(keys: &str) -> Option<Vec<&'static str>> {
    let current = canonical_combo(keys);
    let blocked: &[&[&str]] = &[
        &["cmd", "shift", "backspace"],
        &["cmd", "option", "backspace"],
        &["cmd", "ctrl", "q"],
        &["cmd", "shift", "q"],
        &["cmd", "option", "shift", "q"],
    ];
    for combo in blocked {
        let target: BTreeSet<String> = combo.iter().map(|s| (*s).to_string()).collect();
        if target.is_subset(&current) {
            return Some(combo.to_vec());
        }
    }
    None
}

fn canonical_combo(keys: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in keys.split('+').map(|s| s.trim().to_ascii_lowercase()) {
        if raw.is_empty() {
            continue;
        }
        let normalized = match raw.as_str() {
            "command" => "cmd",
            "control" => "ctrl",
            "alt" => "option",
            x => x,
        };
        out.insert(normalized.to_string());
    }
    out
}

fn parse_xy(v: Option<&Value>) -> Result<Option<(i64, i64)>, ToolError> {
    let Some(v) = v else { return Ok(None) };
    let arr = v
        .as_array()
        .ok_or_else(|| ToolError::InvalidParams("coordinate must be [x,y]".into()))?;
    if arr.len() != 2 {
        return Err(ToolError::InvalidParams(
            "coordinate must have two items".into(),
        ));
    }
    let x = arr[0]
        .as_i64()
        .ok_or_else(|| ToolError::InvalidParams("coordinate[0] must be int".into()))?;
    let y = arr[1]
        .as_i64()
        .ok_or_else(|| ToolError::InvalidParams("coordinate[1] must be int".into()))?;
    Ok(Some((x, y)))
}

fn parse_modifiers(params: &Value) -> Vec<String> {
    params
        .get("modifiers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_ascii_lowercase()))
                .collect()
        })
        .unwrap_or_default()
}

fn action_to_json(action: &ActionResult) -> String {
    json!({"ok": action.ok, "action": action.action, "message": action.message, "meta": action.meta})
        .to_string()
}

async fn maybe_capture_after(
    backend: &dyn ComputerUseBackend,
    action: &ActionResult,
    capture_after: bool,
) -> Result<String, ToolError> {
    if !capture_after {
        return Ok(action_to_json(action));
    }
    let capture = backend.capture("som", None).await?;
    if capture.image_b64.is_some() {
        let output = capture_to_output(&capture)?;
        if let Some(payload) = output.strip_prefix(ACP_MULTIMODAL_PREFIX) {
            let mut parts: Vec<Value> = serde_json::from_str(payload)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            if let Some(first) = parts.get_mut(0).and_then(|v| v.as_object_mut()) {
                let original = first
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                first.insert(
                    "text".to_string(),
                    Value::String(format!(
                        "[{}] ok={} — {}\n\n{}",
                        action.action, action.ok, action.message, original
                    )),
                );
            }
            let encoded = serde_json::to_string(&parts)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            return Ok(format!("{ACP_MULTIMODAL_PREFIX}{encoded}"));
        }
        return Ok(output);
    }
    let mut obj: Value = serde_json::from_str(&capture_to_output(&capture)?)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    if let Some(map) = obj.as_object_mut() {
        map.insert("action".into(), Value::String(action.action.clone()));
        map.insert("ok".into(), Value::Bool(action.ok));
        map.insert("message".into(), Value::String(action.message.clone()));
    }
    Ok(obj.to_string())
}

fn capture_to_output(capture: &CaptureResult) -> Result<String, ToolError> {
    let summary = format!(
        "capture mode={} app={} window={} elements={}",
        capture.mode,
        capture.app,
        capture.window_title,
        capture.elements.len()
    );
    if capture.mode != "ax" {
        if let Some(b64) = &capture.image_b64 {
            let mime = capture
                .image_mime
                .clone()
                .unwrap_or_else(|| "image/jpeg".to_string());
            let parts = json!([
                {"type":"text","text": summary},
                {"type":"image_url","image_url":{"url": format!("data:{mime};base64,{b64}")}}
            ]);
            let encoded = serde_json::to_string(&parts)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            return Ok(format!("{ACP_MULTIMODAL_PREFIX}{encoded}"));
        }
    }
    let elements: Vec<Value> = capture
        .elements
        .iter()
        .map(|e| json!({"index": e.index, "role": e.role, "label": e.label}))
        .collect();
    Ok(
        json!({"mode": capture.mode, "app": capture.app, "window_title": capture.window_title, "elements": elements, "summary": summary})
            .to_string(),
    )
}

async fn capture_to_file_output(capture: &CaptureResult) -> Result<String, ToolError> {
    let b64 = capture.image_b64.as_deref().ok_or_else(|| {
        ToolError::ExecutionFailed("capture_to_file requires image-capable capture mode".to_string())
    })?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| ToolError::ExecutionFailed(format!("decode capture image: {e}")))?;
    let (ext, mime) = match capture.image_mime.as_deref().unwrap_or("image/png") {
        "image/jpeg" => ("jpg", "image/jpeg"),
        "image/webp" => ("webp", "image/webp"),
        _ => ("png", "image/png"),
    };
    let path = std::env::temp_dir().join(format!("hermes-capture-{}.{}", Uuid::new_v4(), ext));
    fs::write(&path, bytes)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("write capture file: {e}")))?;
    Ok(
        json!({
            "ok": true,
            "action": "capture_to_file",
            "file_path": path.to_string_lossy().to_string(),
            "mime": mime,
            "mode": capture.mode,
            "summary": "capture saved to local file; use send_message(file=...) to deliver it"
        })
        .to_string(),
    )
}

async fn capture_desktop_to_path(path: &Path) -> Result<(), ToolError> {
    if cfg!(target_os = "windows") {
        let escaped = path.to_string_lossy().replace('\'', "''");
        let script = format!(
            "Add-Type -AssemblyName System.Drawing; \
             Add-Type -AssemblyName System.Windows.Forms; \
             $b=[System.Windows.Forms.SystemInformation]::VirtualScreen; \
             $bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height; \
             $g=[System.Drawing.Graphics]::FromImage($bmp); \
             $g.CopyFromScreen($b.Left,$b.Top,0,0,$bmp.Size); \
             $bmp.Save('{escaped}',[System.Drawing.Imaging.ImageFormat]::Png); \
             $g.Dispose(); $bmp.Dispose();"
        );
        run_capture_command("powershell", &["-NoProfile", "-Command", &script]).await?;
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        run_capture_command("screencapture", &["-x", &path.to_string_lossy()]).await?;
        return Ok(());
    }
    if cfg!(target_os = "linux") {
        let path_s = path.to_string_lossy().to_string();
        if command_exists("grim") && run_capture_command("grim", &[&path_s]).await.is_ok() {
            return Ok(());
        }
        if command_exists("gnome-screenshot")
            && run_capture_command("gnome-screenshot", &["-f", &path_s])
                .await
                .is_ok()
        {
            return Ok(());
        }
        if command_exists("scrot") && run_capture_command("scrot", &[&path_s]).await.is_ok() {
            return Ok(());
        }
    }
    Err(ToolError::ExecutionFailed(
        "No desktop capture command available".to_string(),
    ))
}

async fn run_capture_command(program: &str, args: &[&str]) -> Result<(), ToolError> {
    let status = Command::new(program)
        .args(args)
        .status()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn {program}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(ToolError::ExecutionFailed(format!(
            "{program} exited with status {status}"
        )))
    }
}
