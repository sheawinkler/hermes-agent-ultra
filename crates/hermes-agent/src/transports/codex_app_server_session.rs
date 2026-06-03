//! Session adapter for codex app-server (Python `codex_app_server_session.py`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermes_core::Message;
use serde_json::Value;

use crate::interrupt::InterruptController;
use crate::transports::codex_app_server::{CodexAppServerClient, CodexAppServerError};
use crate::transports::codex_event_projector::{has_turn_aborted_marker, CodexEventProjector};

const STDERR_TAIL_LINES: usize = 12;
#[derive(Debug, Clone, Default)]
pub struct TurnResult {
    pub final_text: String,
    pub projected_messages: Vec<Message>,
    pub tool_iterations: u32,
    pub interrupted: bool,
    pub error: Option<String>,
    pub turn_id: Option<String>,
    pub thread_id: Option<String>,
    pub should_retire: bool,
}

pub struct CodexAppServerSession {
    cwd: String,
    codex_bin: String,
    codex_home: Option<String>,
    client: Option<CodexAppServerClient>,
    thread_id: Option<String>,
    interrupt: Arc<InterruptController>,
    pending_file_changes: Mutex<HashMap<String, String>>,
    closed: bool,
    auto_approve_exec: bool,
    auto_approve_apply_patch: bool,
}

impl CodexAppServerSession {
    pub fn new(
        cwd: impl Into<String>,
        interrupt: Arc<InterruptController>,
        codex_bin: Option<String>,
        codex_home: Option<String>,
    ) -> Self {
        Self {
            cwd: cwd.into(),
            codex_bin: codex_bin.unwrap_or_else(|| "codex".to_string()),
            codex_home,
            client: None,
            thread_id: None,
            interrupt,
            pending_file_changes: Mutex::new(HashMap::new()),
            closed: false,
            auto_approve_exec: false,
            auto_approve_apply_patch: false,
        }
    }

