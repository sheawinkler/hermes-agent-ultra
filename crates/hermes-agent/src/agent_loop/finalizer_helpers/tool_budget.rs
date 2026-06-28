fn is_housekeeping_tool_name(name: &str) -> bool {
    matches!(
        name,
        "memory" | "todo" | "skill_manage" | "session_search" | "skills"
    )
}

fn is_discovery_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("search")
        || lower.contains("find")
        || lower.contains("grep")
        || lower.contains("list")
        || lower.contains("read")
        || lower.contains("view")
        || lower.contains("scan")
        || lower.contains("context_pack")
        || lower == "terminal"
        || lower == "execute_code"
}

fn is_execution_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "terminal" | "execute_code" | "apply_patch" | "edit_file" | "run_command"
    )
}

fn is_non_repo_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "weather", "sports", "tarot", "zillow", "shopping", "gmail", "calendar", "artwork", "deal",
        "coursera", "datacamp", "jobkorea",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn is_messaging_tool_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "telegram",
        "discord",
        "slack",
        "mattermost",
        "signal",
        "whatsapp",
        "wecom",
        "weixin",
        "qqbot",
        "dingtalk",
        "feishu",
        "gmail",
        "calendar",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn compact_tool_args_for_signature(raw: &str) -> String {
    raw.split_whitespace().collect::<String>()
}

fn discovery_signature(tool_calls: &[ToolCall]) -> Option<String> {
    let mut fingerprints: Vec<String> = tool_calls
        .iter()
        .filter(|tc| is_discovery_tool_name(&tc.function.name))
        .map(|tc| {
            format!(
                "{}:{}",
                tc.function.name,
                compact_tool_args_for_signature(&tc.function.arguments)
            )
        })
        .collect();
    if fingerprints.is_empty() {
        return None;
    }
    fingerprints.sort();
    let mut hasher = Sha256::new();
    for fp in fingerprints {
        hasher.update(fp.as_bytes());
        hasher.update(b"\n");
    }
    Some(format!("{:x}", hasher.finalize()))
}

fn apply_repo_review_tool_profile_narrowing(
    tool_calls: &mut Vec<ToolCall>,
    messages: &[Message],
) -> Option<String> {
    if !detect_repo_review_intent(messages) {
        return None;
    }
    if detect_tool_profile_escape_hatch(messages) {
        return Some(
            "[SYSTEM] Repo-review tool profile narrowing bypassed by explicit operator escape hatch."
                .to_string(),
        );
    }
    let mode = repo_review_tool_profile_mode();
    if mode == RepoReviewToolProfileMode::Off {
        return None;
    }
    let allow_messaging = detect_communication_intent(messages);
    let mut filtered_messaging = 0usize;
    let mut filtered_non_repo = 0usize;
    let mut filtered_focus = 0usize;
    tool_calls.retain(|tc| {
        let mut should_filter = false;
        if is_messaging_tool_name(&tc.function.name) && !allow_messaging {
            filtered_messaging += 1;
            should_filter = true;
        } else if is_non_repo_tool_name(&tc.function.name)
            && !is_discovery_tool_name(&tc.function.name)
            && !is_execution_tool_name(&tc.function.name)
        {
            filtered_non_repo += 1;
            should_filter = true;
        } else if mode == RepoReviewToolProfileMode::Focus
            && !is_discovery_tool_name(&tc.function.name)
            && !is_execution_tool_name(&tc.function.name)
            && !is_housekeeping_tool_name(&tc.function.name)
            && !tc
                .function
                .name
                .to_ascii_lowercase()
                .contains("contextlattice")
        {
            filtered_focus += 1;
            should_filter = true;
        }
        if should_filter {
            return false;
        }
        true
    });
    let filtered = filtered_messaging + filtered_non_repo + filtered_focus;
    if filtered == 0 {
        return None;
    }
    Some(format!(
        "[SYSTEM] Repo-review tool profile narrowed this turn (mode={}): skipped {} low-signal call(s) (messaging={}, non-repo={}, focus={}) to keep focus on code evidence. `todo` remains enabled for task organization. If notifications are required, request telegram/discord/slack explicitly.",
        mode.as_str(), filtered, filtered_messaging, filtered_non_repo, filtered_focus
    ))
}

