//! Agent-facing deterministic replay trace controls.
//!
//! The TUI exposes these through `/raw trace ...`; this tool makes the same
//! replay log surface callable from the Rust tool registry.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

const TOOL_NAME: &str = "replay_trace_control";
const DEFAULT_TAIL_LIMIT: usize = 20;
const MAX_TAIL_LIMIT: usize = 200;
const DEFAULT_FOCUS_LIMIT: usize = 150;
const MAX_FOCUS_LIMIT: usize = 1_000;
const DEFAULT_GRAPH_LIMIT: usize = 80;
const MAX_GRAPH_LIMIT: usize = 500;
const DEFAULT_EXPORT_LIMIT: usize = 100;
const MAX_EXPORT_LIMIT: usize = 1_000;
const MAX_DIFF_HASH_SAMPLES: usize = 100;
const PREVIEW_CHARS: usize = 2_000;

#[derive(Clone, Default)]
pub struct ReplayTraceControlHandler {
    replay_dir_override: Option<PathBuf>,
}

impl ReplayTraceControlHandler {
    pub fn new() -> Self {
        Self {
            replay_dir_override: None,
        }
    }

    fn replay_dir(&self) -> PathBuf {
        self.replay_dir_override
            .clone()
            .unwrap_or_else(default_replay_dir)
    }

    #[cfg(test)]
    fn with_replay_dir(path: PathBuf) -> Self {
        Self {
            replay_dir_override: Some(path),
        }
    }
}

