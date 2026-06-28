fn detect_deep_repo_audit_intent(messages: &[Message]) -> bool {
    if !detect_repo_review_intent(messages) {
        return false;
    }
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    [
        "deep",
        "deeply",
        "comprehensive",
        "full ",
        "full-scope",
        "end-to-end",
        "line-by-line",
        "thorough",
        "complete",
        "surgical",
        "parity",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn objective_guard_policy(messages: &[Message]) -> (bool, bool, bool) {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);

    let objective_active = !objective.is_empty()
        || user.contains("/objective")
        || user.contains("objective:")
        || user.contains("goal:");
    let repo_like = detect_repo_review_intent(messages)
        || combined.contains("plan")
        || combined.contains("analysis")
        || combined.contains("review");
    let trading_like = [
        "solana",
        "wallet",
        "trade",
        "trading",
        "pnl",
        "profit",
        "exponent",
        "objective",
    ]
    .iter()
    .any(|needle| combined.contains(needle));
    let guard_active = objective_active && repo_like;
    let deep_audit_required = guard_active && detect_deep_repo_audit_intent(messages);

    (guard_active, trading_like, deep_audit_required)
}

fn objective_mode_system_hint(messages: &[Message]) -> Option<String> {
    let (guard_active, requires_analytics, deep_audit_required) = objective_guard_policy(messages);
    if !guard_active {
        return None;
    }
    let analytics_line = if requires_analytics {
        "2) ANALYTICS_VERIFIED: include copied metric values (or `objective_state=unproven` with blocker)."
    } else {
        "2) ANALYTICS_VERIFIED: include objective-state evidence relevant to this task."
    };
    let deep_audit_line = if deep_audit_required {
        format!(
            "3) {OBJECTIVE_DEEP_AUDIT_TAG} include `scope_complete=true|false`, at least {OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS} `workstream=<name> status=<complete|blocked|unproven> evidence(file=...|cmd=...)` lines, plus breadth evidence (>= {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES} unique files and >= {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS} unique commands), and explicit `unknowns=` + `blockers=` fields."
        )
    } else {
        String::new()
    };
    Some(format!(
        "[SYSTEM] Objective-mode guard active. Before finalizing, output sections exactly:\n\
         1) {OBJECTIVE_PATCH_TAG} each proposed change must include `path=...` and `exists_now=true|false`.\n\
         {analytics_line}\n\
         {deep_audit_line}\n\
         Use only evidence verified in this run; if missing evidence, state `unproven` explicitly."
    ))
}

fn section_after_tag<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let tag_lc = tag.to_ascii_lowercase();
    let start = text.find(&tag_lc)?;
    Some(&text[start + tag_lc.len()..])
}

fn unique_values_for_markers(section: &str, markers: &[&str]) -> HashSet<String> {
    let mut values = HashSet::new();
    for raw_line in section.lines() {
        let line = raw_line.trim();
        for marker in markers {
            if let Some(idx) = line.find(marker) {
                let candidate = line[idx + marker.len()..]
                    .trim()
                    .trim_matches('`')
                    .trim_matches('"')
                    .trim_matches('\'');
                if candidate.is_empty() {
                    continue;
                }
                let token = candidate
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .trim_end_matches(',')
                    .trim_end_matches(';');
                if !token.is_empty() {
                    values.insert(token.to_string());
                }
                break;
            }
        }
    }
    values
}

fn deep_audit_workstream_lines(section: &str) -> Vec<String> {
    section
        .lines()
        .map(str::trim)
        .filter(|line| {
            line.contains("workstream=")
                || line.contains("workstream:")
                || line.contains("stream=")
                || line.contains("stream:")
        })
        .map(str::to_string)
        .collect()
}

fn workstream_line_has_terminal_status(line: &str) -> bool {
    line.contains("status=complete")
        || line.contains("status: complete")
        || line.contains("status=done")
        || line.contains("status: done")
        || line.contains("status=blocked")
        || line.contains("status: blocked")
        || line.contains("status=unproven")
        || line.contains("status: unproven")
}

fn workstream_line_is_complete(line: &str) -> bool {
    line.contains("status=complete")
        || line.contains("status: complete")
        || line.contains("status=done")
        || line.contains("status: done")
}

fn workstream_line_has_evidence(line: &str) -> bool {
    line.contains("file=")
        || line.contains("file:")
        || line.contains("path=")
        || line.contains("path:")
        || line.contains("cmd=")
        || line.contains("cmd:")
        || line.contains("command=")
        || line.contains("command:")
}

