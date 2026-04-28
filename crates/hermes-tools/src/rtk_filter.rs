//! RTK-style tool I/O filtering.
//!
//! This module provides:
//! - lightweight command input normalization (safe rewrite)
//! - token-reduction output filtering for verbose tool outputs
//! - dual logging (raw + filtered) for operator auditability

use std::collections::BTreeSet;
use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::Utc;
use regex::Regex;
use serde::Serialize;
use serde_json::Value;
use tracing::warn;

const COMMAND_KEYS: &[&str] = &[
    "command",
    "cmd",
    "shell_command",
    "bash_command",
    "terminal_command",
];

const DEFAULT_HEAD_LINES: usize = 120;
const DEFAULT_TAIL_LINES: usize = 80;
const DEFAULT_REPEAT_KEEP: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawModeState {
    pub enabled: bool,
    pub once_pending: bool,
}

#[derive(Debug, Clone)]
pub struct RtkFilterEngine {
    log_dir: PathBuf,
    head_lines: usize,
    tail_lines: usize,
    repeat_keep: usize,
}

#[derive(Debug, Serialize)]
struct RtkLogRecord<'a> {
    ts: String,
    tool: &'a str,
    command: Option<&'a str>,
    bypassed: bool,
    rewrite_applied: bool,
    raw_len: usize,
    filtered_len: usize,
    raw_output: &'a str,
    filtered_output: &'a str,
    params: &'a Value,
}

impl Default for RtkFilterEngine {
    fn default() -> Self {
        Self::from_env()
    }
}

impl RtkFilterEngine {
    pub fn from_env() -> Self {
        let log_dir = std::env::var("HERMES_RTK_LOG_DIR")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| hermes_config::hermes_home().join("logs/rtk"));
        let head_lines = std::env::var("HERMES_RTK_HEAD_LINES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_HEAD_LINES);
        let tail_lines = std::env::var("HERMES_RTK_TAIL_LINES")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_TAIL_LINES);
        let repeat_keep = std::env::var("HERMES_RTK_REPEAT_KEEP")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(DEFAULT_REPEAT_KEEP);
        Self {
            log_dir,
            head_lines,
            tail_lines,
            repeat_keep: repeat_keep.max(1),
        }
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn rewrite_params(&self, tool_name: &str, params: &Value, bypassed: bool) -> (Value, bool) {
        if bypassed || !is_command_oriented_tool(tool_name, params) {
            return (params.clone(), false);
        }
        let mut rewritten = params.clone();
        let mut changed = false;
        if let Some(command) = command_field_mut(&mut rewritten) {
            let next = normalize_command(command);
            if next != *command {
                *command = next;
                changed = true;
            }
        }
        (rewritten, changed)
    }

    pub fn filter_and_log(
        &self,
        tool_name: &str,
        params: &Value,
        raw_output: &str,
        bypassed: bool,
        rewrite_applied: bool,
    ) -> String {
        let filtered = if bypassed {
            raw_output.to_string()
        } else {
            self.filter_output(tool_name, params, raw_output)
        };
        self.log_dual(
            tool_name,
            params,
            raw_output,
            &filtered,
            bypassed,
            rewrite_applied,
        );
        filtered
    }

    fn filter_output(&self, tool_name: &str, params: &Value, raw_output: &str) -> String {
        let mut text = strip_ansi(raw_output);
        if text.is_empty() {
            return text;
        }
        text = text.replace("\r\n", "\n").replace('\r', "\n");
        text = redact_secrets(&text);

        let mut lines: Vec<String> = text.lines().map(|line| line.to_string()).collect();
        lines = collapse_repeated_lines(lines, self.repeat_keep);
        lines = collapse_blank_runs(lines);

        if is_command_oriented_tool(tool_name, params) {
            lines = drop_noise_lines(lines);
            lines = summarize_long_output(lines, self.head_lines, self.tail_lines);
        } else if lines.len() > (self.head_lines + self.tail_lines + 20) {
            lines = summarize_long_output(lines, self.head_lines, self.tail_lines);
        }

        let out = lines.join("\n").trim().to_string();
        if out.is_empty() {
            raw_output.trim().to_string()
        } else {
            out
        }
    }

