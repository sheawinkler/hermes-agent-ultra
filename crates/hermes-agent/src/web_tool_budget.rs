//! Per-run web tool budgets: separate pools per tool, attempted vs billable accounting, URL/query dedup.
//!
//! Scope is **one user message** (one `AgentLoop::run` / `run_stream` invocation), not the
//! whole session thread.

use std::collections::{HashMap, HashSet};

use hermes_config::WebResearchConfig;
use hermes_core::{Message, MessageRole, ToolCall, ToolResult};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Limits & state
// ---------------------------------------------------------------------------

/// How per-pool caps interact with [`apply_web_tool_budget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetMode {
    /// Per-tool pools (`search_max` / `extract_max` / `browser_max`) are enforced.
    #[default]
    Global,
    /// Task policy owns search/extract pools; global layer only fuses (attempt / aggregate / errors).
    TaskPrimary,
}

/// Per-run limits loaded from environment (see `docs/sop/web_tool_budget.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebToolBudgetLimits {
    pub browser_max: u32,
    pub extract_max: u32,
    pub search_max: u32,
    /// Optional aggregate backstop (`HERMES_WEB_TOOL_BUDGET_MAX_CALLS`). When `None`, only per-pool caps apply.
    pub aggregate_max: Option<u32>,
    /// Hard safety cap on all attempted web calls, including failures.
    pub max_attempt_total: u32,
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
            max_attempt_total: env_u32("HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS", 12),
            max_consecutive_errors: env_u32("HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS", 2),
        }
    }

    pub fn from_web_research_config(config: &WebResearchConfig) -> Self {
        let search_max = config.message_caps.max_total_search.max(config.max_search);
        let extract_max = config
            .message_caps
            .max_total_extract
            .max(config.max_extract);
        let default_total = search_max
            .saturating_add(extract_max)
            .saturating_add(config.max_browser)
            .max(config.max_total);
        let aggregate_max = env_u32_opt("HERMES_WEB_TOOL_BUDGET_MAX_CALLS").or(Some(default_total));
        Self {
            browser_max: env_u32_or("HERMES_BROWSER_BUDGET_MAX_CALLS", config.max_browser),
            extract_max: env_u32_or("HERMES_WEB_EXTRACT_BUDGET_MAX_CALLS", extract_max),
            search_max: env_u32_or("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", search_max),
            aggregate_max,
            max_attempt_total: env_u32_or(
                "HERMES_WEB_TOOL_BUDGET_MAX_ATTEMPTS",
                config.message_caps.max_attempt_total.max(
                    aggregate_max
                        .unwrap_or(config.max_total)
                        .saturating_add(4)
                        .max(12),
                ),
            ),
            max_consecutive_errors: env_u32_or(
                "HERMES_WEB_TOOL_BUDGET_MAX_CONSECUTIVE_ERRORS",
                config.max_consecutive_errors,
            ),
        }
    }

    pub fn from_dynamic_pools(
        search_max: u32,
        extract_max: u32,
        browser_max: u32,
        aggregate_max: Option<u32>,
        max_consecutive_errors: u32,
    ) -> Self {
        Self {
            search_max,
            extract_max,
            browser_max,
            aggregate_max: aggregate_max.filter(|v| *v > 0),
            max_attempt_total: aggregate_max.unwrap_or(8).saturating_add(4).max(12),
            max_consecutive_errors,
        }
    }
}

/// Mutable counters for the current run.
#[derive(Debug, Clone, Default)]
pub struct WebToolBudgetState {
    /// Attempted executions this user message (pre-check / batch simulation).
    pub attempted_browser: u32,
    pub attempted_extract: u32,
    pub attempted_search: u32,
    /// Billable successes per pool (cost / success accounting).
    pub browser_used: u32,
    pub extract_used: u32,
    pub search_used: u32,
    /// Billable successes across all web tools (for optional aggregate backstop only).
    pub billable_total: u32,
    /// Non-billable failures per pool, tracked for diagnostics and retry policy.
    pub browser_failed: u32,
    pub extract_failed: u32,
    pub search_failed: u32,
    pub consecutive_error_turns: u32,
    pub successful_search_queries: HashSet<String>,
    pub failed_search_queries: HashMap<String, u32>,
}

