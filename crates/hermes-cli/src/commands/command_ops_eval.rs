#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkillsExecutionTier {
    Trusted,
    Balanced,
    Open,
}

impl SkillsExecutionTier {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trusted" => Some(Self::Trusted),
            "balanced" => Some(Self::Balanced),
            "open" | "permissive" => Some(Self::Open),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Balanced => "balanced",
            Self::Open => "open",
        }
    }
}

fn skills_execution_tier() -> SkillsExecutionTier {
    std::env::var("HERMES_SKILLS_EXECUTION_TIER")
        .ok()
        .as_deref()
        .and_then(SkillsExecutionTier::parse)
        .unwrap_or(SkillsExecutionTier::Balanced)
}

fn skills_tier_bypass_enabled() -> bool {
    std::env::var("HERMES_SKILLS_TIER_BYPASS")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn skills_action_blocked_by_tier(
    tier: SkillsExecutionTier,
    action: &str,
    name: Option<&str>,
) -> bool {
    let name_lc = name.map(|v| v.to_ascii_lowercase());
    match tier {
        SkillsExecutionTier::Trusted => {
            matches!(
                action,
                "install" | "update" | "sync" | "publish" | "uninstall" | "reset" | "subscribe"
            ) || (action == "tap" && matches!(name_lc.as_deref(), Some("add" | "remove")))
                || (action == "opt-in" && matches!(name_lc.as_deref(), Some("--sync")))
                || (action == "opt-out" && matches!(name_lc.as_deref(), Some("--remove")))
                || (action == "snapshot" && matches!(name_lc.as_deref(), Some("import")))
        }
        SkillsExecutionTier::Balanced => {
            matches!(action, "publish" | "reset")
                || (action == "opt-out" && matches!(name_lc.as_deref(), Some("--remove")))
                || (action == "snapshot" && matches!(name_lc.as_deref(), Some("import")))
        }
        SkillsExecutionTier::Open => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoReviewBudgetProfile {
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
    std::env::set_var(
        REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
        repeat_threshold.to_string(),
    );
    std::env::set_var(
        REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
        low_signal_threshold.to_string(),
    );
    std::env::set_var(REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT, keep_repeat.to_string());
    std::env::set_var(
        REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
        keep_low_signal.to_string(),
    );
    std::env::set_var(
        REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
        format!("{:.3}", min_signal_score),
    );
    std::env::set_var(REPO_REVIEW_BUDGET_ENV_PROFILE, profile.as_str());
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

fn summarize_gate_report(path: &Path, key: &str) -> Option<String> {
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

fn utc_compact_stamp() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string()
}

fn system_time_rfc3339(time: SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(time).to_rfc3339()
}

fn normalize_session_role(raw: Option<&str>) -> &'static str {
    match raw.unwrap_or_default().trim().to_ascii_lowercase().as_str() {
        "assistant" => "assistant",
        "user" => "user",
        "system" => "system",
        "tool" => "tool",
        _ => "unknown",
    }
}

fn text_has_tool_markers(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "<tool_call",
        "<tool_use",
        "tool_call",
        "\"tool\":",
        "\"tool_name\":",
        "`tool`",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn text_has_patch_markers(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "[objective_patch]",
        "exists_now=true",
        "verified_exists=true",
        "apply_patch",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[derive(Debug, Clone, Serialize)]
struct SessionEvalStats {
    name: String,
    message_count: usize,
    user_count: usize,
    assistant_count: usize,
    has_tool_activity: bool,
    has_objective_activity: bool,
    has_patch_evidence: bool,
    modified_at: String,
}

fn load_session_eval_stats(path: &Path, modified: SystemTime) -> Option<SessionEvalStats> {
    let doc = read_json_file(path)?;
    let messages = doc.get("messages")?.as_array()?;
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut has_tool_activity = false;
    let mut has_objective_activity = false;
    let mut has_patch_evidence = false;

    for message in messages.iter().filter_map(|m| m.as_object()) {
        let role = normalize_session_role(message.get("role").and_then(|v| v.as_str()));
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        match role {
            "user" => user_count += 1,
            "assistant" => assistant_count += 1,
            _ => {}
        }
        if text_has_tool_markers(content) {
            has_tool_activity = true;
        }
        let lower = content.to_ascii_lowercase();
        if lower.contains("/objective") || lower.contains("[objective_") {
            has_objective_activity = true;
        }
        if text_has_patch_markers(content) {
            has_patch_evidence = true;
        }
    }

    Some(SessionEvalStats {
        name: path.file_stem()?.to_string_lossy().to_string(),
        message_count: messages.len(),
        user_count,
        assistant_count,
        has_tool_activity,
        has_objective_activity,
        has_patch_evidence,
        modified_at: system_time_rfc3339(modified),
    })
}

fn load_latest_session_eval_stats(
    sessions_dir: &Path,
    max_sessions: usize,
) -> Vec<SessionEvalStats> {
    let mut entries: Vec<(PathBuf, SystemTime)> = std::fs::read_dir(sessions_dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.filter_map(|e| e.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((path, modified))
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    entries
        .into_iter()
        .take(max_sessions.max(1))
        .filter_map(|(path, modified)| load_session_eval_stats(&path, modified))
        .collect()
}

fn median_usize(values: &[usize]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    } else {
        sorted[mid] as f64
    }
}

fn build_session_eval_report(
    repo_root: &Path,
    sessions_dir: &Path,
    sessions: &[SessionEvalStats],
) -> serde_json::Value {
    let message_counts: Vec<usize> = sessions.iter().map(|s| s.message_count).collect();
    let total_messages: usize = message_counts.iter().sum();
    let avg_messages = if sessions.is_empty() {
        0.0
    } else {
        total_messages as f64 / sessions.len() as f64
    };
    let tool_sessions = sessions.iter().filter(|s| s.has_tool_activity).count();
    let objective_sessions = sessions.iter().filter(|s| s.has_objective_activity).count();
    let patch_sessions = sessions.iter().filter(|s| s.has_patch_evidence).count();
    let user_turns: usize = sessions.iter().map(|s| s.user_count).sum();
    let assistant_turns: usize = sessions.iter().map(|s| s.assistant_count).sum();
    let latest = sessions.first().map(|s| s.modified_at.clone());
    let min_tool_sessions = std::cmp::max(1usize, sessions.len() / 5);
    let mut reasons = Vec::new();
    if sessions.is_empty() {
        reasons.push("no_saved_sessions");
    }
    if avg_messages < 2.0 {
        reasons.push("avg_messages_too_low");
    }
    if assistant_turns < user_turns {
        reasons.push("assistant_turns_below_user_turns");
    }
    if tool_sessions < min_tool_sessions {
        reasons.push("low_tool_activity_ratio");
    }
    let ok = !sessions.is_empty()
        && avg_messages >= 2.0
        && assistant_turns >= user_turns
        && tool_sessions >= min_tool_sessions;

    serde_json::json!({
        "ok": ok,
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "sessions_dir": sessions_dir,
        "summary": {
            "sessions_analyzed": sessions.len(),
            "avg_messages_per_session": (avg_messages * 100.0).round() / 100.0,
            "median_messages_per_session": (median_usize(&message_counts) * 100.0).round() / 100.0,
            "tool_activity_sessions": tool_sessions,
            "objective_activity_sessions": objective_sessions,
            "patch_evidence_sessions": patch_sessions,
            "user_turns": user_turns,
            "assistant_turns": assistant_turns,
            "latest_session_modified_at": latest,
        },
        "reasons": reasons,
        "sessions": sessions.iter().take(10).collect::<Vec<_>>(),
    })
}

fn write_json_report(path: &Path, report: &serde_json::Value) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(report)
        .map_err(|e| AgentError::Config(format!("serialize report: {e}")))?;
    std::fs::write(path, format!("{raw}\n"))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn run_session_eval_harness_native(
    repo_root: &Path,
    sessions_dir: &Path,
    max_sessions: usize,
    out_json: Option<&Path>,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let sessions = load_latest_session_eval_stats(sessions_dir, max_sessions);
    let report = build_session_eval_report(repo_root, sessions_dir, &sessions);
    let out_path = out_json.map(PathBuf::from).unwrap_or_else(|| {
        repo_root
            .join(".sync-reports")
            .join(format!("session-eval-harness-{}.json", utc_compact_stamp()))
    });
    write_json_report(&out_path, &report)?;
    Ok((report, out_path))
}

fn duration_json_to_secs(raw: Option<&serde_json::Value>) -> f64 {
    match raw {
        Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(s)) => {
            s.trim().trim_end_matches('s').parse::<f64>().unwrap_or(0.0)
        }
        Some(serde_json::Value::Object(map)) => {
            let secs = map
                .get("secs")
                .or_else(|| map.get("seconds"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let nanos = map
                .get("nanos")
                .or_else(|| map.get("nanoseconds"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            secs + nanos / 1_000_000_000.0
        }
        _ => 0.0,
    }
}

fn json_f64(raw: Option<&serde_json::Value>) -> f64 {
    raw.and_then(|v| v.as_f64()).unwrap_or(0.0)
}

fn extract_eval_metrics(record: &serde_json::Value) -> serde_json::Value {
    let metrics = record
        .get("metrics")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let total = json_f64(metrics.get("total")).max(1.0);
    let total_duration = duration_json_to_secs(metrics.get("total_duration"));
    serde_json::json!({
        "total": total,
        "pass_at_1": json_f64(metrics.get("pass_at_1")),
        "mean_task_duration_secs": total_duration / total,
        "total_cost_usd": json_f64(metrics.get("total_cost_usd")),
    })
}

fn latest_eval_files(evals_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(evals_dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.filter_map(|e| e.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    files.sort_by(|a, b| {
        let am = a
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let bm = b
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        bm.cmp(&am).then_with(|| a.cmp(b))
    });
    files
}

fn relative_change(current: f64, baseline: f64) -> f64 {
    if baseline <= 0.0 {
        if current <= 0.0 {
            0.0
        } else {
            1.0
        }
    } else {
        (current - baseline) / baseline
    }
}

fn eval_metric(metrics: &serde_json::Value, key: &str) -> f64 {
    metrics.get(key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

#[derive(Debug, Clone, Copy)]
struct EvalTrendGateOptions {
    allow_missing_baseline: bool,
    max_pass_at_1_drop: f64,
    max_mean_task_duration_increase: f64,
    max_cost_increase: f64,
}

impl Default for EvalTrendGateOptions {
    fn default() -> Self {
        Self {
            allow_missing_baseline: false,
            max_pass_at_1_drop: 0.03,
            max_mean_task_duration_increase: 0.40,
            max_cost_increase: 0.50,
        }
    }
}

fn run_eval_trend_gate_native(
    repo_root: &Path,
    current: Option<&Path>,
    baseline: Option<&Path>,
    report_path: Option<&Path>,
    options: EvalTrendGateOptions,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let out_path = report_path.map(PathBuf::from).unwrap_or_else(|| {
        repo_root
            .join(".sync-reports")
            .join(format!("eval-trend-gate-{}.json", utc_compact_stamp()))
    });
    let latest = latest_eval_files(&repo_root.join("evals"));
    let current_path = current
        .map(PathBuf::from)
        .or_else(|| latest.first().cloned());
    let baseline_path = baseline
        .map(PathBuf::from)
        .or_else(|| latest.get(1).cloned());

    let missing_inputs = current_path.as_ref().is_none_or(|p| !p.exists())
        || baseline_path.as_ref().is_none_or(|p| !p.exists());
    if missing_inputs {
        let report = serde_json::json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "ok": options.allow_missing_baseline,
            "reason": "missing_eval_inputs",
            "allow_missing_baseline": options.allow_missing_baseline,
            "current_path": current_path,
            "baseline_path": baseline_path,
        });
        write_json_report(&out_path, &report)?;
        return Ok((report, out_path));
    }

    let current_path = current_path.expect("checked present");
    let baseline_path = baseline_path.expect("checked present");
    let current_record = read_json_file(&current_path).ok_or_else(|| {
        AgentError::Config(format!("read eval report {}", current_path.display()))
    })?;
    let baseline_record = read_json_file(&baseline_path).ok_or_else(|| {
        AgentError::Config(format!("read eval report {}", baseline_path.display()))
    })?;
    let current_metrics = extract_eval_metrics(&current_record);
    let baseline_metrics = extract_eval_metrics(&baseline_record);
    let pass_drop =
        eval_metric(&baseline_metrics, "pass_at_1") - eval_metric(&current_metrics, "pass_at_1");
    let duration_increase = relative_change(
        eval_metric(&current_metrics, "mean_task_duration_secs"),
        eval_metric(&baseline_metrics, "mean_task_duration_secs"),
    );
    let cost_increase = relative_change(
        eval_metric(&current_metrics, "total_cost_usd"),
        eval_metric(&baseline_metrics, "total_cost_usd"),
    );
    let checks = vec![
        serde_json::json!({
            "name": "pass_at_1_drop",
            "value": pass_drop,
            "limit": options.max_pass_at_1_drop,
            "ok": pass_drop <= options.max_pass_at_1_drop,
        }),
        serde_json::json!({
            "name": "mean_task_duration_increase",
            "value": duration_increase,
            "limit": options.max_mean_task_duration_increase,
            "ok": duration_increase <= options.max_mean_task_duration_increase,
        }),
        serde_json::json!({
            "name": "total_cost_increase",
            "value": cost_increase,
            "limit": options.max_cost_increase,
            "ok": cost_increase <= options.max_cost_increase,
        }),
    ];
    let gate_ok = checks
        .iter()
        .all(|check| check.get("ok").and_then(|v| v.as_bool()).unwrap_or(false));
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "ok": gate_ok,
        "current_path": current_path,
        "baseline_path": baseline_path,
        "current_metrics": current_metrics,
        "baseline_metrics": baseline_metrics,
        "checks": checks,
        "report_path": out_path,
    });
    write_json_report(&out_path, &report)?;
    Ok((report, out_path))
}

fn format_json_report_with_path(
    report: &serde_json::Value,
    path: &Path,
) -> Result<String, AgentError> {
    let raw = serde_json::to_string_pretty(report)
        .map_err(|e| AgentError::Config(format!("serialize report: {e}")))?;
    Ok(format!("{raw}\nreport_path={}", path.display()))
}

fn tail_chars(input: &str, max_chars: usize) -> String {
    let len = input.chars().count();
    if len <= max_chars {
        return input.to_string();
    }
    input.chars().skip(len.saturating_sub(max_chars)).collect()
}

async fn run_autopilot_probe_command(
    command: &str,
    cwd: &Path,
    max_tail: usize,
) -> serde_json::Value {
    let started = chrono::Utc::now();
    let output = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await;
    let finished = chrono::Utc::now();
    match output {
        Ok(output) => serde_json::json!({
            "command": command,
            "exit_code": output.status.code().unwrap_or(-1),
            "ok": output.status.success(),
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
            "duration_ms": (finished - started).num_milliseconds().max(0),
            "stdout_tail": tail_chars(&String::from_utf8_lossy(&output.stdout), max_tail),
            "stderr_tail": tail_chars(&String::from_utf8_lossy(&output.stderr), max_tail),
        }),
        Err(err) => serde_json::json!({
            "command": command,
            "exit_code": -1,
            "ok": false,
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
            "duration_ms": (finished - started).num_milliseconds().max(0),
            "stdout_tail": "",
            "stderr_tail": format!("spawn failed: {err}"),
        }),
    }
}

fn autopilot_native_section_from_report(
    command: &str,
    report: &serde_json::Value,
    path: &Path,
) -> serde_json::Value {
    let ok = report.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let stdout_tail = serde_json::to_string_pretty(report).unwrap_or_else(|_| report.to_string());
    serde_json::json!({
        "command": command,
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "started_at": report.get("generated_at").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "finished_at": report.get("generated_at").and_then(|v| v.as_str()).unwrap_or("unknown"),
        "duration_ms": 0,
        "stdout_tail": format!("{}\nreport_path={}", stdout_tail, path.display()),
        "stderr_tail": "",
    })
}

fn contextlattice_orchestrator_url() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("CONTEXTLATTICE_URL"))
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string())
}

async fn contextlattice_preflight_section() -> serde_json::Value {
    let started = chrono::Utc::now();
    let base = contextlattice_orchestrator_url();
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(err) => {
            let finished = chrono::Utc::now();
            return serde_json::json!({
                "command": format!("GET {base}/health + POST {base}/memory/search"),
                "exit_code": -1,
                "ok": false,
                "started_at": started.to_rfc3339(),
                "finished_at": finished.to_rfc3339(),
                "duration_ms": (finished - started).num_milliseconds().max(0),
                "stdout_tail": "",
                "stderr_tail": format!("ContextLattice HTTP client build failed: {err}"),
            });
        }
    };

    let health_result = client
        .get(format!("{base}/health"))
        .send()
        .await
        .and_then(|resp| resp.error_for_status());
    let (health_ok, health_json, mut warnings) = match health_result {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(value) => {
                let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                (ok, value, Vec::<String>::new())
            }
            Err(err) => (
                false,
                serde_json::json!({}),
                vec![format!("health_json_parse_failed: {err}")],
            ),
        },
        Err(err) => (
            false,
            serde_json::json!({}),
            vec![format!("health_request_failed: {err}")],
        ),
    };

    let search_payload = serde_json::json!({
        "agent_id": std::env::var("CONTEXTLATTICE_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
        "query": "hermes-ultra contextlattice intelligence preflight",
        "limit": 2,
        "retrieval_mode": "fast",
    });
    let search_result = client
        .post(format!("{base}/memory/search"))
        .json(&search_payload)
        .send()
        .await
        .and_then(|resp| resp.error_for_status());
    let search_json = match search_result {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(value) => value,
            Err(err) => {
                warnings.push(format!("search_json_parse_failed: {err}"));
                serde_json::json!({})
            }
        },
        Err(err) => {
            warnings.push(format!("search_request_failed: {err}"));
            serde_json::json!({})
        }
    };
    let degraded = search_json
        .get("degraded")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let retrieval = search_json
        .get("retrieval_debug")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let pending_total = health_json
        .pointer("/telemetry/queueDepth")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let payload = serde_json::json!({
        "health": health_json,
        "warnings": warnings,
        "context_pack": {
            "retrieval": retrieval,
            "result_state": search_json.get("result_state").cloned().unwrap_or(serde_json::Value::Null),
            "degraded": degraded,
        },
        "status": {
            "queue": {
                "pendingTotal": pending_total,
            }
        }
    });
    let stdout_tail =
        serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    let finished = chrono::Utc::now();
    serde_json::json!({
        "command": format!("GET {base}/health + POST {base}/memory/search"),
        "exit_code": if health_ok && !degraded { 0 } else { 1 },
        "ok": health_ok && !degraded,
        "started_at": started.to_rfc3339(),
        "finished_at": finished.to_rfc3339(),
        "duration_ms": (finished - started).num_milliseconds().max(0),
        "stdout_tail": tail_chars(&stdout_tail, 240000),
        "stderr_tail": "",
    })
}

fn parse_hotpath_ns_from_text(text: &str) -> Option<u64> {
    let needle = "tool_policy_hot_path_ns_per_eval=";
    let idx = text.rfind(needle)?;
    text[idx + needle.len()..]
        .lines()
        .next()
        .and_then(|v| v.trim().parse::<u64>().ok())
}

fn autopilot_section_ok(section: &serde_json::Value) -> bool {
    section.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
}

fn autopilot_section_text(section: &serde_json::Value) -> String {
    format!(
        "{}\n{}",
        section
            .get("stdout_tail")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
        section
            .get("stderr_tail")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
    )
}

fn parse_contextlattice_payload(section: &serde_json::Value) -> Option<serde_json::Value> {
    let raw = section
        .get("stdout_tail")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        return None;
    }
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&raw[start..=end]).ok()
}

fn contextlattice_autopilot_summary(
    payload: Option<&serde_json::Value>,
) -> (bool, usize, i64, String, serde_json::Value, i64) {
    let Some(payload) = payload else {
        return (false, 0, 0, String::new(), serde_json::json!({}), 0);
    };
    let healthy = payload
        .get("health")
        .and_then(|h| h.get("ok"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let warnings = payload
        .get("warnings")
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    let retrieval = payload
        .pointer("/context_pack/retrieval")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let route_owner_class = retrieval
        .get("route_owner_class")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let source_counts = retrieval
        .get("source_counts")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let python_fallbacks = retrieval
        .pointer("/fallback_counts/python_hot_path_total")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let queue_pending_total = payload
        .pointer("/status/queue/pendingTotal")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (
        healthy,
        warnings,
        python_fallbacks,
        route_owner_class,
        source_counts,
        queue_pending_total,
    )
}

fn build_performance_autopilot_recommendations(
    hotpath: &serde_json::Value,
    eval_gate: &serde_json::Value,
    mcp_gate: &serde_json::Value,
    context_gate: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut recs = Vec::new();
    let ns = parse_hotpath_ns_from_text(&autopilot_section_text(hotpath));
    let ctx_payload = parse_contextlattice_payload(context_gate);
    let (ctx_healthy, _warnings, python_fallbacks, _route_owner, source_counts, queue_pending) =
        contextlattice_autopilot_summary(ctx_payload.as_ref());

    if !autopilot_section_ok(hotpath) {
        recs.push(serde_json::json!({
            "id": "HOTPATH_FAIL",
            "severity": "P0",
            "title": "Hot-path benchmark failed",
            "recommendation": "Run `cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture` and resolve regressions before release.",
        }));
    } else if ns.is_some_and(|v| v > 12_000) {
        recs.push(serde_json::json!({
            "id": "HOTPATH_SLOW",
            "severity": "P1",
            "title": "Tool policy hot-path latency above target",
            "recommendation": "Keep `HERMES_TOOL_POLICY_PRESET=standard`, review deny-pattern complexity, and rerun the Rust hot-path benchmark.",
        }));
    }
    if !autopilot_section_ok(eval_gate) {
        recs.push(serde_json::json!({
            "id": "EVAL_TREND_FAIL",
            "severity": "P0",
            "title": "Eval trend gate failed",
            "recommendation": "Run `/ops eval run` and address the latest eval trend report before promotion.",
        }));
    }
    if !autopilot_section_ok(mcp_gate) {
        recs.push(serde_json::json!({
            "id": "MCP_STALE_RECOVERY_FAIL",
            "severity": "P1",
            "title": "MCP stale transport recovery regression",
            "recommendation": "Run `cargo test -p hermes-mcp` and restore reconnect-on-stale behavior before promotion.",
        }));
    }
    if !autopilot_section_ok(context_gate) {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_PREFLIGHT_FAIL",
            "severity": "P0",
            "title": "ContextLattice preflight failed",
            "recommendation": "Run `/integrations snapshot` and resolve orchestrator health/retrieval before long objective loops.",
        }));
    } else if !ctx_healthy {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_UNHEALTHY",
            "severity": "P1",
            "title": "ContextLattice health is degraded",
            "recommendation": "Use `/objective context max` and confirm orchestrator health/retrieval lanes before long-running objective loops.",
        }));
    }
    if python_fallbacks > 0 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_PYTHON_FALLBACK",
            "severity": "P1",
            "title": "ContextLattice retrieval fallback detected",
            "recommendation": "Investigate non-native fallback causes and keep Go/Rust lanes hot to avoid degraded memory-intelligence behavior.",
        }));
    }
    if source_counts.as_object().is_some_and(|m| m.is_empty()) && python_fallbacks == 0 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE",
            "severity": "P1",
            "title": "ContextLattice source coverage is empty",
            "recommendation": "Use broader same-project context-pack and ensure topic rollups/primary stores return at least one grounded hit.",
        }));
    }
    if queue_pending > 8 {
        recs.push(serde_json::json!({
            "id": "CONTEXTLATTICE_QUEUE_PRESSURE",
            "severity": "P2",
            "title": "ContextLattice queue pressure elevated",
            "recommendation": "Reduce write burst size or raise checkpoint spacing for long loops until pending queue normalizes.",
        }));
    }
    if recs.is_empty() {
        recs.push(serde_json::json!({
            "id": "PERF_STABLE",
            "severity": "P3",
            "title": "Performance checks stable",
            "recommendation": "No immediate tuning required. Keep nightly elite gate cadence.",
        }));
    }
    recs
}

