//! Adaptive web research: dynamic per-message budgets (planner) and stop decisions (evaluator).

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hermes_config::WebResearchConfig;
use hermes_core::{Message, ToolCall, ToolResult, ToolSchema};
use hermes_intelligence::auxiliary::{AuxiliaryClient, AuxiliaryRequest, AuxiliaryTask};
use serde::Deserialize;
use serde_json::Value;

use crate::web_tool_budget::{
    WebToolBudgetLimits, WebToolBudgetState, apply_web_query_dedup, apply_web_tool_budget,
    apply_web_url_dedup, budget_block_should_notify_user, is_billable_web_tool_result,
    is_budgeted_web_tool, record_web_tool_results, web_tool_budget_user_notice,
    web_url_dedup_user_notice,
};

const PLANNER_TASK: &str = "web_research_planner";
const EVALUATOR_TASK: &str = "web_research_evaluator";

/// Planner output — always clamped to [`WebResearchConfig`] ceilings before use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebResearchPlan {
    pub need_web: bool,
    pub search_budget: u32,
    pub extract_budget: u32,
    pub browser_budget: u32,
    pub total_budget: u32,
    pub stop_conditions: String,
}

/// Evaluator output — structural rules still apply when this is missing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebResearchDecision {
    pub continue_web: bool,
    pub sufficient_answer: bool,
    pub final_answer_instruction: String,
}

#[derive(Debug, Clone, Default)]
pub struct WebResearchEvidence {
    pub successful_searches: u32,
    pub successful_extracts: u32,
    pub successful_browser: u32,
    pub last_assistant_draft: Option<String>,
}

/// Per-user-message controller: planner budgets, evaluator gating, structural dedup/errors.
#[derive(Debug)]
pub struct WebResearchController {
    config: WebResearchConfig,
    limits: WebToolBudgetLimits,
    budget_state: WebToolBudgetState,
    plan: Option<WebResearchPlan>,
    evidence: WebResearchEvidence,
    web_stopped: bool,
    force_finalize: bool,
    notice_seen: HashSet<String>,
    planner_invoked: bool,
    /// Planner set `browser_budget` to 0 but light web tools failed — grant fallback browser pool.
    browser_budget_escalated: bool,
    #[cfg(test)]
    test_llm: Option<WebResearchTestLlm>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct WebResearchTestLlm {
    pub planner_json: Option<String>,
    pub evaluator_json: Option<String>,
}

impl WebResearchController {
    pub fn new(config: WebResearchConfig) -> Self {
        let limits = limits_for_config(&config, None);
        Self {
            config,
            limits,
            budget_state: WebToolBudgetState::new(),
            plan: None,
            evidence: WebResearchEvidence::default(),
            web_stopped: false,
            force_finalize: false,
            notice_seen: HashSet::new(),
            planner_invoked: false,
            browser_budget_escalated: false,
            #[cfg(test)]
            test_llm: None,
        }
    }

    #[cfg(test)]
    pub fn with_test_llm(config: WebResearchConfig, test_llm: WebResearchTestLlm) -> Self {
        let mut c = Self::new(config);
        c.test_llm = Some(test_llm);
        c
    }

    pub fn force_finalize(&self) -> bool {
        self.force_finalize
    }

    pub fn planner_invoked(&self) -> bool {
        self.planner_invoked
    }

    pub fn filter_tool_schemas<'a>(&self, schemas: &'a [ToolSchema]) -> Vec<ToolSchema> {
        if self.should_strip_web_tools() {
            return schemas
                .iter()
                .filter(|s| !is_budgeted_web_tool(&s.name))
                .cloned()
                .collect();
        }
        let mut out: Vec<ToolSchema> = schemas.to_vec();
        if self.config.enabled && self.planner_invoked {
            out.retain(|s| match s.name.as_str() {
                "browser_navigate" => self.limits.browser_max > 0,
                "web_search" => self.limits.search_max > 0,
                "web_extract" => self.limits.extract_max > 0,
                _ => true,
            });
        }
        out
    }

    fn should_strip_web_tools(&self) -> bool {
        self.config.enabled && (self.web_stopped || self.force_finalize)
    }

