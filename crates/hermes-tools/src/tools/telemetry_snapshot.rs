//! Agent-facing runtime telemetry snapshots.
//!
//! The TUI exposes a compact view through `/telemetry`; this tool makes the
//! repo/gate/provider portion callable from the Rust tool registry.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::repo::detect_repo_root_from_cwd;

const TOOL_NAME: &str = "telemetry_snapshot";

#[derive(Clone, Default)]
pub struct TelemetrySnapshotHandler;

impl TelemetrySnapshotHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for TelemetrySnapshotHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let provider = params
            .get("provider")
            .and_then(Value::as_str)
            .or_else(|| {
                params
                    .get("model")
                    .and_then(Value::as_str)
                    .and_then(|model| model.split_once(':').map(|(provider, _)| provider))
            })
            .unwrap_or("openai");
        let repo_root = params
            .get("repo_root")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(detect_repo_root_from_cwd);

        match action.as_str() {
            "status" | "gates" | "lane" => {
                let gates = repo_root
                    .as_ref()
                    .map(|root| telemetry_gate_snapshot(root))
                    .unwrap_or_else(|| {
                        json!({
                            "status": "unknown",
                            "reason": "repo_root_not_detected",
                        })
                    });
                Ok(json!({
                    "status": "ok",
                    "provider": provider,
                    "provider_health": provider_health_snapshot(provider),
                    "repo_root": repo_root.as_ref().map(|path| path.display().to_string()),
                    "gates": gates,
                    "lane_hints": if action == "lane" { Some(lane_hints()) } else { None },
                })
                .to_string())
            }
            "help" => Ok(json!({
                "status": "ok",
                "tool": TOOL_NAME,
                "actions": ["status", "gates", "lane", "help"],
                "notes": [
                    "mirrors `/telemetry status` gate/provider snapshot without requiring a TUI",
                    "repo_root defaults to the nearest parent with `.git`"
                ],
            })
            .to_string()),
            _ => Err(ToolError::InvalidParams(format!(
                "unknown action '{action}'; expected status|gates|lane|help"
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": ["status", "gates", "lane", "help"],
                "description": "Telemetry action. Defaults to status."
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "Provider slug for health classification."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Optional provider:model string; provider is inferred when provider is omitted."
            }),
        );
        props.insert(
            "repo_root".into(),
            json!({
                "type": "string",
                "description": "Optional repo root. Defaults to nearest parent with `.git`."
            }),
        );
        tool_schema(
            TOOL_NAME,
            "Report runtime provider and repo gate telemetry without a TUI.",
            JsonSchema::object(props, vec![]),
        )
    }
}

pub fn provider_health_snapshot(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "nous" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth" | "qwen-oauth" => {
            "oauth-capable"
        }
        "openai" | "anthropic" | "openrouter" => "api-key/session",
        _ => "unknown",
    }
}

pub fn telemetry_gate_line(repo_root: &Path) -> String {
    let report_dir = repo_root.join(".sync-reports");
    let eval = latest_json_report(&report_dir, "eval-trend-gate-")
        .and_then(|p| summarize_gate_report(&p, "eval"))
        .unwrap_or_else(|| "eval=unknown".to_string());
    let autopilot = latest_json_report(&report_dir, "performance-autopilot-")
        .and_then(|p| summarize_performance_autopilot_report(&p, "autopilot"))
        .unwrap_or_else(|| "autopilot=unknown".to_string());
    let replay = latest_json_report(&report_dir, "deterministic-replay-")
        .and_then(|p| summarize_gate_report(&p, "replay"))
        .unwrap_or_else(|| "replay=unknown".to_string());
    format!("gates: {eval}; {autopilot}; {replay}")
}

pub fn telemetry_gate_snapshot(repo_root: &Path) -> Value {
    let report_dir = repo_root.join(".sync-reports");
    json!({
        "status": if report_dir.exists() { "ok" } else { "missing_report_dir" },
        "report_dir": report_dir.display().to_string(),
        "eval": gate_entry(&report_dir, "eval-trend-gate-", "eval"),
        "autopilot": autopilot_entry(&report_dir, "performance-autopilot-", "autopilot"),
        "replay": gate_entry(&report_dir, "deterministic-replay-", "replay"),
        "slash_parity": gate_entry(&report_dir, "upstream-slash-parity-gate-", "slash_parity"),
        "differential_parity": gate_entry(&report_dir, "differential-parity-gate-", "differential_parity"),
        "elite_sync": gate_entry(&report_dir, "elite-sync-gate-", "elite_sync"),
        "slo_rollback": gate_entry(&report_dir, "slo-auto-rollback-", "slo_rollback"),
    })
}

pub fn lane_hints() -> Vec<&'static str> {
    vec![
        "Ctrl+L toggle activity lane",
        "Ctrl+O switch lane mode (live/cockpit)",
        "Ctrl+G force transcript refresh + jump latest",
    ]
}

fn latest_json_report(report_dir: &Path, prefix: &str) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(report_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with(prefix) && name.ends_with(".json") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    reports.sort();
    reports.into_iter().last()
}

