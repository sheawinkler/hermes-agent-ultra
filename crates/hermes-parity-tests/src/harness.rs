//! Load JSON fixtures and assert Rust outputs match `expected` (golden values
//! aligned with Python `research/hermes-agent`).

use std::path::Path;

use hermes_core::tool_call_parser::{format_tool_calls, parse_tool_calls, separate_text_and_calls};
use hermes_core::types::ToolCall;
use hermes_intelligence::anthropic_adapter::{
    common_betas_for_base_url, default_anthropic_beta_list, fast_mode_request_beta_list,
    is_oauth_token, normalize_model_name, sanitize_tool_id,
};
use hermes_intelligence::usage_pricing::resolve_billing_route;
use hermes_intelligence::{
    estimate_tokens_rough, get_model_context_length, infer_provider_from_url, supports_tools,
    supports_vision, ErrorCategory, ErrorClassifier, RetryStrategy,
};
use hermes_skills::{
    determine_verdict, resolve_trust_level, scan_content, should_allow_install, Finding,
    InstallDecision, ScanResult,
};
use hermes_config::resolve_agent_path;
use hermes_tools::approval::{check_approval, ApprovalDecision};
use hermes_tools::code_execution_env::scrub_child_env;
use hermes_tools::code_execution_stubs::{generate_hermes_tools_module, RpcTransport};
use hermes_tools::extract_media;
use hermes_tools::v4a_patch::{parse_v4a_patch, OperationType};
use std::collections::BTreeMap;
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
    /// When `true`, run only on Windows hosts (skipped elsewhere).
    #[serde(default)]
    pub windows_only: bool,
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
        #[cfg(not(windows))]
        if case.windows_only {
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
/// Match Python `tools/checkpoint_manager._validate_commit_hash`.
pub fn checkpoint_validate_commit_hash(commit_hash: &str) -> Option<String> {
    if commit_hash.is_empty() || commit_hash.trim().is_empty() {
        return Some("Empty commit hash".to_string());
    }
    if commit_hash.starts_with('-') {
        return Some(format!(
            "Invalid commit hash (must not start with '-'): {}",
            checkpoint_python_repr(commit_hash)
        ));
    }
    let valid_len = (4..=64).contains(&commit_hash.len());
    let valid_hex = commit_hash.chars().all(|c| c.is_ascii_hexdigit());
    if !valid_len || !valid_hex {
        return Some(format!(
            "Invalid commit hash (expected 4-64 hex characters): {}",
            checkpoint_python_repr(commit_hash)
        ));
    }
    None
}

fn checkpoint_python_repr(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointShortstat {
    pub files_changed: u64,
    pub insertions: u64,
    pub deletions: u64,
}

/// Match Python `CheckpointManager._parse_shortstat` for git shortstat lines.
pub fn checkpoint_parse_shortstat(stat_line: &str) -> CheckpointShortstat {
    CheckpointShortstat {
        files_changed: extract_shortstat_count(stat_line, "file"),
        insertions: extract_shortstat_count(stat_line, "insertion"),
        deletions: extract_shortstat_count(stat_line, "deletion"),
    }
}

fn extract_shortstat_count(stat_line: &str, marker: &str) -> u64 {
    let Some(marker_idx) = stat_line.find(marker) else {
        return 0;
    };
    let prefix = stat_line[..marker_idx].trim_end();
    let digits_start = prefix
        .rfind(|c: char| !c.is_ascii_digit())
        .map(|idx| idx + 1)
        .unwrap_or(0);
    prefix[digits_start..].parse().unwrap_or(0)
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
        "parse_tool_calls" => {
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.content".to_string())?;
            let mut calls = parse_tool_calls(content).map_err(|e| e.to_string())?;
            for call in &mut calls {
                call.id.clear();
            }
            serde_json::to_value(calls).map_err(|e| e.to_string())
        }
        "separate_text_and_calls" => {
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.content".to_string())?;
            let (text, mut calls) = separate_text_and_calls(content);
            for call in &mut calls {
                call.id.clear();
            }
            Ok(json!({"text": text, "calls": calls}))
        }
        "checkpoint_shadow_dir_id" => {
            let abs = input
                .get("abs_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.abs_path".to_string())?;
            Ok(Value::String(checkpoint_shadow_dir_id(abs)))
        }

        // -- model_metadata ops --
        "get_model_context_length" => {
            let model = input
                .get("model")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.model".to_string())?;
            Ok(json!(get_model_context_length(model)))
        }
        "supports_vision" => {
            let model = input
                .get("model")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.model".to_string())?;
            Ok(json!(supports_vision(model)))
        }
        "supports_tools" => {
            let model = input
                .get("model")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.model".to_string())?;
            Ok(json!(supports_tools(model)))
        }
        "estimate_tokens_rough" => {
            let text = input
                .get("text")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.text".to_string())?;
            Ok(json!(estimate_tokens_rough(text)))
        }
        "infer_provider_from_url" => {
            let base_url = input
                .get("base_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.base_url".to_string())?;
            match infer_provider_from_url(base_url) {
                Some(p) => Ok(json!(p)),
                None => Ok(Value::Null),
            }
        }

        // -- usage_pricing ops --
        "resolve_billing_route" => {
            let model_name = input
                .get("model_name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.model_name".to_string())?;
            let provider = input.get("provider").and_then(|v| v.as_str());
            let base_url = input.get("base_url").and_then(|v| v.as_str());
            let route = resolve_billing_route(model_name, provider, base_url);
            Ok(serde_json::to_value(route).map_err(|e| e.to_string())?)
        }

        // -- approval ops --
        "check_approval" => {
            let command = input
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.command".to_string())?;
            let decision = check_approval(command);
            let label = match decision {
                ApprovalDecision::Approved => "Approved",
                ApprovalDecision::Denied => "Denied",
                ApprovalDecision::RequiresConfirmation => "RequiresConfirmation",
            };
            Ok(json!(label))
        }

        // -- error_classifier ops --
        "classify_error" => {
            let error_type = input
                .get("error_type")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.error_type".to_string())?;
            let message = input.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let retry_after = input.get("retry_after_secs").and_then(|v| v.as_u64());

            let error = match error_type {
                "RateLimited" => hermes_core::AgentError::RateLimited {
                    retry_after_secs: retry_after,
                },
                "AuthFailed" => hermes_core::AgentError::AuthFailed(message.to_string()),
                "ContextTooLong" => hermes_core::AgentError::ContextTooLong,
                "Timeout" => hermes_core::AgentError::Timeout(message.to_string()),
                "LlmApi" => hermes_core::AgentError::LlmApi(message.to_string()),
                "Gateway" => hermes_core::AgentError::Gateway(message.to_string()),
                "Io" => hermes_core::AgentError::Io(message.to_string()),
                other => return Err(format!("unknown error_type: {}", other)),
            };

            let classifier = ErrorClassifier::new();
            let category = classifier.classify(&error);
            let strategy = classifier.recommend_strategy(&category);

            let cat_label = match &category {
                ErrorCategory::RateLimit { .. } => "RateLimit",
                ErrorCategory::AuthFailed => "AuthFailed",
                ErrorCategory::ContextTooLong => "ContextTooLong",
                ErrorCategory::ServerError { .. } => "ServerError",
                ErrorCategory::NetworkError => "NetworkError",
                ErrorCategory::InvalidRequest => "InvalidRequest",
                ErrorCategory::ModelOverloaded => "ModelOverloaded",
                ErrorCategory::Timeout => "Timeout",
                ErrorCategory::Unknown => "Unknown",
            };
            let strat_label = match &strategy {
                RetryStrategy::RetryWithBackoff { .. } => "RetryWithBackoff",
                RetryStrategy::RetryOnce => "RetryOnce",
                RetryStrategy::NoRetry => "NoRetry",
                RetryStrategy::UseFallbackModel => "UseFallbackModel",
            };

            Ok(json!({
                "category": cat_label,
                "strategy": strat_label,
            }))
        }

        // -- v4a_patch ops --
        "parse_v4a_patch" => {
            let patch = input
                .get("patch")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.patch".to_string())?;
            let (ops, _err) = parse_v4a_patch(patch);
            let ops_json: Vec<Value> = ops
                .iter()
                .map(|op| {
                    json!({
                        "operation": match op.operation {
                            OperationType::Add => "add",
                            OperationType::Update => "update",
                            OperationType::Delete => "delete",
                            OperationType::Move => "move",
                        },
                        "file_path": op.file_path,
                        "new_path": op.new_path,
                        "hunks_count": op.hunks.len(),
                    })
                })
                .collect();
            Ok(json!({
                "ops_count": ops.len(),
                "ops": ops_json,
            }))
        }

        // -- skills_guard ops --
        "resolve_trust_level" => {
            let source = input
                .get("source")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.source".to_string())?;
            Ok(json!(resolve_trust_level(source)))
        }
        "determine_verdict" => {
            let findings: Vec<Finding> = input
                .get("findings")
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()
                .map_err(|e| e.to_string())?
                .unwrap_or_default();
            Ok(json!(determine_verdict(&findings)))
        }
        "should_allow_install" => {
            let result: ScanResult = serde_json::from_value(
                input
                    .get("result")
                    .cloned()
                    .ok_or_else(|| "missing input.result".to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let force = input
                .get("force")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let (decision, reason) = should_allow_install(&result, force);
            let allowed = match decision {
                InstallDecision::Allowed => Value::Bool(true),
                InstallDecision::Blocked => Value::Bool(false),
                InstallDecision::NeedsConfirmation => Value::Null,
            };
            Ok(json!({ "allowed": allowed, "reason": reason }))
        }
        "generate_hermes_tools_module" => {
            let enabled_tools: Vec<String> = input
                .get("enabled_tools")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .ok_or_else(|| "missing input.enabled_tools".to_string())?;
            let transport = input
                .get("transport")
                .and_then(|v| v.as_str())
                .unwrap_or("uds");
            let rpc_transport = match transport {
                "uds" | "tcp" => RpcTransport::Uds,
                other => {
                    return Err(format!(
                        "unsupported transport {other:?} (only uds/tcp for local PTC)"
                    ));
                }
            };
            Ok(Value::String(generate_hermes_tools_module(
                &enabled_tools,
                rpc_transport,
            )))
        }
        "scrub_child_env" => {
            let env_obj = input
                .get("env")
                .and_then(|v| v.as_object())
                .ok_or_else(|| "missing input.env object".to_string())?;
            let mut source = BTreeMap::new();
            for (k, v) in env_obj {
                let val = v
                    .as_str()
                    .ok_or_else(|| format!("env.{k} must be string"))?
                    .to_string();
                source.insert(k.clone(), val);
            }
            let passthrough: Vec<String> = input
                .get("passthrough")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_default();
            let is_windows = input
                .get("is_windows")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let out = scrub_child_env(&source, |k| passthrough.iter().any(|p| p == k), is_windows);
            Ok(serde_json::to_value(out).map_err(|e| e.to_string())?)
        }

        "scan_content_pattern_ids" => {
            let rel_path = input
                .get("rel_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.rel_path".to_string())?;
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.content".to_string())?;
            let mut ids: Vec<String> = scan_content(rel_path, content)
                .into_iter()
                .map(|f| f.pattern_id)
                .collect();
            ids.sort();
            ids.dedup();
            Ok(json!(ids))
        }

        "resolve_agent_path" => {
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.path".to_string())?;
            if let Some(home) = input.get("hermes_home").and_then(|v| v.as_str()) {
                unsafe {
                    std::env::set_var("HERMES_HOME", home);
                }
            }
            let resolved = resolve_agent_path(path);
            let normalized = resolved
                .to_string_lossy()
                .replace('\\', "/");
            Ok(Value::String(normalized))
        }

        "extract_media" => {
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.content".to_string())?;
            let (media, cleaned) = extract_media(content);
            let media_json: Vec<Value> = media
                .into_iter()
                .map(|(path, is_voice)| json!([path, is_voice]))
                .collect();
            Ok(json!({
                "media": media_json,
                "cleaned": cleaned,
            }))
        }


        "checkpoint_validate_commit_hash" => {
            let commit_hash = input
                .get("commit_hash")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.commit_hash".to_string())?;
            match checkpoint_validate_commit_hash(commit_hash) {
                None => Ok(Value::Null),
                Some(err) => Ok(Value::String(err)),
            }
        }
        "checkpoint_parse_shortstat" => {
            let stat_line = input
                .get("stat_line")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing input.stat_line".to_string())?;
            serde_json::to_value(checkpoint_parse_shortstat(stat_line)).map_err(|e| e.to_string())
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
    fn parity_model_metadata_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("model_metadata"))
            .expect("model_metadata fixtures");
    }

    #[test]
    fn parity_usage_pricing_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("usage_pricing")).expect("usage_pricing fixtures");
    }

    #[test]
    fn parity_approval_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("approval")).expect("approval fixtures");
    }

    #[test]
    fn parity_v4a_patch_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("v4a_patch")).expect("v4a_patch fixtures");
    }

    #[test]
    fn parity_error_classifier_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("error_classifier"))
            .expect("error_classifier fixtures");
    }

    #[test]
    fn parity_skills_guard_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("skills_guard")).expect("skills_guard fixtures");
    }

    #[test]
    fn parity_code_execution_env_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("code_execution_env"))
            .expect("code_execution_env fixtures");
    }

    #[test]
    fn parity_code_execution_stubs_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("code_execution_stubs"))
            .expect("code_execution_stubs fixtures");
    }

    #[test]
    fn parity_send_message_fixtures() {
        run_fixtures_in_dir(&fixtures_dir().join("send_message")).expect("send_message fixtures");
    }

    #[test]
    fn parity_all_active_fixtures_recursive() {
        run_all_active_fixtures(&fixtures_dir()).expect("all active fixtures");
    }

    #[test]

    #[test]
    fn checkpoint_commit_hash_validation_matches_python_samples() {
        assert_eq!(
            checkpoint_validate_commit_hash("abc"),
            Some("Invalid commit hash (expected 4-64 hex characters): 'abc'".to_string())
        );
        assert_eq!(checkpoint_validate_commit_hash("abcd"), None);
        assert_eq!(
            checkpoint_validate_commit_hash("-abc123"),
            Some("Invalid commit hash (must not start with '-'): '-abc123'".to_string())
        );
    }

    #[test]
    fn checkpoint_shortstat_parser_matches_python_samples() {
        assert_eq!(
            checkpoint_parse_shortstat("1 file changed, 2 insertions(+), 3 deletions(-)"),
            CheckpointShortstat {
                files_changed: 1,
                insertions: 2,
                deletions: 3,
            }
        );
        assert_eq!(
            checkpoint_parse_shortstat("2 files changed, 10 insertions(+)"),
            CheckpointShortstat {
                files_changed: 2,
                insertions: 10,
                deletions: 0,
            }
        );
    }

    fn checkpoint_shadow_dir_id_matches_python_sample() {
        assert_eq!(
            checkpoint_shadow_dir_id("/workspace/demo"),
            "4de1d2f8b60db00a"
        );
    }
}
