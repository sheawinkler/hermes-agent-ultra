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

async fn run_self_evolution_loop_native(
    repo_root: &Path,
    objective: &str,
) -> Result<(serde_json::Value, PathBuf), AgentError> {
    let path = report_path_with_stamp(repo_root, "self-evolution-loop");
    let parity_release = parity_release_gate_section(repo_root);
    let shared_backlog = shared_backlog_gate_section(repo_root);
    let golden_ok = autopilot_section_ok(&parity_release) && autopilot_section_ok(&shared_backlog);
    let golden_parity = serde_json::json!({
        "command": "native parity release/backlog gates",
        "exit_code": if golden_ok { 0 } else { 1 },
        "ok": golden_ok,
        "elapsed_ms": 0,
        "stdout_tail": serde_json::json!({
            "parity_release": parity_release,
            "shared_backlog": shared_backlog,
        }).to_string(),
        "stderr_tail": "",
    });
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
    let (elite_report, elite_path) = run_elite_sync_gate_native(repo_root).await?;
    let elite_sync = gate_section_from_report("native elite sync gate", &elite_report, &elite_path);
    let sections = serde_json::json!({
        "golden_parity": golden_parity,
        "eval_trend": eval_trend,
        "elite_sync": elite_sync,
    });
    let section_values: Vec<&serde_json::Value> = sections
        .as_object()
        .map(|m| m.values().collect())
        .unwrap_or_default();
    let total = section_values.len();
    let passed = section_values
        .iter()
        .filter(|section| autopilot_section_ok(section))
        .count();
    let ok = total == 0 || passed == total;
    let recommendations = build_self_evolution_recommendations_native(objective, &sections);
    let report = serde_json::json!({
        "generated_at": chrono::Utc::now().to_rfc3339(),
        "repo_root": repo_root,
        "objective": objective.trim(),
        "ok": ok,
        "summary": {
            "total_sections": total,
            "passed_sections": passed,
            "failed_sections": total.saturating_sub(passed),
            "intelligence_index": if total == 0 { 100.0 } else { ((passed as f64 / total as f64) * 10000.0).round() / 100.0 },
        },
        "sections": sections,
        "recommendations": recommendations,
        "report_path": path,
    });
    write_json_report(&path, &report)?;
    Ok((report, path))
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

fn summarize_performance_autopilot_report(path: &Path, key: &str) -> Option<String> {
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

async fn handle_dashboard_command(
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

fn handle_simulate_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = app.tool_registry.policy_counters();
        emit_command_output(
            app,
            format!(
                "Tool-policy simulation\n\
                 usage: /simulate <tool_name> [json-params]\n\
                 examples:\n  /simulate terminal {{\"cmd\":\"ls\"}}\n  /simulate skill_manage {{\"action\":\"view\",\"skill\":\"contextlattice-agent-contract\"}}\n\
                 counters: allow={} deny={} audit_only={} simulate={} would_block={}",
                counters.allow, counters.deny, counters.audit_only, counters.simulate, counters.would_block
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
                std::env::remove_var(key);
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
        std::env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
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
    std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", next.as_str());
    emit_command_output(
        app,
        format!("repo_review_tool_profile mode set to `{}`", next),
    );
    Ok(CommandResult::Handled)
}

async fn handle_ops_eval_command(
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
            let (report, path) = run_session_eval_harness_native(
                &repo_root,
                &hermes_config::hermes_home().join("sessions"),
                25,
                None,
            )?;
            let out = format_json_report_with_path(&report, &path)?;
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

async fn handle_qos_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
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

fn handle_ops_skills_tier_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "skills_tier={} (bypass={})",
                skills_execution_tier().as_str(),
                if skills_tier_bypass_enabled() {
                    "ON"
                } else {
                    "OFF"
                }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let Some(next) = SkillsExecutionTier::parse(args[0]) else {
        emit_command_output(
            app,
            "Usage: /ops skills-tier [status|trusted|balanced|open]",
        );
        return Ok(CommandResult::Handled);
    };
    std::env::set_var("HERMES_SKILLS_EXECUTION_TIER", next.as_str());
    emit_command_output(
        app,
        format!(
            "skills_tier set to '{}' for this runtime process.",
            next.as_str()
        ),
    );
    Ok(CommandResult::Handled)
}

async fn handle_ops_gate_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let repo_root = discover_repo_root_for_about();
    match sub.as_str() {
        "status" => {
            if let Some(repo_root) = repo_root.as_ref() {
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
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "Eval gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let (report, path) = run_eval_trend_gate_native(
                repo_root,
                None,
                None,
                None,
                EvalTrendGateOptions {
                    allow_missing_baseline: true,
                    ..Default::default()
                },
            )?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "elite" => {
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "Elite gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let (report, path) = run_elite_sync_gate_native(repo_root).await?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "slo" => {
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "SLO gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let check_cmd = std::env::var("HERMES_SLO_CHECK_CMD").ok();
            let rollback_cmd = std::env::var("HERMES_SLO_ROLLBACK_CMD").ok();
            let (Some(check), Some(rollback)) = (check_cmd, rollback_cmd) else {
                emit_command_output(
                    app,
                    "Set HERMES_SLO_CHECK_CMD and HERMES_SLO_ROLLBACK_CMD, then run `/ops gate slo`.",
                );
                return Ok(CommandResult::Handled);
            };
            let (report, path) =
                run_slo_auto_rollback_native(repo_root, &check, &rollback, false, None).await?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops gate [status|eval|elite|slo]");
            Ok(CommandResult::Handled)
        }
    }
}

async fn handle_ops_evolve_command(
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
            let objective = app.session_objective.as_deref().unwrap_or_default();
            let (report, path) = run_self_evolution_loop_native(&repo_root, objective).await?;
            let out = format_json_report_with_path(&report, &path)?;
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

async fn handle_ops_autopilot_command(
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
            let (report, json_path, md_path) =
                run_performance_autopilot_native(&repo_root, None).await?;
            let mut out = format_json_report_with_path(&report, &json_path)?;
            let _ = write!(out, "\nmarkdown_path={}", md_path.display());
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
            let (report, json_path, md_path) =
                run_performance_autopilot_native(&repo_root, Some(&env_path)).await?;
            let mut out = format_json_report_with_path(&report, &json_path)?;
            let _ = write!(out, "\nmarkdown_path={}", md_path.display());
            let kvs = parse_env_file_kv(&env_path);
            let mut applied = Vec::new();
            for (k, v) in kvs {
                if AUTOPILOT_ALLOWED_ENV_KEYS
                    .iter()
                    .any(|allowed| *allowed == k)
                {
                    std::env::set_var(&k, &v);
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
                    report_dir.join("performance-autopilot-runtime.jsonl").display()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "profile" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => emit_command_output(
                    app,
                    format!("autopilot profile={profile} (mode={mode})"),
                ),
                Some("list") => emit_command_output(
                    app,
                    "Autopilot profiles:\n- balanced: default stability/perf mix\n- throughput: lower latency and tighter loop cadence\n- quality: stronger verification and replay focus\n- reliability: prioritize retries/recovery and degraded-source tolerance\n- safety: strictest gate posture with conservative policy knobs",
                ),
                Some("balanced" | "throughput" | "quality" | "reliability" | "safety") => {
                    let value = next.unwrap_or_else(|| "off".to_string());
                    std::env::set_var("HERMES_PERF_AUTOPILOT_PROFILE", &value);
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
                    std::env::set_var("HERMES_PERF_AUTOPILOT_MODE", &value);
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
            std::env::remove_var("HERMES_PERF_AUTOPILOT_MODE");
            std::env::remove_var("HERMES_PERF_AUTOPILOT_PROFILE");
            std::env::remove_var("HERMES_PERF_AUTOPILOT_STATUS");
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

async fn handle_ops_cockpit_command(
    app: &mut App,
    _args: &[&str],
) -> Result<CommandResult, AgentError> {
    let counters = app.tool_registry.policy_counters();
    let budget = RepoReviewBudgetRuntime::from_env();
    let board = render_mission_board(
        &app.current_model,
        app.session_objective.as_deref(),
        background_job_counts(),
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
        enumerate_saved_sessions(&hermes_config::hermes_home().join("sessions")).len();
    let mut out = String::new();
    out.push_str("Ops Cockpit\n");
    out.push_str("===========\n");
    let _ = writeln!(out, "session: {}", app.session_id);
    let _ = writeln!(out, "model: {}", app.current_model);
    let _ = writeln!(
        out,
        "policy: profile={} mode={} preset={} sandbox={} skills_tier={}",
        current_policy_profile_name(),
        std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
        std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "relaxed".into()),
        std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE").unwrap_or_else(|_| "balanced".into()),
        std::env::var("HERMES_SKILLS_EXECUTION_TIER").unwrap_or_else(|_| "balanced".into())
    );
    let _ = writeln!(
        out,
        "planner_capability_router={} compaction_governance={} replay_trace={}",
        plan_capability_mode().as_str(),
        compaction_governance_mode().as_str(),
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

async fn handle_ops_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
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
            skills_execution_tier().as_str(),
            if skills_tier_bypass_enabled() {
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
        "model" => handle_model_command(app, &args[1..]).await,
        "mode" => handle_policy_command(app, &args[1..]),
        "personality" => handle_personality_command(app, &args[1..]),
        "mouse" => handle_mouse_command(app, &args[1..]),
        "yolo" => handle_yolo_command(app),
        "reasoning" => handle_reasoning_command(app, &args[1..]),
        "raw" => handle_raw_command(app, &args[1..]),
        "verbose" => handle_verbose_command(app),
        "statusbar" => handle_statusbar_command(app),
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