    pub fn finalization_system_hint(&self) -> Option<String> {
        if !self.force_finalize {
            return None;
        }
        Some(
            "Web research is complete for this user message. Do not call web_search, web_extract, \
             or browser_navigate. Answer now from existing tool results and conversation context. \
             Clearly mark any information that could not be verified from retrieved sources."
                .to_string(),
        )
    }

    pub fn tool_calls_include_web(tool_calls: &[ToolCall]) -> bool {
        tool_calls.iter().any(|tc| is_budgeted_web_tool(&tc.function.name))
    }

    /// Lazy planner on first web tool batch; no-op when disabled or planner off.
    pub async fn ensure_plan_on_first_web(
        &mut self,
        auxiliary: Option<&Arc<AuxiliaryClient>>,
        user_message: &str,
        tool_calls: &[ToolCall],
    ) {
        if !self.config.enabled || !Self::tool_calls_include_web(tool_calls) {
            return;
        }
        if self.planner_invoked {
            return;
        }
        self.planner_invoked = true;
        if !self.config.planner_enabled {
            self.plan = Some(fallback_plan(&self.config));
            self.limits = limits_for_config(&self.config, self.plan.as_ref());
            return;
        }
        let plan = match self.fetch_plan(auxiliary, user_message).await {
            Some(p) => p,
            None => {
                tracing::warn!("web_research planner failed; using fallback budgets");
                fallback_plan(&self.config)
            }
        };
        self.limits = limits_for_config(&self.config, Some(&plan));
        if !plan.need_web {
            self.web_stopped = true;
            self.force_finalize = true;
        }
        self.plan = Some(plan);
    }

    /// Evaluator + structural stop before executing another web batch.
    pub async fn gate_web_batch(
        &mut self,
        auxiliary: Option<&Arc<AuxiliaryClient>>,
        messages: &[Message],
        tool_calls: &mut Vec<ToolCall>,
        turn: u32,
    ) -> (Vec<(String, ToolResult)>, Vec<String>) {
        let mut user_notices = Vec::new();
        if !self.config.enabled {
            let blocked = apply_web_tool_budget(
                &self.budget_state,
                &WebToolBudgetLimits::from_env(),
                tool_calls,
                turn,
            );
            return (blocked, user_notices);
        }

        if self.web_stopped || self.force_finalize {
            let blocked = self.block_all_web_calls(tool_calls, turn, "web research stopped");
            return (blocked, user_notices);
        }

        if self.config.evaluator_enabled
            && self.planner_invoked
            && self.evidence_has_activity()
            && Self::tool_calls_include_web(tool_calls)
        {
            if let Some(decision) = self.fetch_decision(auxiliary, messages).await {
                if decision.sufficient_answer || !decision.continue_web {
                    self.web_stopped = true;
                    self.force_finalize = true;
                    let blocked =
                        self.block_all_web_calls(tool_calls, turn, "evaluator stop");
                    if !decision.final_answer_instruction.is_empty() {
                        tracing::debug!(
                            instruction_len = decision.final_answer_instruction.len(),
                            "web_research evaluator finalization instruction"
                        );
                    }
                    return (blocked, user_notices);
                }
            }
        }

        let mut deferred = apply_web_query_dedup(messages, &mut self.budget_state, tool_calls, turn);
        deferred.extend(apply_web_url_dedup(messages, tool_calls, turn));
        if !deferred.is_empty() {
            let dedup_notice = if deferred.iter().any(|(n, _)| n == "web_search") {
                "重复检索请求已跳过，将基于已有搜索结果回答。（本则用户消息内不再重复检索）".to_string()
            } else {
                web_url_dedup_user_notice()
            };
            if self.emit_notice_once("dedup", dedup_notice.clone()) {
                user_notices.push(dedup_notice);
            }
            if tool_calls.iter().all(|tc| is_budgeted_web_tool(&tc.function.name)) {
                self.web_stopped = true;
                self.force_finalize = true;
            }
        }

        let budget_blocked = apply_web_tool_budget(
            &self.budget_state,
            &self.limits,
            tool_calls,
            turn,
        );
        if !budget_blocked.is_empty() {
            let blocked_by_errors = self.budget_state.consecutive_error_turns
                >= self.limits.max_consecutive_errors;
            let searches_still_scheduled = tool_calls
                .iter()
                .any(|tc| tc.function.name == "web_search");
            let extracts_still_scheduled = tool_calls
                .iter()
                .any(|tc| tc.function.name == "web_extract");
            let browser_still_scheduled = tool_calls
                .iter()
                .any(|tc| tc.function.name == "browser_navigate");
            for (tool_name, _) in &budget_blocked {
                // Same-batch trim: model often issues N parallel web_search while planner
                // budget is smaller. Do not tell the user quota is exhausted when at least
                // one call of that pool still runs this turn.
                if tool_name == "web_search" && searches_still_scheduled {
                    continue;
                }
                if tool_name == "web_extract" && extracts_still_scheduled {
                    continue;
                }
                if tool_name == "browser_navigate" && browser_still_scheduled {
                    continue;
                }
                if !budget_block_should_notify_user(tool_name, &self.limits) {
                    continue;
                }
                let notice = web_tool_budget_user_notice(tool_name, blocked_by_errors);
                if self.emit_notice_once(&format!("budget:{tool_name}"), notice.clone()) {
                    user_notices.push(notice);
                }
            }
            if tool_calls.is_empty() && !self.any_web_pool_has_capacity() {
                self.web_stopped = true;
                self.force_finalize = true;
            }
        }
        deferred.extend(budget_blocked);
        (deferred, user_notices)
    }