    fn log_dual(
        &self,
        tool_name: &str,
        params: &Value,
        raw_output: &str,
        filtered_output: &str,
        bypassed: bool,
        rewrite_applied: bool,
    ) {
        if create_dir_all(&self.log_dir).is_err() {
            return;
        }
        let command = extract_command(params);
        let record = RtkLogRecord {
            ts: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            tool: tool_name,
            command,
            bypassed,
            rewrite_applied,
            raw_len: raw_output.len(),
            filtered_len: filtered_output.len(),
            raw_output,
            filtered_output,
            params,
        };

        let raw_path = self.log_dir.join("raw.jsonl");
        let filtered_path = self.log_dir.join("filtered.jsonl");

        let raw_line = serde_json::json!({
            "ts": record.ts,
            "tool": record.tool,
            "command": record.command,
            "bypassed": record.bypassed,
            "rewrite_applied": record.rewrite_applied,
            "raw_len": record.raw_len,
            "output": record.raw_output,
            "params": record.params,
        })
        .to_string();
        let filtered_line = serde_json::json!({
            "ts": record.ts,
            "tool": record.tool,
            "command": record.command,
            "bypassed": record.bypassed,
            "rewrite_applied": record.rewrite_applied,
            "raw_len": record.raw_len,
            "filtered_len": record.filtered_len,
            "output": record.filtered_output,
            "params": record.params,
        })
        .to_string();

        if let Err(e) = append_jsonl(&raw_path, &raw_line) {
            warn!("RTK raw log append failed: {}", e);
        }
        if let Err(e) = append_jsonl(&filtered_path, &filtered_line) {
            warn!("RTK filtered log append failed: {}", e);
        }
    }
}

fn append_jsonl(path: &Path, line: &str) -> Result<(), std::io::Error> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

fn extract_command(params: &Value) -> Option<&str> {
    let obj = params.as_object()?;
    for key in COMMAND_KEYS {
        if let Some(v) = obj.get(*key).and_then(Value::as_str) {
            return Some(v);
        }
    }
    if let Some(args) = obj.get("arguments").and_then(Value::as_object) {
        for key in COMMAND_KEYS {
            if let Some(v) = args.get(*key).and_then(Value::as_str) {
                return Some(v);
            }
        }
    }
    None
}

fn command_field_mut(params: &mut Value) -> Option<&mut String> {
    let obj = params.as_object_mut()?;
    let top_key = COMMAND_KEYS
        .iter()
        .copied()
        .find(|key| matches!(obj.get(*key), Some(Value::String(_))));
    if let Some(key) = top_key {
        return match obj.get_mut(key) {
            Some(Value::String(s)) => Some(s),
            _ => None,
        };
    }
    if let Some(args) = obj.get_mut("arguments").and_then(Value::as_object_mut) {
        let nested_key = COMMAND_KEYS
            .iter()
            .copied()
            .find(|key| matches!(args.get(*key), Some(Value::String(_))));
        if let Some(key) = nested_key {
            return match args.get_mut(key) {
                Some(Value::String(s)) => Some(s),
                _ => None,
            };
        }
    }
    None
}

fn normalize_command(command: &str) -> String {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Keep semantics unchanged: only collapse embedded newlines/tabs into spaces
    // and trim shell-unsafe leading/trailing whitespace.
    collapse_newline_whitespace(trimmed)
}

fn collapse_newline_whitespace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_space = false;
    for ch in input.chars() {
        let is_space = matches!(ch, '\n' | '\r' | '\t');
        if is_space {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
            continue;
        }
        prev_space = ch == ' ';
        out.push(ch);
    }
    out.trim().to_string()
}

fn is_command_oriented_tool(tool_name: &str, params: &Value) -> bool {
    let lower = tool_name.to_ascii_lowercase();
    if lower.contains("terminal")
        || lower.contains("process")
        || lower.contains("execute")
        || lower.contains("shell")
        || lower.contains("bash")
    {
        return true;
    }
    extract_command(params).is_some()
}

fn ansi_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\x1B\[[0-9;?]*[ -/]*[@-~]").expect("ansi regex"))
}

fn strip_ansi(s: &str) -> String {
    ansi_regex().replace_all(s, "").to_string()
}

fn noise_regexes() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        vec![
            Regex::new(r"^\s*Compiling\s+.+$").expect("regex"),
            Regex::new(r"^\s*Downloading\s+.+$").expect("regex"),
            Regex::new(r"^\s*Downloaded\s+.+$").expect("regex"),
            Regex::new(r"^\s*Installing\s+.+$").expect("regex"),
            Regex::new(r"^\s*Finished\s+.+$").expect("regex"),
            Regex::new(r"^\s*Fresh\s+.+$").expect("regex"),
            Regex::new(r"^\s*test\s+.+\s\.\.\.\sok$").expect("regex"),
        ]
    })
}

fn signal_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(error|failed|failure|panic|panicked|warn|warning|traceback)\b")
            .expect("signal regex")
    })
}

fn drop_noise_lines(lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .filter(|line| {
            if signal_regex().is_match(line) {
                return true;
            }
            !noise_regexes().iter().any(|re| re.is_match(line))
        })
        .collect()
}

