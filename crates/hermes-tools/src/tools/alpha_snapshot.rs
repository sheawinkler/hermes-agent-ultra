//! Read-only alpha objective and mission runtime snapshots.
//!
//! The CLI owns mutation-capable `/objective` and `/mission` slash commands.
//! These tools expose the same persisted runtime surface to agents as JSON
//! without bootstrapping files, refreshing reports, or writing queue state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use indexmap::IndexMap;
use serde_json::{json, Value};

const OBJECTIVE_TOOL_NAME: &str = "objective_snapshot";
const MISSION_TOOL_NAME: &str = "mission_snapshot";

const OBJECTIVE_FILES: &[(&str, &str)] = &[
    ("contract", "objective_contract.json"),
    ("profile", "objective_profile.json"),
    ("contextlattice_policy", "contextlattice_policy.json"),
    ("simulation_policy", "objective_simulation_policy.json"),
    ("ensemble_policy", "objective_ensemble_policy.json"),
    ("learning_ledger", "objective_learning_ledger.json"),
    ("dag", "objective_dag.json"),
    ("claim_verifier_policy", "claim_verifier_policy.json"),
    ("quorum_policy", "quorum_policy.json"),
    ("eval_trend", "objective_eval_trend.json"),
    ("subagents", "subagents.json"),
];

const MISSION_FILES: &[(&str, &str)] = &[
    ("loops", "loops.json"),
    ("queue", "loop_queue.jsonl"),
    ("runtime", "loop_runtime.json"),
    ("trading_config", "trading/runtime_config.json"),
    ("trading_last_report", "trading/last_report.json"),
    ("trading_drift_baseline", "trading/drift_baseline.json"),
];

#[derive(Clone, Default)]
pub struct ObjectiveSnapshotHandler;

impl ObjectiveSnapshotHandler {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Clone, Default)]
pub struct MissionSnapshotHandler;

impl MissionSnapshotHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ObjectiveSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = action_param(&params, "status");
        let include_content = params
            .get("include_content")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let tail = bounded_tail(params.get("tail"));
        let root = alpha_root_from_params(&params);

        let payload = match action.as_str() {
            "status" => objective_status_snapshot(&root, include_content, tail),
            "files" => json!({
                "status": "ok",
                "alpha_root": root.display().to_string(),
                "files": file_rows(&root, OBJECTIVE_FILES, include_content),
                "mutable": false,
            }),
            "contract" => single_file_payload(
                &root,
                "contract",
                "objective_contract.json",
                include_content,
            ),
            "policies" => objective_policies_snapshot(&root, include_content),
            "ledger" => objective_ledger_snapshot(&root, tail, include_content),
            "dag" => objective_dag_snapshot(&root, include_content),
            "eval" => objective_eval_snapshot(&root, tail, include_content),
            "help" => json!({
                "status": "ok",
                "tool": OBJECTIVE_TOOL_NAME,
                "actions": ["status", "files", "contract", "policies", "ledger", "dag", "eval", "help"],
                "notes": [
                    "read-only objective runtime snapshot",
                    "does not create alpha runtime files or mutate objective state",
                    "set include_content=true to include redacted parsed JSON payloads"
                ],
            }),
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown action '{action}'; expected status|files|contract|policies|ledger|dag|eval|help"
                )));
            }
        };

        Ok(payload.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "files", "contract", "policies", "ledger", "dag", "eval", "help"],
                "description": "Objective snapshot action. Defaults to status."
            }),
        );
        props.insert(
            "alpha_root".into(),
            json!({
                "type": "string",
                "description": "Optional alpha runtime directory. Defaults to <HERMES_HOME>/alpha."
            }),
        );
        props.insert(
            "include_content".into(),
            json!({
                "type": "boolean",
                "description": "Include redacted parsed JSON content for requested files. Defaults to false."
            }),
        );
        props.insert(
            "tail".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 50,
                "description": "Number of ledger/eval rows to include. Defaults to 5."
            }),
        );
        tool_schema(
            OBJECTIVE_TOOL_NAME,
            "Return read-only Objective OS, subagent, ContextLattice, policy, ledger, DAG, and eval runtime state.",
            JsonSchema::object(props, vec![]),
        )
    }
}