    fn evidence_has_activity(&self) -> bool {
        self.evidence.successful_searches > 0
            || self.evidence.successful_extracts > 0
            || self.evidence.successful_browser > 0
    }

    fn any_web_pool_has_capacity(&self) -> bool {
        self.budget_state.attempted_search < self.limits.search_max
            || self.budget_state.attempted_extract < self.limits.extract_max
            || (self.limits.browser_max > 0
                && self.budget_state.attempted_browser < self.limits.browser_max)
    }

    /// Record tool outcomes; returns `true` when browser pool was escalated (refresh tool schemas).
    pub fn record_results(&mut self, tool_calls: &[ToolCall], results: &[ToolResult]) -> bool {
        record_web_tool_results(&mut self.budget_state, &self.limits, tool_calls, results);
        let mut light_failure = false;
        for tc in tool_calls {
            if !is_budgeted_web_tool(&tc.function.name) {
                continue;
            }
            let result = results.iter().find(|r| r.tool_call_id == tc.id);
            if is_failed_light_web_attempt(&tc.function.name, result) {
                light_failure = true;
            }
            let billable = result.is_some_and(|r| {
                !r.is_error && is_billable_web_tool_result(&tc.function.name, &r.content)
            });
            if !billable {
                continue;
            }
            match tc.function.name.as_str() {
                "web_search" => {
                    self.evidence.successful_searches =
                        self.evidence.successful_searches.saturating_add(1)
                }
                "web_extract" => {
                    self.evidence.successful_extracts =
                        self.evidence.successful_extracts.saturating_add(1)
                }
                "browser_navigate" => {
                    self.evidence.successful_browser =
                        self.evidence.successful_browser.saturating_add(1)
                }
                _ => {}
            }
        }
        if light_failure {
            self.maybe_escalate_browser_after_light_failure()
        } else {
            false
        }
    }

    fn maybe_escalate_browser_after_light_failure(&mut self) -> bool {
        if !self.config.enabled
            || !self.planner_invoked
            || self.browser_budget_escalated
            || self.limits.browser_max > 0
        {
            return false;
        }
        let plan_need_web = self.plan.as_ref().is_none_or(|p| p.need_web);
        if !plan_need_web {
            return false;
        }
        let grant = browser_escalation_budget(&self.config);
        self.limits.browser_max = grant;
        if let Some(plan) = self.plan.as_mut() {
            plan.browser_budget = grant;
            let component_total = plan
                .search_budget
                .saturating_add(plan.extract_budget)
                .saturating_add(grant);
            if plan.total_budget < component_total {
                plan.total_budget = component_total.min(self.config.max_total);
            }
        }
        self.browser_budget_escalated = true;
        tracing::info!(
            browser_max = grant,
            fallback_browser = self.config.fallback_browser,
            "web_research browser budget escalated after web_search/web_extract failure"
        );
        // Planner zeroed browser but fetch path still needs Playwright — reopen unless evaluator ended the run.
        if !self.evidence_has_activity() {
            self.web_stopped = false;
            self.force_finalize = false;
        }
        true
    }

