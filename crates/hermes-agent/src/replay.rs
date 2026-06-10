//! Replay recording — hash-chained event journal for session replay and debugging.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agent_config::AgentConfig;

// ---------------------------------------------------------------------------
// Route-learning state (also persisted for smart routing adaptation)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RouteLearningStats {
    pub(crate) samples: u32,
    pub(crate) success_rate: f64,
    pub(crate) avg_latency_ms: f64,
    pub(crate) consecutive_failures: u32,
    pub(crate) updated_at_unix_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct RouteLearningState {
    pub(crate) schema_version: u32,
    pub(crate) saved_at_unix_ms: i64,
    pub(crate) entries: HashMap<String, RouteLearningStats>,
}

// ---------------------------------------------------------------------------
// Replay recorder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub(crate) struct ReplayRecorder {
    pub(crate) path: Option<PathBuf>,
    pub(crate) state: Option<Arc<Mutex<ReplayState>>>,
}

#[derive(Debug, Clone)]
pub(crate) struct ReplayState {
    pub(crate) seq: u64,
    pub(crate) prev_hash: String,
    pub(crate) trace_root: String,
}

impl ReplayRecorder {
    pub(crate) fn for_session(config: &AgentConfig, session_id: &str) -> Self {
        let enabled = std::env::var("HERMES_REPLAY_ENABLED")
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false);
        if !enabled {
            return Self {
                path: None,
                state: None,
            };
        }
        let root = config
            .hermes_home
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HERMES_HOME").ok().map(PathBuf::from))
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|home| PathBuf::from(home).join(".hermes-agent-ultra"))
            })
            .unwrap_or_else(|| PathBuf::from(".hermes-agent-ultra"));
        let dir = root.join("logs").join("replay");
        if std::fs::create_dir_all(&dir).is_err() {
            return Self {
                path: None,
                state: None,
            };
        }
        let sid = if session_id.trim().is_empty() {
            format!("session-{}", Utc::now().format("%Y%m%dT%H%M%SZ"))
        } else {
            session_id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect::<String>()
        };
        let initial_prev_hash = short_sha256_hex(&format!("session:{sid}:v1"));
        let trace_root = short_sha256_hex(&format!("trace:{sid}:v1"));
        Self {
            path: Some(dir.join(format!("{sid}.jsonl"))),
            state: Some(Arc::new(Mutex::new(ReplayState {
                seq: 0,
                prev_hash: initial_prev_hash,
                trace_root,
            }))),
        }
    }

    pub(crate) fn record(&self, event: &str, payload: Value) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let mut redacted = payload;
        redact_json_value(&mut redacted);
        let canonical_payload =
            serde_json::to_string(&redacted).unwrap_or_else(|_| "{}".to_string());
        let (seq, prev_hash, event_hash, trace_id) = {
            let mut guard = state.lock().unwrap();
            guard.seq = guard.seq.saturating_add(1);
            let seq = guard.seq;
            let prev_hash = guard.prev_hash.clone();
            let event_hash =
                short_sha256_hex(&format!("{seq}|{event}|{prev_hash}|{canonical_payload}"));
            let trace_id = format!("{}-{:08x}", guard.trace_root, seq);
            guard.prev_hash = event_hash.clone();
            (seq, prev_hash, event_hash, trace_id)
        };
        let line = serde_json::json!({
            "ts": Utc::now().to_rfc3339(),
            "seq": seq,
            "trace_id": trace_id,
            "event": event,
            "prev_hash": prev_hash,
            "event_hash": event_hash,
            "payload": redacted,
        })
        .to_string();
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

// ---------------------------------------------------------------------------
// Redaction helpers
// ---------------------------------------------------------------------------

pub(crate) fn replay_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.contains("api_key")
        || k.contains("token")
        || k.contains("secret")
        || k.contains("password")
        || k.contains("authorization")
        || k.contains("cookie")
        || k.contains("session")
}

pub(crate) fn short_sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(16);
    for b in digest.iter().take(8) {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", b);
    }
    out
}

pub(crate) fn redact_sensitive_text(value: &str) -> Option<String> {
    lazy_static::lazy_static! {
        static ref SECRET_PATTERNS: Vec<regex::Regex> = vec![
            regex::Regex::new(r"(?i)bearer\\s+[A-Za-z0-9._\\-]{8,}").unwrap(),
            regex::Regex::new(r"sk-[A-Za-z0-9]{8,}").unwrap(),
            regex::Regex::new(r"gh[pousr]_[A-Za-z0-9]{12,}").unwrap(),
            regex::Regex::new(r"xox[baprs]-[A-Za-z0-9\\-]{10,}").unwrap(),
            regex::Regex::new(r"(?i)(api[_-]?key|token|secret|password)\\s*[:=]\\s*[A-Za-z0-9._\\-]{6,}").unwrap(),
        ];
    }
    let mut redacted = value.to_string();
    let mut changed = false;
    for pattern in SECRET_PATTERNS.iter() {
        let next = pattern.replace_all(&redacted, "[redacted]").to_string();
        if next != redacted {
            changed = true;
            redacted = next;
        }
    }
    if changed { Some(redacted) } else { None }
}

pub(crate) fn truncate_hook_preview(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if total <= max_chars.max(1) {
        return text.to_string();
    }
    let keep_head = max_chars.saturating_sub(96).max(64);
    let head: String = text.chars().take(keep_head).collect();
    let omitted = total.saturating_sub(keep_head);
    format!("{head}\n...[truncated {omitted} chars]...")
}

pub(crate) fn redact_json_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if replay_sensitive_key(k) {
                    *v = Value::String("[redacted]".to_string());
                } else {
                    redact_json_value(v);
                }
            }
        }
        Value::Array(arr) => {
            for v in arr {
                redact_json_value(v);
            }
        }
        Value::String(raw) => {
            if let Some(redacted) = redact_sensitive_text(raw) {
                *raw = redacted;
            }
        }
        _ => {}
    }
}