#[async_trait]
impl ToolHandler for MissionSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = action_param(&params, "status");
        let include_content = params
            .get("include_content")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let tail = bounded_tail(params.get("tail"));
        let root = alpha_root_from_params(&params);

        let payload = match action.as_str() {
            "status" => mission_status_snapshot(&root, include_content, tail),
            "files" => json!({
                "status": "ok",
                "alpha_root": root.display().to_string(),
                "files": file_rows(&root, MISSION_FILES, include_content),
                "mutable": false,
            }),
            "loops" => mission_loops_snapshot(&root, include_content),
            "queue" => mission_queue_snapshot(&root, tail, include_content),
            "runtime" => mission_runtime_snapshot(&root, include_content),
            "trading" => trading_snapshot(&root, include_content),
            "help" => json!({
                "status": "ok",
                "tool": MISSION_TOOL_NAME,
                "actions": ["status", "files", "loops", "queue", "runtime", "trading", "help"],
                "notes": [
                    "read-only mission control snapshot",
                    "does not run loop recovery, queue replay, or trading report refresh",
                    "set include_content=true to include redacted parsed JSON payloads"
                ],
            }),
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown action '{action}'; expected status|files|loops|queue|runtime|trading|help"
                )));
            }
        };

        Ok(payload.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "files", "loops", "queue", "runtime", "trading", "help"],
                "description": "Mission snapshot action. Defaults to status."
            }),
        );
        props.insert(
            "alpha_root".into(),
            json!({
                "type": "string",
                "description": "Optional alpha runtime directory. Defaults to <HERMES_HOME>/alpha."
            }),
        );
        props.insert(
            "include_content".into(),
            json!({
                "type": "boolean",
                "description": "Include redacted parsed JSON content for requested files. Defaults to false."
            }),
        );
        props.insert(
            "tail".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 50,
                "description": "Number of queue rows to include. Defaults to 5."
            }),
        );
        tool_schema(
            MISSION_TOOL_NAME,
            "Return read-only mission, loop runtime, queue, and private trading objective runtime state.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn objective_status_snapshot(root: &Path, include_content: bool, tail: usize) -> Value {
    let contract = read_json(root.join("objective_contract.json"));
    let profile = read_json(root.join("objective_profile.json"));
    let context_policy = read_json(root.join("contextlattice_policy.json"));
    let subagents = read_json(root.join("subagents.json"));
    let ledger = read_json(root.join("objective_learning_ledger.json"));
    let dag = read_json(root.join("objective_dag.json"));
    let eval = read_json(root.join("objective_eval_trend.json"));

    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "objective": contract.as_ref().map(objective_contract_summary).unwrap_or_else(missing_summary),
        "profile": profile.as_ref().map(profile_summary).unwrap_or_else(missing_summary),
        "contextlattice": context_policy.as_ref().map(context_policy_summary).unwrap_or_else(missing_summary),
        "subagents": subagents.as_ref().map(subagent_summary).unwrap_or_else(missing_summary),
        "ledger": ledger.as_ref().map(|value| ledger_summary(value, tail)).unwrap_or_else(missing_summary),
        "dag": dag.as_ref().map(dag_summary).unwrap_or_else(missing_summary),
        "eval": eval.as_ref().map(|value| eval_summary(value, tail)).unwrap_or_else(missing_summary),
        "files": file_rows(root, OBJECTIVE_FILES, include_content),
        "mutable": false,
        "secret_values_emitted": false,
    })
}

pub fn mission_status_snapshot(root: &Path, include_content: bool, tail: usize) -> Value {
    let loops = read_json(root.join("loops.json"));
    let runtime = read_json(root.join("loop_runtime.json"));
    let trading = read_json(root.join("trading").join("last_report.json"));

    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "mission": {
            "loops": loops.as_ref().map(loops_summary).unwrap_or_else(missing_summary),
            "runtime": runtime.as_ref().map(runtime_summary).unwrap_or_else(missing_summary),
            "queue": queue_summary(&root.join("loop_queue.jsonl"), tail),
            "trading": trading.as_ref().map(trading_report_summary).unwrap_or_else(missing_summary),
        },
        "files": file_rows(root, MISSION_FILES, include_content),
        "mutable": false,
        "secret_values_emitted": false,
    })
}