#[async_trait]
impl ToolHandler for ReplayTraceControlHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();

        match action.as_str() {
            "status" => Ok(status_payload(&self.replay_dir(), &params)?.to_string()),
            "on" | "enable" => {
                set_replay_enabled(true);
                Ok(status_payload(&self.replay_dir(), &params)?.to_string())
            }
            "off" | "disable" => {
                set_replay_enabled(false);
                Ok(status_payload(&self.replay_dir(), &params)?.to_string())
            }
            "toggle" => {
                set_replay_enabled(!replay_enabled_runtime());
                Ok(status_payload(&self.replay_dir(), &params)?.to_string())
            }
            "path" => {
                let root = self.replay_dir();
                let path = resolve_replay_path(&root, &params)?;
                Ok(json!({
                    "status": "ok",
                    "replay_dir": root.display().to_string(),
                    "path": path.display().to_string(),
                    "exists": path.exists(),
                })
                .to_string())
            }
            "tail" => {
                let limit = parse_limit(&params, DEFAULT_TAIL_LIMIT, MAX_TAIL_LIMIT);
                let root = self.replay_dir();
                let path = resolve_replay_path(&root, &params)?;
                let rows = tail_rows(&path, limit)?;
                Ok(json!({
                    "status": if path.exists() { "ok" } else { "not_found" },
                    "path": path.display().to_string(),
                    "limit": limit,
                    "rows": rows,
                })
                .to_string())
            }
            "focus" => {
                let trace_id = params
                    .get("trace_id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ToolError::InvalidParams("focus requires trace_id".to_string())
                    })?;
                let limit = parse_limit(&params, DEFAULT_FOCUS_LIMIT, MAX_FOCUS_LIMIT);
                let root = self.replay_dir();
                let path = resolve_replay_path(&root, &params)?;
                let rows = focus_rows(&path, trace_id, limit)?;
                Ok(json!({
                    "status": if path.exists() { "ok" } else { "not_found" },
                    "trace_id": trace_id,
                    "path": path.display().to_string(),
                    "limit": limit,
                    "rows": rows,
                })
                .to_string())
            }
            "graph" => {
                let limit = parse_limit(&params, DEFAULT_GRAPH_LIMIT, MAX_GRAPH_LIMIT);
                let root = self.replay_dir();
                let path = resolve_replay_path(&root, &params)?;
                let edges = graph_edges(&path, limit)?;
                Ok(json!({
                    "status": if path.exists() { "ok" } else { "not_found" },
                    "path": path.display().to_string(),
                    "limit": limit,
                    "edges": edges,
                })
                .to_string())
            }
            "verify" => {
                let root = self.replay_dir();
                let path = resolve_verify_path(&root, &params)?;
                if path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
                {
                    let summary = verify_replay_export(&path)?;
                    let ok = summary
                        .get("exists")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    Ok(json!({
                        "status": if ok { "pass" } else { "fail" },
                        "path": path.display().to_string(),
                        "summary": summary,
                    })
                    .to_string())
                } else {
                    let summary = verify_replay_log(&path)?;
                    let ok = summary.exists && summary.parse_errors == 0 && summary.chain_breaks == 0;
                    Ok(json!({
                        "status": if ok { "pass" } else { "fail" },
                        "path": path.display().to_string(),
                        "summary": summary.to_json(),
                    })
                    .to_string())
                }
            }
            "export" => {
                let limit = parse_limit(&params, DEFAULT_EXPORT_LIMIT, MAX_EXPORT_LIMIT);
                let root = self.replay_dir();
                let path = resolve_replay_path(&root, &params)?;
                let output_path = export_path(&root, &params)?;
                if !path.exists() {
                    return Ok(json!({
                        "status": "not_found",
                        "limit": limit,
                        "rows": 0,
                        "source": path.display().to_string(),
                        "output": output_path.display().to_string(),
                    })
                    .to_string());
                }
                let rows = export_rows(&path, &output_path, limit)?;
                Ok(json!({
                    "status": "ok",
                    "limit": limit,
                    "rows": rows,
                    "source": path.display().to_string(),
                    "output": output_path.display().to_string(),
                })
                .to_string())
            }
            "diff" => {
                let root = self.replay_dir();
                Ok(diff_replay_exports(&root, &params)?.to_string())
            }
            "help" => Ok(json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "on", "off", "toggle", "path", "tail", "focus", "graph", "verify", "export", "diff", "help"],
                "notes": [
                    "mirrors `/raw trace ...` without requiring a TUI",
                    "session_id selects `<hermes_home>/logs/replay/<session_id>.jsonl`",
                    "tail/focus/graph/export are bounded and redact secret-like values",
                    "diff compares replay export JSON files by event_hash"
                ],
            })
            .to_string()),
            _ => Err(ToolError::InvalidParams(format!(
                "unknown action '{action}'; expected status|on|off|toggle|path|tail|focus|graph|verify|export|diff|help"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "on", "off", "toggle", "path", "tail", "focus", "graph", "verify", "export", "diff", "help"],
                "description": "Replay trace action. Defaults to status."
            }),
        );
        props.insert(
            "session_id".into(),
            json!({
                "type": "string",
                "description": "Replay session id. Defaults to HERMES_REPLAY_SESSION_ID, HERMES_SESSION_ID, or 'session'."
            }),
        );
        props.insert(
            "path".into(),
            json!({
                "type": "string",
                "description": "Optional replay log path. Relative paths resolve under the replay log directory; absolute paths must stay under it."
            }),
        );
        props.insert(
            "trace_id".into(),
            json!({
                "type": "string",
                "description": "Trace id or substring for focus action."
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_EXPORT_LIMIT,
                "description": "Maximum rows for tail/focus/graph/export. Each action clamps to its own cap."
            }),
        );
        props.insert(
            "output_path".into(),
            json!({
                "type": "string",
                "description": "Optional export path. Relative paths are placed under replay exports; absolute paths must stay under the replay directory."
            }),
        );
        props.insert(
            "left_path".into(),
            json!({
                "type": "string",
                "description": "Left replay export JSON for diff. Relative paths resolve under replay exports; absolute paths must stay under the replay directory."
            }),
        );
        props.insert(
            "right_path".into(),
            json!({
                "type": "string",
                "description": "Right replay export JSON for diff. Relative paths resolve under replay exports; absolute paths must stay under the replay directory."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Inspect, verify, export, and diff deterministic replay traces without a TUI.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ReplayVerifySummary {
    exists: bool,
    entries: usize,
    parse_errors: usize,
    chain_breaks: usize,
    bytes: u64,
}

impl ReplayVerifySummary {
    fn missing() -> Self {
        Self {
            exists: false,
            entries: 0,
            parse_errors: 0,
            chain_breaks: 0,
            bytes: 0,
        }
    }

    fn to_json(self) -> Value {
        json!({
            "exists": self.exists,
            "entries": self.entries,
            "parse_errors": self.parse_errors,
            "chain_breaks": self.chain_breaks,
            "bytes": self.bytes,
        })
    }
}

fn status_payload(root: &Path, params: &Value) -> Result<Value, ToolError> {
    let path = resolve_replay_path(root, params)?;
    Ok(json!({
        "status": "ok",
        "enabled": replay_enabled_runtime(),
        "session_id": session_id(params),
        "replay_dir": root.display().to_string(),
        "path": path.display().to_string(),
        "exists": path.exists(),
    }))
}

fn replay_enabled_runtime() -> bool {
    std::env::var("HERMES_REPLAY_ENABLED")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn set_replay_enabled(enabled: bool) {
    unsafe { std::env::set_var("HERMES_REPLAY_ENABLED", if enabled { "1" } else { "0" }) };
}

fn default_replay_dir() -> PathBuf {
    hermes_config::hermes_home().join("logs").join("replay")
}

fn session_id(params: &Value) -> String {
    params
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_session_id)
        .or_else(|| env_session_id("HERMES_REPLAY_SESSION_ID"))
        .or_else(|| env_session_id("HERMES_SESSION_ID"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "session".to_string())
}

fn env_session_id(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| sanitize_session_id(&value))
}

fn sanitize_session_id(raw: &str) -> String {
    let sanitized: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.trim().is_empty() {
        "session".to_string()
    } else {
        sanitized
    }
}

fn parse_limit(params: &Value, default: usize, max: usize) -> usize {
    params
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or(default)
        .clamp(1, max)
}

fn resolve_replay_path(root: &Path, params: &Value) -> Result<PathBuf, ToolError> {
    let Some(raw_path) = params
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(root.join(format!("{}.jsonl", session_id(params))));
    };

    safe_path_under_root(root, raw_path, "path")
}

fn resolve_verify_path(root: &Path, params: &Value) -> Result<PathBuf, ToolError> {
    let Some(raw_path) = params
        .get("path")
        .or_else(|| params.get("export_path"))
        .or_else(|| params.get("output_path"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(root.join(format!("{}.jsonl", session_id(params))));
    };

    let candidate = Path::new(raw_path);
    if candidate.is_absolute() {
        return safe_path_under_root(root, raw_path, "path");
    }
    let relative = safe_relative_path(raw_path, "path")?;
    if candidate
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
        Ok(root.join("exports").join(relative))
    } else {
        Ok(root.join(relative))
    }
}

fn export_path(root: &Path, params: &Value) -> Result<PathBuf, ToolError> {
    let default = root
        .join("exports")
        .join(format!("{}-tail.json", session_id(params)));
    let Some(raw_path) = params
        .get("output_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(default);
    };

    if Path::new(raw_path).is_absolute() {
        return safe_path_under_root(root, raw_path, "output_path");
    }
    let relative = safe_relative_path(raw_path, "output_path")?;
    Ok(root.join("exports").join(relative))
}

fn resolve_export_input_path(
    root: &Path,
    params: &Value,
    keys: &[&str],
    field: &str,
) -> Result<PathBuf, ToolError> {
    let raw_path = string_param(params, keys)
        .ok_or_else(|| ToolError::InvalidParams(format!("diff requires {field}")))?;
    export_input_path(root, raw_path, field)
}

fn export_input_path(root: &Path, raw_path: &str, field: &str) -> Result<PathBuf, ToolError> {
    let raw_path = raw_path.trim();
    if raw_path.is_empty() {
        return Err(ToolError::InvalidParams(format!(
            "{field} must not be empty"
        )));
    }
    if Path::new(raw_path).is_absolute() {
        return safe_path_under_root(root, raw_path, field);
    }
    let relative = safe_relative_path(raw_path, field)?;
    Ok(root.join("exports").join(relative))
}

fn string_param<'a>(params: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| params.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|value| !value.is_empty())
}

fn safe_path_under_root(root: &Path, raw_path: &str, field: &str) -> Result<PathBuf, ToolError> {
    let candidate = PathBuf::from(raw_path);
    if has_parent_component(&candidate) {
        return Err(ToolError::InvalidParams(format!(
            "{field} must not contain parent directory components"
        )));
    }
    if candidate.is_absolute() {
        if candidate.starts_with(root) {
            return Ok(candidate);
        }
        return Err(ToolError::InvalidParams(format!(
            "{field} must stay under replay dir {}",
            root.display()
        )));
    }
    let relative = safe_relative_path(raw_path, field)?;
    Ok(root.join(relative))
}

fn safe_relative_path(raw_path: &str, field: &str) -> Result<PathBuf, ToolError> {
    let candidate = PathBuf::from(raw_path);
    if candidate.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(ToolError::InvalidParams(format!(
            "{field} must be relative and must not escape replay dir"
        )));
    }
    Ok(candidate)
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn read_log(path: &Path) -> Result<Option<String>, ToolError> {
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read_to_string(path)
        .map(Some)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", path.display())))
}

fn tail_rows(path: &Path, limit: usize) -> Result<Vec<Value>, ToolError> {
    let Some(raw) = read_log(path)? else {
        return Ok(Vec::new());
    };
    let mut rows: Vec<Value> = raw
        .lines()
        .rev()
        .take(limit)
        .enumerate()
        .map(|(reverse_index, line)| summarize_replay_line(line, reverse_index))
        .collect();
    rows.reverse();
    Ok(rows)
}

fn replay_entries(path: &Path, limit: usize) -> Result<Vec<Value>, ToolError> {
    let Some(raw) = read_log(path)? else {
        return Ok(Vec::new());
    };
    Ok(raw
        .lines()
        .rev()
        .take(limit)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}

fn summarize_replay_line(line: &str, reverse_index: usize) -> Value {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => summarize_replay_value(&value),
        Err(err) => json!({
            "valid_json": false,
            "reverse_index": reverse_index,
            "error": err.to_string(),
            "preview": redact_and_truncate(line),
        }),
    }
}

fn summarize_replay_value(value: &Value) -> Value {
    json!({
        "valid_json": true,
        "seq": value.get("seq").and_then(Value::as_u64),
        "event": value.get("event").and_then(Value::as_str),
        "trace_id": value.get("trace_id").and_then(Value::as_str),
        "prev_hash": value.get("prev_hash").and_then(Value::as_str),
        "event_hash": value.get("event_hash").and_then(Value::as_str),
        "turn": value.get("payload").and_then(|payload| payload.get("turn")).and_then(Value::as_u64),
        "payload_preview": value.get("payload").map(|payload| redact_and_truncate(&payload.to_string())),
    })
}

fn focus_rows(path: &Path, trace_filter: &str, limit: usize) -> Result<Vec<Value>, ToolError> {
    let rows = replay_entries(path, limit)?;
    Ok(rows
        .into_iter()
        .filter(|row| {
            row.get("trace_id")
                .and_then(Value::as_str)
                .is_some_and(|trace_id| trace_id == trace_filter || trace_id.contains(trace_filter))
        })
        .map(|row| summarize_replay_value(&row))
        .collect())
}

fn graph_edges(path: &Path, limit: usize) -> Result<Vec<Value>, ToolError> {
    let rows = replay_entries(path, limit)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            json!({
                "seq": row.get("seq").and_then(Value::as_u64),
                "event": row.get("event").and_then(Value::as_str),
                "trace_id": row.get("trace_id").and_then(Value::as_str),
                "prev_hash": row.get("prev_hash").and_then(Value::as_str),
                "event_hash": row.get("event_hash").and_then(Value::as_str),
            })
        })
        .collect())
}