    pub fn note_assistant_draft(&mut self, content: Option<&str>) {
        let text = content.map(str::trim).unwrap_or("");
        if text.len() >= 40 {
            self.evidence.last_assistant_draft = Some(text.to_string());
        }
    }

    fn emit_notice_once(&mut self, key: &str, notice: String) -> bool {
        self.notice_seen.insert(format!("{key}:{notice}"))
    }

    fn block_all_web_calls(
        &mut self,
        tool_calls: &mut Vec<ToolCall>,
        turn: u32,
        reason: &str,
    ) -> Vec<(String, ToolResult)> {
        let mut blocked = Vec::new();
        let mut kept = Vec::new();
        for tc in tool_calls.drain(..) {
            if is_budgeted_web_tool(&tc.function.name) {
                tracing::info!(
                    turn = turn,
                    tool = %tc.function.name,
                    reason = reason,
                    scope = "run",
                    "web_research block"
                );
                blocked.push((
                    tc.function.name.clone(),
                    ToolResult::err(
                        tc.id,
                        format!(
                            "Web research stopped on turn {turn}: {reason}. \
                             Do not retry web_search/web_extract/browser_navigate for this user message."
                        ),
                    ),
                ));
            } else {
                kept.push(tc);
            }
        }
        *tool_calls = kept;
        blocked
    }

    async fn fetch_plan(
        &self,
        auxiliary: Option<&Arc<AuxiliaryClient>>,
        user_message: &str,
    ) -> Option<WebResearchPlan> {
        #[cfg(test)]
        if let Some(test) = &self.test_llm {
            if let Some(raw) = &test.planner_json {
                return parse_plan_json(raw, &self.config);
            }
        }
        let aux = auxiliary?;
        let system = planner_system_prompt(&self.config);
        let user = format!(
            "User message:\n{}\n\nOutput JSON only with keys: need_web (bool), search_budget, \
             extract_budget, browser_budget, total_budget (non-negative integers), stop_conditions (string).",
            user_message.trim()
        );
        let request = AuxiliaryRequest::new(
            AuxiliaryTask::Custom(PLANNER_TASK.to_string()),
            vec![Message::system(system), Message::user(user)],
        )
        .with_temperature(0.0)
        .with_max_tokens(400)
        .with_timeout(Duration::from_secs(45));
        let text = aux.call(request).await.ok()?.text()?.to_string();
        parse_plan_json(&text, &self.config)
    }

    async fn fetch_decision(
        &self,
        auxiliary: Option<&Arc<AuxiliaryClient>>,
        messages: &[Message],
    ) -> Option<WebResearchDecision> {
        #[cfg(test)]
        if let Some(test) = &self.test_llm {
            if let Some(raw) = &test.evaluator_json {
                return parse_decision_json(raw);
            }
        }
        let aux = auxiliary?;
        let transcript = summarize_web_evidence(messages, &self.evidence);
        let system = "You decide whether more web tools are worthwhile. Output JSON only with keys: \
                      continue_web (bool), sufficient_answer (bool), final_answer_instruction (string).";
        let user = format!("Evidence summary:\n{transcript}");
        let request = AuxiliaryRequest::new(
            AuxiliaryTask::Custom(EVALUATOR_TASK.to_string()),
            vec![Message::system(system), Message::user(user)],
        )
        .with_temperature(0.0)
        .with_max_tokens(300)
        .with_timeout(Duration::from_secs(45));
        let text = aux.call(request).await.ok()?.text()?.to_string();
        parse_decision_json(&text)
    }
}

/// Browser pool granted when planner set 0 but search/extract failed (uses `fallback_browser`, default 2).
fn browser_escalation_budget(config: &WebResearchConfig) -> u32 {
    config.fallback_browser.max(1).min(config.max_browser)
}

fn is_failed_light_web_attempt(tool_name: &str, result: Option<&ToolResult>) -> bool {
    if !matches!(tool_name, "web_search" | "web_extract") {
        return false;
    }
    let Some(r) = result else {
        return true;
    };
    if r.is_error {
        return true;
    }
    !is_billable_web_tool_result(tool_name, &r.content)
}

