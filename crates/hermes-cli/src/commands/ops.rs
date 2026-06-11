//! OPS commands — operator control plane, dashboards, simulation, autopilot,
//! self-evolution, gate, QoS routing, task-depth profiles, and budget controls.
//!
//! Extracted from `mod.rs` to keep the slash-command handler file focused on
//! dispatch and shared plumbing.

use std::fmt::Write as _;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::SystemTime;

use hermes_core::AgentError;

use super::{
    background, compress, discover_repo_root_for_about, plan_capability_mode, policy,
    read_json_file, replay_enabled_runtime, session, skills,
};
use crate::App;
use crate::alpha_runtime::render_mission_board;
use crate::commands::{CommandResult, emit_command_output};

// ---------------------------------------------------------------------------
// RepoReviewBudgetProfile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoReviewBudgetProfile {
    Balanced,
    Aggressive,
    Relaxed,
    Off,
}

impl RepoReviewBudgetProfile {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "balanced" => Some(Self::Balanced),
            "aggressive" => Some(Self::Aggressive),
            "relaxed" => Some(Self::Relaxed),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
            Self::Relaxed => "relaxed",
            Self::Off => "off",
        }
    }
}

const REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD: &str = "HERMES_REPO_REVIEW_REPEAT_STREAK_THRESHOLD";
const REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD: &str =
    "HERMES_REPO_REVIEW_LOW_SIGNAL_STREAK_THRESHOLD";
const REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT: &str = "HERMES_REPO_REVIEW_KEEP_LIMIT_REPEAT";
const REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL: &str = "HERMES_REPO_REVIEW_KEEP_LIMIT_LOW_SIGNAL";
const REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE: &str = "HERMES_REPO_REVIEW_MIN_SIGNAL_SCORE";
const REPO_REVIEW_BUDGET_ENV_PROFILE: &str = "HERMES_REPO_REVIEW_BUDGET_PROFILE";

#[derive(Debug, Clone, PartialEq)]
struct RepoReviewBudgetRuntime {
    repeat_threshold: usize,
    low_signal_threshold: usize,
    keep_repeat: usize,
    keep_low_signal: usize,
    min_signal_score: f64,
    profile: RepoReviewBudgetProfile,
}

impl RepoReviewBudgetRuntime {
    fn from_env() -> Self {
        let repeat_threshold = std::env::var(REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let low_signal_threshold = std::env::var(REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let keep_repeat = std::env::var(REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(2)
            .clamp(1, 12);
        let keep_low_signal = std::env::var(REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(1)
            .clamp(1, 12);
        let min_signal_score = std::env::var(REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE)
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.22)
            .clamp(0.0, 1.0);
        let profile = std::env::var(REPO_REVIEW_BUDGET_ENV_PROFILE)
            .ok()
            .as_deref()
            .and_then(RepoReviewBudgetProfile::parse)
            .unwrap_or(RepoReviewBudgetProfile::Balanced);
        Self {
            repeat_threshold,
            low_signal_threshold,
            keep_repeat,
            keep_low_signal,
            min_signal_score,
            profile,
        }
    }
}

fn apply_repo_review_budget_profile(profile: RepoReviewBudgetProfile) {
    let (repeat_threshold, low_signal_threshold, keep_repeat, keep_low_signal, min_signal_score) =
        match profile {
            RepoReviewBudgetProfile::Balanced => (2usize, 2usize, 2usize, 1usize, 0.22f64),
            RepoReviewBudgetProfile::Aggressive => (1usize, 1usize, 1usize, 1usize, 0.35f64),
            RepoReviewBudgetProfile::Relaxed => (3usize, 3usize, 3usize, 2usize, 0.15f64),
            RepoReviewBudgetProfile::Off => (12usize, 12usize, 12usize, 12usize, 0.01f64),
        };
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
        repeat_threshold.to_string(),
    );
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
        low_signal_threshold.to_string(),
    );
    crate::env_vars::set_var(REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT, keep_repeat.to_string());
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
        keep_low_signal.to_string(),
    );
    crate::env_vars::set_var(
        REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
        format!("{:.3}", min_signal_score),
    );
    crate::env_vars::set_var(REPO_REVIEW_BUDGET_ENV_PROFILE, profile.as_str());
}

// ---------------------------------------------------------------------------
// Report helpers
// ---------------------------------------------------------------------------

pub(crate) fn latest_json_report(report_dir: &Path, prefix: &str) -> Option<PathBuf> {
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

pub(crate) fn summarize_gate_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
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

fn summarize_self_evolution_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let idx = report
        .get("summary")
        .and_then(|s| s.get("intelligence_index"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let recs = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    Some(format!(
        "{}={} idx={:.2} recs={} @ {} ({})",
        key,
        ok,
        idx,
        recs,
        generated,
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string())
    ))
}

fn self_evolution_recommendations(path: &Path) -> Vec<String> {
    let report = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let Some(items) = report.get("recommendations").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            let sev = obj.get("severity").and_then(|v| v.as_str()).unwrap_or("PX");
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let cmd = obj.get("command").and_then(|v| v.as_str()).unwrap_or("");
            Some(format!("[{sev}] {id}: {title}\n  cmd: {cmd}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Autopilot constants & helpers
// ---------------------------------------------------------------------------

const AUTOPILOT_ALLOWED_ENV_KEYS: &[&str] = &[
    "HERMES_TOOL_POLICY_PRESET",
    "HERMES_TOOL_POLICY_MODE",
    "HERMES_MODEL_CATALOG_GUARD",
    "HERMES_MODEL_AUTO_REMEDIATE",
    "HERMES_REPLAY_ENABLED",
    "HERMES_PERF_AUTOPILOT_STATUS",
    "HERMES_PERF_AUTOPILOT_PROFILE",
    "HERMES_PERF_AUTOPILOT_MODE",
];

pub(crate) fn summarize_performance_autopilot_report(path: &Path, key: &str) -> Option<String> {
    let report = read_json_file(path)?;
    let ok = report
        .get("ok")
        .and_then(|v| v.as_bool())
        .map(|v| if v { "pass" } else { "fail" })
        .unwrap_or("unknown");
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let recommendations = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let severe = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|item| {
                    item.get("severity")
                        .and_then(|v| v.as_str())
                        .is_some_and(|sev| {
                            sev.eq_ignore_ascii_case("P0") || sev.eq_ignore_ascii_case("P1")
                        })
                })
                .count()
        })
        .unwrap_or(0);
    let adaptive_idx = report
        .get("adaptive_index")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let profile = report
        .get("profile_recommendation")
        .and_then(|v| v.as_str())
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

fn performance_autopilot_recommendations(path: &Path) -> Vec<String> {
    let report = match read_json_file(path) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let Some(items) = report.get("recommendations").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let id = obj.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN");
            let sev = obj.get("severity").and_then(|v| v.as_str()).unwrap_or("PX");
            let title = obj.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let rec = obj
                .get("recommendation")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Some(format!("[{sev}] {id}: {title}\n  recommendation: {rec}"))
        })
        .collect()
}