fn verify_replay_log(path: &Path) -> Result<ReplayVerifySummary, ToolError> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ReplayVerifySummary::missing());
        }
        Err(err) => {
            return Err(ToolError::ExecutionFailed(format!(
                "stat {}: {err}",
                path.display()
            )));
        }
    };
    let Some(raw) = read_log(path)? else {
        return Ok(ReplayVerifySummary::missing());
    };
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    let mut chain_breaks = 0usize;
    let mut last_event_hash: Option<String> = None;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            }
        };
        entries = entries.saturating_add(1);
        let prev_hash = parsed
            .get("prev_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        let event_hash = parsed
            .get("event_hash")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let (Some(last), Some(prev)) = (last_event_hash.as_ref(), prev_hash.as_ref()) {
            if last != prev {
                chain_breaks = chain_breaks.saturating_add(1);
            }
        }
        if let Some(hash) = event_hash {
            last_event_hash = Some(hash);
        }
    }
    Ok(ReplayVerifySummary {
        exists: true,
        entries,
        parse_errors,
        chain_breaks,
        bytes: metadata.len(),
    })
}

fn read_replay_export_rows(path: &Path) -> Result<Option<Vec<Value>>, ToolError> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", path.display())))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|e| ToolError::ExecutionFailed(format!("parse {}: {e}", path.display())))?;
    Ok(Some(
        parsed
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    ))
}

