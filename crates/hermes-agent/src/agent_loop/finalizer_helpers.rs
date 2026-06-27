/// Extract the last user and assistant content from a message slice for memory sync.
fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

fn latest_user_content(messages: &[Message]) -> Option<&str> {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
}

fn extract_session_objective(messages: &[Message]) -> Option<String> {
    messages
        .iter()
        .find(|m| matches!(m.role, hermes_core::MessageRole::System))
        .and_then(|m| m.content.as_deref())
        .and_then(|content| content.strip_prefix(SESSION_OBJECTIVE_PREFIX))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn detect_repo_review_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let combined = format!("{} {}", user, objective);
    let review_terms = [
        "repo",
        "repository",
        "codebase",
        "review",
        "audit",
        "inspect",
        "diagnose",
        "debug",
        "patch",
        "implement",
        "fix",
        "research",
        "analyze",
        "analysis",
        "assess",
        "report",
        "read-only",
        "readonly",
    ];
    let has_review_signal = review_terms.iter().any(|needle| combined.contains(needle));
    let has_path_signal =
        combined.contains('/') || combined.contains(".rs") || combined.contains(".py");
    has_review_signal && has_path_signal
}

fn detect_communication_intent(messages: &[Message]) -> bool {
    let text = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let comm_terms = [
        "telegram",
        "discord",
        "slack",
        "whatsapp",
        "signal",
        "notify",
        "notification",
        "send message",
        "message me",
        "dm",
    ];
    comm_terms.iter().any(|needle| text.contains(needle))
}

fn detect_tool_profile_escape_hatch(messages: &[Message]) -> bool {
    let text = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let escape_terms = [
        "allow all tools",
        "disable narrowing",
        "open tool profile",
        "no tool filtering",
        "bypass tool profile",
    ];
    escape_terms.iter().any(|needle| text.contains(needle))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepoReviewToolProfileMode {
    Off,
    Balanced,
    Focus,
}

impl RepoReviewToolProfileMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "open" => Some(Self::Off),
            "balanced" | "default" => Some(Self::Balanced),
            "focus" | "strict" => Some(Self::Focus),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Balanced => "balanced",
            Self::Focus => "focus",
        }
    }
}

fn repo_review_tool_profile_mode() -> RepoReviewToolProfileMode {
    std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .as_deref()
        .and_then(RepoReviewToolProfileMode::parse)
        .unwrap_or(RepoReviewToolProfileMode::Balanced)
}

fn detect_exploratory_repo_research_intent(messages: &[Message]) -> bool {
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
        "explore",
        "investigate",
        "understand",
        "diagnose",
        "audit",
        "research",
        "analyze",
        "analysis",
        "assess",
        "report",
        "read-only",
        "readonly",
        "deep",
        "root cause",
        "why",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn exploratory_problem_solving_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_exploratory_repo_research_intent(messages) {
        return None;
    }
    Some(
        "[SYSTEM] Exploratory problem-solving protocol active. \
1) Start by declaring workstreams (`workstream=<name>`) that cover the full problem surface. \
2) Run focused evidence collection per workstream (`file=...`, `cmd=...`) rather than repeated broad scans. \
3) After each evidence batch, update status per workstream (`complete|blocked|unproven`) and refine next probes. \
4) Do not finalize until high-leverage workstreams are either complete or explicitly blocked with concrete blockers and next actions. \
5) Final synthesis must include `REPO_RESEARCH_PLAN: complete` or `REPO_RESEARCH_PLAN: blocked`, plus `workstream=... status=... file=... cmd=...` evidence lines."
            .to_string(),
    )
}