fn parse_env_file_kv(path: &Path) -> Vec<(String, String)> {
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let (k, v) = trimmed.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn write_autopilot_runtime_event(
    report_dir: &Path,
    session_id: &str,
    mode: &str,
    profile: &str,
    applied: &[(String, String)],
) {
    let path = report_dir.join("performance-autopilot-runtime.jsonl");
    let created_at = format!("{:?}", SystemTime::now());
    let payload = serde_json::json!({
        "created_at": created_at,
        "session_id": session_id,
        "mode": mode,
        "profile": profile,
        "applied": applied,
    });
    if let Ok(line) = serde_json::to_string(&payload) {
        if let Ok(mut fh) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(&mut fh, "{line}");
        }
    }
}

fn dashboard_status_line_from_payload(payload: &serde_json::Value) -> String {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let url = payload.get("url").and_then(|v| v.as_str()).unwrap_or("n/a");
    format!(
        "dashboard: {} ({})",
        if enabled { "ON" } else { "OFF" },
        url
    )
}

// ---------------------------------------------------------------------------
// Shell helpers
// ---------------------------------------------------------------------------

async fn run_ops_shell_command(command: &str) -> Result<String, AgentError> {
    let output = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("ops shell command failed: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut msg = String::new();
    if !stdout.is_empty() {
        msg.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str("stderr:\n");
        msg.push_str(&stderr);
    }
    if msg.is_empty() {
        msg = format!("(exit: {})", output.status);
    } else if !output.status.success() {
        msg = format!("(exit: {})\n{}", output.status, msg);
    }
    Ok(msg)
}

async fn run_current_hermes_cli_command(args: &[&str]) -> Result<String, AgentError> {
    let exe = std::env::current_exe()
        .map_err(|e| AgentError::Io(format!("resolve current executable: {e}")))?;
    let output = tokio::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("run current hermes command failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut msg = String::new();
    if !stdout.is_empty() {
        msg.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str("stderr:\n");
        msg.push_str(&stderr);
    }
    if msg.is_empty() {
        msg = format!("(exit: {})", output.status);
    } else if !output.status.success() {
        msg = format!("(exit: {})\n{}", output.status, msg);
    }
    Ok(msg)
}

fn shell_escape(input: &str) -> String {
    let escaped = input.replace('\'', "'\"'\"'");
    format!("'{}'", escaped)
}

// ---------------------------------------------------------------------------
// /dashboard
// ---------------------------------------------------------------------------

pub(crate) async fn handle_dashboard_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mut params = serde_json::json!({
        "action": action
    });
    if let Some(host) = args.get(1) {
        params["host"] = serde_json::Value::String((*host).to_string());
    }
    if let Some(port) = args.get(2).and_then(|raw| raw.parse::<u16>().ok()) {
        params["port"] = serde_json::json!(port);
    }
    if args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("--insecure"))
    {
        params["insecure"] = serde_json::json!(true);
    }

    let raw = app
        .tool_registry
        .dispatch_async("dashboard_control", params)
        .await;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({"result": raw}));

    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        emit_command_output(app, format!("Dashboard command failed: {err}"));
        return Ok(CommandResult::Handled);
    }

    let rendered = match action.as_str() {
        "enable" | "on" => format!(
            "Dashboard enabled at {}\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "disable" | "off" => format!(
            "Dashboard disabled (URL: {})\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "url" => format!(
            "{}\n{}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        _ => format!(
            "{}\nConfig: {}",
            dashboard_status_line_from_payload(&parsed),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
    };
    emit_command_output(app, rendered);
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /simulate
// ---------------------------------------------------------------------------

pub(crate) fn handle_simulate_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = app.tool_registry.policy_counters();
        emit_command_output(
            app,
            format!(
                "Tool-policy simulation\n\
                 usage: /simulate <tool_name> [json-params]\n\
                 examples:\n  /simulate terminal {{\"cmd\":\"ls\"}}\n  /simulate skill_manage {{\"action\":\"view\",\"skill\":\"contextlattice-agent-contract\"}}\n\
                 counters: allow={} deny={} audit_only={} simulate={} would_block={}",
                counters.allow,
                counters.deny,
                counters.audit_only,
                counters.simulate,
                counters.would_block
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let tool_name = args[0].trim();
    if tool_name.is_empty() {
        emit_command_output(app, "Usage: /simulate <tool_name> [json-params]");
        return Ok(CommandResult::Handled);
    }
    let params = if args.len() > 1 {
        let raw = args[1..].join(" ");
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                emit_command_output(app, "simulate params must be a JSON object.");
                return Ok(CommandResult::Handled);
            }
            Err(err) => {
                emit_command_output(
                    app,
                    format!("simulate params parse error: {}\nraw={}", err, raw),
                );
                return Ok(CommandResult::Handled);
            }
        }
    } else {
        serde_json::json!({})
    };

    let decision = app
        .tool_registry
        .evaluate_policy_preview(tool_name, &params);
    let payload = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "decision": {
            "allow": decision.allow,
            "mode": decision.mode.as_str(),
            "audited_only": decision.audited_only,
            "simulated": decision.simulated,
            "would_block": decision.would_block,
            "code": decision.code,
            "reason": decision.reason,
        }
    });
    emit_command_output(
        app,
        serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("serialize simulate result: {e}")))?,
    );
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// Route helpers
// ---------------------------------------------------------------------------