fn verify_replay_export(path: &Path) -> Result<Value, ToolError> {
    let Some(rows) = read_replay_export_rows(path)? else {
        return Ok(json!({
            "kind": "export",
            "exists": false,
            "rows": 0,
            "hashes": 0,
            "status": "missing",
        }));
    };
    let hashes = replay_event_hashes(&rows);
    Ok(json!({
        "kind": "export",
        "exists": true,
        "rows": rows.len(),
        "hashes": hashes.len(),
        "status": if rows.is_empty() { "empty" } else { "ok" },
    }))
}

fn diff_replay_exports(root: &Path, params: &Value) -> Result<Value, ToolError> {
    let left_path = resolve_export_input_path(
        root,
        params,
        &["left_path", "a_path", "path_a", "export_a_path", "export_a"],
        "left_path",
    )?;
    let right_path = resolve_export_input_path(
        root,
        params,
        &[
            "right_path",
            "b_path",
            "path_b",
            "export_b_path",
            "export_b",
        ],
        "right_path",
    )?;
    let left_rows = read_replay_export_rows(&left_path)?.ok_or_else(|| {
        ToolError::ExecutionFailed(format!("read {}: file not found", left_path.display()))
    })?;
    let right_rows = read_replay_export_rows(&right_path)?.ok_or_else(|| {
        ToolError::ExecutionFailed(format!("read {}: file not found", right_path.display()))
    })?;
    let left_hashes = replay_event_hashes(&left_rows);
    let right_hashes = replay_event_hashes(&right_rows);
    let only_left_count = left_hashes.difference(&right_hashes).count();
    let only_right_count = right_hashes.difference(&left_hashes).count();
    let only_left_hashes: Vec<String> = left_hashes
        .difference(&right_hashes)
        .take(MAX_DIFF_HASH_SAMPLES)
        .cloned()
        .collect();
    let only_right_hashes: Vec<String> = right_hashes
        .difference(&left_hashes)
        .take(MAX_DIFF_HASH_SAMPLES)
        .cloned()
        .collect();
    Ok(json!({
        "status": "ok",
        "left": {
            "path": left_path.display().to_string(),
            "rows": left_rows.len(),
            "hashes": left_hashes.len(),
        },
        "right": {
            "path": right_path.display().to_string(),
            "rows": right_rows.len(),
            "hashes": right_hashes.len(),
        },
        "overlap_hashes": left_hashes.intersection(&right_hashes).count(),
        "only_in_left": only_left_count,
        "only_in_right": only_right_count,
        "only_in_left_hashes": only_left_hashes,
        "only_in_right_hashes": only_right_hashes,
        "hash_samples_truncated": only_left_count > MAX_DIFF_HASH_SAMPLES || only_right_count > MAX_DIFF_HASH_SAMPLES,
    }))
}