fn detect_web_research_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    [
        "web search",
        "search the web",
        "across the web",
        "online research",
        "internet research",
        "browse",
        "browser",
        "cite urls",
        "cite concrete urls",
        "urls",
        "http",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn web_research_system_hint(messages: &[Message], tool_schemas: &[ToolSchema]) -> Option<String> {
    if !detect_web_research_intent(messages) {
        return None;
    }
    let has_web_tool = tool_schemas
        .iter()
        .any(|t| matches!(t.name.as_str(), "web_search" | "web_extract" | "web_crawl"));
    let availability = if has_web_tool {
        "Use `web_search` first, rank candidates by returned `source_quality`/`source_quality_score`, then `web_extract` for the highest-value primary, official, repository, or expert-community sources before final synthesis."
    } else {
        "No web tools are advertised in this session; report that exact blocker instead of inventing sources."
    };
    Some(format!(
        "[SYSTEM] Web research contract active. {availability} Final answer requirements: include `WEB_SEARCH_USED: yes` only after a web tool succeeds, cite concrete http(s) URLs copied from web tool results, include `SOURCE_QUALITY: primary=<n> community=<n> secondary=<n>`, and separate observed source evidence from speculation. For web evidence use `url=<http(s) URL>` or raw URLs; do not substitute local `file=` evidence for web sources. If every web tool call fails or no web tool is available, write `WEB_SEARCH_USED: no` with the exact blocker."
    ))
}

fn terminal_command_system_hint(tool_schemas: &[ToolSchema]) -> Option<&'static str> {
    if !tool_schemas.iter().any(|tool| tool.name == "terminal") {
        return None;
    }
    Some(
        "[SYSTEM] Terminal command contract: the `terminal` tool already executes commands through the configured shell. \
         Do not wrap commands in `bash -lc`, `sh -c`, or `zsh -c`; those shell-string wrappers require explicit approval. \
         Prefer direct commands and separate terminal calls for read-only probes.",
    )
}

fn detect_google_workspace_intent(messages: &[Message]) -> bool {
    let user = messages
        .iter()
        .filter(|message| matches!(message.role, MessageRole::User))
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
        .to_ascii_lowercase();
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    combined.contains("gmail")
        || combined.contains("google workspace")
        || combined.contains("google cli")
        || combined.contains("@gmail.com")
        || (combined.contains("google") && combined.contains("email"))
}

fn history_includes_google_workspace_skill(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        matches!(m.role, MessageRole::Tool)
            && m.name.as_deref() == Some("skill_view")
            && m.content
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("google workspace")
    })
}

fn history_includes_google_workspace_setup_probe(messages: &[Message]) -> bool {
    let command_seen = messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Assistant) {
            return false;
        }
        m.tool_calls.as_ref().is_some_and(|calls| {
            calls.iter().any(|call| {
                call.function.name == "terminal" && {
                    let args = call.function.arguments.to_ascii_lowercase();
                    args.contains("google-workspace")
                        && (args.contains("setup.py") || args.contains("google_api.py"))
                }
            })
        })
    });
    if !command_seen {
        return false;
    }
    messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Tool) || m.name.as_deref() != Some("terminal") {
            return false;
        }
        let content = m
            .content
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        content.contains("google_token.json")
            || content.contains("authenticated")
            || content.contains("not_authenticated")
            || content.contains("no token")
            || content.contains("google_client_secret.json")
            || content.contains("no such file")
            || content.contains("error")
    })
}

fn history_includes_google_workspace_auth_blocker(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Tool) || m.name.as_deref() != Some("terminal") {
            return false;
        }
        let content = m
            .content
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        content.contains("not_authenticated")
            || content.contains("not authenticated")
            || content.contains("no token at")
            || content.contains("run the setup script first")
    })
}

fn history_includes_gmail_api_probe(messages: &[Message]) -> bool {
    let command_seen = messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Assistant) {
            return false;
        }
        m.tool_calls.as_ref().is_some_and(|calls| {
            calls.iter().any(|call| {
                call.function.name == "terminal" && {
                    let args = call.function.arguments.to_ascii_lowercase();
                    args.contains("google_api.py") && args.contains("gmail")
                }
            })
        })
    });
    if !command_seen {
        return false;
    }
    messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Tool) || m.name.as_deref() != Some("terminal") {
            return false;
        }
        let content = m
            .content
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        !content.contains("not_authenticated")
            && !content.contains("not authenticated")
            && !content.contains("run the setup script first")
            && (content.contains("\"id\"")
                || content.contains("\"messages\"")
                || content.contains("\"subject\"")
                || content.contains("\"snippet\"")
                || content.contains("\"body\""))
    })
}

