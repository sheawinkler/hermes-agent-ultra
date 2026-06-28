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

