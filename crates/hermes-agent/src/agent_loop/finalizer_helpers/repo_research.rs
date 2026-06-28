fn explicit_task_anchors(messages: &[Message]) -> Vec<String> {
    let Some(user) = latest_user_content(messages) else {
        return Vec::new();
    };
    let mut anchors = Vec::new();
    for raw in user.split_whitespace() {
        let token = raw
            .trim()
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '[' | ']' | '{' | '}'
                )
            })
            .trim_start_matches('#');
        if token.len() < 4 {
            continue;
        }
        let token_lc = token.to_ascii_lowercase();
        let explicit = token.contains('@')
            || token.starts_with('/')
            || token.starts_with("http://")
            || token.starts_with("https://")
            || token.contains(".rs")
            || token.contains(".py")
            || token.contains('_')
            || token.contains('-')
            || token.contains('/')
            || token_lc.contains("gmail")
            || token_lc.contains("google")
            || token_lc.contains("solana")
            || token_lc.contains("telegram")
            || token_lc.contains("contextlattice")
            || token_lc.contains("algotrader")
            || token_lc.contains("hermes");
        if explicit {
            anchors.push(token_lc);
        }
    }
    anchors.sort();
    anchors.dedup();
    anchors.truncate(12);
    anchors
}

fn finalizer_task_focus_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_TASK_FOCUS_MAX_RETRIES {
        return false;
    }
    if assistant_text.trim().is_empty() {
        return false;
    }
    let anchors = explicit_task_anchors(messages);
    if anchors.is_empty() {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    if lower.contains("blocked")
        || lower.contains("unproven")
        || lower.contains("cannot verify")
        || lower.contains("not authenticated")
    {
        return false;
    }
    anchors
        .iter()
        .take(8)
        .all(|anchor| !lower.contains(anchor.as_str()))
}

fn repo_research_retry_prompt() -> &'static str {
    "[SYSTEM] Repo research planning contract failed. Before final synthesis, produce a grounded research map.\n\
     Requirements now:\n\
     - include `REPO_RESEARCH_PLAN: complete` when covered, or `REPO_RESEARCH_PLAN: blocked` with exact blockers\n\
     - for complete research, include at least three lines shaped like `workstream=<name> status=<complete|blocked|unproven> file=<existing path> cmd=<probe>`\n\
     - include at least two distinct `file=`/`path=` evidence markers and two distinct `cmd=`/`command=` probes\n\
     - include `confidence=<high|medium|low>`\n\
     - mark uncertain claims as `status=unproven` instead of presenting them as findings."
}

fn repo_research_workstream_evidence_lines(assistant_text: &str) -> usize {
    assistant_text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("workstream=")
                && lower.contains("status=")
                && (lower.contains("file=") || lower.contains("path="))
                && (lower.contains("cmd=") || lower.contains("command="))
        })
        .count()
}

fn repo_research_command_count(assistant_text: &str) -> usize {
    assistant_text
        .lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("cmd=") || lower.contains("command=")
        })
        .count()
}

fn finalizer_repo_research_plan_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_REPO_RESEARCH_PLAN_MAX_RETRIES
        || !detect_exploratory_repo_research_intent(messages)
    {
        return false;
    }
    if assistant_text.trim().is_empty() {
        return false;
    }

    let lower = assistant_text.to_ascii_lowercase();
    let blocked_or_unproven = lower.contains("repo_research_plan: blocked")
        || lower.contains("repo_research_plan=blocked")
        || lower.contains("repo_research_plan: unproven")
        || lower.contains("repo_research_plan=unproven");
    if blocked_or_unproven {
        return !(lower.contains("blocker")
            && (lower.contains("cmd=")
                || lower.contains("command=")
                || lower.contains("file=")
                || lower.contains("path=")));
    }

    let complete = lower.contains("repo_research_plan: complete")
        || lower.contains("repo_research_plan=complete");
    let has_confidence = lower.contains("confidence=high")
        || lower.contains("confidence=medium")
        || lower.contains("confidence=low")
        || lower.contains("confidence:");
    if !complete || !has_confidence {
        return true;
    }

    if repo_research_workstream_evidence_lines(assistant_text) < REPO_RESEARCH_MIN_WORKSTREAMS {
        return true;
    }
    if extract_explicit_evidence_paths(assistant_text).len() < REPO_RESEARCH_MIN_UNIQUE_FILES {
        return true;
    }
    if repo_research_command_count(assistant_text) < REPO_RESEARCH_MIN_COMMANDS {
        return true;
    }
    assistant_references_missing_evidence_paths(assistant_text)
}