fn objective_policies_snapshot(root: &Path, include_content: bool) -> Value {
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "policies": {
            "contextlattice": file_snapshot(root, "contextlattice_policy", "contextlattice_policy.json", include_content),
            "simulation": file_snapshot(root, "simulation_policy", "objective_simulation_policy.json", include_content),
            "ensemble": file_snapshot(root, "ensemble_policy", "objective_ensemble_policy.json", include_content),
            "claim_verifier": file_snapshot(root, "claim_verifier_policy", "claim_verifier_policy.json", include_content),
            "quorum": file_snapshot(root, "quorum_policy", "quorum_policy.json", include_content),
        },
        "mutable": false,
    })
}

fn objective_ledger_snapshot(root: &Path, tail: usize, include_content: bool) -> Value {
    let file = file_snapshot(
        root,
        "learning_ledger",
        "objective_learning_ledger.json",
        include_content,
    );
    let summary = read_json(root.join("objective_learning_ledger.json"))
        .as_ref()
        .map(|value| ledger_summary(value, tail))
        .unwrap_or_else(missing_summary);
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "ledger": summary,
        "file": file,
        "mutable": false,
    })
}

fn objective_dag_snapshot(root: &Path, include_content: bool) -> Value {
    let file = file_snapshot(root, "dag", "objective_dag.json", include_content);
    let summary = read_json(root.join("objective_dag.json"))
        .as_ref()
        .map(dag_summary)
        .unwrap_or_else(missing_summary);
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "dag": summary,
        "file": file,
        "mutable": false,
    })
}

fn objective_eval_snapshot(root: &Path, tail: usize, include_content: bool) -> Value {
    let file = file_snapshot(
        root,
        "eval_trend",
        "objective_eval_trend.json",
        include_content,
    );
    let summary = read_json(root.join("objective_eval_trend.json"))
        .as_ref()
        .map(|value| eval_summary(value, tail))
        .unwrap_or_else(missing_summary);
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "eval": summary,
        "file": file,
        "mutable": false,
    })
}

fn mission_loops_snapshot(root: &Path, include_content: bool) -> Value {
    let file = file_snapshot(root, "loops", "loops.json", include_content);
    let summary = read_json(root.join("loops.json"))
        .as_ref()
        .map(loops_summary)
        .unwrap_or_else(missing_summary);
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "loops": summary,
        "file": file,
        "mutable": false,
    })
}

fn mission_queue_snapshot(root: &Path, tail: usize, include_content: bool) -> Value {
    let path = root.join("loop_queue.jsonl");
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "queue": queue_summary(&path, tail),
        "file": text_file_snapshot(root, "queue", "loop_queue.jsonl", include_content),
        "mutable": false,
    })
}

fn mission_runtime_snapshot(root: &Path, include_content: bool) -> Value {
    let file = file_snapshot(root, "runtime", "loop_runtime.json", include_content);
    let summary = read_json(root.join("loop_runtime.json"))
        .as_ref()
        .map(runtime_summary)
        .unwrap_or_else(missing_summary);
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "runtime": summary,
        "file": file,
        "mutable": false,
    })
}

fn trading_snapshot(root: &Path, include_content: bool) -> Value {
    let report = read_json(root.join("trading").join("last_report.json"));
    let config = read_json(root.join("trading").join("runtime_config.json"));
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "trading": {
            "config": config.as_ref().map(trading_config_summary).unwrap_or_else(missing_summary),
            "last_report": report.as_ref().map(trading_report_summary).unwrap_or_else(missing_summary),
        },
        "files": [
            file_snapshot(root, "trading_config", "trading/runtime_config.json", include_content),
            file_snapshot(root, "trading_last_report", "trading/last_report.json", include_content),
            file_snapshot(root, "trading_drift_baseline", "trading/drift_baseline.json", include_content),
        ],
        "mutable": false,
        "secret_values_emitted": false,
    })
}

fn single_file_payload(root: &Path, name: &str, rel: &str, include_content: bool) -> Value {
    json!({
        "status": "ok",
        "alpha_root": root.display().to_string(),
        "file": file_snapshot(root, name, rel, include_content),
        "mutable": false,
    })
}

fn file_rows(root: &Path, files: &[(&str, &str)], include_content: bool) -> Vec<Value> {
    files
        .iter()
        .map(|(name, rel)| {
            if rel.ends_with(".jsonl") {
                text_file_snapshot(root, name, rel, include_content)
            } else {
                file_snapshot(root, name, rel, include_content)
            }
        })
        .collect()
}

