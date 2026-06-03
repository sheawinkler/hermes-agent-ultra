//! Agent-facing RTK raw trace controls.
//!
//! The TUI already exposes `/raw ...`; this tool provides the same RTK
//! operator surface to the Rust tool registry without dispatching target tools.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use regex::Regex;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::ToolRegistry;

const TOOL_NAME: &str = "raw_trace_control";
const DEFAULT_TAIL_LIMIT: usize = 20;
const MAX_TAIL_LIMIT: usize = 200;
const DEFAULT_EXPORT_LIMIT: usize = 100;
const MAX_EXPORT_LIMIT: usize = 1_000;
const PREVIEW_CHARS: usize = 2_000;

#[derive(Clone)]
pub struct RawTraceControlHandler {
    registry: Arc<ToolRegistry>,
}

impl RawTraceControlHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolHandler for RawTraceControlHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();

        match action.as_str() {
            "status" => Ok(status_payload(&self.registry).to_string()),
            "on" | "enable" => {
                self.registry.set_raw_mode(true);
                Ok(status_payload(&self.registry).to_string())
            }
            "off" | "disable" => {
                self.registry.set_raw_mode(false);
                Ok(status_payload(&self.registry).to_string())
            }
            "toggle" => {
                let state = self.registry.raw_mode_state();
                self.registry.set_raw_mode(!state.enabled);
                Ok(status_payload(&self.registry).to_string())
            }
            "once" => {
                self.registry.set_raw_mode_once();
                Ok(status_payload(&self.registry).to_string())
            }
            "path" => Ok(json!({
                "status": "ok",
                "rtk_log_dir": self.registry.rtk_log_dir().display().to_string(),
                "raw_log": log_path(&self.registry, StreamKind::Raw).display().to_string(),
                "filtered_log": log_path(&self.registry, StreamKind::Filtered).display().to_string(),
            })
            .to_string()),
            "tail" => {
                let stream = stream_kind(&params)?;
                let limit = parse_limit(&params, DEFAULT_TAIL_LIMIT, MAX_TAIL_LIMIT);
                let path = log_path(&self.registry, stream);
                let rows = tail_log_rows(&path, limit)?;
                Ok(json!({
                    "status": "ok",
                    "stream": stream.as_str(),
                    "path": path.display().to_string(),
                    "limit": limit,
                    "rows": rows,
                })
                .to_string())
            }
            "verify" => {
                let raw = verify_log(&log_path(&self.registry, StreamKind::Raw))?;
                let filtered = verify_log(&log_path(&self.registry, StreamKind::Filtered))?;
                let ok = raw.parse_errors == 0 && filtered.parse_errors == 0;
                Ok(json!({
                    "status": if ok { "pass" } else { "fail" },
                    "raw": raw.to_json(),
                    "filtered": filtered.to_json(),
                    "rtk_log_dir": self.registry.rtk_log_dir().display().to_string(),
                })
                .to_string())
            }
            "export" => {
                let stream = stream_kind(&params)?;
                let limit = parse_limit(&params, DEFAULT_EXPORT_LIMIT, MAX_EXPORT_LIMIT);
                let source = log_path(&self.registry, stream);
                let output_path = export_path(&self.registry.rtk_log_dir(), &params, stream)?;
                let written = export_rows(&source, &output_path, stream, limit)?;
                Ok(json!({
                    "status": "ok",
                    "stream": stream.as_str(),
                    "limit": limit,
                    "rows": written,
                    "source": source.display().to_string(),
                    "output": output_path.display().to_string(),
                })
                .to_string())
            }
            "help" => Ok(json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "on", "off", "toggle", "once", "path", "tail", "verify", "export"],
                "streams": ["filtered", "raw"],
                "notes": [
                    "tail returns bounded redacted previews",
                    "export writes selected JSONL rows under the RTK log directory and returns the path",
                    "this tool never dispatches another tool call"
                ],
            })
            .to_string()),
            _ => Err(ToolError::InvalidParams(format!(
                "unknown action '{action}'; expected status|on|off|toggle|once|path|tail|verify|export|help"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "on", "off", "toggle", "once", "path", "tail", "verify", "export", "help"],
                "description": "RTK raw trace action. Defaults to status."
            }),
        );
        props.insert(
            "stream".into(),
            json!({
                "type": "string",
                "enum": ["filtered", "raw"],
                "description": "Which RTK JSONL stream to inspect for tail/export. Defaults to filtered."
            }),
        );
        props.insert(
            "limit".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": MAX_EXPORT_LIMIT,
                "description": "Maximum rows for tail/export; tail clamps to 200 and export clamps to 1000."
            }),
        );
        props.insert(
            "output_path".into(),
            json!({
                "type": "string",
                "description": "Optional export path. Relative paths are placed under RTK exports; absolute paths must stay under the RTK log directory."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Inspect and control RTK raw-mode dual logs without dispatching target tools.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamKind {
    Raw,
    Filtered,
}

impl StreamKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Filtered => "filtered",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Raw => "raw.jsonl",
            Self::Filtered => "filtered.jsonl",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct VerifySummary {
    exists: bool,
    entries: usize,
    parse_errors: usize,
    bytes: u64,
}

impl VerifySummary {
    fn to_json(self) -> Value {
        json!({
            "exists": self.exists,
            "entries": self.entries,
            "parse_errors": self.parse_errors,
            "bytes": self.bytes,
        })
    }
}

fn status_payload(registry: &ToolRegistry) -> Value {
    let state = registry.raw_mode_state();
    let log_dir = registry.rtk_log_dir();
    json!({
        "status": "ok",
        "raw_mode": state.enabled,
        "raw_once_pending": state.once_pending,
        "scope": "current_tool_registry",
        "rtk_log_dir": log_dir.display().to_string(),
        "raw_log": log_dir.join(StreamKind::Raw.file_name()).display().to_string(),
        "filtered_log": log_dir.join(StreamKind::Filtered.file_name()).display().to_string(),
    })
}

fn stream_kind(params: &Value) -> Result<StreamKind, ToolError> {
    match params
        .get("stream")
        .and_then(Value::as_str)
        .unwrap_or("filtered")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "raw" => Ok(StreamKind::Raw),
        "filtered" | "filter" => Ok(StreamKind::Filtered),
        other => Err(ToolError::InvalidParams(format!(
            "unknown stream '{other}'; expected raw|filtered"
        ))),
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

fn log_path(registry: &ToolRegistry, stream: StreamKind) -> PathBuf {
    registry.rtk_log_dir().join(stream.file_name())
}

fn tail_log_rows(path: &Path, limit: usize) -> Result<Vec<Value>, ToolError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", path.display())))?;
    let mut rows: Vec<Value> = raw
        .lines()
        .rev()
        .take(limit)
        .enumerate()
        .map(|(idx, line)| summarize_log_line(line, idx))
        .collect();
    rows.reverse();
    Ok(rows)
}

fn summarize_log_line(line: &str, reverse_index: usize) -> Value {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => {
            let output = value
                .get("output")
                .and_then(Value::as_str)
                .map(redact_and_truncate);
            json!({
                "valid_json": true,
                "ts": value.get("ts").and_then(Value::as_str),
                "tool": value.get("tool").and_then(Value::as_str),
                "command": value.get("command").and_then(Value::as_str).map(redact_and_truncate),
                "bypassed": value.get("bypassed").and_then(Value::as_bool),
                "rewrite_applied": value.get("rewrite_applied").and_then(Value::as_bool),
                "raw_len": value.get("raw_len").and_then(Value::as_u64),
                "filtered_len": value.get("filtered_len").and_then(Value::as_u64),
                "output_preview": output,
            })
        }
        Err(err) => json!({
            "valid_json": false,
            "reverse_index": reverse_index,
            "error": err.to_string(),
            "preview": redact_and_truncate(line),
        }),
    }
}

fn verify_log(path: &Path) -> Result<VerifySummary, ToolError> {
    if !path.exists() {
        return Ok(VerifySummary {
            exists: false,
            entries: 0,
            parse_errors: 0,
            bytes: 0,
        });
    }
    let metadata = std::fs::metadata(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("stat {}: {e}", path.display())))?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", path.display())))?;
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        entries = entries.saturating_add(1);
        if serde_json::from_str::<Value>(line).is_err() {
            parse_errors = parse_errors.saturating_add(1);
        }
    }
    Ok(VerifySummary {
        exists: true,
        entries,
        parse_errors,
        bytes: metadata.len(),
    })
}