fn recommendation_ids(recommendations: &[serde_json::Value]) -> HashSet<String> {
    recommendations
        .iter()
        .filter_map(|rec| rec.get("id").and_then(|v| v.as_str()))
        .map(|id| id.to_string())
        .collect()
}

fn compute_performance_autopilot_indexes(
    hotpath: &serde_json::Value,
    eval_gate: &serde_json::Value,
    mcp_gate: &serde_json::Value,
    context_gate: &serde_json::Value,
    recommendations: &[serde_json::Value],
) -> serde_json::Value {
    let ns = parse_hotpath_ns_from_text(&autopilot_section_text(hotpath));
    let checks = [
        autopilot_section_ok(hotpath),
        autopilot_section_ok(eval_gate),
        autopilot_section_ok(mcp_gate),
        autopilot_section_ok(context_gate),
    ];
    let fail_count = checks.iter().filter(|ok| !**ok).count();
    let mut performance = 100.0f64;
    if let Some(ns) = ns {
        if ns > 12_000 {
            let overflow_ratio = ((ns - 12_000) as f64 / 12_000.0).min(3.0);
            performance -= (overflow_ratio * 10.0).min(30.0);
        }
    }
    if !autopilot_section_ok(hotpath) {
        performance -= 35.0;
    }
    if !autopilot_section_ok(mcp_gate) {
        performance -= 20.0;
    }
    if !autopilot_section_ok(context_gate) {
        performance -= 25.0;
    }
    performance = performance.clamp(0.0, 100.0);

    let mut intelligence = 100.0f64;
    for rec in recommendations {
        let sev = rec
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("P3")
            .to_ascii_uppercase();
        intelligence -= match sev.as_str() {
            "P0" => 22.0,
            "P1" => 12.0,
            "P2" => 6.0,
            _ => 2.0,
        };
    }
    if !autopilot_section_ok(eval_gate) {
        intelligence -= 18.0;
    }
    if !autopilot_section_ok(context_gate) {
        intelligence -= 20.0;
    }
    let ctx_payload = parse_contextlattice_payload(context_gate);
    let (_healthy, _warnings, python_fallbacks, _route_owner, source_counts, queue_pending) =
        contextlattice_autopilot_summary(ctx_payload.as_ref());
    if python_fallbacks > 0 {
        intelligence -= (python_fallbacks as f64).min(12.0);
    }
    if source_counts.as_object().is_some_and(|m| m.is_empty()) && python_fallbacks == 0 {
        intelligence -= 10.0;
    }
    if queue_pending > 8 {
        intelligence -= 8.0;
    }
    intelligence = intelligence.clamp(0.0, 100.0);

    let profile = if fail_count >= 2 {
        "safety"
    } else if !autopilot_section_ok(eval_gate) || !autopilot_section_ok(context_gate) {
        "quality"
    } else if !autopilot_section_ok(mcp_gate) {
        "reliability"
    } else if ns.is_some_and(|v| v > 12_000) {
        "throughput"
    } else {
        "balanced"
    };
    let mut adaptive_actions = vec![serde_json::json!({
        "key": "HERMES_PERF_AUTOPILOT_PROFILE",
        "value": profile,
        "reason": "profile recommendation from adaptive index",
    })];
    match profile {
        "throughput" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_PRESET","value":"standard","reason":"reduce policy hot-path overhead"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_MODEL_CATALOG_GUARD","value":"1","reason":"avoid invalid model retries"}));
        }
        "quality" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_REPLAY_ENABLED","value":"1","reason":"capture deterministic replay for eval failures"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_MODEL_AUTO_REMEDIATE","value":"1","reason":"promote self-heal recommendation loop"}));
        }
        "reliability" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_MODE","value":"enforce","reason":"stabilize stale transport/recovery behavior"}));
        }
        "safety" => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_TOOL_POLICY_MODE","value":"enforce","reason":"strict policy posture under multi-check failure"}));
            adaptive_actions.push(serde_json::json!({"key":"HERMES_REPLAY_ENABLED","value":"1","reason":"preserve incident evidence during degraded state"}));
        }
        _ => {
            adaptive_actions.push(serde_json::json!({"key":"HERMES_PERF_AUTOPILOT_STATUS","value":"stable","reason":"all checks stable"}));
        }
    }
    serde_json::json!({
        "performance_index": (performance * 100.0).round() / 100.0,
        "intelligence_index": (intelligence * 100.0).round() / 100.0,
        "adaptive_index": ((performance * 0.55 + intelligence * 0.45) * 100.0).round() / 100.0,
        "profile_recommendation": profile,
        "adaptive_actions": adaptive_actions,
    })
}