fn google_workspace_auth_blocker_mutation_guard(
    messages: &[Message],
    tool_calls: &[ToolCall],
) -> Option<&'static str> {
    if !detect_google_workspace_intent(messages)
        || !history_includes_google_workspace_auth_blocker(messages)
    {
        return None;
    }
    let attempts_mutation = tool_calls.iter().any(|call| {
        let name = call.function.name.as_str();
        let args = call.function.arguments.to_ascii_lowercase();
        matches!(
            name,
            "write_file" | "patch" | "apply_patch" | "skill_manage"
        ) || args.contains("--client-secret")
            || args.contains("--auth-url")
            || args.contains("--auth-code")
            || args.contains("google_client_secret.json")
            || args.contains("simulated")
            || args.contains("fake")
            || args.contains("dummy")
    });
    attempts_mutation.then_some(
        "[SYSTEM] Google Workspace auth blocker already observed. This request is a read-only Gmail backfill, not a setup flow. \
         Do not create simulated OAuth clients, write credential files, patch skills, run `--client-secret`, run `--auth-url`, or run `--auth-code`. \
         Final answer must be `GOOGLE_WORKSPACE_USED: no` with the exact NOT_AUTHENTICATED/no-token command output and next legitimate setup command for the user.",
    )
}

fn finalizer_google_workspace_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_GOOGLE_WORKSPACE_MAX_RETRIES
        || !detect_google_workspace_intent(messages)
    {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let marker_text = lower.replace('*', "");
    if !marker_text.contains("google_workspace_used: yes")
        && !marker_text.contains("google_workspace_used: no")
        && !marker_text.contains("google_workspace_used=yes")
        && !marker_text.contains("google_workspace_used=no")
    {
        return true;
    }
    let claims_blocked = [
        "blocked",
        "cannot",
        "no viable",
        "no google",
        "no gmail",
        "not authenticated",
        "no credentials",
        "credentials verification",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let claims_absent_despite_skill = history_includes_google_workspace_skill(messages)
        && [
            "no google workspace",
            "no google/gmail",
            "no gmail/email api",
            "no tools",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    if claims_absent_despite_skill {
        return true;
    }
    let claims_success = [
        "google_workspace_used: yes",
        "emails were found",
        "important emails",
        "gmail search and reading were successful",
        "authenticated and working",
        "full message text",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if claims_success {
        return history_includes_google_workspace_auth_blocker(messages)
            || !history_includes_gmail_api_probe(messages);
    }
    if !claims_blocked {
        return false;
    }
    let has_final_evidence = (lower.contains("setup.py")
        || lower.contains("google_api.py")
        || lower.contains("google_token.json"))
        && (lower.contains("cmd=") || lower.contains("command="));
    !(has_final_evidence && history_includes_google_workspace_setup_probe(messages))
}

fn history_includes_web_tool(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Tool) {
            return false;
        }
        m.name
            .as_deref()
            .is_some_and(|name| matches!(name, "web_search" | "web_extract" | "web_crawl"))
    })
}

fn history_includes_web_extract_or_crawl(messages: &[Message]) -> bool {
    messages.iter().any(|m| {
        if !matches!(m.role, MessageRole::Tool) {
            return false;
        }
        m.name
            .as_deref()
            .is_some_and(|name| matches!(name, "web_extract" | "web_crawl"))
    })
}

fn count_http_urls(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| token.starts_with("http://") || token.starts_with("https://"))
        .map(|token| token.trim_end_matches([',', '.', ')', ']', ';']))
        .collect::<HashSet<_>>()
        .len()
}

fn parse_source_quality_counts(text: &str) -> Option<(u32, u32, u32)> {
    let lower = text.to_ascii_lowercase();
    let idx = lower.find("source_quality")?;
    let tail = &lower[idx..];
    fn marker_count(tail: &str, marker: &str) -> u32 {
        tail.split(marker)
            .nth(1)
            .and_then(|rest| {
                rest.trim_start_matches([' ', ':', '='])
                    .chars()
                    .take_while(|ch| ch.is_ascii_digit())
                    .collect::<String>()
                    .parse::<u32>()
                    .ok()
            })
            .unwrap_or(0)
    }
    Some((
        marker_count(tail, "primary"),
        marker_count(tail, "community"),
        marker_count(tail, "secondary"),
    ))
}