fn file_snapshot(root: &Path, name: &str, rel: &str, include_content: bool) -> Value {
    let path = root.join(rel);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return json!({
            "name": name,
            "path": path.display().to_string(),
            "exists": false,
            "valid_json": false,
        });
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(value) => {
            let mut row = json!({
                "name": name,
                "path": path.display().to_string(),
                "exists": true,
                "bytes": raw.len(),
                "valid_json": true,
                "summary": generic_value_summary(&value),
            });
            if include_content {
                row["content"] = redact_json_value(&value);
                row["content_redacted"] = json!(true);
            }
            row
        }
        Err(err) => json!({
            "name": name,
            "path": path.display().to_string(),
            "exists": true,
            "bytes": raw.len(),
            "valid_json": false,
            "parse_error": err.to_string(),
        }),
    }
}

fn text_file_snapshot(root: &Path, name: &str, rel: &str, include_content: bool) -> Value {
    let path = root.join(rel);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return json!({
            "name": name,
            "path": path.display().to_string(),
            "exists": false,
        });
    };
    let mut row = json!({
        "name": name,
        "path": path.display().to_string(),
        "exists": true,
        "bytes": raw.len(),
        "lines": raw.lines().count(),
    });
    if include_content {
        row["content"] = json!(redact_text_content(&raw));
        row["content_redacted"] = json!(true);
    }
    row
}

fn read_json(path: PathBuf) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()
}

fn action_param(params: &Value, default: &str) -> String {
    params
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or(default)
        .trim()
        .to_ascii_lowercase()
}

fn alpha_root_from_params(params: &Value) -> PathBuf {
    params
        .get("alpha_root")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| hermes_config::hermes_home().join("alpha"))
}

fn bounded_tail(input: Option<&Value>) -> usize {
    input.and_then(Value::as_u64).unwrap_or(5).clamp(1, 50) as usize
}

fn missing_summary() -> Value {
    json!({
        "present": false,
        "status": "missing",
    })
}

fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| {
                    let redacted = if secret_key(key) {
                        json!("[REDACTED]")
                    } else {
                        redact_json_value(value)
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json_value).collect()),
        Value::String(text) if crate::credential_guard::detect_secrets(text).is_some() => {
            json!("[REDACTED]")
        }
        other => other.clone(),
    }
}

fn redact_text_content(raw: &str) -> String {
    raw.lines()
        .map(|line| {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                return redact_json_value(&value).to_string();
            }
            if crate::credential_guard::detect_secrets(line).is_some() {
                "[REDACTED]".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn secret_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("access_key")
        || lower.contains("private_key")
        || lower.contains("password")
        || lower.contains("credential")
        || lower == "authorization"
        || lower == "cookie"
}

fn generic_value_summary(value: &Value) -> Value {
    match value {
        Value::Object(map) => json!({
            "kind": "object",
            "keys": map.keys().cloned().collect::<Vec<_>>(),
        }),
        Value::Array(items) => json!({
            "kind": "array",
            "len": items.len(),
        }),
        other => json!({
            "kind": value_kind(other),
        }),
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn objective_contract_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "objective_id": value.get("id").cloned(),
        "lifecycle_status": value.get("lifecycle_status").cloned(),
        "behavior_mode": value.get("behavior_mode").cloned(),
        "confidence": value.get("confidence").cloned(),
        "trading_sensitive": value.get("trading_sensitive").cloned(),
        "success_criteria": array_len(value, "success_criteria"),
        "utility_terms": value
            .get("utility")
            .and_then(|utility| utility.get("terms"))
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0),
        "hard_constraints": value
            .get("utility")
            .and_then(|utility| utility.get("hard_constraints"))
            .and_then(Value::as_array)
            .map(|items| items.len())
            .unwrap_or(0),
        "horizons": array_len(value, "horizons"),
        "counterfactual_entries": array_len(value, "counterfactual_journal"),
    })
}

fn profile_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "profile_id": value.get("profile_id").cloned(),
        "memory_backend": value.get("memory_backend").cloned(),
        "default_shell": value.get("default_shell").cloned(),
        "preferred_repos": array_len(value, "preferred_repos"),
        "preferred_languages": array_len(value, "preferred_languages"),
    })
}

fn context_policy_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "preflight_required": value.get("preflight_required").cloned(),
        "auto_context_pack_on_mission_start": value.get("auto_context_pack_on_mission_start").cloned(),
        "degradation_aware_planning": value.get("degradation_aware_planning").cloned(),
        "readback_verification_required": value.get("readback_verification_required").cloned(),
        "preferred_retrieval_mode": value.get("preferred_retrieval_mode").cloned(),
        "summary_sink_order": value.get("summary_sink_order").cloned(),
        "taxonomy_count": array_len(value, "shared_topic_taxonomy"),
    })
}