fn route_learning_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-learning.json")
}

fn route_health_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-health.json")
}

fn route_autotune_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.json")
}

fn route_autotune_env_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.env")
}

fn summarize_route_health_state(path: &Path) -> String {
    let Some(report) = read_json_file(path) else {
        return "route_health=unknown".to_string();
    };
    let overall = report
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let score = report
        .get("summary")
        .and_then(|v| v.get("health_score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    format!(
        "route_health={} score={:.2} @ {}",
        overall, score, generated
    )
}

fn summarize_route_health_details(path: &Path) -> Option<String> {
    let report = read_json_file(path)?;
    let entries = report.get("entries")?.as_array()?;
    if entries.is_empty() {
        return Some("route_health_trace=no_entries".to_string());
    }
    let mut ranked = entries
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(|v| v.as_str())?;
            let tier = entry
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let health = entry
                .get("health_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reasons = entry
                .get("reasons")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some((key.to_string(), tier.to_string(), health, reasons))
        })
        .collect::<Vec<_>>();
    if ranked.is_empty() {
        return Some("route_health_trace=no_parseable_entries".to_string());
    }
    ranked.sort_by(|a, b| a.2.total_cmp(&b.2));
    let hottest = ranked
        .iter()
        .take(3)
        .map(|(key, tier, health, reasons)| {
            let reason_text = if reasons.is_empty() {
                "no_reasons".to_string()
            } else {
                reasons.join("|")
            };
            format!("{key} tier={tier} score={health:.2} reasons={reason_text}")
        })
        .collect::<Vec<_>>()
        .join(" ; ");
    Some(format!("route_health_trace={}", hottest))
}

// ---------------------------------------------------------------------------
// /ops budget
// ---------------------------------------------------------------------------