fn export_path(log_dir: &Path, params: &Value, stream: StreamKind) -> Result<PathBuf, ToolError> {
    let exports_dir = log_dir.join("exports");
    let default = exports_dir.join(format!("{}-tail.json", stream.as_str()));
    let Some(raw) = params
        .get("output_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return Ok(default);
    };

    let candidate = PathBuf::from(raw);
    if path_has_escape_components(&candidate) {
        return Err(ToolError::InvalidParams(
            "output_path must not contain parent directory components".into(),
        ));
    }
    if candidate.is_absolute() {
        if candidate.starts_with(log_dir) {
            return Ok(candidate);
        }
        return Err(ToolError::InvalidParams(format!(
            "absolute output_path must stay under RTK log dir {}",
            log_dir.display()
        )));
    }
    if candidate
        .components()
        .any(|c| matches!(c, Component::RootDir | Component::Prefix(_)))
    {
        return Err(ToolError::InvalidParams(
            "relative output_path must not escape RTK exports dir".into(),
        ));
    }
    Ok(exports_dir.join(candidate))
}

fn path_has_escape_components(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn export_rows(
    source: &Path,
    output_path: &Path,
    stream: StreamKind,
    limit: usize,
) -> Result<usize, ToolError> {
    let raw = if source.exists() {
        std::fs::read_to_string(source)
            .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", source.display())))?
    } else {
        String::new()
    };
    let rows: Vec<Value> = raw
        .lines()
        .rev()
        .take(limit)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ToolError::ExecutionFailed(format!("create export dir {}: {e}", parent.display()))
        })?;
    }
    let payload = json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "stream": stream.as_str(),
        "source": source.display().to_string(),
        "rows": rows,
    });
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".into()),
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

    fn handler_with_temp_home() -> (tempfile::TempDir, RawTraceControlHandler) {
        let dir = tempfile::tempdir().expect("temp rtk dir");
        let _guard = ENV_LOCK.lock().expect("env lock");
        let _env = EnvGuard::set("HERMES_RTK_LOG_DIR", dir.path().to_string_lossy().as_ref());
        let registry = Arc::new(ToolRegistry::new());
        drop(_env);
        drop(_guard);
        (dir, RawTraceControlHandler::new(registry))
    }

    #[tokio::test]
    async fn status_and_mode_actions_mutate_registry_state() {
        let (_dir, handler) = handler_with_temp_home();

        let on: Value =
            serde_json::from_str(&handler.execute(json!({"action":"on"})).await.unwrap())
                .expect("json");
        assert_eq!(on["raw_mode"], true);
        assert_eq!(on["raw_once_pending"], false);

        let once: Value =
            serde_json::from_str(&handler.execute(json!({"action":"once"})).await.unwrap())
                .expect("json");
        assert_eq!(once["raw_once_pending"], true);

        let off: Value =
            serde_json::from_str(&handler.execute(json!({"action":"off"})).await.unwrap())
                .expect("json");
        assert_eq!(off["raw_mode"], false);
        assert_eq!(off["raw_once_pending"], false);
    }

    #[tokio::test]
    async fn tail_verify_and_export_are_bounded_and_redacted() {
        let (dir, handler) = handler_with_temp_home();
        std::fs::create_dir_all(dir.path()).expect("mkdir");
        let filtered = dir.path().join("filtered.jsonl");
        std::fs::write(
            &filtered,
            [
                json!({"ts":"t1","tool":"terminal","command":"echo ok","raw_len":2,"filtered_len":2,"output":"ok"}).to_string(),
                json!({"ts":"t2","tool":"terminal","command":"echo token=supersecret123","raw_len":32,"filtered_len":9,"output":"token=supersecret123"}).to_string(),
                "{not-json".to_string(),
            ]
            .join("\n"),
        )
        .expect("write filtered log");

        let tail: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"tail","stream":"filtered","limit": 2}))
                .await
                .unwrap(),
        )
        .expect("tail json");
        assert_eq!(tail["rows"].as_array().expect("rows").len(), 2);
        assert!(!tail.to_string().contains("supersecret123"));
        assert!(tail.to_string().contains("<redacted>"));

        let verify: Value =
            serde_json::from_str(&handler.execute(json!({"action":"verify"})).await.unwrap())
                .expect("verify json");
        assert_eq!(verify["status"], "fail");
        assert_eq!(verify["filtered"]["entries"], 3);
        assert_eq!(verify["filtered"]["parse_errors"], 1);

        let export: Value = serde_json::from_str(
            &handler
                .execute(json!({"action":"export","stream":"filtered","limit":2,"output_path":"sample.json"}))
                .await
                .unwrap(),
        )
        .expect("export json");
        let output = PathBuf::from(export["output"].as_str().expect("output path"));
        assert!(output.exists());
        assert!(output.starts_with(dir.path()));
        assert_eq!(export["rows"], 1);
    }

    #[tokio::test]
    async fn export_rejects_absolute_paths_outside_rtk_dir() {
        let (_dir, handler) = handler_with_temp_home();
        let err = handler
            .execute(json!({
                "action": "export",
                "output_path": "/tmp/hermes-raw-trace-export.json"
            }))
            .await
            .expect_err("absolute escape should fail");
        match err {
            ToolError::InvalidParams(message) => {
                assert!(message.contains("RTK log dir"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn export_rejects_parent_components() {
        let (dir, handler) = handler_with_temp_home();
        let inside_escape = dir.path().join("../escape.json");
        for output_path in [
            "../escape.json".to_string(),
            inside_escape.display().to_string(),
        ] {
            let err = handler
                .execute(json!({
                    "action": "export",
                    "output_path": output_path
                }))
                .await
                .expect_err("parent path should fail");
            match err {
                ToolError::InvalidParams(message) => {
                    assert!(message.contains("parent directory"), "{message}");
                }
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }
}