fn repo_review_repeat_threshold() -> u32 {
    std::env::var("HERMES_REPO_REVIEW_REPEAT_STREAK_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RepoReviewDiscoveryBudgetMode {
    Off,
    Advisory,
    Enforce,
}

impl RepoReviewDiscoveryBudgetMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

fn repo_review_discovery_budget_mode() -> RepoReviewDiscoveryBudgetMode {
    let raw = std::env::var("HERMES_REPO_REVIEW_DISCOVERY_BUDGET_MODE")
        .ok()
        .unwrap_or_else(|| "advisory".to_string());
    match raw.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "disable" | "disabled" => RepoReviewDiscoveryBudgetMode::Off,
        "trim" | "hard" | "enforce" | "strict" => RepoReviewDiscoveryBudgetMode::Enforce,
        _ => RepoReviewDiscoveryBudgetMode::Advisory,
    }
}

fn repo_review_low_signal_threshold() -> u32 {
    std::env::var("HERMES_REPO_REVIEW_LOW_SIGNAL_STREAK_THRESHOLD")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

fn repo_review_keep_limit_repeat() -> usize {
    std::env::var("HERMES_REPO_REVIEW_KEEP_LIMIT_REPEAT")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(2)
        .clamp(1, 12)
}

fn repo_review_keep_limit_low_signal() -> usize {
    std::env::var("HERMES_REPO_REVIEW_KEEP_LIMIT_LOW_SIGNAL")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 12)
}

fn repo_review_min_signal_score() -> f64 {
    std::env::var("HERMES_REPO_REVIEW_MIN_SIGNAL_SCORE")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(0.22)
        .clamp(0.0, 1.0)
}

fn apply_repo_review_discovery_budget_policy(
    tool_calls: &mut Vec<ToolCall>,
    messages: &[Message],
    state: &mut RepoReviewBudgetState,
) -> Option<String> {
    let mode = repo_review_discovery_budget_mode();
    if matches!(mode, RepoReviewDiscoveryBudgetMode::Off) {
        return None;
    }
    if !detect_repo_review_intent(messages) {
        *state = RepoReviewBudgetState::default();
        return None;
    }
    let Some(signature) = discovery_signature(tool_calls) else {
        state.repeat_streak = 0;
        state.last_discovery_signature = None;
        return None;
    };
    let only_discovery_or_housekeeping = tool_calls.iter().all(|tc| {
        is_discovery_tool_name(&tc.function.name) || is_housekeeping_tool_name(&tc.function.name)
    });

    if only_discovery_or_housekeeping
        && state.last_discovery_signature.as_deref() == Some(signature.as_str())
    {
        state.repeat_streak = state.repeat_streak.saturating_add(1);
    } else {
        state.repeat_streak = 0;
    }
    state.last_discovery_signature = Some(signature);

    let repeat_threshold = repo_review_repeat_threshold();
    let low_signal_threshold = repo_review_low_signal_threshold();
    let keep_limit_repeat = repo_review_keep_limit_repeat();
    let keep_limit_low_signal = repo_review_keep_limit_low_signal();

    let repeat_threshold_hit = state.repeat_streak >= repeat_threshold;
    let low_signal_threshold_hit = state.low_signal_streak >= low_signal_threshold;
    if (!repeat_threshold_hit && !low_signal_threshold_hit) || !only_discovery_or_housekeeping {
        return None;
    }

    if matches!(mode, RepoReviewDiscoveryBudgetMode::Advisory) {
        return Some(format!(
            "[SYSTEM] Discovery budget advisory (mode={} repeat_streak={} threshold={} low_signal_streak={} threshold={} last_signal_score={:.2} min_signal={:.2}). Tool calls are not trimmed in advisory mode. Prefer narrower path/glob scope, context-pack pivots, and then move to concrete patch synthesis.",
            mode.as_str(),
            state.repeat_streak + 1,
            repeat_threshold,
            state.low_signal_streak,
            low_signal_threshold,
            state.last_signal_score,
            repo_review_min_signal_score(),
        ));
    }

    let mut kept_per_tool: HashMap<String, usize> = HashMap::new();
    let mut removed = 0usize;
    let keep_limit = if low_signal_threshold_hit {
        keep_limit_low_signal
    } else {
        keep_limit_repeat
    };
    tool_calls.retain(|tc| {
        if !is_discovery_tool_name(&tc.function.name) {
            return true;
        }
        let counter = kept_per_tool.entry(tc.function.name.clone()).or_insert(0);
        if *counter < keep_limit {
            *counter += 1;
            true
        } else {
            removed += 1;
            false
        }
    });

    Some(format!(
        "[SYSTEM] Discovery budget policy engaged (repeat_streak={} threshold={} low_signal_streak={} threshold={} last_signal_score={:.2} min_signal={:.2}). {} duplicate low-yield discovery call(s) were trimmed (per-tool keep limit {}). Refine search scope with targeted paths/globs or context-pack query expansion, then move to synthesis and concrete patch planning.",
        state.repeat_streak + 1,
        repeat_threshold,
        state.low_signal_streak,
        low_signal_threshold,
        state.last_signal_score,
        repo_review_min_signal_score(),
        removed,
        keep_limit
    ))
}