fn has_sufficient_source_quality(text: &str) -> bool {
    let Some((primary, community, secondary)) = parse_source_quality_counts(text) else {
        return false;
    };
    primary > 0 || community >= 2 || (primary + community + secondary >= 3 && community > 0)
}

fn web_research_retry_prompt() -> &'static str {
    "[SYSTEM] Web research contract failed. This request explicitly requires online research.\n\
     Requirements now:\n\
     - call `web_search` with targeted queries before answering\n\
     - use returned `source_quality`/`source_quality_score` metadata to choose sources\n\
     - call `web_extract` or `web_crawl` on at least one highest-value primary, official, repository, or expert-community source\n\
     - final answer must include the exact line `WEB_SEARCH_USED: yes` after successful web tooling\n\
     - cite at least two concrete http(s) URLs copied from web tool results\n\
     - include `SOURCE_QUALITY: primary=<n> community=<n> secondary=<n>` and prefer primary/community evidence over generic SEO summaries\n\
     - for each web finding, include `url=<http(s) URL>` or the raw URL; do not use local `file=` evidence as a substitute\n\
     - separate observed evidence from speculation\n\
     If web tooling is unavailable or errors, final answer must include `WEB_SEARCH_USED: no` and the exact tool error/blocker."
}

fn finalizer_web_research_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_WEB_RESEARCH_MAX_RETRIES || !detect_web_research_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let has_success_line =
        lower.contains("web_search_used: yes") || lower.contains("web_search_used=yes");
    let has_blocked_line =
        lower.contains("web_search_used: no") || lower.contains("web_search_used=no");
    if has_blocked_line && (lower.contains("blocked") || lower.contains("error")) {
        return false;
    }
    !(history_includes_web_tool(messages)
        && history_includes_web_extract_or_crawl(messages)
        && has_success_line
        && count_http_urls(assistant_text) >= 2
        && has_sufficient_source_quality(assistant_text))
}

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

fn objective_eval_score(state: &str) -> f64 {
    match state.trim().to_ascii_lowercase().as_str() {
        "advancing" => 1.0,
        "flat" => 0.5,
        "regressing" => 0.0,
        "unproven" => 0.25,
        _ => 0.4,
    }
}

fn claim_verifier_enabled_runtime() -> bool {
    if let Ok(raw) = std::env::var("HERMES_CLAIM_VERIFIER_ENABLED") {
        return !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        );
    }
    let hermes_home = std::env::var("HERMES_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".hermes")))
        .unwrap_or_else(|| PathBuf::from(".hermes"));
    let path = hermes_home.join("alpha").join("claim_verifier_policy.json");
    let raw = match std::fs::read_to_string(path) {
        Ok(v) => v,
        Err(_) => return true,
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return true,
    };
    parsed
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

fn detect_research_evidence_intent(messages: &[Message]) -> bool {
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
        "research",
        "analyze",
        "analysis",
        "assess",
        "report",
        "read-only",
        "readonly",
        "evidence",
        "recommendation",
        "recommendations",
        "improve",
        "profitability",
    ]
    .iter()
    .any(|needle| combined.contains(needle))
}

fn normalize_evidence_path_token(raw: &str) -> Option<String> {
    let mut token = raw
        .trim()
        .trim_start_matches(['`', '"', '\''])
        .trim_end_matches(['`', '"', '\'', ',', ';', ')', ']', '.'])
        .to_string();
    if token.is_empty()
        || token.starts_with('<')
        || token.starts_with('$')
        || token.starts_with("http://")
        || token.starts_with("https://")
        || token.contains("...")
    {
        return None;
    }
    if let Some((path, suffix)) = token.rsplit_once(':') {
        if !path.is_empty() && suffix.chars().all(|c| c.is_ascii_digit() || c == '-') {
            token = path.to_string();
        }
    }
    Some(token)
}