fn subagent_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "deterministic_lineage": value.get("deterministic_lineage").cloned(),
        "durable_checkpoints": value.get("durable_checkpoints").cloned(),
        "contradiction_detection": value.get("contradiction_detection").cloned(),
        "profiles": array_len(value, "profiles"),
        "roles": value
            .get("profiles")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("role").and_then(Value::as_str).map(ToOwned::to_owned))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
}

fn ledger_summary(value: &Value, tail: usize) -> Value {
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let tail_rows = entries
        .iter()
        .rev()
        .take(tail)
        .map(redact_json_value)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "present": true,
        "updated_at": value.get("updated_at").cloned(),
        "entries": entries.len(),
        "tail": tail_rows,
    })
}

fn dag_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "updated_at": value.get("updated_at").cloned(),
        "objective_id": value.get("objective_id").cloned(),
        "nodes": array_len(value, "nodes"),
        "auto_resume_checkpoint": value.get("auto_resume_checkpoint").cloned(),
        "statuses": status_counts(value.get("nodes").and_then(Value::as_array).into_iter().flatten()),
    })
}

fn eval_summary(value: &Value, tail: usize) -> Value {
    let samples = value
        .get("samples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut scores = samples
        .iter()
        .filter_map(|sample| sample.get("score").and_then(Value::as_f64))
        .collect::<Vec<_>>();
    scores.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let avg_score = if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    };
    let tail_rows = samples
        .iter()
        .rev()
        .take(tail)
        .map(redact_json_value)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "present": true,
        "updated_at": value.get("updated_at").cloned(),
        "samples": samples.len(),
        "avg_score": avg_score,
        "tail": tail_rows,
    })
}

fn loops_summary(value: &Value) -> Value {
    let loops = value.as_array().cloned().unwrap_or_default();
    let enabled = loops
        .iter()
        .filter(|row| row.get("enabled").and_then(Value::as_bool).unwrap_or(false))
        .count();
    let trading_sensitive = loops
        .iter()
        .filter(|row| {
            row.get("trading_sensitive")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .count();
    json!({
        "present": true,
        "loops": loops.len(),
        "enabled": enabled,
        "trading_sensitive": trading_sensitive,
        "public": loops.len().saturating_sub(trading_sensitive),
        "ids": loops
            .iter()
            .filter_map(|row| row.get("id").and_then(Value::as_str).map(ToOwned::to_owned))
            .collect::<Vec<_>>(),
    })
}

fn runtime_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "updated_at": value.get("updated_at").cloned(),
        "loops": array_len(value, "loops"),
        "queue_pending": value.get("queue_pending").cloned(),
        "queue_replayable": value.get("queue_replayable").cloned(),
        "orphaned_events": value.get("orphaned_events").cloned(),
        "statuses": field_counts(value.get("loops").and_then(Value::as_array), "last_status"),
    })
}

fn queue_summary(path: &Path, tail: usize) -> Value {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return json!({
            "present": false,
            "path": path.display().to_string(),
            "events": 0,
            "statuses": {},
            "tail": [],
        });
    };
    let mut events = Vec::new();
    let mut invalid_lines = 0usize;
    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        match serde_json::from_str::<Value>(line) {
            Ok(value) => events.push(value),
            Err(_) => invalid_lines += 1,
        }
    }
    let tail_rows = events
        .iter()
        .rev()
        .take(tail)
        .map(redact_json_value)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "present": true,
        "path": path.display().to_string(),
        "events": events.len(),
        "invalid_lines": invalid_lines,
        "statuses": status_counts(events.iter()),
        "tail": tail_rows,
    })
}

fn trading_config_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "updated_at": value.get("updated_at").cloned(),
        "starting_wallet_sol": value.get("starting_wallet_sol").cloned(),
        "target_wallet_sol": value.get("target_wallet_sol").cloned(),
        "projects": array_len(value, "projects"),
        "enabled_projects": value
            .get("projects")
            .and_then(Value::as_array)
            .map(|projects| {
                projects
                    .iter()
                    .filter(|project| project.get("enabled").and_then(Value::as_bool).unwrap_or(false))
                    .count()
            })
            .unwrap_or(0),
    })
}

