//! Load JSON fixtures and assert Rust outputs match `expected` (golden values
//! aligned with Python `research/hermes-agent`).

use std::path::Path;

use hermes_core::tool_call_parser::format_tool_calls;
use hermes_core::types::ToolCall;
use hermes_intelligence::anthropic_adapter::{
    common_betas_for_base_url, default_anthropic_beta_list, fast_mode_request_beta_list,
    is_oauth_token, normalize_model_name, sanitize_tool_id,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// Top-level fixture file (one file may contain multiple cases).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityFixtureFile {
    /// Schema version for forward-compatible readers.
    pub schema_version: u32,
    /// Logical name, e.g. `anthropic_adapter`.
    pub fixture_group: String,
    /// Python source path for human traceability (e.g. `agent/anthropic_adapter.py`).
    pub python_module: String,
    pub cases: Vec<ParityCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParityCase {
    pub id: String,
    /// Dispatch key, e.g. `normalize_model_name`, `format_tool_calls`.
    pub op: String,
    /// Operation-specific JSON args.
    pub input: Value,
    /// Golden output (must match Rust serialization).
    pub expected: Value,
    /// When `true`, this case is skipped (scaffold / waiting for Rust port).
    #[serde(default)]
    pub skip: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ParityError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("fixture {fixture}: case {case}: {msg}")]
    Mismatch {
        fixture: String,
        case: String,
        msg: String,
    },

    #[error("unknown op {op} in case {case}")]
    UnknownOp { op: String, case: String },

    #[error("dispatch error: {0}")]
    Dispatch(String),
}

/// Load and parse a single fixture JSON file.
pub fn load_fixture_file(path: &Path) -> Result<ParityFixtureFile, ParityError> {
    let raw = std::fs::read_to_string(path)?;
    let f: ParityFixtureFile = serde_json::from_str(&raw)?;
    Ok(f)
}

/// Run every case in one file; returns `Ok(())` only if all match.
pub fn run_fixture_file(path: &Path) -> Result<(), ParityError> {
    let fixture_name = path.display().to_string();
    let file = load_fixture_file(path)?;
    if file.schema_version != 1 {
        return Err(ParityError::Dispatch(format!(
            "unsupported schema_version {} in {}",
            file.schema_version, fixture_name
        )));
    }
    for case in &file.cases {
        if case.skip {
            continue;
        }
        let actual = dispatch_case(&case.op, &case.input).map_err(ParityError::Dispatch)?;
        if actual != case.expected {
            return Err(ParityError::Mismatch {
                fixture: fixture_name,
                case: case.id.clone(),
                msg: format!("expected {:#?}, got {:#?}", case.expected, actual),
            });
        }
    }
    Ok(())
}

/// Run all `*.json` files directly under `dir` (non-recursive). Useful for tests.
pub fn run_fixtures_in_dir(dir: &Path) -> Result<(), ParityError> {
    for entry in std::fs::read_dir(dir).map_err(ParityError::Io)? {
        let entry = entry.map_err(ParityError::Io)?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) == Some("json") {
            run_fixture_file(&p)?;
        }
    }
    Ok(())
}

/// Run every `*.json` under `fixtures_root`, recursively, **excluding** any path segment
/// named `pending` (placeholders for future parity).
pub fn run_all_active_fixtures(fixtures_root: &Path) -> Result<(), ParityError> {
    let mut files = Vec::new();
    collect_json_fixtures(fixtures_root, &mut files)?;
    files.sort();
    for p in files {
        run_fixture_file(&p)?;
    }
    Ok(())
}

