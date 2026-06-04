//! Read-only operational snapshots for agent-callable diagnostics.
//!
//! `/ops status` and `/qos status` are operator-facing slash surfaces. This
//! tool exposes the same kind of evidence as structured JSON without allowing
//! agents to mutate policy, budgets, or runtime modes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::repo::detect_repo_root_from_cwd;
use crate::tool_policy::{default_tool_policy_counters_path, load_tool_policy_counters};
use crate::tools::telemetry_snapshot::telemetry_gate_snapshot;
use crate::{ToolPolicyCounters, ToolRegistry};

const TOOL_NAME: &str = "ops_snapshot";

#[derive(Clone)]
pub struct OpsSnapshotHandler {
    registry: Arc<ToolRegistry>,
}

impl OpsSnapshotHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolHandler for OpsSnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let repo_root = params
            .get("repo_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(detect_repo_root_from_cwd);
        let include_tools = params
            .get("include_tools")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        let payload = match action.as_str() {
            "status" => ops_snapshot(&self.registry, repo_root.as_deref(), include_tools),
            "policy" => json!({
                "status": "ok",
                "policy": policy_snapshot(&self.registry),
                "tools": if include_tools { Some(tool_registry_snapshot(&self.registry)) } else { None },
            }),
            "qos" => json!({
                "status": "ok",
                "qos": qos_snapshot(),
            }),
            "gates" => json!({
                "status": "ok",
                "repo_root": repo_root.as_ref().map(|path| path.display().to_string()),
                "gates": repo_root.as_deref().map(telemetry_gate_snapshot).unwrap_or_else(|| json!({
                    "status": "unknown",
                    "reason": "repo_root_not_detected",
                })),
            }),
            "help" => json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "policy", "qos", "gates", "help"],
                "notes": [
                    "read-only operational snapshot",
                    "does not mutate policy, budgets, dashboard config, or runtime modes",
                    "repo_root defaults to nearest parent with `.git`"
                ],
            }),
            _ => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown action '{action}'; expected status|policy|qos|gates|help"
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
                "enum": ["status", "policy", "qos", "gates", "help"],
                "description": "Snapshot action. Defaults to status."
            }),
        );
        props.insert(
            "repo_root".into(),
            json!({
                "type": "string",
                "description": "Optional repo root for gate report lookup. Defaults to nearest parent with `.git`."
            }),
        );
        props.insert(
            "include_tools".into(),
            json!({
                "type": "boolean",
                "description": "Include tool registry counts in status/policy snapshots. Defaults to true."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Return read-only ops, policy, QoS, gate, and tool-registry diagnostics as structured JSON.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn ops_snapshot(
    registry: &ToolRegistry,
    repo_root: Option<&Path>,
    include_tools: bool,
) -> Value {
    json!({
        "status": "ok",
        "policy": policy_snapshot(registry),
        "runtime": runtime_knobs_snapshot(),
        "qos": qos_snapshot(),
        "repo_root": repo_root.map(|path| path.display().to_string()),
        "gates": repo_root.map(telemetry_gate_snapshot).unwrap_or_else(|| json!({
            "status": "unknown",
            "reason": "repo_root_not_detected",
        })),
        "tools": if include_tools { Some(tool_registry_snapshot(registry)) } else { None },
        "mutable": false,
    })
}

fn policy_snapshot(registry: &ToolRegistry) -> Value {
    let live = registry.policy_counters();
    let counters_path = default_tool_policy_counters_path();
    let persisted = load_tool_policy_counters(&counters_path).unwrap_or_default();
    json!({
        "mode": env_nonempty("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|| "enforce".to_string()),
        "preset": env_nonempty("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|| "relaxed".to_string()),
        "policy_file": env_nonempty("HERMES_TOOL_POLICY_FILE"),
        "sandbox_profile": env_nonempty("HERMES_EXECUTION_SANDBOX_PROFILE").unwrap_or_else(|| "balanced".to_string()),
        "allowlist_set": env_nonempty("HERMES_TOOL_POLICY_ALLOWLIST").is_some(),
        "denylist_set": env_nonempty("HERMES_TOOL_POLICY_DENYLIST").is_some(),
        "deny_param_patterns_set": env_nonempty("HERMES_TOOL_POLICY_DENY_PARAM_PATTERNS").is_some(),
        "max_param_bytes": env_nonempty("HERMES_TOOL_POLICY_MAX_PARAM_BYTES").and_then(|v| v.parse::<usize>().ok()),
        "counters_live": counters_json(&live),
        "counters_persisted": counters_json(&persisted),
        "counters_path": counters_path.display().to_string(),
    })
}

fn runtime_knobs_snapshot() -> Value {
    json!({
        "autopilot": {
            "mode": env_nonempty("HERMES_PERF_AUTOPILOT_MODE").unwrap_or_else(|| "advisory".to_string()),
            "profile": env_nonempty("HERMES_PERF_AUTOPILOT_PROFILE").unwrap_or_else(|| "off".to_string()),
            "status": env_nonempty("HERMES_PERF_AUTOPILOT_STATUS"),
        },
        "repo_review_budget": repo_review_budget_snapshot(),
        "repo_review_tool_profile": env_nonempty("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE").unwrap_or_else(|| "off".to_string()),
        "skills_tier": env_nonempty("HERMES_SKILLS_EXECUTION_TIER").unwrap_or_else(|| "balanced".to_string()),
        "skills_tier_bypass": env_truthy("HERMES_SKILLS_TIER_BYPASS"),
        "replay_trace": env_truthy("HERMES_REPLAY_TRACE") || env_truthy("HERMES_TRACE_REPLAY"),
    })
}

fn repo_review_budget_snapshot() -> Value {
    json!({
        "profile": env_nonempty("HERMES_REPO_REVIEW_BUDGET_PROFILE").unwrap_or_else(|| "balanced".to_string()),
        "repeat_threshold": env_usize("HERMES_REPO_REVIEW_REPEAT_STREAK_THRESHOLD", 2, 1, 12),
        "low_signal_threshold": env_usize("HERMES_REPO_REVIEW_LOW_SIGNAL_STREAK_THRESHOLD", 2, 1, 12),
        "keep_repeat": env_usize("HERMES_REPO_REVIEW_KEEP_LIMIT_REPEAT", 2, 1, 12),
        "keep_low_signal": env_usize("HERMES_REPO_REVIEW_KEEP_LIMIT_LOW_SIGNAL", 1, 1, 12),
        "min_signal_score": env_f64("HERMES_REPO_REVIEW_MIN_SIGNAL_SCORE", 0.22, 0.0, 1.0),
    })
}

fn qos_snapshot() -> Value {
    let home = hermes_config::hermes_home();
    let learning_path = home.join("route-learning.json");
    let health_path = home.join("route-health.json");
    let autotune_path = home.join("route-autotune.json");
    let autotune_env_path = home.join("route-autotune.env");

    let learning_entries = read_json_file(&learning_path)
        .and_then(|value| {
            value
                .get("entries")
                .and_then(Value::as_array)
                .map(|items| items.len())
        })
        .unwrap_or(0);

    json!({
        "route_learning": {
            "path": learning_path.display().to_string(),
            "available": learning_path.exists(),
            "entries": learning_entries,
        },
        "route_health": route_health_snapshot(&health_path),
        "route_autotune": {
            "state_path": autotune_path.display().to_string(),
            "state_available": autotune_path.exists(),
            "env_path": autotune_env_path.display().to_string(),
            "env_available": autotune_env_path.exists(),
        },
    })
}

fn route_health_snapshot(path: &Path) -> Value {
    let Some(report) = read_json_file(path) else {
        return json!({
            "path": path.display().to_string(),
            "available": false,
            "overall": "unknown",
            "health_score": null,
            "generated_at": null,
            "weakest": [],
        });
    };

    let mut weakest =
        report
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                .iter()
                .filter_map(|entry| {
                    let key = entry.get("key").and_then(Value::as_str)?;
                    let tier = entry
                        .get("tier")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let score = entry
                        .get("health_score")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    Some((score, json!({
                        "key": key,
                        "tier": tier,
                        "health_score": score,
                        "reasons": entry.get("reasons").cloned().unwrap_or_else(|| json!([])),
                    })))
                })
                .collect::<Vec<_>>()
            })
            .unwrap_or_default();
    weakest.sort_by(|a, b| a.0.total_cmp(&b.0));
    let weakest_values = weakest
        .into_iter()
        .take(3)
        .map(|(_, value)| value)
        .collect::<Vec<_>>();

    json!({
        "path": path.display().to_string(),
        "available": true,
        "overall": report.get("overall").and_then(Value::as_str).unwrap_or("unknown"),
        "health_score": report.get("summary").and_then(|value| value.get("health_score")).and_then(Value::as_f64),
        "generated_at": report.get("generated_at").and_then(Value::as_str),
        "weakest": weakest_values,
    })
}

fn tool_registry_snapshot(registry: &ToolRegistry) -> Value {
    let tools = registry.list_tools();
    let mut by_toolset: BTreeMap<String, usize> = BTreeMap::new();
    let mut unavailable = Vec::new();
    for tool in &tools {
        *by_toolset.entry(tool.toolset.clone()).or_insert(0) += 1;
        if !registry.is_available(&tool.name) {
            unavailable.push(tool.name.clone());
        }
    }
    unavailable.sort();
    json!({
        "registered": tools.len(),
        "available": tools.len().saturating_sub(unavailable.len()),
        "unavailable": unavailable,
        "by_toolset": by_toolset,
    })
}

fn counters_json(counters: &ToolPolicyCounters) -> Value {
    json!({
        "allow": counters.allow,
        "deny": counters.deny,
        "audit_only": counters.audit_only,
        "simulate": counters.simulate,
        "would_block": counters.would_block,
    })
}

fn read_json_file(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_truthy(key: &str) -> bool {
    env_nonempty(key).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn env_usize(key: &str, default: usize, min: usize, max: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

fn env_f64(key: &str, default: f64, min: f64, max: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<f64>().ok())
        .unwrap_or(default)
        .clamp(min, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handler_returns_read_only_status_snapshot() {
        let registry = ToolRegistry::new();
        let handler = OpsSnapshotHandler::new(Arc::new(registry));
        let payload: Value = serde_json::from_str(
            &handler
                .execute(json!({"include_tools": false}))
                .await
                .expect("execute"),
        )
        .expect("json");

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["mutable"], false);
        assert_eq!(payload["tools"], Value::Null);
        assert!(payload.get("policy").is_some());
        assert!(payload.get("qos").is_some());
        assert!(payload.get("runtime").is_some());
    }

    #[tokio::test]
    async fn handler_reports_tool_registry_counts() {
        let registry = ToolRegistry::new();
        registry.register(
            "unit_tool",
            "unit",
            tool_schema(
                "unit_tool",
                "unit test tool",
                JsonSchema::object(IndexMap::new(), vec![]),
            ),
            Arc::new(OpsSnapshotHandler::new(Arc::new(registry.clone()))),
            Arc::new(|| true),
            vec![],
            true,
            "unit test tool",
            "u",
            None,
        );
        let handler = OpsSnapshotHandler::new(Arc::new(registry));
        let payload: Value =
            serde_json::from_str(&handler.execute(json!({})).await.unwrap()).expect("json");

        assert_eq!(payload["tools"]["registered"], 1);
        assert_eq!(payload["tools"]["available"], 1);
        assert_eq!(payload["tools"]["by_toolset"]["unit"], 1);
    }

    #[test]
    fn route_health_snapshot_extracts_weakest_entries() {
        let temp = tempfile::tempdir().expect("temp");
        let path = temp.path().join("route-health.json");
        std::fs::write(
            &path,
            json!({
                "overall": "degraded",
                "generated_at": "2026-06-04T00:00:00Z",
                "summary": {"health_score": 0.42},
                "entries": [
                    {"key": "a", "tier": "ok", "health_score": 0.9, "reasons": []},
                    {"key": "b", "tier": "critical", "health_score": 0.1, "reasons": ["fail"]},
                    {"key": "c", "tier": "watch", "health_score": 0.3, "reasons": ["slow"]}
                ]
            })
            .to_string(),
        )
        .expect("write");

        let snapshot = route_health_snapshot(&path);
        assert_eq!(snapshot["available"], true);
        assert_eq!(snapshot["overall"], "degraded");
        assert_eq!(snapshot["weakest"][0]["key"], "b");
        assert_eq!(snapshot["weakest"][1]["key"], "c");
    }
}
