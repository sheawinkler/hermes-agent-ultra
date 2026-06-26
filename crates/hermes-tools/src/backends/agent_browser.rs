//! agent-browser CLI subprocess backend (Python `browser_tool._run_browser_command` parity).

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use super::browser_auth::BrowserAuthContext;
use super::browser_snapshot_util::process_snapshot_text;
use crate::tools::browser::BrowserBackend;
use async_trait::async_trait;
use hermes_core::ToolError;
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::Mutex;

const DEFAULT_TASK_ID: &str = "default";
const COMMAND_TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone)]
enum BrowserCommand {
    Direct(PathBuf),
    Npx,
}

/// Returns true when agent-browser CLI is discoverable on PATH or via npx.
pub fn is_available() -> bool {
    resolve_browser_command().is_some()
}

fn resolve_browser_command() -> Option<BrowserCommand> {
    if let Ok(explicit) = std::env::var("HERMES_AGENT_BROWSER_CMD") {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return Some(BrowserCommand::Direct(PathBuf::from(trimmed)));
        }
    }
    if which_agent_browser().is_some() {
        return which_agent_browser();
    }
    if which_npx().is_some() {
        return Some(BrowserCommand::Npx);
    }
    None
}

fn which_agent_browser() -> Option<BrowserCommand> {
    std::env::var_os("PATH").and_then(|paths| {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(if cfg!(windows) {
                "agent-browser.cmd"
            } else {
                "agent-browser"
            });
            if candidate.is_file() {
                return Some(BrowserCommand::Direct(candidate));
            }
            let plain = dir.join("agent-browser");
            if plain.is_file() {
                return Some(BrowserCommand::Direct(plain));
            }
        }
        None
    })
}

fn which_npx() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(if cfg!(windows) { "npx.cmd" } else { "npx" });
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    })
}

fn effective_task_id(task_id: Option<&str>) -> String {
    task_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_TASK_ID)
        .to_string()
}

fn normalize_ref(ref_id: &str) -> String {
    let trimmed = ref_id.trim();
    if trimmed.starts_with('@') {
        trimmed.to_string()
    } else {
        format!("@{trimmed}")
    }
}

pub struct AgentBrowserBackend {
    cmd: BrowserCommand,
    sessions: StdMutex<HashMap<String, String>>,
    /// Serialize subprocess calls — parallel `open` spawns multiple Chrome windows.
    command_lock: Mutex<()>,
}