fn extract_explicit_evidence_paths(assistant_text: &str) -> Vec<String> {
    let markers = ["file=", "path=", "file:", "path:"];
    let mut paths = Vec::new();
    for line in assistant_text.lines() {
        let lower = line.to_ascii_lowercase();
        for marker in markers {
            let mut search_start = 0usize;
            while let Some(relative_idx) = lower[search_start..].find(marker) {
                let value_start = search_start + relative_idx + marker.len();
                let raw = line[value_start..].trim_start();
                let raw = raw
                    .split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';' | ')' | ']'))
                    .next()
                    .unwrap_or_default();
                if let Some(token) = normalize_evidence_path_token(raw) {
                    paths.push(token);
                }
                search_start = value_start;
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn assistant_references_missing_evidence_paths_from_base(
    assistant_text: &str,
    base: &Path,
) -> bool {
    extract_explicit_evidence_paths(assistant_text)
        .iter()
        .any(|token| {
            let path = Path::new(token);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                base.join(path)
            };
            !resolved.exists()
        })
}

fn assistant_references_missing_evidence_paths(assistant_text: &str) -> bool {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    assistant_references_missing_evidence_paths_from_base(assistant_text, &base)
}

fn finalizer_claim_requires_evidence_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if !claim_verifier_enabled_runtime() {
        return false;
    }
    if retry_count >= FINALIZER_EVIDENCE_MAX_RETRIES || !detect_repo_review_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let claims_completion = [
        "completed",
        "implemented",
        "fixed",
        "done",
        "resolved",
        "ready",
        "finished",
        "shipped",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let research_evidence_required = detect_research_evidence_intent(messages);
    if !(claims_completion || research_evidence_required) {
        return false;
    }
    let has_evidence = lower.contains("file=")
        || lower.contains("path=")
        || lower.contains("cmd=")
        || lower.contains("exists_now=")
        || lower.contains("`/users/")
        || lower.contains("cargo test");
    let has_confidence = lower.contains("confidence=high")
        || lower.contains("confidence=medium")
        || lower.contains("confidence=low")
        || lower.contains("confidence:");
    if research_evidence_required {
        let has_explicit_path_evidence = lower.contains("file=") || lower.contains("path=");
        let has_explicit_command_evidence = lower.contains("cmd=") || lower.contains("command=");
        if !(has_explicit_path_evidence && has_explicit_command_evidence && has_confidence) {
            return true;
        }
        if assistant_references_missing_evidence_paths(assistant_text) {
            return true;
        }
        return false;
    }
    !(has_evidence && has_confidence)
}

fn strip_list_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    let without_bullet = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
        .unwrap_or(trimmed);
    let mut chars = without_bullet.char_indices();
    let mut end_idx = 0usize;
    while let Some((idx, ch)) = chars.next() {
        if ch.is_ascii_digit() {
            end_idx = idx + ch.len_utf8();
            continue;
        }
        if (ch == '.' || ch == ')') && end_idx > 0 {
            let tail = &without_bullet[idx + ch.len_utf8()..];
            return tail.trim_start();
        }
        break;
    }
    without_bullet
}

fn finalizer_output_quality_requires_retry(assistant_text: &str, retry_count: u32) -> bool {
    if retry_count >= FINALIZER_OUTPUT_QUALITY_MAX_RETRIES {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    let placeholder_markers = [
        "[url](url)",
        "(url)",
        "[paper details](url)",
        "pack of authors",
        "attached separately",
        "attached reference output",
        "full evidence available",
        "full remand data",
        "see telemetry evidence answer",
        "proposed calibration: redacted",
        "working summary",
        "<tool_call",
        "</tool_call>",
        "lorem ipsum",
        "<insert",
        "<todo",
    ];
    if placeholder_markers
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return true;
    }

    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut in_code_block = false;
    for raw_line in assistant_text.lines() {
        let line = raw_line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        let normalized = strip_list_prefix(line).trim().to_ascii_lowercase();
        if normalized.len() < 24 {
            continue;
        }
        let entry = counts.entry(normalized).or_insert(0);
        *entry += 1;
        if *entry >= 3 {
            return true;
        }
    }
    false
}