fn collapse_repeated_lines(lines: Vec<String>, keep_each: usize) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    let mut out = Vec::with_capacity(lines.len());
    let mut last = String::new();
    let mut run = 0usize;
    let mut first = true;

    for line in lines {
        if first {
            out.push(line.clone());
            last = line;
            run = 1;
            first = false;
            continue;
        }
        if line == last {
            run += 1;
            if run <= keep_each {
                out.push(line.clone());
            }
        } else {
            if run > keep_each {
                out.push(format!("[… {} repeated lines omitted …]", run - keep_each));
            }
            out.push(line.clone());
            last = line;
            run = 1;
        }
    }
    if run > keep_each {
        out.push(format!("[… {} repeated lines omitted …]", run - keep_each));
    }
    out
}

fn collapse_blank_runs(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let mut blank_run = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push(String::new());
            }
            continue;
        }
        blank_run = 0;
        out.push(line);
    }
    out
}

fn summarize_long_output(lines: Vec<String>, head: usize, tail: usize) -> Vec<String> {
    let len = lines.len();
    if len <= head + tail + 20 {
        return lines;
    }

    let mut keep = BTreeSet::new();
    for idx in 0..head.min(len) {
        keep.insert(idx);
    }
    for idx in len.saturating_sub(tail)..len {
        keep.insert(idx);
    }
    for (idx, line) in lines.iter().enumerate() {
        if signal_regex().is_match(line) {
            keep.insert(idx);
        }
    }

    let mut out = Vec::new();
    let mut prev: Option<usize> = None;
    for idx in keep {
        if let Some(p) = prev {
            if idx > p + 1 {
                out.push(format!("[… {} lines omitted …]", idx - p - 1));
            }
        }
        out.push(lines[idx].clone());
        prev = Some(idx);
    }
    out
}

fn redact_patterns() -> &'static [Regex] {
    static RES: OnceLock<Vec<Regex>> = OnceLock::new();
    RES.get_or_init(|| {
        vec![
            Regex::new(
                r#"(?i)\b(api[_-]?key|token|secret|password)\b\s*[:=]\s*["']?([A-Za-z0-9_\-./]{8,})"#,
            )
            .expect("regex"),
            Regex::new(r"\bsk-[A-Za-z0-9]{20,}\b").expect("regex"),
            Regex::new(r"\bghp_[A-Za-z0-9]{20,}\b").expect("regex"),
            // Telegram-style bot token: <bot_id>:<token>
            Regex::new(r"\b[0-9]{7,12}:[A-Za-z0-9_-]{20,}\b").expect("regex"),
        ]
    })
}

fn redact_secrets(input: &str) -> String {
    let mut out = input.to_string();
    for re in redact_patterns() {
        out = re
            .replace_all(&out, |caps: &regex::Captures| {
                let key = caps.get(1).map(|m| m.as_str()).unwrap_or("token");
                format!("{key}=<redacted>")
            })
            .to_string();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn rewrite_params_normalizes_newlines_only() {
        let engine = RtkFilterEngine::default();
        let params = json!({ "command": "  echo hello\\nworld\t\t" });
        let (rewritten, changed) = engine.rewrite_params("terminal", &params, false);
        assert!(changed);
        assert_eq!(
            rewritten["command"].as_str().unwrap_or(""),
            "echo hello\\nworld"
        );
    }

    #[test]
    fn filter_preserves_signal_and_summarizes_long_output() {
        let engine = RtkFilterEngine {
            log_dir: std::env::temp_dir().join("hermes-rtk-test"),
            head_lines: 4,
            tail_lines: 3,
            repeat_keep: 2,
        };
        let mut raw = String::new();
        for i in 0..80 {
            raw.push_str(&format!("noise line {}\n", i));
        }
        raw.push_str("error: boom\n");
        for i in 80..150 {
            raw.push_str(&format!("tail line {}\n", i));
        }

        let out = engine.filter_output("terminal", &json!({ "command": "cargo test" }), &raw);
        assert!(out.contains("error: boom"));
        assert!(out.contains("omitted"));
    }

    #[test]
    fn dual_logs_are_written() {
        let dir = tempdir().expect("tempdir");
        let engine = RtkFilterEngine {
            log_dir: dir.path().to_path_buf(),
            head_lines: 4,
            tail_lines: 4,
            repeat_keep: 2,
        };
        let params = json!({"command":"ls -la"});
        let _ = engine.filter_and_log("terminal", &params, "line1\nline2", false, false);

        assert!(dir.path().join("raw.jsonl").exists());
        assert!(dir.path().join("filtered.jsonl").exists());
    }

    #[test]
    fn redact_secrets_redacts_telegram_style_token() {
        let text = "telegram=1234567890:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let redacted = redact_secrets(text);
        assert!(!redacted.contains("1234567890:"));
        assert!(redacted.contains("<redacted>"));
    }
}