fn collect_json_fixtures(dir: &Path, out: &mut Vec<std::path::PathBuf>) -> Result<(), ParityError> {
    if dir.file_name().and_then(|n| n.to_str()) == Some("pending") {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(ParityError::Io)? {
        let entry = entry.map_err(ParityError::Io)?;
        let p = entry.path();
        if p.is_dir() {
            collect_json_fixtures(&p, out)?;
        } else if p.extension().and_then(|s| s.to_str()) == Some("json") {
            if p.file_name().and_then(|n| n.to_str()) == Some("registry.json") {
                continue;
            }
            out.push(p);
        }
    }
    Ok(())
}

/// First 16 hex chars of SHA-256(abs_path_str), matching Python
/// `hashlib.sha256(abs_path.encode()).hexdigest()[:16]` used for shadow repo dir names.
pub fn checkpoint_shadow_dir_id(abs_path_str: &str) -> String {
    let digest = Sha256::digest(abs_path_str.as_bytes());
    digest[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

/// Execute one logical op and return JSON [`Value`] for comparison.
pub fn dispatch_case(op: &str, input: &Value) -> Result<Value, String> {
    match op {
        "normalize_model_name" => {
            let model = input
                .get("model")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.model".to_string())?;
            let preserve = input
                .get("preserve_dots")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(Value::String(normalize_model_name(model, preserve)))
        }
        "sanitize_tool_id" => {
            let tool_id = input
                .get("tool_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.tool_id".to_string())?;
            Ok(Value::String(sanitize_tool_id(tool_id)))
        }
        "is_oauth_token" => {
            let key = input
                .get("key")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.key".to_string())?;
            Ok(Value::Bool(is_oauth_token(key)))
        }
        "common_betas_for_base_url" => {
            let base_url =
                input
                    .get("base_url")
                    .and_then(|v| if v.is_null() { None } else { v.as_str() });
            let v: Vec<Value> = common_betas_for_base_url(base_url)
                .into_iter()
                .map(|s| json!(s))
                .collect();
            Ok(Value::Array(v))
        }
        "default_anthropic_beta_list" => {
            let base_url =
                input
                    .get("base_url")
                    .and_then(|v| if v.is_null() { None } else { v.as_str() });
            let is_oauth = input
                .get("is_oauth")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "missing input.is_oauth".to_string())?;
            let v: Vec<Value> = default_anthropic_beta_list(base_url, is_oauth)
                .into_iter()
                .map(|s| json!(s))
                .collect();
            Ok(Value::Array(v))
        }
        "fast_mode_request_beta_list" => {
            let base_url =
                input
                    .get("base_url")
                    .and_then(|v| if v.is_null() { None } else { v.as_str() });
            let is_oauth = input
                .get("is_oauth")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| "missing input.is_oauth".to_string())?;
            match fast_mode_request_beta_list(base_url, is_oauth) {
                None => Ok(Value::Null),
                Some(list) => {
                    let v: Vec<Value> = list.into_iter().map(|s| json!(s)).collect();
                    Ok(Value::Array(v))
                }
            }
        }
        "format_tool_calls" => {
            let calls: Vec<ToolCall> = serde_json::from_value(
                input
                    .get("calls")
                    .cloned()
                    .ok_or_else(|| "missing input.calls".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            Ok(Value::String(format_tool_calls(&calls)))
        }
        "checkpoint_shadow_dir_id" => {
            let abs = input
                .get("abs_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.abs_path".to_string())?;
            Ok(Value::String(checkpoint_shadow_dir_id(abs)))
        }
        _ => Err(format!("unknown op: {}", op)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
    }

    #[test]
    fn parity_anthropic_adapter_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("anthropic_adapter")).expect("anthropic fixtures");
    }

    #[test]
    fn parity_hermes_core_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("hermes_core")).expect("core fixtures");
    }

    #[test]
    fn parity_all_active_fixtures_recursive() {
        run_all_active_fixtures(&fixtures_dir()).expect("all active fixtures");
    }

    #[test]
    fn checkpoint_shadow_dir_id_matches_python_sample() {
        assert_eq!(
            checkpoint_shadow_dir_id("/workspace/demo"),
            "4de1d2f8b60db00a"
        );
    }
}