impl AgentBrowserBackend {
    pub fn new() -> Result<Self, ToolError> {
        let cmd = resolve_browser_command().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "agent-browser CLI not found. Install with: npm install -g agent-browser \
                 && agent-browser install --with-deps"
                    .into(),
            )
        })?;
        Ok(Self {
            cmd,
            sessions: StdMutex::new(HashMap::new()),
            command_lock: Mutex::new(()),
        })
    }

    pub fn try_new() -> Option<Self> {
        Self::new().ok()
    }

    /// Drop cached browser session mapping for a turn task (Python turn-end cleanup).
    pub fn release_task_session(&self, task_id: &str) {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return;
        }
        if let Ok(mut guard) = self.sessions.lock() {
            if guard.remove(task_id).is_some() {
                tracing::debug!(task_id = %task_id, "released agent-browser session mapping");
            }
        }
    }

    fn session_name_for(&self, task_id: &str) -> String {
        let ctx = BrowserAuthContext::for_scope(task_id);
        let mut guard = self.sessions.lock().expect("browser sessions lock");
        guard
            .entry(task_id.to_string())
            .or_insert_with(|| ctx.session_name.clone())
            .clone()
    }

    fn auth_context_for(&self, task_id: &str) -> BrowserAuthContext {
        BrowserAuthContext::for_scope(task_id)
    }

    fn socket_dir(&self, session_name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("agent-browser-{session_name}"))
    }

    async fn run_command(
        &self,
        task_id: &str,
        command: &str,
        args: &[String],
        timeout_secs: u64,
    ) -> Result<Value, ToolError> {
        let _guard = self.command_lock.lock().await;
        let session_name = self.session_name_for(task_id);
        let auth = self.auth_context_for(task_id);
        let socket_dir = self.socket_dir(&session_name);
        std::fs::create_dir_all(&socket_dir).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to create browser socket dir: {e}"))
        })?;

        let mut cmd = match &self.cmd {
            BrowserCommand::Direct(path) => Command::new(path),
            BrowserCommand::Npx => {
                let npx = which_npx().unwrap_or_else(|| PathBuf::from("npx"));
                let mut c = Command::new(npx);
                c.arg("agent-browser");
                c
            }
        };

        cmd.arg("--session")
            .arg(&session_name)
            .arg("--json")
            .arg(command)
            .args(args)
            .env("AGENT_BROWSER_SOCKET_DIR", &socket_dir);
        auth.apply_to_command(&mut cmd);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        #[cfg(windows)]
        {
            cmd.as_std_mut().creation_flags(0x08000000);
        }

        let timeout = Duration::from_secs(timeout_secs.max(5));
        let child = cmd.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to spawn agent-browser: {e}"))
        })?;

        let output = tokio::time::timeout(timeout, child.wait_with_output())
            .await
            .map_err(|_| {
                ToolError::ExecutionFailed(format!(
                    "agent-browser '{command}' timed out after {timeout_secs}s"
                ))
            })?
            .map_err(|e| ToolError::ExecutionFailed(format!("agent-browser wait failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.trim().is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed(format!(
                "agent-browser '{command}' produced no output (status={:?}): {stderr}",
                output.status.code()
            )));
        }

        serde_json::from_str(stdout.trim()).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "agent-browser JSON parse error: {e}; stdout={}",
                stdout.chars().take(400).collect::<String>()
            ))
        })
    }

    fn command_timeout_secs() -> u64 {
        std::env::var("HERMES_BROWSER_COMMAND_TIMEOUT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(COMMAND_TIMEOUT_SECS)
    }

    /// Compact snapshot after navigate (Python `browser_navigate` auto-snapshot path).
    /// Uses `user_task = None` so oversized pages truncate only — no LLM summarization.
    async fn compact_snapshot_after_open(&self, task: &str) -> Option<(String, usize)> {
        let result = match self
            .run_command(
                task,
                "snapshot",
                &[("-c".to_string())],
                Self::command_timeout_secs(),
            )
            .await
        {
            Ok(value) => value,
            Err(err) => {
                tracing::debug!(
                    task_id = task,
                    error = %err,
                    "auto snapshot after navigate failed"
                );
                return None;
            }
        };

        if result.get("success").and_then(|v| v.as_bool()) == Some(false) {
            tracing::debug!(
                task_id = task,
                "auto snapshot after navigate returned success=false"
            );
            return None;
        }

        let (raw, count) = compact_snapshot_from_command_result(&result)?;
        let processed = if raw.is_empty() {
            raw
        } else {
            process_snapshot_text(&raw, None).await
        };
        Some((processed, count))
    }
}

