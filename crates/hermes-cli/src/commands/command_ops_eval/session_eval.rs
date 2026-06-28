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
