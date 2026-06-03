//! Per-run web tool budgets: separate pools per tool, billable-only accounting, URL dedup.
//!
//! Scope is **one user message** (one `AgentLoop::run` / `run_stream` invocation), not the
//! whole session thread.

use std::collections::{HashMap, HashSet};

use hermes_core::{Message, MessageRole, ToolCall, ToolResult};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Limits & state
// ---------------------------------------------------------------------------

/// Per-run limits loaded from environment (see `docs/sop/web_tool_budget.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebToolBudgetLimits {
    pub browser_max: u32,
    pub extract_max: u32,
    pub search_max: u32,
    /// Optional aggregate backstop (`HERMES_WEB_TOOL_BUDGET_MAX_CALLS`). When `None`, only per-pool caps apply.
    pub aggregate_max: Option<u32>,
    pub max_consecutive_errors: u32,
}

impl WebToolBudgetLimits {
    pub fn from_env() -> Self {
        Self {
            browser_max: env_u32("HERMES_BROWSER_BUDGET_MAX_CALLS", 2),
            extract_max: env_u32("HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS", 5),
            search_max: env_u32("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", 2),
            aggregate_max: std::env::var("HERMES_WEB_TOOL_BUDGET_MAX_CALLS")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|v| *v > 0),
            max_consecutive_errors: env_u32("HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS", 2),
        }
    }
}

/// Mutable counters for the current run.
#[derive(Debug, Clone, Default)]
pub struct WebToolBudgetState {
    pub browser_used: u32,
    pub extract_used: u32,
    pub search_used: u32,
    /// Billable successes across all web tools (for optional aggregate backstop only).
    pub billable_total: u32,
    pub consecutive_error_turns: u32,
}