fn limits_for_config(config: &WebResearchConfig, plan: Option<&WebResearchPlan>) -> WebToolBudgetLimits {
    if let Some(plan) = plan {
        WebToolBudgetLimits::from_dynamic_pools(
            plan.search_budget.min(config.max_search),
            plan.extract_budget.min(config.max_extract),
            plan.browser_budget.min(config.max_browser),
            Some(plan.total_budget.min(config.max_total)),
            config.max_consecutive_errors,
        )
    } else {
        WebToolBudgetLimits::from_web_research_config(config)
    }
}

fn fallback_plan(config: &WebResearchConfig) -> WebResearchPlan {
    WebResearchPlan {
        need_web: true,
        search_budget: config.fallback_search,
        extract_budget: config.fallback_extract,
        browser_budget: config.fallback_browser,
        total_budget: config
            .fallback_search
            .saturating_add(config.fallback_extract)
            .saturating_add(config.fallback_browser)
            .min(config.max_total),
        stop_conditions: "fallback budgets".to_string(),
    }
}

fn planner_system_prompt(config: &WebResearchConfig) -> String {
    if let Some(path) = config.planner_prompt_path.as_deref() {
        if let Ok(text) = std::fs::read_to_string(Path::new(path)) {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        tracing::warn!(path = path, "web_research planner_prompt_path unreadable; using default");
    }
    "You allocate web tool budgets per user message. Prefer small budgets for simple lookups \
     and larger only when multi-source verification is clearly required. Never exceed configured \
     ceilings in your output — they will be clamped in Rust anyway."
        .to_string()
}

#[derive(Debug, Deserialize)]
struct RawWebResearchPlan {
    #[serde(default)]
    need_web: bool,
    #[serde(default)]
    search_budget: Option<i64>,
    #[serde(default)]
    extract_budget: Option<i64>,
    #[serde(default)]
    browser_budget: Option<i64>,
    #[serde(default)]
    total_budget: Option<i64>,
    #[serde(default)]
    stop_conditions: Option<String>,
}

pub fn parse_plan_json(raw: &str, config: &WebResearchConfig) -> Option<WebResearchPlan> {
    let value = extract_json_value(raw)?;
    let raw_plan: RawWebResearchPlan = serde_json::from_value(value).ok()?;
    let mut search = clamp_budget(raw_plan.search_budget, config.max_search);
    let mut extract = clamp_budget(raw_plan.extract_budget, config.max_extract);
    let browser = clamp_budget(raw_plan.browser_budget, config.max_browser);
    let mut total = clamp_budget(
        raw_plan.total_budget,
        config.max_total,
    );
    if total == 0 {
        total = search.saturating_add(extract).saturating_add(browser);
    }
    if raw_plan.need_web {
        let search_floor = config.fallback_search.max(1);
        if search > 0 && search < search_floor {
            search = search_floor.min(config.max_search);
        }
        let extract_floor = config.fallback_extract.max(1);
        if extract > 0 && extract < extract_floor {
            extract = extract_floor.min(config.max_extract);
        }
        let component_total = search.saturating_add(extract).saturating_add(browser);
        if total < component_total {
            total = component_total.min(config.max_total);
        }
    }
    total = total.min(config.max_total);
    tracing::debug!(
        search = search,
        extract = extract,
        browser = browser,
        total = total,
        need_web = raw_plan.need_web,
        "web_research plan parsed"
    );
    Some(WebResearchPlan {
        need_web: raw_plan.need_web,
        search_budget: search,
        extract_budget: extract,
        browser_budget: browser,
        total_budget: total,
        stop_conditions: raw_plan
            .stop_conditions
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

#[derive(Debug, Deserialize)]
struct RawWebResearchDecision {
    #[serde(default = "default_true")]
    continue_web: bool,
    #[serde(default)]
    sufficient_answer: bool,
    #[serde(default)]
    final_answer_instruction: Option<String>,
}

fn default_true() -> bool {
    true
}

pub fn parse_decision_json(raw: &str) -> Option<WebResearchDecision> {
    let value = extract_json_value(raw)?;
    let raw_decision: RawWebResearchDecision = serde_json::from_value(value).ok()?;
    Some(WebResearchDecision {
        continue_web: raw_decision.continue_web,
        sufficient_answer: raw_decision.sufficient_answer,
        final_answer_instruction: raw_decision
            .final_answer_instruction
            .unwrap_or_default()
            .trim()
            .to_string(),
    })
}

fn clamp_budget(raw: Option<i64>, max: u32) -> u32 {
    let v = raw.unwrap_or(0).max(0) as u32;
    v.min(max)
}

fn extract_json_value(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    serde_json::from_str(trimmed.get(start..=end)?).ok()
}

fn summarize_web_evidence(messages: &[Message], evidence: &WebResearchEvidence) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "successful_searches={} successful_extracts={} successful_browser={}",
        evidence.successful_searches, evidence.successful_extracts, evidence.successful_browser
    ));
    if let Some(draft) = &evidence.last_assistant_draft {
        let snippet: String = draft.chars().take(500).collect();
        lines.push(format!("assistant_draft_snippet={snippet}"));
    }
    let mut recent_queries: VecDeque<String> = VecDeque::new();
    for msg in messages.iter().rev().take(40) {
        if let Some(calls) = msg.tool_calls.as_ref() {
            for tc in calls {
                if tc.function.name == "web_search" {
                    if let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) {
                        if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                            recent_queries.push_front(q.to_string());
                        }
                    }
                }
            }
        }
    }
    if !recent_queries.is_empty() {
        lines.push(format!("recent_queries={recent_queries:?}"));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, JsonSchema};

    fn test_config() -> WebResearchConfig {
        WebResearchConfig {
            max_search: 4,
            max_extract: 5,
            max_browser: 2,
            max_total: 8,
            fallback_search: 2,
            ..Default::default()
        }
    }

    #[test]
    fn parse_plan_clamps_oversized_budgets() {
        let cfg = test_config();
        let raw = r#"{"need_web":true,"search_budget":99,"extract_budget":99,"browser_budget":99,"total_budget":99}"#;
        let plan = parse_plan_json(raw, &cfg).unwrap();
        assert_eq!(plan.search_budget, 4);
        assert_eq!(plan.total_budget, 8);
    }

    #[test]
    fn parse_plan_negative_fields_use_zero() {
        let cfg = test_config();
        let raw = r#"{"need_web":true,"search_budget":-3,"extract_budget":1}"#;
        let plan = parse_plan_json(raw, &cfg).unwrap();
        assert_eq!(plan.search_budget, 0);
    }

    #[test]
    fn parse_plan_need_web_raises_sub_fallback_search() {
        let cfg = test_config();
        let plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":1,"extract_budget":1,"browser_budget":0,"total_budget":2}"#,
            &cfg,
        )
        .unwrap();
        assert_eq!(plan.search_budget, cfg.fallback_search);
    }

    #[tokio::test]
    async fn fake_planner_budget_one_blocks_second_search() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::with_test_llm(
            cfg.clone(),
            WebResearchTestLlm {
                planner_json: Some(
                    r#"{"need_web":true,"search_budget":1,"extract_budget":0,"browser_budget":0,"total_budget":1}"#
                        .to_string(),
                ),
                evaluator_json: None,
            },
        );
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":1,"extract_budget":0,"browser_budget":0,"total_budget":1}"#,
            &cfg,
        );
        // Test tight pool cap independent of need_web search floor.
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(1, 0, 0, Some(1), 2);
        ctrl.planner_invoked = true;

        let mk = |id: &str| ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"a"}"#.into(),
            },
            extra_content: None,
        };
        let mut calls = vec![mk("1")];
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);
        ctrl.budget_state.attempted_search = 1;

        let mut calls2 = vec![mk("2")];
        let (blocked2, _) = ctrl.gate_web_batch(None, &[], &mut calls2, 2).await;
        assert_eq!(blocked2.len(), 1);
        assert!(calls2.is_empty());
    }

    #[tokio::test]
    async fn fake_planner_budget_four_allows_multiple_searches() {
        let cfg = test_config();
        let plan_json =
            r#"{"need_web":true,"search_budget":4,"extract_budget":0,"browser_budget":0,"total_budget":4}"#;
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(plan_json, &cfg);
        ctrl.limits = limits_for_config(&cfg, ctrl.plan.as_ref());
        ctrl.planner_invoked = true;

        for i in 0..4 {
            let mut calls = vec![ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"q{i}"}}"#),
                },
                extra_content: None,
            }];
            let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, i + 1).await;
            assert!(blocked.is_empty(), "turn {}", i + 1);
            ctrl.budget_state.attempted_search = ctrl.budget_state.attempted_search.saturating_add(1);
        }
    }

    #[tokio::test]
    async fn batch_dual_search_budget_one_no_quota_notice_when_one_runs() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":1,"extract_budget":0,"browser_budget":0,"total_budget":1}"#,
            &cfg,
        );
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(1, 0, 0, Some(1), 2);
        ctrl.planner_invoked = true;

        let mut calls: Vec<ToolCall> = (0..2)
            .map(|i| ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"batch{i}"}}"#),
                },
                extra_content: None,
            })
            .collect();
        let (blocked, notices) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert_eq!(calls.len(), 1);
        assert_eq!(blocked.len(), 1);
        assert!(
            notices.is_empty(),
            "partial same-batch trim must not emit exhaustion notice"
        );
    }

    #[tokio::test]
    async fn batch_three_searches_budget_two_executes_two() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":0,"browser_budget":0,"total_budget":2}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(&cfg, ctrl.plan.as_ref());
        ctrl.planner_invoked = true;

        let mut calls: Vec<ToolCall> = (0..3)
            .map(|i| ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"batch{i}"}}"#),
                },
                extra_content: None,
            })
            .collect();
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert_eq!(calls.len(), 2);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn escalate_browser_after_extract_failure_when_planner_zeroed_browser() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":1,"browser_budget":0,"total_budget":3}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(&cfg, ctrl.plan.as_ref());
        ctrl.planner_invoked = true;

        let calls = vec![ToolCall {
            id: "e1".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://fanqienovel.com/page"}"#.into(),
            },
            extra_content: None,
        }];
        let results = vec![ToolResult::err("e1", "HTTP 403 blocks automated access")];
        assert!(ctrl.record_results(&calls, &results));
        assert_eq!(ctrl.limits.browser_max, cfg.fallback_browser);
        assert!(ctrl.filter_tool_schemas(&[ToolSchema::new(
            "browser_navigate",
            "",
            JsonSchema::new("object")
        )])
        .iter()
        .any(|s| s.name == "browser_navigate"));
    }

    #[test]
    fn escalate_browser_skipped_when_search_succeeds() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":1,"browser_budget":0,"total_budget":3}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(&cfg, ctrl.plan.as_ref());
        ctrl.planner_invoked = true;

        let calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"番茄小说 推荐"}"#.into(),
            },
            extra_content: None,
        }];
        let body = "x".repeat(120);
        let results = vec![ToolResult::ok("s1", body)];
        assert!(!ctrl.record_results(&calls, &results));
        assert_eq!(ctrl.limits.browser_max, 0);
    }

    #[tokio::test]
    async fn browser_budget_zero_block_silent_and_schema_stripped() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":1,"browser_budget":0,"total_budget":3}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(&cfg, ctrl.plan.as_ref());
        ctrl.planner_invoked = true;

        let mut calls = vec![ToolCall {
            id: "b1".into(),
            function: FunctionCall {
                name: "browser_navigate".into(),
                arguments: r#"{"url":"https://fanqienovel.com"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, notices) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
        assert!(
            notices.is_empty(),
            "browser_max=0 is intentional; must not tell user quota exhausted"
        );
        assert!(
            !ctrl.force_finalize(),
            "browser-only block must not stop search/extract for this message"
        );

        let schemas = vec![
            ToolSchema::new("web_search", "", JsonSchema::new("object")),
            ToolSchema::new("browser_navigate", "", JsonSchema::new("object")),
        ];
        let filtered = ctrl.filter_tool_schemas(&schemas);
        assert!(filtered.iter().any(|s| s.name == "web_search"));
        assert!(!filtered.iter().any(|s| s.name == "browser_navigate"));
    }

    #[tokio::test]
    async fn fake_evaluator_stop_blocks_web_and_sets_finalize() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::with_test_llm(
            cfg.clone(),
            WebResearchTestLlm {
                planner_json: None,
                evaluator_json: Some(
                    r#"{"continue_web":false,"sufficient_answer":true,"final_answer_instruction":"answer now"}"#
                        .to_string(),
                ),
            },
        );
        ctrl.planner_invoked = true;
        ctrl.evidence.successful_searches = 1;

        let mut calls = vec![ToolCall {
            id: "w1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"more"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 2).await;
        assert_eq!(blocked.len(), 1);
        assert!(ctrl.force_finalize());
        assert!(calls.is_empty());
    }
}