fn trading_report_summary(value: &Value) -> Value {
    json!({
        "present": true,
        "generated_at": value.get("generated_at").cloned(),
        "projects": array_len(value, "projects"),
        "wallet_progress_pct": value.get("wallet_progress_pct").cloned(),
        "ruin_probability": value.get("ruin_probability").cloned(),
        "volatility_sizing_factor": value.get("volatility_sizing_factor").cloned(),
        "canary_recommendation": value.get("canary_recommendation").cloned(),
        "promotion_candidate": value.get("promotion_candidate").cloned(),
        "risk_governor_mode": value
            .get("risk_governor")
            .and_then(|g| g.get("mode"))
            .cloned(),
        "capital_allocator": array_len(value, "capital_allocator"),
        "strategy_weights": value
            .get("strategy_weights")
            .and_then(Value::as_object)
            .map(|map| map.len())
            .unwrap_or(0),
        "pnl_decomposition": value
            .get("pnl_decomposition")
            .and_then(Value::as_object)
            .map(|map| map.len())
            .unwrap_or(0),
        "hypotheses": array_len(value, "hypotheses"),
        "experiments": array_len(value, "experiments"),
        "backtest_matrix": array_len(value, "backtest_matrix"),
        "walkforward_checks": array_len(value, "walkforward_checks"),
        "meta_ranking": array_len(value, "meta_ranking"),
        "canary_pipeline": array_len(value, "canary_pipeline"),
        "repo_drift": array_len(value, "repo_drift"),
        "run_context_audits": array_len(value, "run_context_audits"),
        "env_provenance": array_len(value, "env_provenance"),
        "replay_canary": array_len(value, "replay_canary"),
        "remediation_runbook": array_len(value, "remediation_runbook"),
        "research_sources": array_len(value, "research_sources"),
    })
}

fn array_len(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.len())
        .unwrap_or(0)
}