impl WebToolBudgetState {
    pub fn new() -> Self {
        Self::default()
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

pub fn is_budgeted_web_tool(name: &str) -> bool {
    matches!(name, "web_search" | "web_extract" | "browser_navigate")
}

// ---------------------------------------------------------------------------
// URL normalization & dedup (P1)
// ---------------------------------------------------------------------------

fn normalize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    let scheme = parsed.scheme().to_ascii_lowercase();
    let mut path = parsed.path().to_string();
    while path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    let query = parsed.query().map(|q| format!("?{q}")).unwrap_or_default();
    Some(format!("{scheme}://{host}{path}{query}"))
}

fn url_from_tool_arguments(name: &str, arguments: &str) -> Option<String> {
    if !matches!(name, "web_extract" | "browser_navigate") {
        return None;
    }
    let args: Value = serde_json::from_str(arguments).ok()?;
    args.get("url")
        .and_then(|v| v.as_str())
        .and_then(|u| normalize_url(u))
}

fn is_successful_web_extract_content(content: &str) -> bool {
    if looks_like_tool_error_output(content) {
        return false;
    }
    let trimmed = content.trim();
    if trimmed.len() < 80 {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = value.as_object() {
            if let Some(body) = obj.get("content").and_then(|v| v.as_str()) {
                return !body.trim().is_empty();
            }
        }
    }
    true
}

/// URLs with a prior **successful** `web_extract` in conversation messages.
pub fn cached_extract_urls_from_messages(messages: &[Message]) -> HashSet<String> {
    let mut call_names: HashMap<String, String> = HashMap::new();
    for msg in messages {
        if msg.role != MessageRole::Assistant {
            continue;
        }
        let Some(calls) = msg.tool_calls.as_ref() else {
            continue;
        };
        for tc in calls {
            call_names.insert(tc.id.clone(), tc.function.name.clone());
        }
    }

    let mut urls = HashSet::new();
    for msg in messages {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let Some(tid) = msg.tool_call_id.as_ref() else {
            continue;
        };
        if !matches!(call_names.get(tid).map(String::as_str), Some("web_extract")) {
            continue;
        }
        let Some(content) = msg.content.as_deref() else {
            continue;
        };
        if !is_successful_web_extract_content(content) {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(content) {
            if let Some(u) = value.get("url").and_then(|v| v.as_str()) {
                if let Some(n) = normalize_url(u) {
                    urls.insert(n);
                    continue;
                }
            }
        }
        // Fallback: scan assistant tool_calls for matching id and arguments url
        for msg_a in messages {
            if msg_a.role != MessageRole::Assistant {
                continue;
            }
            let Some(calls) = msg_a.tool_calls.as_ref() else {
                continue;
            };
            for tc in calls {
                if tc.id == *tid {
                    if let Some(n) = url_from_tool_arguments(&tc.function.name, &tc.function.arguments)
                    {
                        urls.insert(n);
                    }
                }
            }
        }
    }
    urls
}

pub fn web_url_dedup_user_notice() -> String {
    "该 URL 已在当前会话上下文中抓取，请基于已有 tool 结果回答。（本则用户消息内不再重复抓取）"
        .to_string()
}

/// Block `web_extract` / `browser_navigate` when the same URL was already extracted successfully.
pub fn apply_web_url_dedup(
    messages: &[Message],
    tool_calls: &mut Vec<ToolCall>,
    turn: u32,
) -> Vec<(String, ToolResult)> {
    let cached = cached_extract_urls_from_messages(messages);
    if cached.is_empty() {
        return Vec::new();
    }

    let mut blocked = Vec::new();
    let mut kept = Vec::with_capacity(tool_calls.len());
    for tc in tool_calls.drain(..) {
        let name = tc.function.name.as_str();
        if !matches!(name, "web_extract" | "browser_navigate") {
            kept.push(tc);
            continue;
        }
        let Some(url) = url_from_tool_arguments(name, &tc.function.arguments) else {
            kept.push(tc);
            continue;
        };
        if cached.contains(&url) {
            let reason = format!(
                "Web URL dedup on turn {turn}: blocked '{name}' for URL already extracted in context ({url})."
            );
            tracing::info!(
                turn = turn,
                tool = name,
                url = %url,
                scope = "run",
                "web_tool_budget dedup block"
            );
            blocked.push((
                tc.function.name.clone(),
                ToolResult::err(tc.id, reason),
            ));
            continue;
        }
        kept.push(tc);
    }
    *tool_calls = kept;
    blocked
}

// ---------------------------------------------------------------------------
// Budget pre-check
// ---------------------------------------------------------------------------

pub fn web_tool_budget_user_notice(tool_name: &str, blocked_by_errors: bool) -> String {
    let scope = "（本则用户消息配额）";
    match tool_name {
        "web_search" => format!("网络检索次数已达上限{scope}，将基于已有信息直接回复。"),
        "web_extract" | "browser_navigate" if blocked_by_errors => {
            format!("网页读取多次失败{scope}，将基于已有信息直接回复。")
        }
        "web_extract" | "browser_navigate" => {
            format!("网页抓取次数已达上限{scope}，将基于已有信息直接回复。可换 URL 或基于上文 tool 结果回答。")
        }
        _ => format!("工具 {tool_name} 调用受限{scope}，将基于已有信息直接回复。"),
    }
}

pub fn apply_web_tool_budget(
    state: &WebToolBudgetState,
    limits: &WebToolBudgetLimits,
    tool_calls: &mut Vec<ToolCall>,
    turn: u32,
) -> Vec<(String, ToolResult)> {
    let mut blocked_results = Vec::new();
    let blocked_by_errors = state.consecutive_error_turns >= limits.max_consecutive_errors;
    let mut kept = Vec::with_capacity(tool_calls.len());

    for tc in tool_calls.drain(..) {
        if !is_budgeted_web_tool(&tc.function.name) {
            kept.push(tc);
            continue;
        }

        let pool_block = match tc.function.name.as_str() {
            "browser_navigate" => state.browser_used >= limits.browser_max,
            "web_extract" => state.extract_used >= limits.extract_max,
            "web_search" => state.search_used >= limits.search_max,
            _ => false,
        };
        let aggregate_block = limits
            .aggregate_max
            .is_some_and(|max| state.billable_total >= max);

        let block = blocked_by_errors || pool_block || aggregate_block;
        if block {
            let (used, limit, pool) = match tc.function.name.as_str() {
                "browser_navigate" => (state.browser_used, limits.browser_max, "browser"),
                "web_extract" => (state.extract_used, limits.extract_max, "extract"),
                "web_search" => (state.search_used, limits.search_max, "search"),
                _ => (state.billable_total, limits.aggregate_max.unwrap_or(0), "aggregate"),
            };
            let reason = if blocked_by_errors {
                format!(
                    "Web tool budget guard on turn {turn}: blocked '{}' after {} consecutive web-tool error turn(s).",
                    tc.function.name, state.consecutive_error_turns
                )
            } else if aggregate_block && !pool_block {
                format!(
                    "Web tool aggregate budget exceeded on turn {turn}: blocked '{}' (billable_total={}).",
                    tc.function.name, state.billable_total
                )
            } else {
                format!(
                    "Web tool budget exceeded on turn {turn}: blocked '{}' pool={pool} used={used} limit={limit}.",
                    tc.function.name
                )
            };
            tracing::info!(
                turn = turn,
                tool = %tc.function.name,
                pool = pool,
                used = used,
                limit = limit,
                scope = "run",
                blocked_by_errors = blocked_by_errors,
                "web_tool_budget block"
            );
            blocked_results.push((tc.function.name.clone(), ToolResult::err(tc.id, reason)));
            continue;
        }
        kept.push(tc);
    }
    *tool_calls = kept;
    blocked_results
}

// ---------------------------------------------------------------------------
// Post-execute billing
// ---------------------------------------------------------------------------

pub fn is_billable_web_tool_result(name: &str, output: &str) -> bool {
    if !is_budgeted_web_tool(name) {
        return false;
    }
    if looks_like_tool_error_output(output) {
        return false;
    }
    if name == "browser_navigate" && is_browser_non_billable_failure(output) {
        return false;
    }
    true
}

fn is_browser_non_billable_failure(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("did not become ready")
        || lower.contains("cdp not reachable")
        || lower.contains("chrome executable not found")
        || lower.contains("failed to open")
        || lower.contains("navigation failed")
}

pub fn looks_like_tool_error_output(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = value.as_object() {
            if let Some(err) = obj.get("error") {
                if !err.is_null() {
                    return true;
                }
            }
            if let Some(success) = obj.get("success").and_then(|v| v.as_bool()) {
                if !success {
                    return true;
                }
            }
            if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                if status.eq_ignore_ascii_case("error") || status.eq_ignore_ascii_case("failed") {
                    return true;
                }
            }
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.contains("invalid tool parameters")
        || lower.contains("missing '")
}

pub fn record_web_tool_results(
    state: &mut WebToolBudgetState,
    _limits: &WebToolBudgetLimits,
    tool_calls: &[ToolCall],
    results: &[ToolResult],
) {
    let mut web_turn_calls: u32 = 0;
    let mut web_turn_billable: u32 = 0;

    for tc in tool_calls {
        if !is_budgeted_web_tool(&tc.function.name) {
            continue;
        }
        web_turn_calls = web_turn_calls.saturating_add(1);
        let result = results.iter().find(|r| r.tool_call_id == tc.id);
        let billable = result.is_some_and(|r| {
            !r.is_error && is_billable_web_tool_result(&tc.function.name, &r.content)
        });
        if billable {
            web_turn_billable = web_turn_billable.saturating_add(1);
            match tc.function.name.as_str() {
                "browser_navigate" => state.browser_used = state.browser_used.saturating_add(1),
                "web_extract" => state.extract_used = state.extract_used.saturating_add(1),
                "web_search" => state.search_used = state.search_used.saturating_add(1),
                _ => {}
            }
            state.billable_total = state.billable_total.saturating_add(1);
            tracing::debug!(
                tool = %tc.function.name,
                browser_used = state.browser_used,
                extract_used = state.extract_used,
                search_used = state.search_used,
                billable_total = state.billable_total,
                scope = "run",
                "web_tool_budget billable"
            );
        }
    }

    if web_turn_calls > 0 {
        if web_turn_billable == 0 {
            state.consecutive_error_turns = state.consecutive_error_turns.saturating_add(1);
        } else {
            state.consecutive_error_turns = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, Message};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_pools_independent_browser_exhausted_extract_allowed() {
        let limits = WebToolBudgetLimits {
            browser_max: 1,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_consecutive_errors: 2,
        };
        let mut state = WebToolBudgetState {
            browser_used: 1,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "e1".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://example.com/page"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1);
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);

        calls = vec![ToolCall {
            id: "b2".into(),
            function: FunctionCall {
                name: "browser_navigate".into(),
                arguments: r#"{"url":"https://example.com"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        let _ = &mut state;
    }

    #[test]
    fn test_apply_web_tool_budget_caps_web_search_calls() {
        let _guard = env_lock();
        hermes_core::test_env::set_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", "2");
        let limits = WebToolBudgetLimits::from_env();
        let state = WebToolBudgetState {
            search_used: 2,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"test"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        hermes_core::test_env::remove_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS");
    }

    #[test]
    fn test_browser_failure_not_billed() {
        let mut state = WebToolBudgetState::default();
        let limits = WebToolBudgetLimits {
            browser_max: 2,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_consecutive_errors: 2,
        };
        let calls = vec![ToolCall {
            id: "b1".into(),
            function: FunctionCall {
                name: "browser_navigate".into(),
                arguments: r#"{"url":"https://example.com"}"#.into(),
            },
            extra_content: None,
        }];
        let results = vec![ToolResult::err(
            "b1",
            "browser_navigate timed out after 60s",
        )];
        record_web_tool_results(&mut state, &limits, &calls, &results);
        assert_eq!(state.browser_used, 0);
    }

    #[test]
    fn test_aggregate_backstop_blocks_when_enabled() {
        let limits = WebToolBudgetLimits {
            browser_max: 10,
            extract_max: 10,
            search_max: 10,
            aggregate_max: Some(2),
            max_consecutive_errors: 2,
        };
        let state = WebToolBudgetState {
            billable_total: 2,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "e1".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://example.com/new"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_dedup_blocks_repeat_extract_url() {
        let url = "https://Example.COM/page/";
        let norm = normalize_url(url).unwrap();
        let messages = vec![
            Message::assistant_with_tool_calls(
                None,
                vec![ToolCall {
                    id: "tc1".into(),
                    function: FunctionCall {
                        name: "web_extract".into(),
                        arguments: format!(r#"{{"url":"{url}"}}"#),
                    },
                    extra_content: None,
                }],
            ),
            Message::tool_result(
                "tc1",
                format!(
                    r#"{{"url":"{url}","content":"{}"}}"#,
                    "x".repeat(120)
                ),
            ),
        ];
        let cached = cached_extract_urls_from_messages(&messages);
        assert!(cached.contains(&norm));

        let mut calls = vec![ToolCall {
            id: "tc2".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://example.com/page"}"#.to_string(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_url_dedup(&messages, &mut calls, 2);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_user_notice_includes_per_message_scope() {
        let msg = web_tool_budget_user_notice("web_extract", false);
        assert!(msg.contains("本则用户消息"));
    }

    #[test]
    fn test_normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://Example.COM/foo/"),
            Some("https://example.com/foo".to_string())
        );
    }
}