fn gate_entry(report_dir: &Path, prefix: &str, key: &str) -> Value {
    let Some(path) = latest_json_report(report_dir, prefix) else {
        return json!({
            "status": "unknown",
            "summary": format!("{key}=unknown"),
        });
    };
    let Some(report) = read_json_file(&path) else {
        return json!({
            "status": "unreadable",
            "path": path.display().to_string(),
            "summary": format!("{key}=unknown"),
        });
    };
    let ok = report.get("ok").and_then(Value::as_bool);
    let generated_at = report
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    json!({
        "status": ok.map(|value| if value { "pass" } else { "fail" }).unwrap_or("unknown"),
        "ok": ok,
        "generated_at": generated_at,
        "path": path.display().to_string(),
        "file": path.file_name().map(|name| name.to_string_lossy().to_string()),
        "summary": summarize_gate_report(&path, key).unwrap_or_else(|| format!("{key}=unknown")),
    })
}

fn autopilot_entry(report_dir: &Path, prefix: &str, key: &str) -> Value {
    let Some(path) = latest_json_report(report_dir, prefix) else {
        return json!({
            "status": "unknown",
            "summary": format!("{key}=unknown"),
        });
    };
    let Some(report) = read_json_file(&path) else {
        return json!({
            "status": "unreadable",
            "path": path.display().to_string(),
            "summary": format!("{key}=unknown"),
        });
    };
    let ok = report.get("ok").and_then(Value::as_bool);
    json!({
        "status": ok.map(|value| if value { "pass" } else { "fail" }).unwrap_or("unknown"),
        "ok": ok,
        "generated_at": report.get("generated_at").and_then(Value::as_str).unwrap_or("unknown"),
        "adaptive_index": report.get("adaptive_index").and_then(Value::as_f64),
        "profile_recommendation": report.get("profile_recommendation").and_then(Value::as_str),
        "recommendations": report.get("recommendations").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
        "path": path.display().to_string(),
        "file": path.file_name().map(|name| name.to_string_lossy().to_string()),
        "summary": summarize_performance_autopilot_report(&path, key).unwrap_or_else(|| format!("{key}=unknown")),
    })
}

fn summarize_gate_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(Value::as_bool)
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Some(format!(
        "{}={} @ {} ({})",
        key,
        ok,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

fn summarize_performance_autopilot_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(Value::as_bool)
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let recommendations = report
        .get("recommendations")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let severe = report
        .get("recommendations")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter(|item| {
                    item.get("severity")
                        .and_then(Value::as_str)
                        .is_some_and(|sev| {
                            sev.eq_ignore_ascii_case("P0") || sev.eq_ignore_ascii_case("P1")
                        })
                })
                .count()
        })
        .unwrap_or(0);
    let adaptive_idx = report
        .get("adaptive_index")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let profile = report
        .get("profile_recommendation")
        .and_then(Value::as_str)
        .unwrap_or("balanced");
    Some(format!(
        "{}={} idx={:.2} profile={} recs={} severe={} @ {} ({})",
        key,
        ok,
        adaptive_idx,
        profile,
        recommendations,
        severe,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

fn read_json_file(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<Value>(&raw).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_health_matches_slash_contract() {
        assert_eq!(provider_health_snapshot("nous"), "oauth-capable");
        assert_eq!(provider_health_snapshot("openai"), "api-key/session");
        assert_eq!(provider_health_snapshot("unknown-provider"), "unknown");
    }

    #[test]
    fn gate_line_summarizes_latest_reports() {
        let temp = tempfile::tempdir().expect("temp");
        let report_dir = temp.path().join(".sync-reports");
        std::fs::create_dir_all(&report_dir).expect("mkdir reports");
        std::fs::write(
            report_dir.join("eval-trend-gate-20260604.json"),
            json!({"ok": true, "generated_at": "2026-06-04T00:00:00Z"}).to_string(),
        )
        .expect("write eval");
        std::fs::write(
            report_dir.join("performance-autopilot-20260604.json"),
            json!({
                "ok": true,
                "generated_at": "2026-06-04T00:01:00Z",
                "adaptive_index": 0.82,
                "profile_recommendation": "quality",
                "recommendations": [{"severity": "P1"}, {"severity": "P3"}]
            })
            .to_string(),
        )
        .expect("write autopilot");

        let line = telemetry_gate_line(temp.path());
        assert!(line.contains("eval=pass"));
        assert!(line.contains("autopilot=pass idx=0.82 profile=quality recs=2 severe=1"));
        assert!(line.contains("replay=unknown"));
    }

    #[tokio::test]
    async fn handler_reports_structured_gate_snapshot() {
        let temp = tempfile::tempdir().expect("temp");
        let report_dir = temp.path().join(".sync-reports");
        std::fs::create_dir_all(&report_dir).expect("mkdir reports");
        std::fs::write(
            report_dir.join("upstream-slash-parity-gate-20260604.json"),
            json!({"ok": true, "generated_at": "2026-06-04T00:02:00Z"}).to_string(),
        )
        .expect("write slash gate");

        let handler = TelemetrySnapshotHandler::new();
        let payload: Value = serde_json::from_str(
            &handler
                .execute(json!({
                    "action": "lane",
                    "model": "nous:Hermes-4",
                    "repo_root": temp.path().display().to_string()
                }))
                .await
                .unwrap(),
        )
        .expect("json");

        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["provider"], "nous");
        assert_eq!(payload["provider_health"], "oauth-capable");
        assert_eq!(payload["gates"]["slash_parity"]["status"], "pass");
        assert_eq!(payload["lane_hints"].as_array().unwrap().len(), 3);
    }
}