fn replay_event_hashes(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| row.get("event_hash").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect()
}

fn export_rows(source: &Path, output_path: &Path, limit: usize) -> Result<usize, ToolError> {
    let rows = replay_entries(source, limit)?;
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ToolError::ExecutionFailed(format!("create export dir {}: {e}", parent.display()))
        })?;
    }
    let payload = json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "source_replay": source.display().to_string(),
        "rows": rows,
    });
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| ToolError::ExecutionFailed(format!("write {}: {e}", output_path.display())))?;
    Ok(payload["rows"].as_array().map(Vec::len).unwrap_or(0))
}

fn redact_and_truncate(input: &str) -> String {
    let redacted = secret_regex()
        .replace_all(input, |caps: &regex::Captures| {
            let key = caps.get(1).map(|m| m.as_str()).unwrap_or("token");
            format!("{key}=<redacted>")
        })
        .to_string();
    truncate_chars(&redacted, PREVIEW_CHARS)
}

fn secret_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|token|secret|password)\b\s*[:=]\s*["']?([A-Za-z0-9_\-./]{8,})"#,
        )
        .expect("secret redaction regex")
    })
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let cutoff = value
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(value.len());
    format!("{}...", &value[..cutoff])
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = &self.original {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    struct TestEnv {
        replay_dir: PathBuf,
        _dir: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        _session: EnvGuard,
        _replay_session: EnvGuard,
    }

    fn handler_with_home() -> (TestEnv, ReplayTraceControlHandler) {
        let guard = ENV_LOCK.lock().expect("env lock");
        let dir = tempfile::tempdir().expect("temp hermes home");
        let replay_dir = dir.path().join("logs").join("replay");
        let session = EnvGuard::remove("HERMES_SESSION_ID");
        let replay_session = EnvGuard::remove("HERMES_REPLAY_SESSION_ID");
        let handler = ReplayTraceControlHandler::with_replay_dir(replay_dir.clone());
        (
            TestEnv {
                replay_dir,
                _dir: dir,
                _guard: guard,
                _session: session,
                _replay_session: replay_session,
            },
            handler,
        )
    }

    #[tokio::test]
    async fn status_and_mode_actions_update_replay_env() {
        let (_env, handler) = handler_with_home();
        let _enabled = EnvGuard::remove("HERMES_REPLAY_ENABLED");

        let on: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"on","session_id":"s/1"}))
                .await
                .unwrap(),
        )
        .expect("json");
        assert_eq!(on["enabled"], true);
        assert!(on["path"].as_str().expect("path").ends_with("s_1.jsonl"));

        let off: Value =
            serde_json::from_str(&handler.execute(json!({"action":"off"})).await.unwrap())
                .expect("json");
        assert_eq!(off["enabled"], false);
    }

    #[tokio::test]
    async fn tail_focus_graph_verify_and_export_replay_rows() {
        let (env, handler) = handler_with_home();
        std::fs::create_dir_all(&env.replay_dir).expect("mkdir replay");
        let log = env.replay_dir.join("session-a.jsonl");
        std::fs::write(
            &log,
            [
                json!({"seq":1,"event":"start","trace_id":"trace-a-00000001","prev_hash":"seed","event_hash":"a","payload":{"turn":1,"prompt":"ok"}}).to_string(),
                json!({"seq":2,"event":"tool","trace_id":"trace-a-00000002","prev_hash":"a","event_hash":"b","payload":{"turn":1,"command":"echo token=supersecret123"}}).to_string(),
                "{not-json".to_string(),
            ]
            .join("\n"),
        )
        .expect("write replay");

        let tail: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"tail","session_id":"session-a","limit":3}))
                .await
                .unwrap(),
        )
        .expect("tail json");
        assert_eq!(tail["rows"].as_array().expect("rows").len(), 3);
        assert!(!tail.to_string().contains("supersecret123"));
        assert!(tail.to_string().contains("<redacted>"));

        let focus: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"focus","session_id":"session-a","trace_id":"00000002"}))
                .await
                .unwrap(),
        )
        .expect("focus json");
        assert_eq!(focus["rows"].as_array().expect("rows").len(), 1);

        let graph: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"graph","session_id":"session-a"}))
                .await
                .unwrap(),
        )
        .expect("graph json");
        assert_eq!(graph["edges"].as_array().expect("edges").len(), 2);

        let verify: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"verify","session_id":"session-a"}))
                .await
                .unwrap(),
        )
        .expect("verify json");
        assert_eq!(verify["status"], "fail");
        assert_eq!(verify["summary"]["entries"], 2);
        assert_eq!(verify["summary"]["parse_errors"], 1);
        assert_eq!(verify["summary"]["chain_breaks"], 0);

        let export: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"export","session_id":"session-a","limit":3,"output_path":"sample.json"}))
                .await
                .unwrap(),
        )
        .expect("export json");
        let output = PathBuf::from(export["output"].as_str().expect("output path"));
        assert!(output.exists());
        assert!(output.starts_with(&env.replay_dir));
        assert_eq!(export["rows"], 2);

        let verify_export: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"verify","path":"sample.json"}))
                .await
                .unwrap(),
        )
        .expect("verify export json");
        assert_eq!(verify_export["status"], "pass");
        assert_eq!(verify_export["summary"]["kind"], "export");
        assert_eq!(verify_export["summary"]["rows"], 2);
        assert_eq!(verify_export["summary"]["hashes"], 2);
    }

    #[tokio::test]
    async fn rejects_replay_path_escapes() {
        let (env, handler) = handler_with_home();
        let outside = env.replay_dir.join("../outside.jsonl");
        for params in [
            json!({"action":"path","path":"../outside.jsonl"}),
            json!({"action":"path","path":outside.display().to_string()}),
            json!({"action":"export","output_path":"/tmp/hermes-replay-export.json"}),
            json!({"action":"diff","left_path":"../a.json","right_path":"b.json"}),
            json!({"action":"diff","left_path":"a.json","right_path":"/tmp/b.json"}),
        ] {
            let err = handler
                .execute(params)
                .await
                .expect_err("escape should fail");
            match err {
                ToolError::InvalidParams(message) => {
                    assert!(
                        message.contains("replay dir")
                            || message.contains("parent directory")
                            || message.contains("escape"),
                        "{message}"
                    );
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn empty_replay_session_env_falls_through_and_missing_export_does_not_write() {
        let (env, handler) = handler_with_home();
        let _replay_session = EnvGuard::set("HERMES_REPLAY_SESSION_ID", "");
        let _session = EnvGuard::set("HERMES_SESSION_ID", "fallback/session");

        let status: Value =
            serde_json::from_str(&handler.execute(json!({"action":"status"})).await.unwrap())
                .expect("status json");
        assert_eq!(status["session_id"], "fallback_session");

        let export: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"export","output_path":"missing.json"}))
                .await
                .unwrap(),
        )
        .expect("export json");
        assert_eq!(export["status"], "not_found");
        assert_eq!(export["rows"], 0);
        let output = PathBuf::from(export["output"].as_str().expect("output path"));
        assert!(output.starts_with(&env.replay_dir));
        assert!(!output.exists());
    }

    #[tokio::test]
    async fn diff_compares_replay_export_hashes_with_aliases() {
        let (env, handler) = handler_with_home();
        let exports = env.replay_dir.join("exports");
        std::fs::create_dir_all(&exports).expect("mkdir exports");
        std::fs::write(
            exports.join("a.json"),
            serde_json::to_string_pretty(&json!({
                "rows": [
                    {"seq": 1, "event_hash": "aaa"},
                    {"seq": 2, "event_hash": "bbb"},
                    {"seq": 3, "event_hash": "shared"}
                ]
            }))
            .expect("json a"),
        )
        .expect("write a");
        std::fs::write(
            exports.join("b.json"),
            serde_json::to_string_pretty(&json!({
                "rows": [
                    {"seq": 1, "event_hash": "ccc"},
                    {"seq": 2, "event_hash": "shared"},
                    {"seq": 3, "event_hash": "shared"}
                ]
            }))
            .expect("json b"),
        )
        .expect("write b");

        let diff: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"diff","export_a":"a.json","export_b":"b.json"}))
                .await
                .unwrap(),
        )
        .expect("diff json");

        assert_eq!(diff["status"], "ok");
        assert_eq!(diff["left"]["rows"], 3);
        assert_eq!(diff["left"]["hashes"], 3);
        assert_eq!(diff["right"]["rows"], 3);
        assert_eq!(diff["right"]["hashes"], 2);
        assert_eq!(diff["overlap_hashes"], 1);
        assert_eq!(diff["only_in_left"], 2);
        assert_eq!(diff["only_in_right"], 1);
        assert_eq!(diff["only_in_left_hashes"], json!(["aaa", "bbb"]));
        assert_eq!(diff["only_in_right_hashes"], json!(["ccc"]));
        assert_eq!(diff["hash_samples_truncated"], false);
    }
}