impl WebToolBudgetState {
    pub fn new() -> Self {
        Self::default()
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    env_u32_opt(key).unwrap_or(default)
}

fn env_u32_or(key: &str, default: u32) -> u32 {
    env_u32_opt(key).unwrap_or(default)
}

fn env_u32_opt(key: &str) -> Option<u32> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|v| *v > 0)
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
                    if let Some(n) =
                        url_from_tool_arguments(&tc.function.name, &tc.function.arguments)
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
fn normalize_search_query(query: &str) -> String {
    query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn query_from_tool_arguments(name: &str, arguments: &str) -> Option<String> {
    if name != "web_search" {
        return None;
    }
    let args: Value = serde_json::from_str(arguments).ok()?;
    args.get("query")
        .and_then(|v| v.as_str())
        .map(normalize_search_query)
        .filter(|q| !q.is_empty())
}

/// Block same-batch duplicate searches and queries already satisfied this user message.
pub fn apply_web_query_dedup(
    _messages: &[Message],
    state: &mut WebToolBudgetState,
    tool_calls: &mut Vec<ToolCall>,
    turn: u32,
) -> Vec<(String, ToolResult)> {
    let mut blocked = Vec::new();
    let mut kept = Vec::with_capacity(tool_calls.len());
    let mut batch_queries = HashSet::new();
    for tc in tool_calls.drain(..) {
        if tc.function.name != "web_search" {
            kept.push(tc);
            continue;
        }
        let Some(query) = query_from_tool_arguments(&tc.function.name, &tc.function.arguments)
        else {
            kept.push(tc);
            continue;
        };
        let already_satisfied = state.successful_search_queries.contains(&query);
        let duplicate_in_batch = !batch_queries.insert(query.clone());
        let failed_too_often = state
            .failed_search_queries
            .get(&query)
            .copied()
            .unwrap_or(0)
            > 1;
        if already_satisfied || duplicate_in_batch || failed_too_often {
            let reason = format!(
                "Web query dedup on turn {turn}: blocked repeat web_search for query ({query})."
            );
            tracing::info!(
                turn = turn,
                query = %query,
                already_satisfied = already_satisfied,
                duplicate_in_batch = duplicate_in_batch,
                failed_too_often = failed_too_often,
                scope = "run",
                "web_tool_budget query dedup block"
            );
            blocked.push((tc.function.name.clone(), ToolResult::err(tc.id, reason)));
            continue;
        }
        kept.push(tc);
    }
    *tool_calls = kept;
    blocked
}

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
            blocked.push((tc.function.name.clone(), ToolResult::err(tc.id, reason)));
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

/// Whether a blocked web tool should surface a user-visible quota notice.
pub fn budget_block_should_notify_user(tool_name: &str, limits: &WebToolBudgetLimits) -> bool {
    match tool_name {
        // Planner intentionally set browser budget to 0; blocking is expected, not exhaustion.
        "browser_navigate" if limits.browser_max == 0 => false,
        _ => true,
    }
}

pub fn web_tool_budget_user_notice(tool_name: &str, blocked_by_errors: bool) -> String {
    let scope = "（本则用户消息配额）";
    match tool_name {
        "web_search" => format!("网络检索次数已达上限{scope}，将基于已有信息直接回复。"),
        "web_extract" | "browser_navigate" if blocked_by_errors => {
            format!("网页读取多次失败{scope}，将基于已有信息直接回复。")
        }
        "web_extract" => format!(
            "网页抓取次数已达上限{scope}，将基于已有信息直接回复。可换 URL 或基于上文 tool 结果回答。"
        ),
        "browser_navigate" => format!("浏览器打开次数已达上限{scope}，将基于已有信息直接回复。"),
        _ => format!("工具 {tool_name} 调用受限{scope}，将基于已有信息直接回复。"),
    }
}

fn web_budget_stop_instruction(tool_name: &str, stop_all_web: bool) -> String {
    if stop_all_web {
        return " Do not retry web_search/web_extract/browser_navigate for this user message; answer now from existing information and mark anything unverified.".to_string();
    }
    format!(
        " Do not retry {tool_name} for this user message; use other available web tools if needed, or answer from existing information and mark anything unverified."
    )
}

pub fn is_attempt_safety_block(output: &str) -> bool {
    output.contains("web_attempt_safety_exhausted")
}

pub fn web_attempt_safety_user_notice() -> String {
    "网络检索多次失败，未获得可验证结果；将标注未完成联网验证。".to_string()
}

fn increment_attempted(state: &mut WebToolBudgetState, tool_name: &str) {
    match tool_name {
        "browser_navigate" => state.attempted_browser = state.attempted_browser.saturating_add(1),
        "web_extract" => state.attempted_extract = state.attempted_extract.saturating_add(1),
        "web_search" => state.attempted_search = state.attempted_search.saturating_add(1),
        _ => {}
    }
}

fn reserve_success_slot(state: &mut WebToolBudgetState, tool_name: &str) {
    match tool_name {
        "browser_navigate" => state.browser_used = state.browser_used.saturating_add(1),
        "web_extract" => state.extract_used = state.extract_used.saturating_add(1),
        "web_search" => state.search_used = state.search_used.saturating_add(1),
        _ => {}
    }
    if is_budgeted_web_tool(tool_name) {
        state.billable_total = state.billable_total.saturating_add(1);
    }
}

impl WebToolBudgetState {
    pub fn attempted_total(&self) -> u32 {
        self.attempted_browser
            .saturating_add(self.attempted_extract)
            .saturating_add(self.attempted_search)
    }

    pub fn successful_total(&self) -> u32 {
        self.browser_used
            .saturating_add(self.extract_used)
            .saturating_add(self.search_used)
    }

    pub fn has_successful_evidence(&self) -> bool {
        self.successful_total() > 0 || self.billable_total > 0
    }
}

pub fn apply_web_tool_budget(
    state: &WebToolBudgetState,
    limits: &WebToolBudgetLimits,
    tool_calls: &mut Vec<ToolCall>,
    turn: u32,
    mode: BudgetMode,
) -> Vec<(String, ToolResult)> {
    let mut blocked_results = Vec::new();
    let blocked_by_errors = state.consecutive_error_turns >= limits.max_consecutive_errors;
    let mut kept = Vec::with_capacity(tool_calls.len());
    let mut simulated = state.clone();

    for tc in tool_calls.drain(..) {
        if !is_budgeted_web_tool(&tc.function.name) {
            kept.push(tc);
            continue;
        }

        let pool_block = match mode {
            BudgetMode::TaskPrimary => false,
            BudgetMode::Global => match tc.function.name.as_str() {
                "browser_navigate" => simulated.browser_used >= limits.browser_max,
                "web_extract" => simulated.extract_used >= limits.extract_max,
                "web_search" => simulated.search_used >= limits.search_max,
                _ => false,
            },
        };
        let aggregate_block = limits
            .aggregate_max
            .is_some_and(|max| simulated.billable_total >= max);
        let attempt_safety_block = simulated.attempted_total() >= limits.max_attempt_total;
        let block = blocked_by_errors || pool_block || aggregate_block || attempt_safety_block;
        if block {
            let (used, limit, pool) = match tc.function.name.as_str() {
                "browser_navigate" => (simulated.browser_used, limits.browser_max, "browser"),
                "web_extract" => (simulated.extract_used, limits.extract_max, "extract"),
                "web_search" => (simulated.search_used, limits.search_max, "search"),
                _ => (
                    simulated.billable_total,
                    limits.aggregate_max.unwrap_or(0),
                    "aggregate",
                ),
            };
            let reason = if blocked_by_errors {
                format!(
                    "Web tool budget guard on turn {turn}: blocked '{}' after {} consecutive web-tool error turn(s).",
                    tc.function.name, state.consecutive_error_turns
                )
            } else if attempt_safety_block && !simulated.has_successful_evidence() {
                format!(
                    "web_attempt_safety_exhausted_without_evidence on turn {turn}: blocked '{}' after {} attempted web call(s).",
                    tc.function.name,
                    simulated.attempted_total()
                )
            } else if attempt_safety_block {
                format!(
                    "web_attempt_safety_exhausted on turn {turn}: blocked '{}' after {} attempted web call(s).",
                    tc.function.name,
                    simulated.attempted_total()
                )
            } else if aggregate_block && !pool_block {
                format!(
                    "Web tool aggregate budget exceeded on turn {turn}: blocked '{}' (billable_total={}).",
                    tc.function.name, simulated.billable_total
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
                attempted_total = simulated.attempted_total(),
                scope = "run",
                mode = ?mode,
                blocked_by_errors = blocked_by_errors,
                "web_tool_budget block"
            );
            let stop_all_web = blocked_by_errors || attempt_safety_block || aggregate_block;
            blocked_results.push((
                tc.function.name.clone(),
                ToolResult::err(
                    tc.id,
                    format!(
                        "{reason}{}",
                        web_budget_stop_instruction(&tc.function.name, stop_all_web)
                    ),
                ),
            ));
            continue;
        }
        increment_attempted(&mut simulated, &tc.function.name);
        reserve_success_slot(&mut simulated, &tc.function.name);
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
    if name == "web_search" {
        return has_useful_web_search_results(output);
    }
    if name == "web_extract" && is_extract_non_billable_failure(output) {
        return false;
    }
    if name == "browser_navigate" && is_browser_non_billable_failure(output) {
        return false;
    }
    true
}

fn has_useful_web_search_results(output: &str) -> bool {
    let trimmed = output.trim();
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return trimmed.len() >= 40;
    };
    let results = value
        .get("results")
        .or_else(|| value.get("web"))
        .or_else(|| value.get("data").and_then(|v| v.get("results")))
        .or_else(|| value.get("data").and_then(|v| v.get("web")))
        .and_then(|v| v.as_array());
    let Some(results) = results else {
        return false;
    };
    results.iter().any(|item| {
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let text = item
            .get("text")
            .or_else(|| item.get("snippet"))
            .or_else(|| item.get("content"))
            .or_else(|| item.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        !url.is_empty() && (!title.is_empty() || !text.is_empty())
    })
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

fn is_extract_non_billable_failure(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.chars().count() < 40 {
        return true;
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.contains("404")
        || lower.contains("not found")
        || lower.contains("page not found")
        || trimmed.contains("页面不存在")
        || trimmed.contains("网页不存在")
        || trimmed.contains("空正文")
}

pub fn looks_like_tool_error_output(output: &str) -> bool {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(obj) = value.as_object() {
            if let Some(exit_code) = obj.get("exit_code").and_then(|v| v.as_i64()) {
                if exit_code != 0 {
                    return true;
                }
            }
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
    if let Some(code) = parse_terminal_exit_code_suffix(trimmed) {
        if code != 0 {
            return true;
        }
    }
    let lower = trimmed.to_ascii_lowercase();
    lower.starts_with("error:")
        || lower.contains("invalid tool parameters")
        || lower.contains("missing '")
}

fn parse_terminal_exit_code_suffix(output: &str) -> Option<i32> {
    const PREFIX: &str = "[exit code: ";
    let start = output.rfind(PREFIX)? + PREFIX.len();
    let rest = &output[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

fn record_web_tool_failure(state: &mut WebToolBudgetState, tool_name: &str, query: Option<String>) {
    match tool_name {
        "browser_navigate" => state.browser_failed = state.browser_failed.saturating_add(1),
        "web_extract" => state.extract_failed = state.extract_failed.saturating_add(1),
        "web_search" => {
            state.search_failed = state.search_failed.saturating_add(1);
            if let Some(query) = query {
                let count = state.failed_search_queries.entry(query).or_insert(0);
                *count = count.saturating_add(1);
            }
        }
        _ => {}
    }
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
        increment_attempted(state, &tc.function.name);
        let query = query_from_tool_arguments(&tc.function.name, &tc.function.arguments);
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
            if let Some(query) = query {
                state.successful_search_queries.insert(query.clone());
                state.failed_search_queries.remove(&query);
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
        } else {
            record_web_tool_failure(state, &tc.function.name, query);
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
            max_attempt_total: 12,
            max_consecutive_errors: 2,
        };
        let mut state = WebToolBudgetState {
            attempted_browser: 1,
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
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
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
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        let _ = &mut state;
    }

    #[test]
    fn test_batch_three_searches_budget_two_keeps_two() {
        let limits = WebToolBudgetLimits {
            browser_max: 2,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_attempt_total: 12,
            max_consecutive_errors: 2,
        };
        let state = WebToolBudgetState::default();
        let mut calls: Vec<ToolCall> = (0..3)
            .map(|i| ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"q{i}"}}"#),
                },
                extra_content: None,
            })
            .collect();
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert_eq!(calls.len(), 2);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn task_primary_skips_search_pool_but_attempt_fuse_still_blocks() {
        let limits = WebToolBudgetLimits {
            browser_max: 2,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_attempt_total: 12,
            max_consecutive_errors: 2,
        };
        let state = WebToolBudgetState {
            attempted_search: 2,
            search_used: 2,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"more"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked =
            apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::TaskPrimary);
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);

        let state_exhausted = WebToolBudgetState {
            attempted_search: 12,
            ..Default::default()
        };
        let mut calls2 = vec![ToolCall {
            id: "s2".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"fuse"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked2 = apply_web_tool_budget(
            &state_exhausted,
            &limits,
            &mut calls2,
            2,
            BudgetMode::TaskPrimary,
        );
        assert_eq!(blocked2.len(), 1);
        assert!(calls2.is_empty());
    }

    #[test]
    fn test_apply_web_tool_budget_caps_web_search_calls() {
        let _guard = env_lock();
        hermes_core::test_env::set_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS", "2");
        let limits = WebToolBudgetLimits::from_env();
        let state = WebToolBudgetState {
            attempted_search: 2,
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
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        hermes_core::test_env::remove_var("HERMES_WEB_SEARCH_BUDGET_MAX_CALLS");
    }

    #[test]
    fn test_budget_block_tells_model_not_to_retry_web_tools() {
        let limits = WebToolBudgetLimits {
            browser_max: 2,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_attempt_total: 12,
            max_consecutive_errors: 2,
        };
        let state = WebToolBudgetState {
            attempted_search: 2,
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
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert!(blocked[0].1.content.contains("Do not retry"));
    }

    #[test]
    fn test_browser_failure_not_billed() {
        let mut state = WebToolBudgetState::default();
        let limits = WebToolBudgetLimits {
            browser_max: 2,
            extract_max: 5,
            search_max: 2,
            aggregate_max: None,
            max_attempt_total: 12,
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
    fn failed_web_search_does_not_consume_success_budget() {
        let mut state = WebToolBudgetState::default();
        let limits = WebToolBudgetLimits::from_dynamic_pools(1, 1, 1, Some(1), 2);
        let calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"rust"}"#.into(),
            },
            extra_content: None,
        }];
        let results = vec![ToolResult::err("s1", "timed out")];
        record_web_tool_results(&mut state, &limits, &calls, &results);
        assert_eq!(state.search_used, 0);
        assert_eq!(state.billable_total, 0);
        assert_eq!(state.search_failed, 1);
    }

    #[test]
    fn empty_web_search_payload_is_not_billable() {
        assert!(!is_billable_web_tool_result(
            "web_search",
            r#"{"data":{"web":[]}}"#
        ));
        assert!(!is_billable_web_tool_result(
            "web_search",
            r#"{"results":[]}"#
        ));
    }

    #[test]
    fn gateway_web_search_payload_is_billable_when_result_has_source() {
        assert!(is_billable_web_tool_result(
            "web_search",
            r#"{"data":{"web":[{"title":"深圳天气","url":"https://weather.example/shenzhen","description":"今日天气"}]}}"#
        ));
    }

    #[test]
    fn web_extract_404_is_not_billable() {
        assert!(!is_billable_web_tool_result(
            "web_extract",
            "HTTP 404 page not found 页面不存在"
        ));
    }

    #[test]
    fn test_aggregate_backstop_blocks_when_enabled() {
        let limits = WebToolBudgetLimits {
            browser_max: 10,
            extract_max: 10,
            search_max: 10,
            aggregate_max: Some(2),
            max_attempt_total: 12,
            max_consecutive_errors: 2,
        };
        let state = WebToolBudgetState {
            attempted_search: 1,
            attempted_extract: 1,
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
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
    }

    #[test]
    fn aggregate_backstop_uses_billable_not_attempted() {
        let limits = WebToolBudgetLimits::from_dynamic_pools(10, 10, 10, Some(2), 2);
        let state = WebToolBudgetState {
            attempted_search: 2,
            billable_total: 0,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "s3".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"fresh"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 1, BudgetMode::Global);
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn failed_query_gets_one_retry_success_query_dedups() {
        let mut state = WebToolBudgetState::default();
        state.failed_search_queries.insert("rust".into(), 1);
        let mut retry = vec![ToolCall {
            id: "s2".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"rust"}"#.into(),
            },
            extra_content: None,
        }];
        assert!(apply_web_query_dedup(&[], &mut state, &mut retry, 2).is_empty());
        state.successful_search_queries.insert("rust".into());
        let blocked = apply_web_query_dedup(&[], &mut state, &mut retry, 3);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn attempt_safety_has_distinct_stop_reason_without_evidence() {
        let limits = WebToolBudgetLimits::from_dynamic_pools(10, 10, 10, Some(8), 2);
        let state = WebToolBudgetState {
            attempted_search: 12,
            ..Default::default()
        };
        let mut calls = vec![ToolCall {
            id: "s13".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"still failing"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = apply_web_tool_budget(&state, &limits, &mut calls, 13, BudgetMode::Global);
        assert!(is_attempt_safety_block(&blocked[0].1.content));
        assert!(blocked[0].1.content.contains("without_evidence"));
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
                format!(r#"{{"url":"{url}","content":"{}"}}"#, "x".repeat(120)),
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
    fn test_budget_block_should_notify_user_browser_zero() {
        let limits = WebToolBudgetLimits::from_dynamic_pools(2, 1, 0, Some(3), 3);
        assert!(!budget_block_should_notify_user(
            "browser_navigate",
            &limits
        ));
        assert!(budget_block_should_notify_user("web_extract", &limits));
    }

    #[test]
    fn test_normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://Example.COM/foo/"),
            Some("https://example.com/foo".to_string())
        );
    }
}