    pub fn ensure_started(&mut self) -> Result<String, String> {
        if let Some(ref tid) = self.thread_id {
            return Ok(tid.clone());
        }
        if self.client.is_none() {
            let env_extra = HashMap::new();
            self.client = Some(CodexAppServerClient::spawn(
                &self.codex_bin,
                self.codex_home.as_deref(),
                &[],
                &env_extra,
            )?);
        }
        let client = self.client.as_ref().expect("client");
        client
            .initialize("hermes", "Hermes Agent", "0.1", Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        let result = client
            .request(
                "thread/start",
                Some(serde_json::json!({ "cwd": self.cwd })),
                Duration::from_secs(15),
            )
            .map_err(|e| e.to_string())?;
        let thread_obj = result.get("thread").cloned().unwrap_or(Value::Null);
        let thread_id = thread_obj
            .get("id")
            .or_else(|| thread_obj.get("sessionId"))
            .or_else(|| result.get("sessionId"))
            .or_else(|| result.get("threadId"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .ok_or_else(|| {
                format!(
                    "codex thread/start returned no thread id (keys: {:?})",
                    result.as_object().map(|o| o.keys().collect::<Vec<_>>())
                )
            })?;
        self.thread_id = Some(thread_id.clone());
        tracing::info!(
            thread_id = %&thread_id[..thread_id.len().min(8)],
            cwd = %self.cwd,
            "codex app-server thread started"
        );
        Ok(thread_id)
    }

    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(client) = self.client.take() {
            client.close();
        }
        self.thread_id = None;
    }

    pub fn run_turn(
        &mut self,
        user_input: &str,
        turn_timeout: Duration,
        notification_poll: Duration,
        post_tool_quiet: Duration,
    ) -> TurnResult {
        let mut result = TurnResult::default();
        if let Err(e) = self.ensure_started() {
            result.error = Some(format_error_with_stderr(
                self.client.as_ref(),
                "codex app-server startup failed",
                &e,
            ));
            result.should_retire = true;
            return result;
        }
        let thread_id = self.thread_id.clone().expect("thread");
        result.thread_id = Some(thread_id.clone());
        let client = self.client.as_ref().expect("client");
        let mut projector = CodexEventProjector::new();

        let start = client
            .request(
                "turn/start",
                Some(serde_json::json!({
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": user_input}],
                })),
                Duration::from_secs(10),
            );
        match start {
            Err(CodexAppServerError { message, .. }) => {
                let stderr_blob = client.stderr_tail(40).join("\n");
                result.error = classify_oauth_failure(&message, &stderr_blob).or_else(|| {
                    Some(format_error_with_stderr(
                        Some(client),
                        "turn/start failed",
                        &message,
                    ))
                });
                if result.error.as_ref().map(|e| e.contains("authentication")).unwrap_or(false) {
                    result.should_retire = true;
                }
                return result;
            }
            Ok(ts) => {
                result.turn_id = ts
                    .get("turn")
                    .and_then(|t| t.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
            }
        }

        let deadline = Instant::now() + turn_timeout;
        let mut turn_complete = false;
        let mut last_tool_completion: Option<Instant> = None;

        while Instant::now() < deadline && !turn_complete {
            if self.interrupt.is_interrupted() {
                self.issue_interrupt(result.turn_id.as_deref());
                result.interrupted = true;
                break;
            }
            if !client.is_alive() {
                let stderr_blob = client.stderr_tail(60).join("\n");
                result.error = classify_oauth_failure("", &stderr_blob).or_else(|| {
                    Some(format_error_with_stderr(
                        Some(client),
                        "codex app-server subprocess exited unexpectedly",
                        "",
                    ))
                });
                result.should_retire = true;
                break;
            }
            if let Some(at) = last_tool_completion {
                if at.elapsed() > post_tool_quiet {
                    self.issue_interrupt(result.turn_id.as_deref());
                    result.interrupted = true;
                    result.error = Some(format!(
                        "codex went silent for {:.0}s after a tool result; retiring app-server session.",
                        post_tool_quiet.as_secs_f64()
                    ));
                    result.should_retire = true;
                    break;
                }
            }

            if let Some(sreq) = client.take_server_request(Duration::ZERO) {
                for _ in 0..8 {
                    if let Some(note) = client.take_notification(Duration::ZERO) {
                        self.track_pending_file_change(&note);
                        let _ = apply_projection(&mut projector, &note, &mut result);
                    } else {
                        break;
                    }
                }
                self.handle_server_request(client, &sreq);
                last_tool_completion = None;
                continue;
            }

            let Some(note) = client.take_notification(notification_poll) else {
                continue;
            };
            self.track_pending_file_change(&note);
            let method = note.get("method").and_then(|v| v.as_str()).unwrap_or("");
            if apply_projection(&mut projector, &note, &mut result) {
                last_tool_completion = Some(Instant::now());
            } else if !result.projected_messages.is_empty() || !result.final_text.is_empty() {
                last_tool_completion = None;
            }

            if method == "turn/completed" {
                turn_complete = true;
                let turn_status = note
                    .pointer("/params/turn/status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !turn_status.is_empty()
                    && turn_status != "completed"
                    && turn_status != "interrupted"
                {
                    if let Some(err_obj) = note.pointer("/params/turn/error") {
                        let err_msg = err_obj.to_string();
                        let stderr_blob = client.stderr_tail(40).join("\n");
                        result.error = classify_oauth_failure(&err_msg, &stderr_blob).or_else(|| {
                            Some(format_error_with_stderr(
                                Some(client),
                                &format!("turn ended status={turn_status}"),
                                &err_msg,
                            ))
                        });
                        if result.error.as_ref().map(|e| e.contains("authentication")).unwrap_or(false)
                        {
                            result.should_retire = true;
                        }
                    }
                }
            }
        }

        if !turn_complete && !result.interrupted {
            self.issue_interrupt(result.turn_id.as_deref());
            result.interrupted = true;
            if result.error.is_none() {
                result.error = Some(format_error_with_stderr(
                    Some(client),
                    &format!("turn timed out after {:.0}s", turn_timeout.as_secs_f64()),
                    "",
                ));
            }
            result.should_retire = true;
        }

        result
    }

    fn issue_interrupt(&self, turn_id: Option<&str>) {
        let (Some(client), Some(thread_id), Some(turn_id)) =
            (self.client.as_ref(), self.thread_id.as_deref(), turn_id)
        else {
            return;
        };
        let _ = client.request(
            "turn/interrupt",
            Some(serde_json::json!({
                "threadId": thread_id,
                "turnId": turn_id,
            })),
            Duration::from_secs(5),
        );
    }

    fn handle_server_request(&self, client: &CodexAppServerClient, req: &Value) {
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let rid = req.get("id").cloned().unwrap_or(Value::Null);
        let params = req.get("params").cloned().unwrap_or(Value::Null);
        let decision = match method {
            "item/commandExecution/requestApproval" => {
                if self.auto_approve_exec {
                    "accept".to_string()
                } else {
                    "decline".to_string()
                }
            }
            "item/fileChange/requestApproval" => {
                if self.auto_approve_apply_patch {
                    "accept".to_string()
                } else {
                    "decline".to_string()
                }
            }
            "item/permissions/requestApproval" => "decline".to_string(),
            "mcpServer/elicitation/request" => {
                let server = params.get("serverName").and_then(|v| v.as_str()).unwrap_or("");
                if server == "hermes-tools" {
                    let _ = client.respond(
                        &rid,
                        serde_json::json!({
                            "action": "accept",
                            "content": null,
                            "_meta": null
                        }),
                    );
                    return;
                }
                let _ = client.respond(
                    &rid,
                    serde_json::json!({
                        "action": "decline",
                        "content": null,
                        "_meta": null
                    }),
                );
                return;
            }
            _ => {
                tracing::warn!(method = %method, "unknown codex server request");
                let _ = client.respond_error(&rid, -32601, &format!("Unsupported method: {method}"), None);
                return;
            }
        };
        let _ = client.respond(&rid, serde_json::json!({ "decision": decision }));
    }

    fn track_pending_file_change(&self, note: &Value) {
        let method = note.get("method").and_then(|v| v.as_str()).unwrap_or("");
        if method == "item/started" {
            if let Some(item) = note.pointer("/params/item") {
                if item.get("type").and_then(|v| v.as_str()) == Some("fileChange") {
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        let summary: Vec<String> = item
                            .get("changes")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| {
                                        let k = c.pointer("/kind/type").and_then(|v| v.as_str())?;
                                        let p = c.get("path").and_then(|v| v.as_str())?;
                                        Some(format!("{k}:{p}"))
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if let Ok(mut pending) = self.pending_file_changes.lock() {
                            pending.insert(id.to_string(), summary.join(", "));
                        }
                    }
                }
            }
        }
    }
}

/// Returns true when a tool-shaped item completed (arms post-tool quiet watchdog).
fn apply_projection(
    projector: &mut CodexEventProjector,
    note: &Value,
    result: &mut TurnResult,
) -> bool {
    let projection = projector.project(note);
    if !projection.messages.is_empty() {
        result.projected_messages.extend(projection.messages);
    }
    if projection.is_tool_iteration {
        result.tool_iterations = result.tool_iterations.saturating_add(1);
    }
    if let Some(text) = projection.final_text {
        result.final_text = text.clone();
        if has_turn_aborted_marker(&text) {
            result.interrupted = true;
            result.error.get_or_insert_with(|| "codex reported turn_aborted".to_string());
        }
    }
    projection.is_tool_iteration
}

fn format_error_with_stderr(
    client: Option<&CodexAppServerClient>,
    prefix: &str,
    exc: &str,
) -> String {
    let base = if exc.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}: {exc}")
    };
    let Some(client) = client else {
        return base;
    };
    let tail = client.stderr_tail(STDERR_TAIL_LINES);
    if tail.is_empty() {
        return base;
    }
    format!(
        "{base}\ncodex stderr (last {} lines):\n{}",
        tail.len(),
        tail.join("\n")
    )
}

fn classify_oauth_failure(message: &str, stderr: &str) -> Option<String> {
    let haystack = format!("{message} {stderr}").to_ascii_lowercase();
    const HINTS: &[&str] = &[
        "invalid_grant",
        "refresh token",
        "token refresh",
        "expired token",
        "not authenticated",
        "unauthorized",
        "401 unauthorized",
        "re-authenticate",
        "please log in",
        "oauth",
    ];
    if HINTS.iter().any(|h| haystack.contains(h)) {
        return Some(
            "Codex authentication failed — your ChatGPT/Codex login looks expired or invalid. \
             Run `codex login` to refresh, then retry. (Fall back to default runtime with \
             `/codex-runtime auto` if the issue persists.)"
                .to_string(),
        );
    }
    None
}