/// Parse agent-browser `snapshot -c` JSON into (text, element_count).
fn compact_snapshot_from_command_result(result: &Value) -> Option<(String, usize)> {
    let data = result.get("data").unwrap_or(result);
    let snapshot_text = data
        .get("snapshot")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let element_count = data
        .get("refs")
        .and_then(|r| r.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    Some((snapshot_text, element_count))
}

fn navigate_open_succeeded(open_result: &Value) -> bool {
    open_result
        .get("success")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn navigate_failure_json(open_result: &Value) -> String {
    let error = open_result
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("Navigation failed");
    json!({
        "success": false,
        "error": error,
    })
    .to_string()
}

fn build_navigate_success_json(
    url_input: &str,
    open_result: &Value,
    task: &str,
    auth: Value,
    elapsed_ms: u64,
    snapshot: Option<(String, usize)>,
) -> String {
    let data = open_result.get("data").unwrap_or(open_result);
    let final_url = data
        .get("url")
        .and_then(|v| v.as_str())
        .unwrap_or(url_input);
    let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");

    let mut response = json!({
        "success": true,
        "status": "navigated",
        "url": final_url,
        "title": title,
        "task_id": task,
        "elapsed_ms": elapsed_ms,
        "backend": "agent-browser",
        "auth": auth,
        "data": data,
    });

    if let Some((snapshot_text, element_count)) = snapshot {
        if let Some(obj) = response.as_object_mut() {
            obj.insert("snapshot".into(), json!(snapshot_text));
            obj.insert("element_count".into(), json!(element_count));
        }
    }

    response.to_string()
}

#[async_trait]
impl BrowserBackend for AgentBrowserBackend {
    async fn navigate(&self, url: &str, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let started = Instant::now();
        let result = self
            .run_command(
                &task,
                "open",
                &[url.to_string()],
                Self::command_timeout_secs().max(60),
            )
            .await?;

        if !navigate_open_succeeded(&result) {
            return Ok(navigate_failure_json(&result));
        }

        let snapshot = self.compact_snapshot_after_open(&task).await;
        Ok(build_navigate_success_json(
            url,
            &result,
            &task,
            self.auth_context_for(&task).metadata(),
            started.elapsed().as_millis() as u64,
            snapshot,
        ))
    }

    async fn snapshot(
        &self,
        full: bool,
        user_task: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let started = Instant::now();
        let mut args = Vec::new();
        if !full {
            args.push("-c".to_string());
        }
        let result = self
            .run_command(&task, "snapshot", &args, Self::command_timeout_secs())
            .await?;

        let mut snapshot_text = result
            .get("data")
            .and_then(|d| d.get("snapshot"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        if !snapshot_text.is_empty() {
            snapshot_text = process_snapshot_text(&snapshot_text, user_task).await;
        }

        let refs = result
            .get("data")
            .and_then(|d| d.get("refs"))
            .cloned()
            .unwrap_or(json!({}));

        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "snapshot": snapshot_text,
            "element_count": refs.as_object().map(|m| m.len()).unwrap_or(0),
            "full": full,
            "user_task": user_task,
            "task_id": task,
            "elapsed_ms": started.elapsed().as_millis() as u64,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn click(&self, ref_id: &str, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let reference = normalize_ref(ref_id);
        let result = self
            .run_command(
                &task,
                "click",
                &[reference.clone()],
                Self::command_timeout_secs(),
            )
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "clicked": reference,
            "task_id": task,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn r#type(
        &self,
        ref_id: &str,
        text: &str,
        task_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let reference = normalize_ref(ref_id);
        let result = self
            .run_command(
                &task,
                "fill",
                &[reference.clone(), text.to_string()],
                Self::command_timeout_secs(),
            )
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "typed": reference,
            "text": text,
            "task_id": task,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn scroll(
        &self,
        direction: &str,
        amount: Option<u32>,
        task_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let px = amount.unwrap_or(500);
        let result = self
            .run_command(
                &task,
                "scroll",
                &[direction.to_string(), px.to_string()],
                Self::command_timeout_secs(),
            )
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "direction": direction,
            "amount": px,
            "task_id": task,
            "backend": "agent-browser",
            "data": result.get("data").cloned().unwrap_or(result),
        })
        .to_string())
    }

    async fn go_back(&self, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let result = self
            .run_command(&task, "back", &[], Self::command_timeout_secs())
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "status": "navigated_back",
            "task_id": task,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn press(&self, key: &str, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let result = self
            .run_command(
                &task,
                "press",
                &[key.to_string()],
                Self::command_timeout_secs(),
            )
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "key": key,
            "task_id": task,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn get_images(
        &self,
        selector: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{sel}')).map(img => ({{src: img.src, alt: img.alt}})))"
        );
        let result = self
            .run_command(&task, "eval", &[js], Self::command_timeout_secs())
            .await?;
        Ok(json!({
            "success": true,
            "selector": sel,
            "task_id": task,
            "backend": "agent-browser",
            "data": result.get("data").cloned().unwrap_or(result),
        })
        .to_string())
    }

    async fn vision(&self, instruction: &str, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        Ok(json!({
            "success": false,
            "error": "browser_vision requires screenshot pipeline; use browser_snapshot + vision_analyze for now",
            "instruction": instruction,
            "task_id": task,
            "backend": "agent-browser",
        })
        .to_string())
    }

    async fn console(&self, action: &str, task_id: Option<&str>) -> Result<String, ToolError> {
        let task = effective_task_id(task_id);
        let (cmd, args): (&str, Vec<String>) = match action {
            "clear" => ("console", vec!["--clear".to_string()]),
            "read" | _ => ("console", vec![]),
        };
        let result = self
            .run_command(&task, cmd, &args, Self::command_timeout_secs())
            .await?;
        Ok(json!({
            "success": result.get("success").and_then(|v| v.as_bool()).unwrap_or(true),
            "action": action,
            "task_id": task,
            "backend": "agent-browser",
            "data": result.get("data").cloned().unwrap_or(result),
        })
        .to_string())
    }
}

/// Select browser backend: agent-browser when available unless forced to CDP.
pub fn create_browser_backend() -> std::sync::Arc<dyn BrowserBackend> {
    use super::browser::CdpBrowserBackend;
    let forced = std::env::var("HERMES_BROWSER_BACKEND")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase());
    match forced.as_deref() {
        Some("cdp") => std::sync::Arc::new(CdpBrowserBackend::from_env()),
        Some("agent-browser") => {
            if let Ok(backend) = AgentBrowserBackend::new() {
                std::sync::Arc::new(backend)
            } else {
                std::sync::Arc::new(CdpBrowserBackend::from_env())
            }
        }
        _ => {
            if let Some(backend) = AgentBrowserBackend::try_new() {
                std::sync::Arc::new(backend)
            } else {
                std::sync::Arc::new(CdpBrowserBackend::from_env())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_ref_adds_at_prefix() {
        assert_eq!(normalize_ref("e5"), "@e5");
        assert_eq!(normalize_ref("@e5"), "@e5");
    }

    #[test]
    fn compact_snapshot_from_command_result_parses_data() {
        let result = json!({
            "success": true,
            "data": {
                "snapshot": "- button [ref=e1]",
                "refs": { "e1": {}, "e2": {} }
            }
        });
        let (text, count) = compact_snapshot_from_command_result(&result).unwrap();
        assert!(text.contains("button"));
        assert_eq!(count, 2);
    }

    #[test]
    fn build_navigate_success_json_includes_auto_snapshot() {
        let open = json!({
            "success": true,
            "data": {
                "url": "https://example.com/final",
                "title": "Example"
            }
        });
        let body = build_navigate_success_json(
            "https://example.com",
            &open,
            "task-1",
            json!({}),
            42,
            Some(("snap".into(), 3)),
        );
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["success"], true);
        assert_eq!(parsed["url"], "https://example.com/final");
        assert_eq!(parsed["title"], "Example");
        assert_eq!(parsed["snapshot"], "snap");
        assert_eq!(parsed["element_count"], 3);
    }

    #[test]
    fn build_navigate_success_json_omits_snapshot_when_none() {
        let open = json!({ "success": true, "data": { "url": "https://a.test", "title": "" } });
        let body = build_navigate_success_json("https://a.test", &open, "t", json!({}), 1, None);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert!(parsed.get("snapshot").is_none());
        assert!(parsed.get("element_count").is_none());
    }

    #[test]
    fn navigate_failure_json_from_open_result() {
        let open = json!({ "success": false, "error": "timeout" });
        let body = navigate_failure_json(&open);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["error"], "timeout");
    }
}