fn deep_audit_verified_patch_items(lower: &str) -> usize {
    let path_hits = ["path=", "path:"]
        .iter()
        .map(|needle| lower.matches(needle).count())
        .sum::<usize>();
    let exists_hits = [
        "exists_now=true",
        "exists_now=false",
        "exists_now: true",
        "exists_now: false",
        "verified_exists=true",
        "verified_exists=false",
        "verified_exists: true",
        "verified_exists: false",
    ]
    .iter()
    .map(|needle| lower.matches(needle).count())
    .sum::<usize>();
    path_hits.min(exists_hits)
}

fn objective_deep_audit_satisfied(lower: &str) -> bool {
    if !lower.contains(&OBJECTIVE_DEEP_AUDIT_TAG.to_ascii_lowercase()) {
        return false;
    }
    if deep_audit_verified_patch_items(lower) < OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS {
        return false;
    }
    let section = section_after_tag(lower, OBJECTIVE_DEEP_AUDIT_TAG).unwrap_or_default();

    let workstream_lines = deep_audit_workstream_lines(section);
    if workstream_lines.len() < OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS {
        return false;
    }
    if workstream_lines.iter().any(|line| {
        !workstream_line_has_terminal_status(line) || !workstream_line_has_evidence(line)
    }) {
        return false;
    }

    let scope_complete_true =
        lower.contains("scope_complete=true") || lower.contains("scope_complete: true");
    let scope_complete_false =
        lower.contains("scope_complete=false") || lower.contains("scope_complete: false");
    if !(scope_complete_true || scope_complete_false) {
        return false;
    }
    if scope_complete_true
        && workstream_lines
            .iter()
            .any(|line| !workstream_line_is_complete(line))
    {
        return false;
    }

    let unique_files = unique_values_for_markers(section, &["file=", "file:", "path=", "path:"]);
    if unique_files.len() < OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES {
        return false;
    }
    let unique_commands =
        unique_values_for_markers(section, &["cmd=", "cmd:", "command=", "command:"]);
    if unique_commands.len() < OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS {
        return false;
    }
    let has_unknowns_field = lower.contains("unknowns=") || lower.contains("unknowns:");
    let has_blockers_field = lower.contains("blockers=") || lower.contains("blockers:");
    has_unknowns_field && has_blockers_field
}

fn objective_guard_satisfied(
    text: &str,
    requires_analytics: bool,
    deep_audit_required: bool,
) -> bool {
    let lower = text.to_ascii_lowercase();
    let has_patch_tag = lower.contains(&OBJECTIVE_PATCH_TAG.to_ascii_lowercase());
    let has_patch_evidence = lower.contains("exists_now=true")
        || lower.contains("exists_now: true")
        || lower.contains("verified_exists=true");
    if !(has_patch_tag && has_patch_evidence) {
        return false;
    }
    if !requires_analytics {
        return true;
    }
    let has_analytics_tag = lower.contains(&OBJECTIVE_ANALYTICS_TAG.to_ascii_lowercase());
    let has_objective_state = lower.contains("objective_state=")
        || lower.contains("objective_state:")
        || lower.contains("metric=");
    let analytics_ok = has_analytics_tag && has_objective_state;
    if !analytics_ok {
        return false;
    }
    if deep_audit_required {
        return objective_deep_audit_satisfied(&lower);
    }
    true
}

fn objective_guard_retry_prompt(requires_analytics: bool, deep_audit_required: bool) -> String {
    let analytics_line = if requires_analytics {
        "Also include copied analytics values and `objective_state=<advancing|flat|regressing|unproven>`."
    } else {
        "Include objective-state evidence even if the objective is currently unproven."
    };
    let deep_audit_line = if deep_audit_required {
        format!(
            "{OBJECTIVE_DEEP_AUDIT_TAG}\n\
             - scope_complete=true|false\n\
             - workstream=<name> status=<complete|blocked|unproven> evidence(file=<path>|cmd=<command>)\n\
             - add at least {OBJECTIVE_DEEP_AUDIT_MIN_WORKSTREAMS} workstream lines\n\
             - file=<verified_path_1>\n\
             - file=<verified_path_2> ... (at least {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_FILES} unique file lines)\n\
             - cmd=<command_1>\n\
             - cmd=<command_2> ... (at least {OBJECTIVE_DEEP_AUDIT_MIN_UNIQUE_COMMANDS} unique command lines)\n\
             - unknowns=<count>\n\
             - blockers=<none|list>\n\
             - include at least {OBJECTIVE_DEEP_AUDIT_MIN_PATCH_ITEMS} verified patch items in {OBJECTIVE_PATCH_TAG}"
        )
    } else {
        String::new()
    };
    format!(
        "[SYSTEM] Objective guard check failed. Re-issue your final response with required sections:\n\
         {OBJECTIVE_PATCH_TAG}\n\
         - path=<verified path>\n\
         - exists_now=true|false\n\
         {OBJECTIVE_ANALYTICS_TAG}\n\
         - objective_state=<value>\n\
         {analytics_line}\n\
         {deep_audit_line}"
    )
}

