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