fn tool_result_signal_score(content: &str, is_error: bool) -> f64 {
    if is_error {
        return 0.0;
    }
    let lower = content.to_ascii_lowercase();
    let mut score: f64 = 0.0;
    if content.len() >= 160 {
        score += 0.25;
    } else if content.len() >= 80 {
        score += 0.15;
    }
    if lower.contains("file=")
        || lower.contains("path=")
        || lower.contains(".rs")
        || lower.contains(".py")
    {
        score += 0.35;
    }
    if lower.contains("cmd=")
        || lower.contains("rg ")
        || lower.contains("sed -n")
        || lower.contains("cargo ")
    {
        score += 0.25;
    }
    if lower.contains("workstream=") {
        score += 0.15;
    }
    if lower.contains("status=complete")
        || lower.contains("status=blocked")
        || lower.contains("status=unproven")
    {
        score += 0.10;
    }
    let evidence_markers = lower.matches("file=").count()
        + lower.matches("path=").count()
        + lower.matches("cmd=").count()
        + lower.matches("command=").count();
    if evidence_markers >= 4 {
        score += 0.15;
    } else if evidence_markers >= 2 {
        score += 0.08;
    }
    if lower.contains("contextlattice") || lower.contains("source_quality") {
        score += 0.05;
    }
    if lower.contains("not found")
        || lower.contains("no such file")
        || lower.contains("\"entries\":[]")
    {
        score -= 0.15;
    }
    score.clamp(0.0, 1.0)
}

fn update_repo_review_budget_state_from_results(
    state: &mut RepoReviewBudgetState,
    messages: &[Message],
    results: &[ToolResult],
) {
    if !detect_repo_review_intent(messages) {
        *state = RepoReviewBudgetState::default();
        return;
    }
    if results.is_empty() {
        state.last_signal_score = 0.0;
        state.low_signal_streak = state.low_signal_streak.saturating_add(1);
        return;
    }
    let avg_signal = results
        .iter()
        .map(|r| tool_result_signal_score(&r.content, r.is_error))
        .sum::<f64>()
        / results.len() as f64;
    state.last_signal_score = avg_signal;
    if avg_signal < repo_review_min_signal_score() {
        state.low_signal_streak = state.low_signal_streak.saturating_add(1);
    } else {
        state.low_signal_streak = 0;
    }
}