fn handle_ops_budget_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let budget = RepoReviewBudgetRuntime::from_env();
        emit_command_output(
            app,
            format!(
                "repo_review_budget profile={}\n\
                 repeat_threshold={} low_signal_threshold={} keep_repeat={} keep_low_signal={} min_signal_score={:.2}",
                budget.profile.as_str(),
                budget.repeat_threshold,
                budget.low_signal_threshold,
                budget.keep_repeat,
                budget.keep_low_signal,
                budget.min_signal_score
            ),
        );
        return Ok(CommandResult::Handled);
    }
    match args[0].to_ascii_lowercase().as_str() {
        "list" => emit_command_output(
            app,
            "Repo-review budget profiles:\n- balanced: default trim cadence\n- aggressive: trim repetitive discovery quickly\n- relaxed: allow broader exploration before trimming\n- off: effectively disable trimming",
        ),
        "clear" => {
            for key in [
                REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT,
                REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
                REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
                REPO_REVIEW_BUDGET_ENV_PROFILE,
            ] {
                crate::env_vars::remove_var(key);
            }
            emit_command_output(app, "Cleared repo-review budget runtime overrides.");
        }
        profile_raw => {
            let Some(profile) = RepoReviewBudgetProfile::parse(profile_raw) else {
                emit_command_output(
                    app,
                    "Usage: /ops budget [status|list|balanced|aggressive|relaxed|off|clear]",
                );
                return Ok(CommandResult::Handled);
            };
            apply_repo_review_budget_profile(profile);
            let budget = RepoReviewBudgetRuntime::from_env();
            emit_command_output(
                app,
                format!(
                    "repo_review_budget set to '{}' (repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2})",
                    profile.as_str(),
                    budget.repeat_threshold,
                    budget.low_signal_threshold,
                    budget.keep_repeat,
                    budget.keep_low_signal,
                    budget.min_signal_score
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /ops tool-profile
// ---------------------------------------------------------------------------

fn handle_ops_tool_profile_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());
    if args.is_empty()
        || args
            .first()
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "status" | "show"))
    {
        emit_command_output(
            app,
            format!(
                "repo_review_tool_profile mode={}\nUse `/ops tool-profile [off|balanced|focus]`.\nEscape hatch: include `allow all tools` or `disable narrowing` in your request.",
                mode
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("list") {
        emit_command_output(
            app,
            "Repo-review tool profile modes:\n- off: disable narrowing (open tool lane)\n- balanced: filter messaging/non-repo noise only\n- focus: balanced + stricter repo-first filtering",
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("clear") {
        crate::env_vars::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
        emit_command_output(
            app,
            "Cleared repo-review tool profile override (default=balanced).",
        );
        return Ok(CommandResult::Handled);
    }
    let next = args[0].to_ascii_lowercase();
    if !matches!(next.as_str(), "off" | "balanced" | "focus") {
        emit_command_output(
            app,
            "Usage: /ops tool-profile [status|list|off|balanced|focus|clear]",
        );
        return Ok(CommandResult::Handled);
    }
    crate::env_vars::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", next.as_str());
    emit_command_output(
        app,
        format!("repo_review_tool_profile mode set to `{}`", next),
    );
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /ops eval
// ---------------------------------------------------------------------------

pub(crate) async fn handle_ops_eval_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Eval controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let latest = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"));
            if let Some(path) = latest {
                let summary = summarize_gate_report(&path, "eval")
                    .unwrap_or_else(|| format!("latest eval report: {}", path.display()));
                emit_command_output(
                    app,
                    format!(
                        "{summary}\nRun `/ops eval run` to generate a fresh session-backed report."
                    ),
                );
            } else {
                emit_command_output(
                    app,
                    "No eval reports found yet. Run `/ops eval run` to generate one.",
                );
            }
            Ok(CommandResult::Handled)
        }
        "run" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-session-eval-harness.py --repo-root . --json",
            )
            .await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "latest" => {
            let Some(path) = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            else {
                emit_command_output(app, "No eval reports found.");
                return Ok(CommandResult::Handled);
            };
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
            emit_command_output(
                app,
                format!(
                    "Latest eval report: {}\n{}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string()),
                    raw
                ),
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops eval [status|run|latest]");
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// /qos
// ---------------------------------------------------------------------------

pub(crate) async fn handle_qos_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" | "show" => {
            let learning_path = route_learning_state_path();
            let health_path = route_health_state_path();
            let autotune_path = route_autotune_state_path();
            let autotune_env = route_autotune_env_path();
            let learning_entries = read_json_file(&learning_path)
                .and_then(|v| {
                    v.get("entries")
                        .and_then(|e| e.as_array())
                        .map(|arr| arr.len())
                })
                .unwrap_or(0usize);
            let health_summary = summarize_route_health_state(&health_path);
            let mut out = String::new();
            let _ = writeln!(out, "Provider QoS router");
            let _ = writeln!(
                out,
                "  route_learning_entries={} ({})",
                learning_entries,
                learning_path.display()
            );
            let _ = writeln!(out, "  {} ({})", health_summary, health_path.display());
            if let Some(trace) = summarize_route_health_details(&health_path) {
                let _ = writeln!(out, "  {}", trace);
            }
            let _ = writeln!(
                out,
                "  route_autotune_state={} ({})",
                if autotune_path.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_path.display()
            );
            let _ = writeln!(
                out,
                "  route_autotune_env={} ({})",
                if autotune_env.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_env.display()
            );
            let _ = writeln!(
                out,
                "  actions: /qos health | /qos autotune plan | /qos autotune apply"
            );
            emit_command_output(app, out.trim_end());
            Ok(CommandResult::Handled)
        }
        "health" => {
            let out = run_current_hermes_cli_command(&["route-health", "--json"]).await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "autotune" => {
            let action = args.get(1).copied().unwrap_or("plan").to_ascii_lowercase();
            let out = match action.as_str() {
                "plan" => {
                    run_current_hermes_cli_command(&["route-autotune", "plan", "--json"]).await?
                }
                "apply" => {
                    run_current_hermes_cli_command(&[
                        "route-autotune",
                        "apply",
                        "--apply",
                        "--json",
                    ])
                    .await?
                }
                _ => {
                    emit_command_output(app, "Usage: /qos autotune [plan|apply]");
                    return Ok(CommandResult::Handled);
                }
            };
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "help" => {
            emit_command_output(app, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// /ops skills-tier
// ---------------------------------------------------------------------------

fn handle_ops_skills_tier_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "skills_tier={} (bypass={})",
                skills::skills_execution_tier().as_str(),
                if skills::skills_tier_bypass_enabled() {
                    "ON"
                } else {
                    "OFF"
                }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let Some(next) = skills::SkillsExecutionTier::parse(args[0]) else {
        emit_command_output(
            app,
            "Usage: /ops skills-tier [status|trusted|balanced|open]",
        );
        return Ok(CommandResult::Handled);
    };
    crate::env_vars::set_var("HERMES_SKILLS_EXECUTION_TIER", next.as_str());
    emit_command_output(
        app,
        format!(
            "skills_tier set to '{}' for this runtime process.",
            next.as_str()
        ),
    );
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /ops gate
// ---------------------------------------------------------------------------

async fn handle_ops_gate_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" => {
            if let Some(repo_root) = discover_repo_root_for_about() {
                let report_dir = repo_root.join(".sync-reports");
                let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                    .and_then(|p| summarize_gate_report(&p, "eval_trend"))
                    .unwrap_or_else(|| "eval_trend=unknown".to_string());
                let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                    .and_then(|p| summarize_gate_report(&p, "slo_rollback"))
                    .unwrap_or_else(|| "slo_rollback=unknown".to_string());
                let elite = latest_json_report(&report_dir, "elite-sync-gate-")
                    .and_then(|p| summarize_gate_report(&p, "elite_sync_gate"))
                    .unwrap_or_else(|| "elite_sync_gate=unknown".to_string());
                emit_command_output(app, format!("{}\n{}\n{}", eval, slo, elite));
            } else {
                emit_command_output(app, "Gate status unavailable outside source checkout.");
            }
            Ok(CommandResult::Handled)
        }
        "eval" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-eval-trend-gate.py --allow-missing-baseline --json",
            )
            .await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "elite" => {
            let out =
                run_ops_shell_command("python3 scripts/run-elite-sync-gate.py --json").await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "slo" => {
            let check_cmd = std::env::var("HERMES_SLO_CHECK_CMD").ok();
            let rollback_cmd = std::env::var("HERMES_SLO_ROLLBACK_CMD").ok();
            let (Some(check), Some(rollback)) = (check_cmd, rollback_cmd) else {
                emit_command_output(
                    app,
                    "Set HERMES_SLO_CHECK_CMD and HERMES_SLO_ROLLBACK_CMD, then run `/ops gate slo`.",
                );
                return Ok(CommandResult::Handled);
            };
            let cmd = format!(
                "python3 scripts/run-slo-auto-rollback.py --check-cmd {} --rollback-cmd {} --json",
                shell_escape(&check),
                shell_escape(&rollback)
            );
            let out = run_ops_shell_command(&cmd).await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops gate [status|eval|elite|slo]");
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// /ops evolve
// ---------------------------------------------------------------------------

pub(crate) async fn handle_ops_evolve_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Self-evolution controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let summary = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "self_evolution"))
                .unwrap_or_else(|| "self_evolution=unknown (no reports yet)".to_string());
            emit_command_output(
                app,
                format!(
                    "{}\nRun `/ops evolve run` to execute the loop now.",
                    summary
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let cmd = if let Some(obj) = app.session_objective.as_deref() {
                format!(
                    "python3 scripts/run-self-evolution-loop.py --json --objective {}",
                    shell_escape(obj)
                )
            } else {
                "python3 scripts/run-self-evolution-loop.py --json".to_string()
            };
            let out = run_ops_shell_command(&cmd).await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest_json_report(&report_dir, "self-evolution-loop-") else {
                emit_command_output(
                    app,
                    "No self-evolution reports found. Run `/ops evolve run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = self_evolution_recommendations(&path);
            if recs.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No recommendations found in {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                );
            } else {
                let file_label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                emit_command_output(
                    app,
                    format!(
                        "Self-evolution recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops evolve [status|run|recommend]");
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// /ops autopilot
// ---------------------------------------------------------------------------

pub(crate) async fn handle_ops_autopilot_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "advisory".to_string());
    let profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());

    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Autopilot controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    let latest = latest_json_report(&report_dir, "performance-autopilot-");

    match sub.as_str() {
        "status" => {
            let summary = latest
                .as_ref()
                .and_then(|p| summarize_performance_autopilot_report(p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown (no reports yet)".to_string());
            emit_command_output(
                app,
                format!(
                    "{}\nmode={} profile={}\nUse `/ops autopilot run` then `/ops autopilot recommend`.",
                    summary, mode, profile
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let out = run_ops_shell_command(
                "python3 scripts/run-performance-autopilot.py --repo-root . --json",
            )
            .await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest else {
                emit_command_output(
                    app,
                    "No performance autopilot reports found. Run `/ops autopilot run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = performance_autopilot_recommendations(&path);
            if recs.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No recommendations found in {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                );
            } else {
                let file_label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                emit_command_output(
                    app,
                    format!(
                        "Autopilot recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        "apply" => {
            let env_path =
                report_dir.join(format!("performance-autopilot-env-{}.env", app.session_id));
            let cmd = format!(
                "python3 scripts/run-performance-autopilot.py --repo-root . --apply-env {} --json",
                shell_escape(&env_path.display().to_string())
            );
            let out = run_ops_shell_command(&cmd).await?;
            let kvs = parse_env_file_kv(&env_path);
            let mut applied = Vec::new();
            for (k, v) in kvs {
                if AUTOPILOT_ALLOWED_ENV_KEYS
                    .iter()
                    .any(|allowed| *allowed == k)
                {
                    crate::env_vars::set_var(&k, &v);
                    applied.push((k, v));
                }
            }
            write_autopilot_runtime_event(&report_dir, &app.session_id, &mode, &profile, &applied);
            let applied_keys = if applied.is_empty() {
                "(none)".to_string()
            } else {
                applied
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            emit_command_output(
                app,
                format!(
                    "{out}\n\nApplied safe runtime knobs: {applied_keys}\nmode={mode} profile={profile}\nlog: {}",
                    report_dir
                        .join("performance-autopilot-runtime.jsonl")
                        .display()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "profile" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => {
                    emit_command_output(app, format!("autopilot profile={profile} (mode={mode})"))
                }
                Some("list") => emit_command_output(
                    app,
                    "Autopilot profiles:\n- balanced: default stability/perf mix\n- throughput: lower latency and tighter loop cadence\n- quality: stronger verification and replay focus\n- reliability: prioritize retries/recovery and degraded-source tolerance\n- safety: strictest gate posture with conservative policy knobs",
                ),
                Some("balanced" | "throughput" | "quality" | "reliability" | "safety") => {
                    let value = next.unwrap_or_else(|| "off".to_string());
                    crate::env_vars::set_var("HERMES_PERF_AUTOPILOT_PROFILE", &value);
                    emit_command_output(app, format!("autopilot profile set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        app,
                        format!(
                            "Unknown profile '{}'. Use `/ops autopilot profile list`.",
                            other
                        ),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "mode" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => emit_command_output(app, format!("autopilot mode={mode}")),
                Some("list") => emit_command_output(
                    app,
                    "Autopilot modes:\n- off: disabled\n- advisory: report + recommendations only\n- enforce: intended to pair with `/ops autopilot apply` during incidents",
                ),
                Some("off" | "advisory" | "enforce") => {
                    let value = next.unwrap_or_else(|| "advisory".to_string());
                    crate::env_vars::set_var("HERMES_PERF_AUTOPILOT_MODE", &value);
                    emit_command_output(app, format!("autopilot mode set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        app,
                        format!("Unknown mode '{}'. Use `/ops autopilot mode list`.", other),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "clear" => {
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_MODE");
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_PROFILE");
            crate::env_vars::remove_var("HERMES_PERF_AUTOPILOT_STATUS");
            emit_command_output(
                app,
                "Cleared autopilot runtime overrides (mode/profile/status).",
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /ops autopilot [status|run|recommend|apply|profile [status|list|balanced|throughput|quality|reliability|safety]|mode [status|list|off|advisory|enforce]|clear]",
            );
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// /ops cockpit
// ---------------------------------------------------------------------------

async fn handle_ops_cockpit_command(
    app: &mut App,
    _args: &[&str],
) -> Result<CommandResult, AgentError> {
    let counters = app.tool_registry.policy_counters();
    let budget = RepoReviewBudgetRuntime::from_env();
    let board = render_mission_board(
        &app.current_model,
        app.session_objective.as_deref(),
        background::background_job_counts(),
    )
    .await?;
    let route_health = summarize_route_health_state(&route_health_state_path());
    let eval_summary = if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        latest_json_report(&report_dir, "session-eval-harness-")
            .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            .and_then(|p| summarize_gate_report(&p, "eval"))
            .unwrap_or_else(|| "eval=unknown".to_string())
    } else {
        "eval=unavailable".to_string()
    };
    let snapshot_count =
        session::enumerate_saved_sessions(&hermes_config::hermes_home().join("sessions")).len();
    let mut out = String::new();
    out.push_str("Ops Cockpit\n");
    out.push_str("===========\n");
    let _ = writeln!(out, "session: {}", app.session_id);
    let _ = writeln!(out, "model: {}", app.current_model);
    let _ = writeln!(
        out,
        "policy: profile={} mode={} preset={} sandbox={} skills_tier={}",
        policy::current_policy_profile_name(),
        std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
        std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "relaxed".into()),
        std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE").unwrap_or_else(|_| "balanced".into()),
        std::env::var("HERMES_SKILLS_EXECUTION_TIER").unwrap_or_else(|_| "balanced".into())
    );
    let _ = writeln!(
        out,
        "planner_capability_router={} compaction_governance={} replay_trace={}",
        plan_capability_mode().as_str(),
        compress::compaction_governance_mode().as_str(),
        if replay_enabled_runtime() {
            "on"
        } else {
            "off"
        }
    );
    let _ = writeln!(
        out,
        "repo_review_budget: profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}",
        budget.profile.as_str(),
        budget.repeat_threshold,
        budget.low_signal_threshold,
        budget.keep_repeat,
        budget.keep_low_signal,
        budget.min_signal_score
    );
    let _ = writeln!(
        out,
        "policy_counters: allow={} deny={} audit_only={} simulate={} would_block={}",
        counters.allow, counters.deny, counters.audit_only, counters.simulate, counters.would_block
    );
    let _ = writeln!(
        out,
        "qos: {} | learning_entries={} | snapshots={}",
        route_health,
        read_json_file(&route_learning_state_path())
            .and_then(|v| v
                .get("entries")
                .and_then(|e| e.as_array())
                .map(|arr| arr.len()))
            .unwrap_or(0usize),
        snapshot_count
    );
    let _ = writeln!(out, "eval: {}", eval_summary);
    out.push('\n');
    out.push_str(&board);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /ops — main dispatcher
// ---------------------------------------------------------------------------

pub(crate) async fn handle_ops_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let yolo = !app.config.approval.require_approval;
        let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "enforce".to_string());
        let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let counters = app.tool_registry.policy_counters();
        let dashboard_status = {
            let raw = app
                .tool_registry
                .dispatch_async("dashboard_control", serde_json::json!({"action":"status"}))
                .await;
            let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(
                |_| serde_json::json!({"enabled":false,"url":"unknown","error":"unparseable"}),
            );
            dashboard_status_line_from_payload(&parsed)
        };
        let gate_status = if let Some(repo_root) = discover_repo_root_for_about() {
            let report_dir = repo_root.join(".sync-reports");
            let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                .and_then(|p| summarize_gate_report(&p, "eval"))
                .unwrap_or_else(|| "eval=unknown".to_string());
            let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                .and_then(|p| summarize_gate_report(&p, "slo"))
                .unwrap_or_else(|| "slo=unknown".to_string());
            let evolve = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "evolve"))
                .unwrap_or_else(|| "evolve=unknown".to_string());
            let autopilot = latest_json_report(&report_dir, "performance-autopilot-")
                .and_then(|p| summarize_performance_autopilot_report(&p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown".to_string());
            format!("{eval}; {slo}; {evolve}; {autopilot}")
        } else {
            "unavailable (non-source checkout)".to_string()
        };
        let autopilot_mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "advisory".to_string());
        let autopilot_profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let repo_review_budget = RepoReviewBudgetRuntime::from_env();
        let tool_profile_mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());

        let out = format!(
            "Operator Control Plane\n\
             \n\
             Runtime:\n\
               session:      {}\n\
               model:        {}\n\
               personality:  {}\n\
             \n\
             Controls:\n\
               yolo:         {}\n\
               mouse:        {}\n\
               statusbar:    ON\n\
               reasoning:    `/ops reasoning status` + `/ops reasoning set ...`\n\
               raw:          toggle via `/ops raw`\n\
               verbose:      toggle via `/ops verbose`\n\
             \n\
             Policy/Gates:\n\
               tool_policy:  mode={} preset={}\n\
               autopilot:    mode={} profile={}\n\
               tool_profile: {}\n\
               repo_budget:  profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}\n\
               task_depth:   {}\n\
               policy_counts allow={} deny={} audit_only={} simulate={} would_block={}\n\
               skills_tier:  {} (bypass={})\n\
               {}\n\
               gate_status:  {}\n\
             \n\
             Quick actions:\n\
               /ops model [provider|provider:model]\n\
               /ops mode [status|list|strict|standard|dev]\n\
               /ops personality [list|name]\n\
               /ops mouse [on|off|toggle]\n\
               /ops yolo\n\
               /ops reasoning [status|on|off|toggle|set <level>]\n\
               /ops raw [on|off|toggle|once|trace ...]\n\
               /ops verbose\n\
               /ops dashboard [status|on|off|url] [host] [port]\n\
               /ops skills-tier [status|trusted|balanced|open]\n\
               /ops tool-profile [status|list|off|balanced|focus]\n\
               /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
               /ops evolve [status|run|recommend]\n\
               /ops eval [status|run|latest]\n\
               /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
               /ops gate [status|eval|elite|slo]\n\
               /ops cockpit\n\
               /mission [status|init]\n\
               /ops help",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            if yolo { "ON" } else { "OFF" },
            if app.mouse_enabled() { "ON" } else { "OFF" },
            policy_mode,
            policy_preset,
            autopilot_mode,
            autopilot_profile,
            tool_profile_mode,
            repo_review_budget.profile.as_str(),
            repo_review_budget.repeat_threshold,
            repo_review_budget.low_signal_threshold,
            repo_review_budget.keep_repeat,
            repo_review_budget.keep_low_signal,
            repo_review_budget.min_signal_score,
            task_depth_runtime_summary(),
            counters.allow,
            counters.deny,
            counters.audit_only,
            counters.simulate,
            counters.would_block,
            skills::skills_execution_tier().as_str(),
            if skills::skills_tier_bypass_enabled() {
                "ON"
            } else {
                "OFF"
            },
            dashboard_status,
            gate_status,
        );
        emit_command_output(app, out);
        return Ok(CommandResult::Handled);
    }

    match args[0].to_ascii_lowercase().as_str() {
        "help" => {
            emit_command_output(
                app,
                "Operator control plane commands:\n\
                 - /ops status\n\
                 - /ops model [provider|provider:model]\n\
                 - /ops mode [status|list|strict|standard|dev]\n\
                 - /ops personality [list|name]\n\
                 - /ops mouse [on|off|toggle]\n\
                 - /ops yolo\n\
                 - /ops reasoning [status|on|off|toggle|set <level>]\n\
                 - /ops raw [on|off|toggle|once|trace ...]\n\
                 - /ops verbose\n\
                 - /ops statusbar\n\
                 - /ops dashboard [status|on|off|url] [host] [port]\n\
                 - /ops skills-tier [status|trusted|balanced|open]\n\
                 - /ops tool-profile [status|list|off|balanced|focus]\n\
                 - /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
                 - /ops evolve [status|run|recommend]\n\
                 - /ops eval [status|run|latest]\n\
                 - /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
                 - /ops gate [status|eval|elite|slo]\n\
                 - /ops cockpit\n\
                 - /mission [status|init]",
            );
            Ok(CommandResult::Handled)
        }
        "model" => super::model::handle_model_command(app, &args[1..]).await,
        "mode" => policy::handle_policy_command(app, &args[1..]),
        "personality" => super::handle_personality_command(app, &args[1..]),
        "mouse" => super::runtime_ui::handle_mouse_command(app, &args[1..]),
        "yolo" => super::handle_yolo_command(app),
        "reasoning" => super::handle_reasoning_command(app, &args[1..]),
        "raw" => super::handle_raw_command(app, &args[1..]),
        "verbose" => super::handle_verbose_command(app),
        "statusbar" => super::runtime_ui::handle_statusbar_command(app),
        "dashboard" => handle_dashboard_command(app, &args[1..]).await,
        "skills-tier" => handle_ops_skills_tier_command(app, &args[1..]),
        "tool-profile" | "toolprofile" | "tool_profile" => {
            handle_ops_tool_profile_command(app, &args[1..])
        }
        "budget" => handle_ops_budget_command(app, &args[1..]),
        "evolve" => handle_ops_evolve_command(app, &args[1..]).await,
        "eval" => handle_ops_eval_command(app, &args[1..]).await,
        "autopilot" => handle_ops_autopilot_command(app, &args[1..]).await,
        "gate" => handle_ops_gate_command(app, &args[1..]).await,
        "cockpit" => handle_ops_cockpit_command(app, &args[1..]).await,
        other => {
            emit_command_output(
                app,
                format!(
                    "Unknown /ops target '{}'. Try `/ops help` for available controls.",
                    other
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// TaskDepthProfile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskDepthProfile {
    Shallow,
    Balanced,
    Deep,
    Max,
}

impl TaskDepthProfile {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "shallow" | "fast" => Some(Self::Shallow),
            "balanced" | "default" => Some(Self::Balanced),
            "deep" | "thorough" => Some(Self::Deep),
            "max" | "exhaustive" => Some(Self::Max),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Shallow => "shallow",
            Self::Balanced => "balanced",
            Self::Deep => "deep",
            Self::Max => "max",
        }
    }
}

fn set_env_var_u64(key: &str, value: u64) {
    crate::env_vars::set_var(key, value.to_string());
}

fn set_env_var_f64(key: &str, value: f64) {
    crate::env_vars::set_var(key, format!("{value:.2}"));
}

pub(crate) fn apply_task_depth_profile(profile: TaskDepthProfile) {
    crate::env_vars::set_var("HERMES_TASK_DEPTH_PROFILE", profile.as_str());
    match profile {
        TaskDepthProfile::Shallow => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 18);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 10);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 1);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 6);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 2800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 5200.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "aggressive");
        }
        TaskDepthProfile::Balanced => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 250);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 12);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 8);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 3500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 6500.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
        TaskDepthProfile::Deep => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 120);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 6);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 3);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 10);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 4800.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 9000.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "relaxed");
        }
        TaskDepthProfile::Max => {
            set_env_var_u64("HERMES_MAX_ITERATIONS", 250);
            set_env_var_u64("HERMES_TOOL_CALL_MAX_CONCURRENCY", 5);
            set_env_var_u64("HERMES_MAX_DELEGATE_DEPTH", 4);
            set_env_var_u64("HERMES_PERF_GOV_WINDOW", 12);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_WARN_MS", 6500.0);
            set_env_var_f64("HERMES_PERF_GOV_LATENCY_CRITICAL_MS", 12000.0);
            crate::env_vars::set_var("HERMES_REPO_REVIEW_BUDGET_PROFILE", "off");
        }
    }
}

fn current_task_depth_profile() -> TaskDepthProfile {
    std::env::var("HERMES_TASK_DEPTH_PROFILE")
        .ok()
        .as_deref()
        .and_then(TaskDepthProfile::parse)
        .unwrap_or(TaskDepthProfile::Balanced)
}

pub(crate) fn task_depth_runtime_summary() -> String {
    let profile = current_task_depth_profile();
    let max_iters = std::env::var("HERMES_MAX_ITERATIONS").unwrap_or_else(|_| "250".to_string());
    let tool_concurrency =
        std::env::var("HERMES_TOOL_CALL_MAX_CONCURRENCY").unwrap_or_else(|_| "12".to_string());
    let delegate_depth =
        std::env::var("HERMES_MAX_DELEGATE_DEPTH").unwrap_or_else(|_| "4".to_string());
    let repo_budget =
        std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE").unwrap_or_else(|_| "off".to_string());
    format!(
        "task_depth profile={} max_iterations={} tool_concurrency={} max_delegate_depth={} repo_budget_profile={}",
        profile.as_str(),
        max_iters,
        tool_concurrency,
        delegate_depth,
        repo_budget
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock;
    use tempfile::tempdir;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    #[test]
    fn repo_review_budget_profile_application_sets_expected_env() {
        let _guard = env_test_lock();
        apply_repo_review_budget_profile(RepoReviewBudgetProfile::Aggressive);
        let runtime = RepoReviewBudgetRuntime::from_env();
        assert_eq!(runtime.profile, RepoReviewBudgetProfile::Aggressive);
        assert_eq!(runtime.repeat_threshold, 1);
        assert_eq!(runtime.low_signal_threshold, 1);
        assert_eq!(runtime.keep_repeat, 1);
        assert_eq!(runtime.keep_low_signal, 1);
        assert!(runtime.min_signal_score >= 0.34);

        apply_repo_review_budget_profile(RepoReviewBudgetProfile::Balanced);
        let runtime_balanced = RepoReviewBudgetRuntime::from_env();
        assert_eq!(runtime_balanced.profile, RepoReviewBudgetProfile::Balanced);
        assert_eq!(runtime_balanced.repeat_threshold, 2);
        assert_eq!(runtime_balanced.low_signal_threshold, 2);
    }

    #[test]
    fn task_depth_profile_application_sets_expected_env() {
        let _guard = env_test_lock();
        apply_task_depth_profile(TaskDepthProfile::Max);
        assert_eq!(
            std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
            Some("max")
        );
        assert_eq!(
            std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
            Some("250")
        );
        assert_eq!(
            std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
                .ok()
                .as_deref(),
            Some("off")
        );

        apply_task_depth_profile(TaskDepthProfile::Balanced);
        assert_eq!(
            std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
            Some("balanced")
        );
        assert_eq!(
            std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
            Some("250")
        );
    }

    #[test]
    fn test_autocomplete_includes_evolve() {
        let results = super::super::autocomplete("/evo");
        assert!(results.contains(&"/evolve"));
    }

    #[test]
    fn summarize_self_evolution_report_formats_fields() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("self-evolution-loop-test.json");
        std::fs::write(
            &path,
            r#"{
  "ok": false,
  "generated_at": "2026-05-02T00:00:00Z",
  "summary": { "intelligence_index": 66.67 },
  "recommendations": [{"id":"PARITY_DRIFT"}]
}"#,
        )
        .expect("write report");
        let line = summarize_self_evolution_report(&path, "self_evolution").expect("summary");
        assert!(line.contains("self_evolution=fail"));
        assert!(line.contains("idx=66.67"));
        assert!(line.contains("recs=1"));
    }

    #[test]
    fn self_evolution_recommendations_extracts_lines() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("self-evolution-loop-test.json");
        std::fs::write(
            &path,
            r#"{
  "recommendations": [
    {
      "id": "EVAL_REGRESSION",
      "severity": "P0",
      "title": "Recover eval trend before promotion",
      "command": "python3 scripts/run-eval-trend-gate.py --json"
    }
  ]
}"#,
        )
        .expect("write report");
        let lines = self_evolution_recommendations(&path);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("EVAL_REGRESSION"));
        assert!(lines[0].contains("python3 scripts/run-eval-trend-gate.py --json"));
    }

    #[test]
    fn summarize_performance_autopilot_report_formats_fields() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("performance-autopilot-test.json");
        std::fs::write(
            &path,
            r#"{
  "ok": true,
  "generated_at": "2026-05-08T00:00:00Z",
  "recommendations": [
    {"id":"PERF_STABLE", "severity":"P3", "title":"stable", "recommendation":"none"}
  ]
}"#,
        )
        .expect("write report");
        let line = summarize_performance_autopilot_report(&path, "autopilot").expect("summary");
        assert!(line.contains("autopilot=pass"));
        assert!(line.contains("recs=1"));
        assert!(line.contains("severe=0"));
    }

    #[test]
    fn performance_autopilot_recommendations_extract_lines() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("performance-autopilot-test.json");
        std::fs::write(
            &path,
            r#"{
  "recommendations": [
    {
      "id":"HOTPATH_SLOW",
      "severity":"P1",
      "title":"Tool policy hot-path latency above target",
      "recommendation":"Keep HERMES_TOOL_POLICY_PRESET=standard"
    }
  ]
}"#,
        )
        .expect("write report");
        let recs = performance_autopilot_recommendations(&path);
        assert_eq!(recs.len(), 1);
        assert!(recs[0].contains("HOTPATH_SLOW"));
        assert!(recs[0].contains("recommendation"));
    }

    #[test]
    fn parse_env_file_kv_ignores_comments() {
        let tmp = tempdir().expect("tempdir");
        let path = tmp.path().join("autopilot.env");
        std::fs::write(
            &path,
            "# comment\nHERMES_TOOL_POLICY_PRESET=standard\n \nINVALID_LINE\nHERMES_REPLAY_ENABLED=1\n",
        )
        .expect("write env");
        let kvs = parse_env_file_kv(&path);
        assert_eq!(kvs.len(), 2);
        assert_eq!(kvs[0].0, "HERMES_TOOL_POLICY_PRESET");
        assert_eq!(kvs[1].0, "HERMES_REPLAY_ENABLED");
    }

    #[test]
    fn test_autocomplete_includes_autopilot() {
        let results = super::super::autocomplete("/auto");
        assert!(results.contains(&"/autopilot"));
    }
}