fn assistant_response_has_execution_evidence(lower: &str) -> bool {
    [
        "file=",
        "path=",
        "cmd=",
        "exists_now=",
        "objective_state=",
        "error:",
        "blocked:",
        "blocker:",
        "run finished",
        "tested",
        "verified",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn detect_execution_required_intent(messages: &[Message]) -> bool {
    let user = latest_user_content(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if user.trim().is_empty() {
        return false;
    }
    let objective = extract_session_objective(messages)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let combined = format!("{} {}", user, objective);
    let action_terms = [
        "proceed",
        "implement",
        "fix",
        "debug",
        "diagnose",
        "run",
        "test",
        "patch",
        "sync",
        "rebuild",
        "verify",
        "connect",
        "integrat",
        "investigate",
        "analyze",
        "review",
    ];
    let has_action = action_terms.iter().any(|needle| combined.contains(needle));
    let has_surface = combined.contains("repo")
        || combined.contains("repository")
        || combined.contains("codebase")
        || combined.contains("contextlattice")
        || combined.contains('/')
        || combined.contains(".rs")
        || combined.contains(".py")
        || combined.contains("session");
    has_action && has_surface
}

fn finalizer_action_execution_requires_retry(
    messages: &[Message],
    assistant_text: &str,
    retry_count: u32,
) -> bool {
    if retry_count >= FINALIZER_ACTION_EXECUTION_MAX_RETRIES {
        return false;
    }
    if !detect_execution_required_intent(messages) {
        return false;
    }
    let lower = assistant_text.to_ascii_lowercase();
    if assistant_response_has_execution_evidence(&lower) {
        return false;
    }
    let deferral_markers = [
        "i will",
        "i'll",
        "let me",
        "i can",
        "i'm going to",
        "proceeding",
        "next i",
        "i should",
        "i would",
    ];
    deferral_markers.iter().any(|needle| lower.contains(needle))
}

fn detect_contextlattice_connect_intent(messages: &[Message]) -> bool {
    let Some(last_user) = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
    else {
        return false;
    };
    let lower = last_user.to_ascii_lowercase();
    if !lower.contains("contextlattice") {
        return false;
    }
    [
        "connect",
        "connection",
        "configure",
        "setup",
        "set up",
        "verify",
        "harden",
        "probe",
        "integrat",
        "health",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn contextlattice_connect_system_hint(messages: &[Message]) -> Option<String> {
    if !detect_contextlattice_connect_intent(messages) {
        return None;
    }
    Some(
        "[SYSTEM] ContextLattice integration intent detected. Execute this order: \
         (1) If available, inspect local instructions file from `HERMES_CONTEXTLATTICE_INSTRUCTIONS_PATH` \
         (2) call `contextlattice_search` for a direct connectivity probe; \
         (3) if needed call `contextlattice_context_pack` for broader grounding; \
         (4) call `contextlattice_write` to checkpoint what was verified. \
         Never use terminal command `contextlattice` for this workflow."
            .to_string(),
    )
}

fn contextlattice_intelligence_system_hint(
    messages: &[Message],
    tool_schemas: &[ToolSchema],
) -> Option<String> {
    let has_context_tools = tool_schemas.iter().any(|t| {
        matches!(
            t.name.as_str(),
            "contextlattice_search"
                | "contextlattice_context_pack"
                | "contextlattice_write"
                | "memory"
        )
    });
    if !has_context_tools {
        return None;
    }

    let objective_active = objective_guard_policy(messages).0;
    let repo_intent = detect_repo_review_intent(messages);
    let connect_intent = detect_contextlattice_connect_intent(messages);
    if !(objective_active || repo_intent || connect_intent) {
        return None;
    }

    Some(
        "[SYSTEM] ContextLattice-first intelligence policy active.\n\
         1) Start with scoped retrieval (`contextlattice_search`) using project + topic path.\n\
         2) If scoped retrieval is empty/degraded, run one broader retrieval in the same project and compare.\n\
         3) For broad or multi-file tasks, run `contextlattice_context_pack` before deep tool loops.\n\
         4) During long execution, checkpoint durable progress with `contextlattice_write`.\n\
         5) Before final answer, run one scoped readback and report contradictions as `unproven` rather than guessing.\n\
         6) Copy numeric facts verbatim; do not normalize or round unless explicitly requested."
            .to_string(),
    )
}

fn is_contextlattice_shell_invocation(raw_args: &str) -> bool {
    let Ok(args) = serde_json::from_str::<Value>(raw_args) else {
        return false;
    };
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or_default();
    let lower = command.to_ascii_lowercase();
    lower == "contextlattice" || lower.starts_with("contextlattice ")
}

fn is_safe_background_review_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    // Suppress operational/status leakage from background passes.
    if lower.contains("status:")
        || lower.contains("status=")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("secret")
        || lower.contains("credential")
    {
        return false;
    }
    // Skip verbose object-like payloads.
    if (trimmed.contains('{') && trimmed.contains('}')) || trimmed.contains('\n') {
        return false;
    }
    true
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    let mut actions: Vec<String> = Vec::new();
    for msg in messages {
        if !matches!(msg.role, hermes_core::MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if !data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            continue;
        }
        if !is_safe_background_review_message(&message) {
            continue;
        }
        let lower = message.to_ascii_lowercase();
        if lower.contains("created") || lower.contains("updated") {
            actions.push(message);
        } else if lower.contains("added") || (!target.is_empty() && lower.contains("add")) {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if message.contains("Entry added") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if lower.contains("removed") || lower.contains("replaced") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("💾 {}", deduped.join(" · ")))
}

fn default_model_cost_per_million(model: &str) -> Option<(f64, f64)> {
    let m = model.to_lowercase();
    if m.contains("gpt-4o-mini") || m.contains("4.1-mini") || m.contains("haiku") {
        return Some((0.15, 0.60));
    }
    if m.contains("gpt-4o") || m.contains("4.1") || m.contains("sonnet") {
        return Some((2.5, 10.0));
    }
    if m.contains("o3") {
        return Some((10.0, 40.0));
    }
    None
}

fn extract_objective_state_marker(text: &str) -> String {
    for line in text.lines() {
        let lowered = line.trim().to_ascii_lowercase();
        if let Some(rest) = lowered.split("objective_state=").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
        if let Some(rest) = lowered.split("objective_state:").nth(1) {
            let token = rest
                .trim_start()
                .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
                .next()
                .unwrap_or("")
                .trim_matches(|c: char| c == ')' || c == '.');
            if !token.is_empty() {
                return token.to_string();
            }
        }
    }
    "unspecified".to_string()
}

fn extract_marker_values(text: &str, marker: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(idx) = line.find(marker) else {
            continue;
        };
        let rest = &line[idx + marker.len()..];
        let value = rest
            .split(|c: char| c.is_whitespace() || c == ')' || c == ',' || c == ';' || c == '|')
            .next()
            .unwrap_or("")
            .trim();
        if value.is_empty() {
            continue;
        }
        let normalized = value.trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
        if normalized.is_empty() || out.iter().any(|v| v == normalized) {
            continue;
        }
        out.push(normalized.to_string());
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn estimate_usage_cost_usd(usage: &UsageStats, model: &str, config: &AgentConfig) -> Option<f64> {
    if let Some(v) = usage.estimated_cost {
        return Some(v.max(0.0));
    }
    let (in_pm, out_pm) = match (
        config.prompt_cost_per_million_usd,
        config.completion_cost_per_million_usd,
    ) {
        (Some(i), Some(o)) => (i, o),
        _ => default_model_cost_per_million(model)?,
    };
    let prompt_cost = (usage.prompt_tokens as f64 / 1_000_000.0) * in_pm;
    let completion_cost = (usage.completion_tokens as f64 / 1_000_000.0) * out_pm;
    Some(prompt_cost + completion_cost)
}

/// Merge two UsageStats, summing token counts and keeping the latest cost estimate.
fn merge_usage(existing: Option<UsageStats>, new: &UsageStats) -> UsageStats {
    match existing {
        Some(prev) => UsageStats {
            prompt_tokens: prev.prompt_tokens + new.prompt_tokens,
            completion_tokens: prev.completion_tokens + new.completion_tokens,
            total_tokens: prev.total_tokens + new.total_tokens,
            estimated_cost: match (prev.estimated_cost, new.estimated_cost) {
                (Some(a), Some(b)) => Some(a + b),
                (Some(a), None) => Some(a),
                (None, Some(b)) => Some(b),
                (None, None) => None,
            },
        },
        None => new.clone(),
    }
}