fn status_counts<'a>(items: impl Iterator<Item = &'a Value>) -> Value {
    let mut counts = BTreeMap::<String, usize>::new();
    for item in items {
        let status = item
            .get("status")
            .or_else(|| item.get("objective_state"))
            .or_else(|| item.get("last_status"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *counts.entry(status).or_default() += 1;
    }
    json!(counts)
}

fn field_counts(items: Option<&Vec<Value>>, field: &str) -> Value {
    let mut counts = BTreeMap::<String, usize>::new();
    if let Some(items) = items {
        for item in items {
            let status = item
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            *counts.entry(status).or_default() += 1;
        }
    }
    json!(counts)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn write_json(path: &Path, value: &Value) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("dirs");
        }
        std::fs::write(path, serde_json::to_string_pretty(value).unwrap()).expect("write json");
    }

    #[tokio::test]
    async fn objective_snapshot_summarizes_alpha_state_without_raw_content_by_default() {
        let home = tempdir().expect("home");
        let alpha = home.path().join("alpha");
        let alpha_root = alpha.display().to_string();
        write_json(
            &alpha.join("objective_contract.json"),
            &json!({
                "id": "obj-demo",
                "lifecycle_status": "active",
                "behavior_mode": "mission",
                "confidence": 0.88,
                "trading_sensitive": false,
                "api_key": "sk-secret-should-not-leak",
                "success_criteria": ["ship"],
                "utility": {
                    "terms": [{"name": "quality", "weight": 1.0}],
                    "hard_constraints": [{"expression": "tests", "hard": true}]
                },
                "horizons": [{"horizon": "intra", "goals": []}],
                "counterfactual_journal": []
            }),
        );
        write_json(
            &alpha.join("contextlattice_policy.json"),
            &json!({
                "preflight_required": true,
                "auto_context_pack_on_mission_start": true,
                "degradation_aware_planning": true,
                "readback_verification_required": true,
                "preferred_retrieval_mode": "deep",
                "shared_topic_taxonomy": ["runbooks/alpha"],
                "summary_sink_order": ["contextlattice", "github"]
            }),
        );
        write_json(
            &alpha.join("subagents.json"),
            &json!({
                "deterministic_lineage": true,
                "durable_checkpoints": true,
                "contradiction_detection": true,
                "profiles": [{"role": "research"}]
            }),
        );
        write_json(
            &alpha.join("objective_learning_ledger.json"),
            &json!({
                "updated_at": "2026-06-04T00:00:00Z",
                "entries": [{"status": "ok", "access_token": "secret-access-token"}]
            }),
        );

        let raw = ObjectiveSnapshotHandler::new()
            .execute(json!({"action": "status", "alpha_root": alpha_root}))
            .await
            .expect("execute");
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["objective"]["objective_id"], "obj-demo");
        assert_eq!(
            payload["contextlattice"]["preferred_retrieval_mode"],
            "deep"
        );
        assert_eq!(payload["subagents"]["profiles"], 1);
        assert_eq!(payload["mutable"], false);
        assert!(!raw.contains("\"content\""));
        assert!(!raw.contains("secret-access-token"));
        assert!(!raw.contains("sk-secret-should-not-leak"));

        let raw_with_content = ObjectiveSnapshotHandler::new()
            .execute(json!({
                "action": "contract",
                "alpha_root": alpha_root,
                "include_content": true,
            }))
            .await
            .expect("execute");
        let payload_with_content: Value =
            serde_json::from_str(&raw_with_content).expect("json with content");
        assert_eq!(
            payload_with_content["file"]["content"]["api_key"],
            "[REDACTED]"
        );
        assert_eq!(payload_with_content["file"]["content_redacted"], true);
        assert!(!raw_with_content.contains("sk-secret-should-not-leak"));
    }

    #[tokio::test]
    async fn mission_snapshot_summarizes_loops_queue_and_private_trading_report() {
        let home = tempdir().expect("home");
        let alpha = home.path().join("alpha");
        let alpha_root = alpha.display().to_string();
        write_json(
            &alpha.join("loops.json"),
            &json!([
                {"id": "primary-objective-loop", "enabled": true, "trading_sensitive": false},
                {"id": "kraken-loop", "enabled": true, "trading_sensitive": true}
            ]),
        );
        write_json(
            &alpha.join("loop_runtime.json"),
            &json!({
                "updated_at": "2026-06-04T00:00:00Z",
                "loops": [{"id": "primary-objective-loop", "last_status": "ok"}],
                "queue_pending": 1,
                "queue_replayable": 1,
                "orphaned_events": 0
            }),
        );
        std::fs::write(
            alpha.join("loop_queue.jsonl"),
            r#"{"id":"evt-1","status":"queued","loop_id":"primary-objective-loop","access_token":"secret-access-token"}"#,
        )
        .expect("queue");
        write_json(
            &alpha.join("trading").join("last_report.json"),
            &json!({
                "generated_at": "2026-06-04T00:00:00Z",
                "projects": [{"id": "algotrader"}],
                "wallet_progress_pct": 0.2,
                "ruin_probability": 0.1,
                "volatility_sizing_factor": 0.8,
                "canary_recommendation": "hold-canary",
                "promotion_candidate": "algotrader",
                "risk_governor": {"mode": "normal"},
                "capital_allocator": [{}],
                "strategy_weights": {"algotrader": 1.0},
                "pnl_decomposition": {"signal": 1.0},
                "hypotheses": [{}],
                "experiments": [{}],
                "backtest_matrix": ["case"],
                "walkforward_checks": ["wf"],
                "meta_ranking": ["algotrader"],
                "canary_pipeline": [{}],
                "repo_drift": [{}],
                "run_context_audits": [{}],
                "env_provenance": [{"conflicting_keys": ["REAL_ALGOTRADER_WS_BUY_GATE_ENABLED"]}],
                "replay_canary": [{}],
                "remediation_runbook": [{}],
                "research_sources": [{}]
            }),
        );

        let raw = MissionSnapshotHandler::new()
            .execute(json!({"action": "status", "alpha_root": alpha_root, "tail": 1}))
            .await
            .expect("execute");
        let payload: Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["mission"]["loops"]["loops"], 2);
        assert_eq!(payload["mission"]["loops"]["trading_sensitive"], 1);
        assert_eq!(payload["mission"]["queue"]["events"], 1);
        assert_eq!(
            payload["mission"]["queue"]["tail"][0]["access_token"],
            "[REDACTED]"
        );
        assert_eq!(
            payload["mission"]["trading"]["risk_governor_mode"],
            "normal"
        );
        assert_eq!(payload["mission"]["trading"]["env_provenance"], 1);
        assert_eq!(payload["mutable"], false);
        assert!(!raw.contains("\"content\""));
        assert!(!raw.contains("secret-access-token"));
    }
}