fn default_performance_autopilot_paths(repo_root: &Path) -> (PathBuf, PathBuf) {
    let stamp = utc_compact_stamp();
    let out_dir = repo_root.join(".sync-reports");
    (
        out_dir.join(format!("performance-autopilot-{stamp}.json")),
        out_dir.join(format!("performance-autopilot-{stamp}.md")),
    )
}

fn write_performance_autopilot_markdown(
    path: &Path,
    report: &serde_json::Value,
) -> Result<(), AgentError> {
    let mut lines = Vec::new();
    lines.push("# Performance Autopilot Report".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- generated_at: `{}`",
        report
            .get("generated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    ));
    lines.push(format!(
        "- ok: `{}`",
        report.get("ok").and_then(|v| v.as_bool()).unwrap_or(false)
    ));
    lines.push(format!(
        "- intelligence_index: `{:.2}`",
        report
            .get("intelligence_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- performance_index: `{:.2}`",
        report
            .get("performance_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- adaptive_index: `{:.2}`",
        report
            .get("adaptive_index")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    ));
    lines.push(format!(
        "- profile_recommendation: `{}`",
        report
            .get("profile_recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
    ));
    lines.push(String::new());
    lines.push("## Sections".to_string());
    if let Some(sections) = report.get("sections").and_then(|v| v.as_object()) {
        for (name, section) in sections {
            lines.push(format!(
                "- `{}`: {} (exit={})",
                name,
                if autopilot_section_ok(section) {
                    "PASS"
                } else {
                    "FAIL"
                },
                section
                    .get("exit_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1)
            ));
        }
    }
    lines.push(String::new());
    lines.push("## Recommendations".to_string());
    if let Some(recs) = report.get("recommendations").and_then(|v| v.as_array()) {
        for rec in recs {
            lines.push(format!(
                "- **{} ({})**: {} - {}",
                rec.get("id").and_then(|v| v.as_str()).unwrap_or("UNKNOWN"),
                rec.get("severity").and_then(|v| v.as_str()).unwrap_or("PX"),
                rec.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                rec.get("recommendation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
            ));
        }
    }
    if let Some(actions) = report.get("adaptive_actions").and_then(|v| v.as_array()) {
        if !actions.is_empty() {
            lines.push(String::new());
            lines.push("## Adaptive Actions".to_string());
            for action in actions {
                lines.push(format!(
                    "- `{}={}` ({})",
                    action.get("key").and_then(|v| v.as_str()).unwrap_or(""),
                    action.get("value").and_then(|v| v.as_str()).unwrap_or(""),
                    action.get("reason").and_then(|v| v.as_str()).unwrap_or("")
                ));
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, format!("{}\n", lines.join("\n").trim()))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn write_performance_autopilot_env(
    path: &Path,
    report: &serde_json::Value,
) -> Result<(), AgentError> {
    let recs: Vec<serde_json::Value> = report
        .get("recommendations")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let rec_ids = recommendation_ids(&recs);
    let mut lines = vec![format!(
        "# generated_at={}",
        report
            .get("generated_at")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
    )];
    if rec_ids.contains("HOTPATH_SLOW") {
        lines.extend([
            "HERMES_TOOL_POLICY_PRESET=standard".to_string(),
            "HERMES_TOOL_POLICY_MODE=enforce".to_string(),
            "HERMES_MODEL_CATALOG_GUARD=1".to_string(),
        ]);
    }
    if rec_ids.contains("EVAL_TREND_FAIL") {
        lines.extend([
            "HERMES_MODEL_AUTO_REMEDIATE=1".to_string(),
            "HERMES_REPLAY_ENABLED=1".to_string(),
        ]);
    }
    if rec_ids.iter().any(|id| {
        matches!(
            id.as_str(),
            "CONTEXTLATTICE_PREFLIGHT_FAIL"
                | "CONTEXTLATTICE_UNHEALTHY"
                | "CONTEXTLATTICE_PYTHON_FALLBACK"
                | "CONTEXTLATTICE_ZERO_SOURCE_COVERAGE"
        )
    }) {
        lines.extend([
            "HERMES_CONTEXTLATTICE_MODE=max".to_string(),
            "HERMES_CONTEXTLATTICE_RETRIEVAL_MODE=deep".to_string(),
            "HERMES_CONTEXTLATTICE_REQUIRE_READBACK=1".to_string(),
        ]);
    }
    if rec_ids.len() == 1 && rec_ids.contains("PERF_STABLE") {
        lines.push("HERMES_PERF_AUTOPILOT_STATUS=stable".to_string());
    }
    lines.push(format!(
        "HERMES_PERF_AUTOPILOT_PROFILE={}",
        report
            .get("profile_recommendation")
            .and_then(|v| v.as_str())
            .unwrap_or("balanced")
    ));
    if let Some(actions) = report.get("adaptive_actions").and_then(|v| v.as_array()) {
        for action in actions {
            let Some(key) = action.get("key").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(value) = action.get("value").and_then(|v| v.as_str()) else {
                continue;
            };
            lines.push(format!("{key}={value}"));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

async fn run_performance_autopilot_native(
    repo_root: &Path,
    apply_env: Option<&Path>,
) -> Result<(serde_json::Value, PathBuf, PathBuf), AgentError> {
    let (json_path, md_path) = default_performance_autopilot_paths(repo_root);
    let hotpath = run_autopilot_probe_command(
        "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture",
        repo_root,
        6000,
    )
    .await;
    let (eval_report, eval_path) = run_eval_trend_gate_native(
        repo_root,
        None,
        None,
        None,
        EvalTrendGateOptions {
            allow_missing_baseline: true,
            ..Default::default()
        },
    )?;
    let eval_gate =
        autopilot_native_section_from_report("native eval trend gate", &eval_report, &eval_path);
    let mcp_gate = run_autopilot_probe_command(
        "cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture",
        repo_root,
        6000,
    )
    .await;
    let context_gate = contextlattice_preflight_section().await;
    let recommendations =
        build_performance_autopilot_recommendations(&hotpath, &eval_gate, &mcp_gate, &context_gate);
    let ok = [&hotpath, &eval_gate, &mcp_gate, &context_gate]
        .iter()
        .all(|section| autopilot_section_ok(section));
    let adaptive = compute_performance_autopilot_indexes(
        &hotpath,
        &eval_gate,
        &mcp_gate,
        &context_gate,
        &recommendations,
    );
    let mut report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": ok,
        "sections": {
            "hotpath": hotpath,
            "eval_trend": eval_gate,
            "mcp_stale_recovery": mcp_gate,
            "contextlattice_preflight": context_gate,
        },
        "recommendations": recommendations,
        "performance_index": adaptive.get("performance_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "intelligence_index": adaptive.get("intelligence_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "adaptive_index": adaptive.get("adaptive_index").cloned().unwrap_or(serde_json::json!(0.0)),
        "profile_recommendation": adaptive.get("profile_recommendation").cloned().unwrap_or(serde_json::json!("balanced")),
        "adaptive_actions": adaptive.get("adaptive_actions").cloned().unwrap_or_else(|| serde_json::json!([])),
        "report_json": json_path,
        "report_markdown": md_path,
    });
    if let Some(env_path) = apply_env {
        write_performance_autopilot_env(env_path, &report)?;
        report["applied_env"] = serde_json::json!(env_path);
    }
    write_json_report(&json_path, &report)?;
    write_performance_autopilot_markdown(&md_path, &report)?;
    Ok((report, json_path, md_path))
}

fn report_path_with_stamp(repo_root: &Path, prefix: &str) -> PathBuf {
    repo_root
        .join(".sync-reports")
        .join(format!("{prefix}-{}.json", utc_compact_stamp()))
}

fn gate_section_from_report(
    command: &str,
    report: &serde_json::Value,
    path: &Path,
) -> serde_json::Value {
    autopilot_native_section_from_report(command, report, path)
}

fn parity_release_gate_section(repo_root: &Path) -> serde_json::Value {
    let path = repo_root.join("docs/parity/global-parity-proof.json");
    let report = read_json_file(&path).unwrap_or_else(|| serde_json::json!({}));
    let ok = report
        .pointer("/release_gate/pass")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    serde_json::json!({
        "command": "read docs/parity/global-parity-proof.json release_gate.pass",
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string()),
        "stderr_tail": if path.exists() { "" } else { "global parity proof missing" },
        "report_path": path,
    })
}

fn shared_backlog_gate_section(repo_root: &Path) -> serde_json::Value {
    let path = repo_root.join("docs/parity/shared-diff-backlog.json");
    let report = read_json_file(&path).unwrap_or_else(|| serde_json::json!({}));
    let pending_classification = report
        .pointer("/summary/pending_classification")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    let pending_review = report
        .pointer("/summary/pending_review")
        .and_then(|v| v.as_i64())
        .unwrap_or(i64::MAX);
    let ok = pending_classification == 0 && pending_review == 0;
    serde_json::json!({
        "command": "read docs/parity/shared-diff-backlog.json summary pending counts",
        "exit_code": if ok { 0 } else { 1 },
        "ok": ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::to_string_pretty(&report.get("summary").cloned().unwrap_or(serde_json::Value::Null)).unwrap_or_default(),
        "stderr_tail": if path.exists() { "" } else { "shared diff backlog missing" },
        "report_path": path,
    })
}

async fn run_slo_auto_rollback_native(
    repo_root: &Path,
    check_cmd: &str,
    rollback_cmd: &str,
    dry_run: bool,
    report_path: Option<&Path>,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path
        .map(PathBuf::from)
        .unwrap_or_else(|| report_path_with_stamp(repo_root, "slo-auto-rollback"));
    let check = run_autopilot_probe_command(check_cmd, repo_root, 4000).await;
    let violated = !autopilot_section_ok(&check);
    let rollback = if violated && dry_run {
        serde_json::json!({
            "command": rollback_cmd,
            "ok": false,
            "skipped": true,
            "reason": "dry_run",
        })
    } else if violated {
        run_autopilot_probe_command(rollback_cmd, repo_root, 4000).await
    } else {
        serde_json::Value::Null
    };
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": !violated,
        "violated": violated,
        "dry_run": dry_run,
        "check": check,
        "rollback": rollback,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
}

async fn run_elite_sync_gate_native(
    repo_root: &Path,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path_with_stamp(repo_root, "elite-sync-gate");
    let runtime_python_guard =
        run_autopilot_probe_command("scripts/check-rust-runtime-no-python.sh", repo_root, 4000)
            .await;
    let placeholder_guard =
        run_autopilot_probe_command("scripts/check-runtime-placeholders.sh", repo_root, 4000).await;
    let hotpath = run_autopilot_probe_command(
        "cargo test -p hermes-tools tool_policy_hot_path_benchmark_report -- --nocapture",
        repo_root,
        4000,
    )
    .await;
    let mcp_stale_recovery = run_autopilot_probe_command(
        "cargo test -p hermes-mcp stale_transport_marker_detection_matches_known_variants -- --nocapture",
        repo_root,
        4000,
    )
    .await;
    let (eval_report, eval_path) = run_eval_trend_gate_native(
        repo_root,
        None,
        None,
        None,
        EvalTrendGateOptions {
            allow_missing_baseline: true,
            ..Default::default()
        },
    )?;
    let eval_trend = gate_section_from_report("native eval trend gate", &eval_report, &eval_path);
    let (autopilot_report, autopilot_json, _autopilot_md) =
        run_performance_autopilot_native(repo_root, None).await?;
    let performance_autopilot = gate_section_from_report(
        "native performance autopilot",
        &autopilot_report,
        &autopilot_json,
    );
    let parity_release = parity_release_gate_section(repo_root);
    let shared_backlog = shared_backlog_gate_section(repo_root);
    let sections = serde_json::json!({
        "runtime_python_guard": runtime_python_guard,
        "placeholder_guard": placeholder_guard,
        "hotpath": hotpath,
        "mcp_stale_recovery": mcp_stale_recovery,
        "eval_trend": eval_trend,
        "performance_autopilot": performance_autopilot,
        "parity_release": parity_release,
        "shared_backlog": shared_backlog,
    });
    let section_values: Vec<&serde_json::Value> = sections
        .as_object()
        .map(|m| m.values().collect())
        .unwrap_or_default();
    let passed = section_values
        .iter()
        .filter(|section| autopilot_section_ok(section))
        .count();
    let total = section_values.len();
    let ok = total > 0 && passed == total;
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "ok": ok,
        "summary": {
            "total_sections": total,
            "passed_sections": passed,
            "failed_sections": total.saturating_sub(passed),
        },
        "sections": sections,
        "rollback": serde_json::Value::Null,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
}

fn self_evolution_recommendation(
    rec_id: &str,
    severity: &str,
    title: &str,
    reason: &str,
    command: &str,
) -> serde_json::Value {
    serde_json::json!({
        "id": rec_id,
        "severity": severity,
        "title": title,
        "reason": reason,
        "command": command,
    })
}

fn build_self_evolution_recommendations_native(
    objective: &str,
    sections: &serde_json::Value,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let objective_hint = if objective.trim().is_empty() {
        String::new()
    } else {
        format!(" Objective: {}.", objective.trim())
    };
    let section_ok = |name: &str| {
        sections
            .get(name)
            .and_then(|v| v.get("ok"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    if !section_ok("golden_parity") {
        out.push(self_evolution_recommendation(
            "PARITY_DRIFT",
            "P0",
            "Resolve command/TUI parity drift before feature work",
            &format!(
                "Native parity proof or shared backlog gate failed.{}",
                objective_hint
            ),
            "/ops gate status && hermes-ultra doctor --deep --snapshot",
        ));
    }
    if !section_ok("eval_trend") {
        out.push(self_evolution_recommendation(
            "EVAL_REGRESSION",
            "P0",
            "Recover eval trend before promotion",
            &format!("Eval trend gate failed.{}", objective_hint),
            "/ops eval run && /qos autotune plan",
        ));
    }
    if !section_ok("elite_sync") {
        out.push(self_evolution_recommendation(
            "ELITE_GATE_FAIL",
            "P0",
            "Hold release and remediate elite gate failures",
            &format!("Native elite gate failed.{}", objective_hint),
            "/ops gate elite",
        ));
    }
    if out.is_empty() {
        out.push(self_evolution_recommendation(
            "PROMOTE_BASELINE",
            "P2",
            "Promote current state as next baseline",
            &format!(
                "All enabled native sections passed; safe to store this run as a quality baseline.{}",
                objective_hint
            ),
            "hermes-ultra doctor --deep --snapshot",
        ));
    }
    out
}

include!("command_ops_eval/evolve_and_ops.rs");
