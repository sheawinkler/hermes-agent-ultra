//! Adaptive web research: dynamic per-message budgets (planner) and stop decisions (evaluator).

use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hermes_config::{WebResearchConfig, web_research::WebResearchTaskProfile};
use chrono::Datelike;
use hermes_core::{Message, ToolCall, ToolResult, ToolSchema};
use hermes_intelligence::auxiliary::{AuxiliaryClient, AuxiliaryRequest, AuxiliaryTask};
use serde::Deserialize;
use serde_json::Value;

use crate::web_tool_budget::{
    BudgetMode, WebToolBudgetLimits, WebToolBudgetState, apply_web_query_dedup,
    apply_web_tool_budget, apply_web_url_dedup, budget_block_should_notify_user,
    is_attempt_safety_block, is_billable_web_tool_result, is_budgeted_web_tool,
    record_web_tool_results, web_attempt_safety_user_notice, web_tool_budget_user_notice,
    web_url_dedup_user_notice,
};

const PLANNER_TASK: &str = "web_research_planner";
const EVALUATOR_TASK: &str = "web_research_evaluator";
const DECOMPOSER_TASK: &str = "web_research_decomposer";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResearchTaskType {
    RealtimeWeather,
    TargetedNumericFact,
    SimpleLookup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ResearchTaskStatus {
    Pending,
    Verified,
    Exhausted,
}

#[derive(Debug, Clone)]
struct SearchEvidence {
    fact_text: String,
    source_url: Option<String>,
    confidence: f32,
}

#[derive(Debug, Clone)]
struct ResearchTask {
    id: usize,
    task_type: ResearchTaskType,
    /// Sub-question text used for entity/term extraction (may be a clause, not the full message).
    focus_text: String,
    entities: Vec<String>,
    time_scope: Option<String>,
    query_terms: Vec<String>,
    answer_criteria: Vec<String>,
    max_search: u32,
    max_extract: u32,
    max_latency_ms: u64,
    status: ResearchTaskStatus,
    search_attempts: u32,
    source_directed_attempts: u32,
    extract_attempts: u32,
    low_signal_count: u32,
    allowed_urls: HashSet<String>,
    evidence: Vec<SearchEvidence>,
}

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
    tasks: Vec<ResearchTask>,
    evidence: WebResearchEvidence,
    web_stopped: bool,
    force_finalize: bool,
    finalization_reason: Option<&'static str>,
    notice_seen: HashSet<String>,
    planner_invoked: bool,
    /// Planner set `browser_budget` to 0 but light web tools failed — grant fallback browser pool.
    browser_budget_escalated: bool,
    /// Inbound user message explicitly asked for browser automation (`browser_navigate`).
    user_requested_browser: bool,
    #[cfg(test)]
    test_llm: Option<WebResearchTestLlm>,
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct WebResearchTestLlm {
    pub planner_json: Option<String>,
    pub evaluator_json: Option<String>,
    pub decomposer_json: Option<String>,
}

impl WebResearchController {
    pub fn new(config: WebResearchConfig) -> Self {
        Self::with_user_message(config, "")
    }

    pub fn with_user_message(config: WebResearchConfig, user_message: &str) -> Self {
        let user_requested_browser = user_explicitly_requested_browser(user_message);
        let limits = limits_for_config(&config, None, &[], user_requested_browser);
        if user_requested_browser {
            tracing::info!(
                browser_max = limits.browser_max,
                "web_research browser enabled by explicit user request"
            );
        }
        Self {
            config,
            limits,
            budget_state: WebToolBudgetState::new(),
            plan: None,
            tasks: Vec::new(),
            evidence: WebResearchEvidence::default(),
            web_stopped: false,
            force_finalize: false,
            finalization_reason: None,
            notice_seen: HashSet::new(),
            planner_invoked: false,
            browser_budget_escalated: false,
            user_requested_browser,
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
        if self.config.enabled && !self.user_requested_browser {
            out.retain(|s| s.name != "browser_navigate");
        }
        if self.config.enabled && self.planner_invoked {
            out.retain(|s| match s.name.as_str() {
                "browser_navigate" => self.limits.browser_max > 0,
                "web_search" => self.limits.search_max > 0,
                "web_extract" => self.limits.extract_max > 0,
                _ => true,
            });
            if !self.tasks.is_empty()
                && self.tasks.iter().all(|t| t.status == ResearchTaskStatus::Verified)
            {
                out.retain(|s| !matches!(s.name.as_str(), "web_extract" | "browser_navigate"));
            }
        }
        out
    }

    /// One-shot guidance injected after web_research planner/decompose runs.
    pub fn active_research_system_hint(&self) -> Option<String> {
        if !self.config.enabled || !self.planner_invoked || self.tasks.is_empty() {
            return None;
        }
        if !self.config.search_snippet_first {
            return None;
        }
        let browser_line = if self.user_requested_browser {
            " Browser is available because the user requested it."
        } else {
            " Do not use browser_navigate for this message."
        };
        Some(format!(
            "Web research policy: prefer web_search snippets when they already contain the answer. \
             When snippets omit a needed figure, web_extract URLs returned by web_search.{browser_line} \
             For today-scoped questions without digits in the user text, avoid web_extract while search \
             budget remains; use another web_search query instead."
        ))
    }

    fn should_strip_web_tools(&self) -> bool {
        self.config.enabled && (self.web_stopped || self.force_finalize)
    }

    pub fn finalization_system_hint(&self) -> Option<String> {
        if !self.force_finalize {
            return None;
        }
        let guidance = match self.finalization_reason {
            Some("attempt_safety_no_evidence") => {
                "No verified web evidence was retrieved. Do not call web tools again for this user message; answer only if possible and clearly state that web verification was not completed."
            }
            Some("quota_with_evidence") => {
                "Web research quota is exhausted for this user message. Answer from retrieved evidence and mark anything not supported by those results as unverified."
            }
            _ => {
                "Web research is complete for this user message. Do not call web_search, web_extract, or browser_navigate. Answer now from existing tool results and conversation context. Clearly mark anything unverified."
            }
        };
        let task_hint = self.task_finalization_hint();
        Some(format!("{guidance}\n{task_hint}"))
    }

    pub fn tool_calls_include_web(tool_calls: &[ToolCall]) -> bool {
        tool_calls
            .iter()
            .any(|tc| is_budgeted_web_tool(&tc.function.name))
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
        self.tasks = decompose_research_tasks(user_message, &self.config);
        if self.config.llm_decomposer_enabled {
            if let Some(tasks) = self
                .fetch_decomposed_tasks(auxiliary, user_message)
                .await
            {
                if !tasks.is_empty() {
                    tracing::info!(task_count = tasks.len(), "web_research llm decomposer applied");
                    self.tasks = tasks;
                }
            }
        }
        if !self.config.planner_enabled {
            self.plan = Some(fallback_plan(
                &self.config,
                &self.tasks,
                self.user_requested_browser,
            ));
            self.limits = limits_for_config(
                &self.config,
                self.plan.as_ref(),
                &self.tasks,
                self.user_requested_browser,
            );
            return;
        }
        let plan = match self.fetch_plan(auxiliary, user_message).await {
            Some(p) => p,
            None => {
                tracing::warn!("web_research planner failed; using fallback budgets");
                fallback_plan(&self.config, &self.tasks, self.user_requested_browser)
            }
        };
        self.limits = limits_for_config(
            &self.config,
            Some(&plan),
            &self.tasks,
            self.user_requested_browser,
        );
        if !plan.need_web {
            self.web_stopped = true;
            self.force_finalize = true;
            self.finalization_reason = Some("planner_stop");
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
                BudgetMode::Global,
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
                    if !decision.final_answer_instruction.is_empty() {
                        tracing::debug!(
                            instruction_len = decision.final_answer_instruction.len(),
                            "web_research evaluator finalization instruction"
                        );
                    }
                    if all_tasks_terminal(&self.tasks) {
                        self.web_stopped = true;
                        self.force_finalize = true;
                        self.finalization_reason = Some("evaluator_stop");
                        let blocked = self.block_all_web_calls(tool_calls, turn, "evaluator stop");
                        return (blocked, user_notices);
                    }
                    tracing::info!(
                        sufficient_answer = decision.sufficient_answer,
                        continue_web = decision.continue_web,
                        pending_tasks = self.tasks.iter().filter(|t| t.status == ResearchTaskStatus::Pending).count(),
                        "web_research evaluator suggested stop but tasks still pending; continue"
                    );
                }
            }
        }

        let mut deferred =
            apply_web_query_dedup(messages, &mut self.budget_state, tool_calls, turn);
        deferred.extend(apply_web_url_dedup(messages, tool_calls, turn));
        if !deferred.is_empty() {
            let dedup_notice = if deferred.iter().any(|(n, _)| n == "web_search") {
                "重复检索请求已跳过，将基于已有搜索结果回答。（本则用户消息内不再重复检索）"
                    .to_string()
            } else {
                web_url_dedup_user_notice()
            };
            if self.emit_notice_once("dedup", dedup_notice.clone()) {
                user_notices.push(dedup_notice);
            }
            if tool_calls
                .iter()
                .all(|tc| is_budgeted_web_tool(&tc.function.name))
                && all_tasks_terminal(&self.tasks)
            {
                self.web_stopped = true;
                self.force_finalize = true;
                self.finalization_reason = Some("dedup");
            }
        }

        trim_parallel_web_calls(tool_calls, self.config.max_parallel_web_calls);
        deferred.extend(self.apply_task_policy(tool_calls, turn));

        let budget_mode = if self.tasks.is_empty() {
            BudgetMode::Global
        } else {
            BudgetMode::TaskPrimary
        };
        let budget_blocked = apply_web_tool_budget(
            &self.budget_state,
            &self.limits,
            tool_calls,
            turn,
            budget_mode,
        );
        if !budget_blocked.is_empty() {
            let blocked_by_errors =
                self.budget_state.consecutive_error_turns >= self.limits.max_consecutive_errors;
            let searches_still_scheduled =
                tool_calls.iter().any(|tc| tc.function.name == "web_search");
            let extracts_still_scheduled = tool_calls
                .iter()
                .any(|tc| tc.function.name == "web_extract");
            let browser_still_scheduled = tool_calls
                .iter()
                .any(|tc| tc.function.name == "browser_navigate");
            let has_evidence = self.budget_state.has_successful_evidence();
            let no_web_capacity_left = !self.any_web_pool_has_capacity();
            for (tool_name, result) in &budget_blocked {
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
                let notice = if is_attempt_safety_block(&result.content) {
                    web_attempt_safety_user_notice()
                } else if !blocked_by_errors && !no_web_capacity_left {
                    continue;
                } else if has_evidence {
                    web_tool_budget_user_notice(tool_name, blocked_by_errors)
                } else {
                    continue;
                };
                if self.emit_notice_once(&format!("budget:{tool_name}"), notice.clone()) {
                    user_notices.push(notice);
                }
            }
            if tool_calls.is_empty()
                && !self.any_web_pool_has_capacity()
                && all_tasks_terminal(&self.tasks)
            {
                self.web_stopped = true;
                self.force_finalize = true;
                self.finalization_reason = if budget_blocked
                    .iter()
                    .any(|(_, r)| is_attempt_safety_block(&r.content))
                    && !has_evidence
                {
                    Some("attempt_safety_no_evidence")
                } else {
                    Some("quota_with_evidence")
                };
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
        if self.budget_state.attempted_total() >= self.limits.max_attempt_total {
            return false;
        }
        if !self.tasks.is_empty() {
            let task_capacity = self.tasks.iter().any(|task| {
                task.status == ResearchTaskStatus::Pending
                    && (task.search_attempts < task.max_search
                        || task.extract_attempts < task.max_extract)
            });
            return task_capacity
                || (self.limits.browser_max > 0
                    && self.budget_state.browser_used < self.limits.browser_max);
        }
        self.budget_state.search_used < self.limits.search_max
            || self.budget_state.extract_used < self.limits.extract_max
            || (self.limits.browser_max > 0
                && self.budget_state.browser_used < self.limits.browser_max)
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
            let accepted = self.record_task_result(tc, result, billable);
            if billable && !accepted && tc.function.name == "web_search" {
                self.deduct_low_signal_search(tc);
            }
            if billable && accepted {
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
        }
        if light_failure {
            self.maybe_escalate_browser_after_light_failure()
        } else {
            false
        }
    }

    fn record_task_result(
        &mut self,
        tc: &ToolCall,
        result: Option<&ToolResult>,
        billable: bool,
    ) -> bool {
        if self.tasks.is_empty() || !billable {
            return billable;
        }
        let task_idx = self
            .infer_task_for_call(tc)
            .or_else(|| first_open_task(&self.tasks));
        let Some(idx) = task_idx else {
            return true;
        };
        let content = result.map(|r| r.content.as_str()).unwrap_or_default();
        match tc.function.name.as_str() {
            "web_search" => {
                let query = query_from_tool_arguments(&tc.function.name, &tc.function.arguments)
                    .unwrap_or_default();
                self.record_task_search(idx, content, &query)
            }
            "web_extract" | "browser_navigate" => self.record_task_extract(idx, content),
            _ => true,
        }
    }

    fn deduct_low_signal_search(&mut self, tc: &ToolCall) {
        self.budget_state.search_used = self.budget_state.search_used.saturating_sub(1);
        self.budget_state.billable_total = self.budget_state.billable_total.saturating_sub(1);
        self.budget_state.search_failed = self.budget_state.search_failed.saturating_add(1);
        if let Some(query) = query_from_tool_arguments(&tc.function.name, &tc.function.arguments) {
            self.budget_state.successful_search_queries.remove(&query);
            *self
                .budget_state
                .failed_search_queries
                .entry(query)
                .or_insert(0) += 1;
        }
    }

    fn record_task_search(&mut self, idx: usize, content: &str, query: &str) -> bool {
        let task_snapshot = self.tasks[idx].clone();
        let items = search_items_from_output(content);
        let mut accepted = false;
        let mut new_urls = Vec::new();
        for item in items {
            if let Some(url) = normalize_url(&item.url) {
                new_urls.push(url);
            }
            if search_item_passes_task(&task_snapshot, &item, &self.config) {
                let confidence = source_confidence(&self.config, &item.url);
                accepted = true;
                self.tasks[idx].evidence.push(SearchEvidence {
                    fact_text: item.text,
                    source_url: Some(item.url),
                    confidence,
                });
            }
        }
        let task = &mut self.tasks[idx];
        if is_source_directed_query(query) {
            task.source_directed_attempts = task.source_directed_attempts.saturating_add(1);
        }
        if accepted || !is_source_directed_query(query) {
            task.search_attempts = task.search_attempts.saturating_add(1);
        }
        task.allowed_urls.extend(new_urls);
        if accepted {
            task.status = ResearchTaskStatus::Verified;
        } else {
            task.low_signal_count = task.low_signal_count.saturating_add(1);
            if task.search_attempts >= task.max_search {
                task.status = ResearchTaskStatus::Exhausted;
            }
        }
        tracing::info!(
            task_id = task.id,
            task_type = ?task.task_type,
            status = ?task.status,
            search_attempts = task.search_attempts,
            extract_attempts = task.extract_attempts,
            low_signal_count = task.low_signal_count,
            accepted = accepted,
            "web_research task search recorded"
        );
        accepted
    }

    fn record_task_extract(&mut self, idx: usize, content: &str) -> bool {
        let accepted = text_passes_task(&self.tasks[idx], content, &self.config);
        let task = &mut self.tasks[idx];
        task.extract_attempts = task.extract_attempts.saturating_add(1);
        if accepted {
            task.status = ResearchTaskStatus::Verified;
            task.evidence.push(SearchEvidence {
                fact_text: content.chars().take(300).collect(),
                source_url: None,
                confidence: 0.75,
            });
        } else if task.extract_attempts >= task.max_extract {
            task.status = ResearchTaskStatus::Exhausted;
        }
        accepted
    }

    fn maybe_escalate_browser_after_light_failure(&mut self) -> bool {
        if !self.user_requested_browser
            || !self.config.enabled
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
        let grant = browser_escalation_budget(&self.config, self.user_requested_browser);
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
            self.finalization_reason = None;
        }
        true
    }

    pub fn note_assistant_draft(&mut self, content: Option<&str>) {
        let text = content.map(str::trim).unwrap_or("");
        if text.len() >= 40 {
            self.evidence.last_assistant_draft = Some(text.to_string());
        }
    }

    fn task_finalization_hint(&self) -> String {
        if self.tasks.is_empty() {
            return "If numeric facts are not verified by web evidence, do not estimate them."
                .into();
        }
        let mut lines = vec![
            "Answer per research task. For targeted_numeric_fact tasks, do not estimate a value unless that task is verified.".to_string(),
            "When multiple tasks are pending, issue one web_search(num_results=5) per unfinished task in the first round; parallel calls in the same batch are allowed.".to_string(),
            "Chinese queries use DDGS meta-search (international + sogou/bing_cn when CJK is detected).".to_string(),
            "If snippets already contain the answer, mark the task done and reply; do not fetch pages.".to_string(),
        ];
        for task in &self.tasks {
            let evidence_preview = task.evidence.first().map(|e| {
                format!(
                    "source={:?} confidence={} text={}",
                    e.source_url,
                    e.confidence,
                    e.fact_text.chars().take(80).collect::<String>()
                )
            });
            lines.push(format!(
                "task#{} type={:?} status={:?} focus={:?} entities={:?} criteria={:?} evidence_count={} max_latency_ms={} evidence_preview={:?}",
                task.id,
                task.task_type,
                task.status,
                task.focus_text,
                task.entities,
                task.answer_criteria,
                task.evidence.len(),
                task.max_latency_ms,
                evidence_preview
            ));
        }
        lines.join("\n")
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

    fn apply_task_policy(
        &mut self,
        tool_calls: &mut Vec<ToolCall>,
        turn: u32,
    ) -> Vec<(String, ToolResult)> {
        if self.tasks.is_empty() {
            return Vec::new();
        }
        let mut blocked = Vec::new();
        let mut kept = Vec::new();
        let mut search_counts: Vec<u32> = self.tasks.iter().map(|t| t.search_attempts).collect();
        let mut source_counts: Vec<u32> = self
            .tasks
            .iter()
            .map(|t| t.source_directed_attempts)
            .collect();
        let mut extract_counts: Vec<u32> = self.tasks.iter().map(|t| t.extract_attempts).collect();
        for tc in tool_calls.drain(..) {
            let name = tc.function.name.as_str();
            if !matches!(name, "web_search" | "web_extract" | "browser_navigate") {
                kept.push(tc);
                continue;
            }
            let task_idx = self.infer_task_for_call(&tc);
            if self.should_block_task_call(
                &tc,
                task_idx,
                &mut search_counts,
                &mut source_counts,
                &mut extract_counts,
            ) {
                if let Some(idx) = task_idx {
                    let task = &self.tasks[idx];
                    tracing::info!(
                        turn,
                        tool = %tc.function.name,
                        task_id = task.id,
                        task_type = ?task.task_type,
                        status = ?task.status,
                        search_attempts = task.search_attempts,
                        extract_attempts = task.extract_attempts,
                        low_signal_count = task.low_signal_count,
                        "web_research task policy block"
                    );
                } else {
                    tracing::info!(turn, tool = %tc.function.name, "web_research task policy block");
                }
                blocked.push((
                    tc.function.name.clone(),
                    ToolResult::err(
                        tc.id,
                        "Web research task policy blocked this tool call: task budget exhausted or URL was not produced by the current task search results.",
                    ),
                ));
            } else {
                kept.push(tc);
            }
        }
        *tool_calls = kept;
        blocked
    }

    fn infer_task_for_call(&self, tc: &ToolCall) -> Option<usize> {
        if let Some(query) = query_from_tool_arguments(&tc.function.name, &tc.function.arguments) {
            if let Some(idx) = best_task_for_text(&self.tasks, &query) {
                return Some(idx);
            }
        }
        if let Some(url) = url_from_tool_arguments(&tc.function.name, &tc.function.arguments) {
            let normalized = normalize_url(&url)?;
            return self
                .tasks
                .iter()
                .position(|task| task.allowed_urls.contains(&normalized));
        }
        None
    }

    fn should_block_task_call(
        &self,
        tc: &ToolCall,
        task_idx: Option<usize>,
        search_counts: &mut [u32],
        source_counts: &mut [u32],
        extract_counts: &mut [u32],
    ) -> bool {
        let Some(idx) = task_idx.or_else(|| first_open_task(&self.tasks)) else {
            return false;
        };
        let task = &self.tasks[idx];
        match tc.function.name.as_str() {
            "web_search" => {
                let query = query_from_tool_arguments(&tc.function.name, &tc.function.arguments)
                    .unwrap_or_default();
                if task.status == ResearchTaskStatus::Verified {
                    return true;
                }
                if is_source_directed_query(&query) {
                    if source_counts[idx] >= 2 {
                        return true;
                    }
                    source_counts[idx] = source_counts[idx].saturating_add(1);
                    return false;
                }
                if search_counts[idx] >= task.max_search {
                    return true;
                }
                search_counts[idx] = search_counts[idx].saturating_add(1);
                false
            }
            "browser_navigate" => {
                if !self.user_requested_browser || task.status == ResearchTaskStatus::Verified {
                    return true;
                }
                false
            }
            "web_extract" => {
                if task.status == ResearchTaskStatus::Verified {
                    return true;
                }
                if self.config.search_snippet_first
                    && task_prefers_snippet_first(task)
                    && task.search_attempts < task.max_search
                {
                    return true;
                }
                if !self.extract_url_allowed(tc, idx) || extract_counts[idx] >= task.max_extract {
                    return true;
                }
                extract_counts[idx] = extract_counts[idx].saturating_add(1);
                false
            }
            _ => false,
        }
    }

    fn extract_url_allowed(&self, tc: &ToolCall, task_idx: usize) -> bool {
        let Some(url) = url_from_tool_arguments(&tc.function.name, &tc.function.arguments) else {
            return false;
        };
        let Some(normalized) = normalize_url(&url) else {
            return false;
        };
        self.tasks[task_idx].allowed_urls.contains(&normalized)
    }

    async fn fetch_decomposed_tasks(
        &self,
        auxiliary: Option<&Arc<AuxiliaryClient>>,
        user_message: &str,
    ) -> Option<Vec<ResearchTask>> {
        #[cfg(test)]
        if let Some(test) = &self.test_llm {
            if let Some(raw) = &test.decomposer_json {
                return parse_decomposer_json(raw, user_message, &self.config);
            }
        }
        let aux = auxiliary?;
        let system = "Split the user message into independent web research tasks. Output JSON only: \
                      {\"tasks\":[{\"task_type\":\"realtime_weather|targeted_numeric_fact|simple_lookup\",\
                      \"focus\":\"sub-question text\"}]}. Max 3 tasks. focus must be a short clause.";
        let user = format!("User message:\n{}", user_message.trim());
        let request = AuxiliaryRequest::new(
            AuxiliaryTask::Custom(DECOMPOSER_TASK.to_string()),
            vec![Message::system(system), Message::user(user)],
        )
        .with_temperature(0.0)
        .with_max_tokens(500)
        .with_timeout(Duration::from_secs(30));
        let text = aux.call(request).await.ok()?.text()?.to_string();
        parse_decomposer_json(&text, user_message, &self.config)
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
fn browser_escalation_budget(config: &WebResearchConfig, user_requested_browser: bool) -> u32 {
    if !user_requested_browser {
        return 0;
    }
    config.fallback_browser.max(1).min(config.max_browser)
}

fn effective_browser_budget(
    config: &WebResearchConfig,
    raw: u32,
    user_requested_browser: bool,
) -> u32 {
    if !user_requested_browser {
        return 0;
    }
    let grant = if raw > 0 {
        raw
    } else {
        config.fallback_browser
    };
    grant.min(config.max_browser)
}

/// True when the inbound user message explicitly asks for browser automation.
fn user_explicitly_requested_browser(user_message: &str) -> bool {
    let text = user_message.trim();
    if text.is_empty() {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    if lower.contains("browser_navigate") {
        return true;
    }
    ["用浏览器", "使用浏览器", "浏览器打开", "浏览器访问", "打开浏览器"]
        .iter()
        .any(|cue| text.contains(cue))
        || [
            "use browser",
            "open in browser",
            "using browser",
            "browser navigate",
        ]
        .iter()
        .any(|cue| lower.contains(cue))
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

fn task_budget_ceilings(tasks: &[ResearchTask], config: &WebResearchConfig) -> (u32, u32) {
    if tasks.is_empty() {
        return (0, 0);
    }
    let search = tasks
        .iter()
        .map(|t| t.max_search)
        .sum::<u32>()
        .min(config.message_caps.max_total_search);
    let extract = tasks
        .iter()
        .map(|t| t.max_extract)
        .sum::<u32>()
        .min(config.message_caps.max_total_extract);
    (search, extract)
}

fn all_tasks_terminal(tasks: &[ResearchTask]) -> bool {
    tasks.is_empty()
        || tasks
            .iter()
            .all(|t| matches!(t.status, ResearchTaskStatus::Verified | ResearchTaskStatus::Exhausted))
}

fn limits_for_config(
    config: &WebResearchConfig,
    plan: Option<&WebResearchPlan>,
    tasks: &[ResearchTask],
    user_requested_browser: bool,
) -> WebToolBudgetLimits {
    if let Some(plan) = plan {
        let (search_max, extract_max) = if !tasks.is_empty() {
            task_budget_ceilings(tasks, config)
        } else {
            (
                plan.search_budget
                    .min(config.message_caps.max_total_search.max(config.max_search)),
                plan.extract_budget.min(
                    config
                        .message_caps
                        .max_total_extract
                        .max(config.max_extract),
                ),
            )
        };
        let browser_max =
            effective_browser_budget(config, plan.browser_budget, user_requested_browser);
        let aggregate = search_max
            .saturating_add(extract_max)
            .saturating_add(browser_max)
            .max(plan.total_budget);
        let mut limits = WebToolBudgetLimits::from_dynamic_pools(
            search_max,
            extract_max,
            browser_max,
            Some(aggregate),
            config.max_consecutive_errors,
        );
        limits.max_attempt_total = limits
            .max_attempt_total
            .max(config.message_caps.max_attempt_total);
        limits
    } else {
        let mut limits = WebToolBudgetLimits::from_web_research_config(config);
        limits.browser_max =
            effective_browser_budget(config, limits.browser_max, user_requested_browser);
        limits
    }
}

fn fallback_plan(
    config: &WebResearchConfig,
    tasks: &[ResearchTask],
    user_requested_browser: bool,
) -> WebResearchPlan {
    let (search_budget, extract_budget) = if !tasks.is_empty() {
        task_budget_ceilings(tasks, config)
    } else {
        (config.fallback_search, config.fallback_extract)
    };
    WebResearchPlan {
        need_web: true,
        search_budget,
        extract_budget,
        browser_budget: effective_browser_budget(
            config,
            config.fallback_browser,
            user_requested_browser,
        ),
        total_budget: search_budget
            .saturating_add(extract_budget)
            .saturating_add(effective_browser_budget(
                config,
                config.fallback_browser,
                user_requested_browser,
            ))
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
        tracing::warn!(
            path = path,
            "web_research planner_prompt_path unreadable; using default"
        );
    }
    "You allocate web research work per user message. Split multi-intent questions into small \
     independent research tasks, keep simple realtime lookups low-latency, and expand only when \
     evidence has not passed task criteria. When multiple tasks are open, the first round should \
     launch one web_search(num_results=5) per unfinished task; parallel calls in the same batch are \
     expected. Search results are not verified evidence until they match the requested entity, \
     time scope, answer type, and required fact signal. For numeric facts, never estimate values \
     when verified evidence is missing. A source-directed query such as a site/domain query is only \
     one branch; if it does not pass evidence criteria, continue with broader public reporting \
     queries before finalizing. Never exceed configured ceilings in your output; they will be \
     clamped in Rust."
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
    let mut total = clamp_budget(raw_plan.total_budget, config.max_total);
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
struct RawDecomposerTask {
    #[serde(default)]
    task_type: Option<String>,
    #[serde(default)]
    focus: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDecomposerOutput {
    #[serde(default)]
    tasks: Vec<RawDecomposerTask>,
}

fn parse_task_type_name(raw: &str) -> Option<ResearchTaskType> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "realtime_weather" | "weather" => Some(ResearchTaskType::RealtimeWeather),
        "targeted_numeric_fact" | "numeric" | "numeric_fact" => {
            Some(ResearchTaskType::TargetedNumericFact)
        }
        "simple_lookup" | "lookup" => Some(ResearchTaskType::SimpleLookup),
        _ => None,
    }
}

fn parse_decomposer_json(
    raw: &str,
    user_message: &str,
    config: &WebResearchConfig,
) -> Option<Vec<ResearchTask>> {
    let value = extract_json_value(raw)?;
    let output: RawDecomposerOutput = serde_json::from_value(value).ok()?;
    let mut tasks = Vec::new();
    for (id, entry) in output.tasks.into_iter().take(3).enumerate() {
        let focus = entry
            .focus
            .filter(|f| !f.trim().is_empty())
            .unwrap_or_else(|| user_message.to_string());
        let task_type = entry
            .task_type
            .as_deref()
            .and_then(parse_task_type_name)
            .unwrap_or_else(|| infer_task_type_from_focus(&focus));
        tasks.push(new_research_task(id, task_type, &focus, config));
    }
    if tasks.is_empty() {
        None
    } else {
        Some(tasks)
    }
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

#[derive(Debug, Clone)]
struct SearchItem {
    title: String,
    url: String,
    text: String,
}

fn decompose_research_tasks(user_message: &str, config: &WebResearchConfig) -> Vec<ResearchTask> {
    let segments = split_intent_segments(user_message);
    let specs: Vec<(ResearchTaskType, String)> = if segments.len() > 1 {
        segments
            .into_iter()
            .map(|segment| (infer_task_type_from_focus(&segment), segment))
            .collect()
    } else {
        vec![(
            infer_task_type_from_focus(user_message),
            user_message.to_string(),
        )]
    };
    specs
        .into_iter()
        .take(3)
        .enumerate()
        .map(|(id, (task_type, focus))| new_research_task(id, task_type, &focus, config))
        .collect()
}

fn split_intent_segments(text: &str) -> Vec<String> {
    let segments: Vec<String> = text
        .split(['，', ',', '。', ';', '；', '?', '？', '!', '！', '\n'])
        .map(trim_segment_noise)
        .filter(|segment| segment.chars().count() >= 4)
        .collect();
    if segments.len() > 1 {
        return segments;
    }
    split_multi_intent_on_whitespace(text).unwrap_or(segments)
}

/// WeCom-style messages often use spaces instead of commas between sub-questions.
fn split_multi_intent_on_whitespace(text: &str) -> Option<Vec<String>> {
    let segments: Vec<String> = text
        .split_whitespace()
        .map(str::trim)
        .filter(|part| part.chars().count() >= 4)
        .map(trim_segment_noise)
        .filter(|segment| segment.chars().count() >= 4)
        .collect();
    if segments.len() >= 2 {
        Some(segments)
    } else {
        None
    }
}

fn trim_parallel_web_calls(tool_calls: &mut Vec<ToolCall>, max_parallel: u32) {
    if max_parallel == 0 {
        return;
    }
    let web_count = tool_calls
        .iter()
        .filter(|tc| matches!(tc.function.name.as_str(), "web_search" | "web_extract" | "browser_navigate"))
        .count() as u32;
    if web_count <= max_parallel {
        return;
    }
    let mut kept_web = 0u32;
    tool_calls.retain(|tc| {
        if !matches!(
            tc.function.name.as_str(),
            "web_search" | "web_extract" | "browser_navigate"
        ) {
            return true;
        }
        if kept_web < max_parallel {
            kept_web += 1;
            true
        } else {
            false
        }
    });
}

/// Classify from structural signals only (digits, calendar year, today time scope).
fn infer_task_type_from_focus(focus_text: &str) -> ResearchTaskType {
    if has_numeric_signal(focus_text) {
        ResearchTaskType::TargetedNumericFact
    } else if extract_time_scope(focus_text).as_deref() == Some("today") {
        ResearchTaskType::RealtimeWeather
    } else {
        ResearchTaskType::SimpleLookup
    }
}

fn task_prefers_snippet_first(task: &ResearchTask) -> bool {
    task.time_scope.as_deref() == Some("today") && !has_numeric_signal(&task.focus_text)
}

fn new_research_task(
    id: usize,
    task_type: ResearchTaskType,
    focus_text: &str,
    config: &WebResearchConfig,
) -> ResearchTask {
    let profile = task_profile(config, task_type);
    ResearchTask {
        id,
        task_type,
        focus_text: focus_text.to_string(),
        entities: extract_entities(focus_text),
        time_scope: extract_time_scope(focus_text),
        query_terms: extract_query_terms(focus_text),
        answer_criteria: answer_criteria(task_type),
        max_search: profile.max_search,
        max_extract: profile.max_extract,
        max_latency_ms: profile.max_latency_ms,
        status: ResearchTaskStatus::Pending,
        search_attempts: 0,
        source_directed_attempts: 0,
        extract_attempts: 0,
        low_signal_count: 0,
        allowed_urls: HashSet::new(),
        evidence: Vec::new(),
    }
}

fn task_profile(config: &WebResearchConfig, task_type: ResearchTaskType) -> WebResearchTaskProfile {
    match task_type {
        ResearchTaskType::RealtimeWeather => config.task_profiles.realtime_weather.clone(),
        ResearchTaskType::TargetedNumericFact => config.task_profiles.targeted_numeric_fact.clone(),
        ResearchTaskType::SimpleLookup => config.task_profiles.simple_lookup.clone(),
    }
}

fn answer_criteria(task_type: ResearchTaskType) -> Vec<String> {
    match task_type {
        ResearchTaskType::TargetedNumericFact => vec!["numeric_signal".into()],
        ResearchTaskType::RealtimeWeather => vec!["time_scope".into(), "entity".into()],
        ResearchTaskType::SimpleLookup => vec!["entity".into()],
    }
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn extract_time_scope(text: &str) -> Option<String> {
    for year in 2000..=2099 {
        let year_text = year.to_string();
        if text.contains(&year_text) {
            return Some(year_text);
        }
    }
    if contains_any(text, &["今天", "今日", "当前", "实时"]) {
        return Some("today".into());
    }
    None
}

fn extract_entities(text: &str) -> Vec<String> {
    extract_query_terms(text)
        .into_iter()
        .filter(|term| !is_filler_term(term))
        .take(3)
        .collect()
}

fn extract_query_terms(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for part in text.split(|c: char| !c.is_alphanumeric() && !is_cjk(c)) {
        let trimmed = trim_common_words(part);
        if trimmed.chars().count() >= 2 && !is_filler_term(&trimmed) {
            push_unique(&mut terms, trimmed.clone());
        }
        for gram in cjk_ngrams(&trimmed, 2) {
            if !is_filler_term(&gram) {
                push_unique(&mut terms, gram);
            }
        }
    }
    terms.into_iter().take(8).collect()
}

fn push_unique(items: &mut Vec<String>, item: String) {
    if !items.contains(&item) {
        items.push(item);
    }
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

/// Strip greeting noise for segment boundaries; preserve time/year tokens in focus text.
fn trim_segment_noise(text: &str) -> String {
    let mut value = text.trim().to_string();
    for word in ["你好", "请问", "帮我", "一下"] {
        value = value.replace(word, "");
    }
    value
        .trim_matches(|c: char| c.is_whitespace() || "，。！？、".contains(c))
        .to_string()
}

fn trim_common_words(text: &str) -> String {
    let mut value = trim_segment_noise(text);
    for word in ["怎么样", "怎么", "什么"] {
        value = value.replace(word, "");
    }
    value
        .trim_matches(|c: char| c.is_whitespace() || "的了呢吗".contains(c))
        .to_string()
}

fn is_filler_term(term: &str) -> bool {
    term.chars().count() < 2 || matches!(term, "的" | "了" | "呢" | "吗" | "吧")
}

fn cjk_ngrams(text: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().filter(|c| is_cjk(*c)).collect();
    if chars.len() < n {
        return Vec::new();
    }
    chars.windows(n).map(|w| w.iter().collect()).collect()
}

fn query_from_tool_arguments(name: &str, arguments: &str) -> Option<String> {
    if name != "web_search" {
        return None;
    }
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|v| v.get("query").and_then(|q| q.as_str()).map(str::to_string))
}

fn is_source_directed_query(query: &str) -> bool {
    let lower = query.to_ascii_lowercase();
    lower.contains("site:")
        || lower.split_whitespace().any(|token| {
            token.contains('.') && !token.chars().all(|c| c.is_ascii_digit() || c == '.')
        })
}

fn url_from_tool_arguments(name: &str, arguments: &str) -> Option<String> {
    if !matches!(name, "web_extract" | "browser_navigate") {
        return None;
    }
    serde_json::from_str::<Value>(arguments).ok().and_then(|v| {
        v.get("url")
            .or_else(|| v.get("source_url"))
            .and_then(|u| u.as_str())
            .map(str::to_string)
    })
}

fn normalize_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.contains("xxxx") || trimmed.contains("XXXX") {
        return None;
    }
    let mut normalized = trimmed.trim_end_matches('/').to_ascii_lowercase();
    if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
        return None;
    }
    if let Some(hash) = normalized.find('#') {
        normalized.truncate(hash);
    }
    Some(normalized)
}

fn search_items_from_output(output: &str) -> Vec<SearchItem> {
    let Ok(value) = serde_json::from_str::<Value>(output.trim()) else {
        return Vec::new();
    };
    let Some(results) = value
        .get("results")
        .or_else(|| value.get("web"))
        .or_else(|| value.get("data").and_then(|v| v.get("results")))
        .or_else(|| value.get("data").and_then(|v| v.get("web")))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    results
        .iter()
        .filter_map(|item| {
            let url = item.get("url").and_then(|v| v.as_str())?.trim().to_string();
            let title = item
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = item
                .get("text")
                .or_else(|| item.get("snippet"))
                .or_else(|| item.get("content"))
                .or_else(|| item.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Some(SearchItem { title, url, text })
        })
        .collect()
}

fn source_confidence(config: &WebResearchConfig, url: &str) -> f32 {
    let weight = source_weight(config, url);
    (0.5 + (weight as f32 / 100.0)).clamp(0.1, 0.95)
}

fn source_weight(config: &WebResearchConfig, url: &str) -> i32 {
    let Some(host) = host_from_url(url) else {
        return 0;
    };
    config
        .source_classes
        .values()
        .filter(|class| class.domain_patterns.iter().any(|p| host_matches(&host, p)))
        .map(|class| class.weight)
        .max()
        .unwrap_or(0)
}

fn host_from_url(url: &str) -> Option<String> {
    let without_scheme = url.split_once("://")?.1;
    Some(without_scheme.split('/').next()?.to_ascii_lowercase())
}

fn host_matches(host: &str, pattern: &str) -> bool {
    let pattern = pattern.trim().to_ascii_lowercase();
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return host == suffix || host.ends_with(&format!(".{suffix}"));
    }
    host == pattern
}

fn search_item_passes_task(
    task: &ResearchTask,
    item: &SearchItem,
    config: &WebResearchConfig,
) -> bool {
    let combined = format!("{} {}", item.title, item.text);
    text_passes_task(task, &combined, config)
}

fn text_passes_task(task: &ResearchTask, text: &str, _config: &WebResearchConfig) -> bool {
    if text.trim().chars().count() < 20 || looks_like_missing_page(text) {
        return false;
    }
    if !matches_time_scope(task, text) || !matches_entity_scope(task, text) {
        return false;
    }
    if !matches_required_terms(task, text) {
        return false;
    }
    match task.task_type {
        ResearchTaskType::RealtimeWeather | ResearchTaskType::SimpleLookup => true,
        ResearchTaskType::TargetedNumericFact => has_numeric_signal(text),
    }
}

fn matches_entity_scope(task: &ResearchTask, text: &str) -> bool {
    let scope_terms: Vec<&str> = if !task.entities.is_empty() {
        task.entities.iter().map(String::as_str).collect()
    } else {
        task.query_terms.iter().map(String::as_str).collect()
    };
    if scope_terms.is_empty() {
        return true;
    }
    let required = match task.task_type {
        ResearchTaskType::TargetedNumericFact => scope_terms.len().min(1),
        ResearchTaskType::RealtimeWeather => 1,
        ResearchTaskType::SimpleLookup => 1,
    };
    scope_terms
        .iter()
        .filter(|term| text.contains(**term))
        .count()
        >= required
}

fn matches_time_scope(task: &ResearchTask, text: &str) -> bool {
    match task.time_scope.as_deref() {
        Some("today") => {
            if contains_any(text, &["今天", "今日", "当前", "实时"]) {
                return true;
            }
            text_matches_today_wall_date(text)
        }
        Some(year) => text.contains(year),
        None => true,
    }
}

fn text_matches_today_wall_date(text: &str) -> bool {
    let dt = hermes_core::now();
    let month = dt.month();
    let day = dt.day();
    let year = dt.year();
    [
        format!("{month}月{day}日"),
        format!("{month}月{day}号"),
        format!("{year}年{month}月{day}日"),
        format!("{year}-{month:02}-{day:02}"),
        format!("{month}/{day}"),
        format!("{month}-{day}"),
    ]
    .iter()
    .any(|token| text.contains(token))
}

fn matches_required_terms(task: &ResearchTask, text: &str) -> bool {
    let terms: Vec<&String> = task
        .query_terms
        .iter()
        .filter(|term| term.chars().count() >= 2)
        .collect();
    if terms.is_empty() {
        return true;
    }
    let required = match task.task_type {
        ResearchTaskType::TargetedNumericFact => 1,
        _ => 1,
    };
    terms
        .iter()
        .filter(|term| text.contains(term.as_str()))
        .count()
        >= required
}

fn has_numeric_signal(text: &str) -> bool {
    text.chars().any(|c| {
        c.is_ascii_digit() || ('０'..='９').contains(&c) || matches!(c, '〇' | '零')
    })
}

fn looks_like_missing_page(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("404")
        || lower.contains("not found")
        || lower.contains("page not found")
        || text.contains("页面不存在")
        || text.contains("网页不存在")
        || text.contains("无法访问")
}

fn first_open_task(tasks: &[ResearchTask]) -> Option<usize> {
    tasks
        .iter()
        .position(|task| task.status != ResearchTaskStatus::Verified)
}

fn best_task_for_text(tasks: &[ResearchTask], text: &str) -> Option<usize> {
    tasks
        .iter()
        .enumerate()
        .filter(|(_, task)| task.status != ResearchTaskStatus::Verified)
        .max_by_key(|(_, task)| {
            task.query_terms
                .iter()
                .filter(|term| text.contains(term.as_str()))
                .count()
        })
        .map(|(idx, _)| idx)
        .or_else(|| first_open_task(tasks))
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
            max_search: 5,
            max_extract: 5,
            max_browser: 2,
            max_total: 8,
            fallback_search: 5,
            ..Default::default()
        }
    }

    #[test]
    fn decomposes_multi_clause_tasks_without_domain_keywords() {
        let cfg = test_config();
        let tasks = decompose_research_tasks("某地今天天气怎么样，某项考试学生有多少人", &cfg);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].task_type, ResearchTaskType::RealtimeWeather);
        assert_eq!(tasks[0].time_scope.as_deref(), Some("today"));
        assert_eq!(tasks[0].max_search, 2);
        assert_eq!(tasks[1].task_type, ResearchTaskType::SimpleLookup);
        assert_eq!(tasks[1].max_search, 2);
    }

    #[test]
    fn numeric_gate_rejects_wrong_year_without_business_rules() {
        let cfg = test_config();
        let task = new_research_task(
            0,
            ResearchTaskType::TargetedNumericFact,
            "2026年某地甲项考试人数",
            &cfg,
        );
        let item = SearchItem {
            title: "2025年某地甲项考试人数发布".into(),
            url: "https://example.gov.cn/a".into(),
            text: "2025年某地甲项考试人数为12345人。".into(),
        };
        assert!(!search_item_passes_task(&task, &item, &cfg));
    }

    #[test]
    fn numeric_gate_accepts_requested_year_topic_and_number() {
        let cfg = test_config();
        let task = new_research_task(
            0,
            ResearchTaskType::TargetedNumericFact,
            "2026年某地甲项考试人数",
            &cfg,
        );
        let item = SearchItem {
            title: "2026年某地甲项考试人数发布".into(),
            url: "https://example.gov.cn/a".into(),
            text: "2026年某地甲项考试人数为12345人，来源为主管部门公告。".into(),
        };
        assert!(search_item_passes_task(&task, &item, &cfg));
    }

    #[test]
    fn task_policy_rejects_extract_url_not_from_search_results() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("2026年某地甲项考试人数", &cfg);
        ctrl.tasks[0]
            .allowed_urls
            .insert("https://example.gov.cn/ok".into());
        let mut calls = vec![ToolCall {
            id: "e1".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://example.gov.cn/fake"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = ctrl.apply_task_policy(&mut calls, 1);
        assert_eq!(blocked.len(), 1);
        assert!(calls.is_empty());
    }

    #[test]
    fn low_signal_search_does_not_consume_successful_evidence_budget() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("2026年某地甲项考试人数", &cfg);
        let calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"2026年某地甲项考试人数"}"#.into(),
            },
            extra_content: None,
        }];
        let results = vec![ToolResult::ok(
            "s1",
            r#"{"data":{"web":[{"title":"2025年某地甲项考试人数","url":"https://example.gov.cn/a","description":"2025年某地甲项考试人数为12345人。"}]}}"#,
        )];
        ctrl.record_results(&calls, &results);
        assert_eq!(ctrl.evidence.successful_searches, 0);
        assert_eq!(ctrl.budget_state.search_used, 0);
    }

    #[test]
    fn source_directed_low_signal_keeps_general_search_budget() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("2026年某地甲项考试人数", &cfg);
        assert!(!ctrl.record_task_search(
            0,
            r#"{"data":{"web":[{"title":"2025年某地甲项考试人数","url":"https://example.com/a","description":"2025年某地甲项考试人数为12345人。"}]}}"#,
            "site:example.com 2026年某地甲项考试人数",
        ));
        assert_eq!(ctrl.tasks[0].source_directed_attempts, 1);
        assert_eq!(ctrl.tasks[0].search_attempts, 0);
    }

    #[test]
    fn parse_plan_clamps_oversized_budgets() {
        let cfg = test_config();
        let raw = r#"{"need_web":true,"search_budget":99,"extract_budget":99,"browser_budget":99,"total_budget":99}"#;
        let plan = parse_plan_json(raw, &cfg).unwrap();
        assert_eq!(plan.search_budget, 5);
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
                decomposer_json: None,
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
        ctrl.budget_state.search_used = 1;
        ctrl.budget_state.billable_total = 1;

        let mut calls2 = vec![mk("2")];
        let (blocked2, _) = ctrl.gate_web_batch(None, &[], &mut calls2, 2).await;
        assert_eq!(blocked2.len(), 1);
        assert!(calls2.is_empty());
    }

    #[tokio::test]
    async fn fake_planner_budget_four_allows_multiple_searches() {
        let cfg = test_config();
        let plan_json = r#"{"need_web":true,"search_budget":4,"extract_budget":0,"browser_budget":0,"total_budget":4}"#;
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(plan_json, &cfg);
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
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
            ctrl.budget_state.attempted_search =
                ctrl.budget_state.attempted_search.saturating_add(1);
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
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
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

    #[tokio::test]
    async fn fallback_allows_third_search_after_two_failures() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = Some(fallback_plan(&cfg, &ctrl.tasks, ctrl.user_requested_browser));
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
        ctrl.planner_invoked = true;
        ctrl.budget_state.attempted_search = 2;
        ctrl.budget_state.search_failed = 2;
        let mut calls = vec![ToolCall {
            id: "s3".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"new query"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, notices) = ctrl.gate_web_batch(None, &[], &mut calls, 3).await;
        assert!(blocked.is_empty());
        assert!(notices.is_empty());
        assert_eq!(calls.len(), 1);
    }

    #[tokio::test]
    async fn no_evidence_quota_block_has_no_exhausted_notice() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg);
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(0, 0, 0, Some(1), 2);
        ctrl.planner_invoked = true;
        let mut calls = vec![ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"x"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, notices) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert_eq!(blocked.len(), 1);
        assert!(notices.is_empty());
    }

    #[tokio::test]
    async fn search_quota_block_does_not_notify_when_other_web_pools_remain() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg);
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(2, 5, 2, Some(8), 2);
        ctrl.planner_invoked = true;
        ctrl.budget_state.attempted_search = 2;
        ctrl.budget_state.search_used = 2;
        ctrl.budget_state.billable_total = 2;
        let mut calls: Vec<ToolCall> = (0..2)
            .map(|i| ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"retry{i}"}}"#),
                },
                extra_content: None,
            })
            .collect();
        let (blocked, notices) = ctrl.gate_web_batch(None, &[], &mut calls, 2).await;
        assert_eq!(blocked.len(), 2);
        assert!(calls.is_empty());
        assert!(notices.is_empty());
        assert!(!ctrl.force_finalize);
    }

    #[tokio::test]
    async fn batch_five_searches_budget_four_no_notice_when_four_run() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":4,"extract_budget":0,"browser_budget":0,"total_budget":4}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
        ctrl.planner_invoked = true;
        let mut calls: Vec<ToolCall> = (0..5)
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
        assert_eq!(calls.len(), 4);
        assert_eq!(blocked.len(), 1);
        assert!(notices.is_empty());
    }

    #[test]
    fn snippet_first_does_not_escalate_browser_after_extract_failure() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":1,"browser_budget":0,"total_budget":3}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
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
        assert!(!ctrl.record_results(&calls, &results));
        assert_eq!(ctrl.limits.browser_max, 0);
        assert!(
            !ctrl
                .filter_tool_schemas(&[ToolSchema::new(
                    "browser_navigate",
                    "",
                    JsonSchema::new("object")
                )])
                .iter()
                .any(|s| s.name == "browser_navigate")
        );
    }

    #[test]
    fn escalate_browser_skipped_when_search_succeeds() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.plan = parse_plan_json(
            r#"{"need_web":true,"search_budget":2,"extract_budget":1,"browser_budget":0,"total_budget":3}"#,
            &cfg,
        );
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
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
        ctrl.limits = limits_for_config(
            &cfg,
            ctrl.plan.as_ref(),
            &ctrl.tasks,
            ctrl.user_requested_browser,
        );
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

    #[test]
    fn fallback_plan_uses_task_sum() {
        let cfg = test_config();
        let tasks = decompose_research_tasks("某地今天天气怎么样，某项考试学生有多少人", &cfg);
        let plan = fallback_plan(&cfg, &tasks, false);
        assert!(plan.search_budget >= 2);
        let limits = limits_for_config(&cfg, Some(&plan), &tasks, false);
        assert!(limits.search_max >= 6);
    }

    #[test]
    fn infer_site_query_routes_by_focus_overlap() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("深圳天气 深圳高考人数", &cfg);
        let tc = ToolCall {
            id: "s1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"site:sz.gov.cn 深圳高考人数"}"#.into(),
            },
            extra_content: None,
        };
        let idx = ctrl.infer_task_for_call(&tc).unwrap();
        assert!(
            ctrl.tasks[idx].focus_text.contains("高考") || ctrl.tasks[idx].focus_text.contains("人数")
        );
    }

    #[tokio::test]
    async fn dual_task_parallel_search_not_global_blocked() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("深圳天气 深圳高考人数", &cfg);
        ctrl.plan = Some(fallback_plan(&cfg, &ctrl.tasks, ctrl.user_requested_browser));
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(2, 5, 2, Some(2), 2);
        ctrl.planner_invoked = true;

        let mut calls: Vec<ToolCall> = vec![
            ToolCall {
                id: "w1".into(),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: r#"{"query":"深圳今天天气"}"#.into(),
                },
                extra_content: None,
            },
            ToolCall {
                id: "n1".into(),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: r#"{"query":"深圳高考报名人数"}"#.into(),
                },
                extra_content: None,
            },
        ];
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 1).await;
        assert!(blocked.is_empty(), "dual parallel search should pass task policy");
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn dual_task_second_round_search_after_first() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("深圳天气 深圳高考人数", &cfg);
        ctrl.plan = Some(fallback_plan(&cfg, &ctrl.tasks, ctrl.user_requested_browser));
        ctrl.limits = WebToolBudgetLimits::from_dynamic_pools(2, 5, 2, Some(2), 2);
        ctrl.planner_invoked = true;
        ctrl.tasks[0].search_attempts = 1;
        ctrl.tasks[1].search_attempts = 1;

        let mut calls = vec![ToolCall {
            id: "n2".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"site:sz.gov.cn 深圳高考人数"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 2).await;
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);
        assert!(!ctrl.web_stopped);
    }

    #[tokio::test]
    async fn evaluator_stop_while_task_pending_does_not_block() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::with_test_llm(
            cfg.clone(),
            WebResearchTestLlm {
                planner_json: None,
                evaluator_json: Some(
                    r#"{"continue_web":false,"sufficient_answer":true,"final_answer_instruction":"answer now"}"#
                        .to_string(),
                ),
                decomposer_json: None,
            },
        );
        ctrl.tasks = decompose_research_tasks("深圳天气 深圳高考人数", &cfg);
        ctrl.planner_invoked = true;
        ctrl.evidence.successful_searches = 1;
        ctrl.tasks[0].status = ResearchTaskStatus::Verified;

        let mut calls = vec![ToolCall {
            id: "n1".into(),
            function: FunctionCall {
                name: "web_search".into(),
                arguments: r#"{"query":"深圳高考报名人数"}"#.into(),
            },
            extra_content: None,
        }];
        let (blocked, _) = ctrl.gate_web_batch(None, &[], &mut calls, 2).await;
        assert!(blocked.is_empty());
        assert_eq!(calls.len(), 1);
        assert!(!ctrl.force_finalize());
        assert!(!ctrl.web_stopped);
    }

    #[tokio::test]
    async fn evaluator_stop_when_all_tasks_terminal() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::with_test_llm(
            cfg.clone(),
            WebResearchTestLlm {
                planner_json: None,
                evaluator_json: Some(
                    r#"{"continue_web":false,"sufficient_answer":true,"final_answer_instruction":"answer now"}"#
                        .to_string(),
                ),
                decomposer_json: None,
            },
        );
        ctrl.tasks = decompose_research_tasks("深圳天气 深圳高考人数", &cfg);
        ctrl.planner_invoked = true;
        ctrl.evidence.successful_searches = 2;
        ctrl.tasks[0].status = ResearchTaskStatus::Verified;
        ctrl.tasks[1].status = ResearchTaskStatus::Verified;

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
                decomposer_json: None,
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

    #[test]
    fn weather_snippet_with_date_not_jintian_passes() {
        let cfg = test_config();
        let task = new_research_task(
            0,
            ResearchTaskType::RealtimeWeather,
            "深圳今天天气怎么样",
            &cfg,
        );
        let dt = hermes_core::now();
        let snippet = format!(
            "深圳天气预报 {}月{}日 多云 气温28℃ 湿度75%",
            dt.month(),
            dt.day()
        );
        assert!(text_passes_task(&task, &snippet, &cfg));
    }

    #[test]
    fn weather_snippet_with_location_and_condition_passes_without_jintian() {
        let cfg = test_config();
        let task = new_research_task(
            0,
            ResearchTaskType::RealtimeWeather,
            "深圳今天天气怎么样",
            &cfg,
        );
        let dt = hermes_core::now();
        let snippet = format!(
            "深圳 {}月{}日 天气预报 多云 气温26到33℃ 有阵雨 东南风3级",
            dt.month(),
            dt.day()
        );
        assert!(text_passes_task(&task, &snippet, &cfg));
    }

    #[test]
    fn segment_decompose_uses_clause_focus_text() {
        let cfg = test_config();
        let msg = "你好，深圳今天天气怎么样，深圳高考学生有多少人呢";
        let tasks = decompose_research_tasks(msg, &cfg);
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].focus_text.contains("天气"));
        assert!(tasks[1].focus_text.contains("高考") || tasks[1].focus_text.contains("学生"));
    }

    #[test]
    fn space_separated_message_decomposes_without_commas() {
        let cfg = test_config();
        let msg = "你好 深圳今天天气怎么样了 深圳高考学生有多少呢";
        let tasks = decompose_research_tasks(msg, &cfg);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].task_type, ResearchTaskType::RealtimeWeather);
        assert_eq!(tasks[0].time_scope.as_deref(), Some("today"));
        assert_eq!(tasks[1].task_type, ResearchTaskType::SimpleLookup);
        assert!(tasks[0].focus_text.contains("天气"));
        assert!(!tasks[0].focus_text.contains("高考"));
        assert!(tasks[1].focus_text.contains("高考") || tasks[1].focus_text.contains("学生"));
    }

    #[test]
    fn infer_task_type_uses_digits_and_today_scope_only() {
        assert_eq!(
            infer_task_type_from_focus("深圳今天天气怎么样"),
            ResearchTaskType::RealtimeWeather
        );
        assert_eq!(
            infer_task_type_from_focus("2026年某地甲项考试人数"),
            ResearchTaskType::TargetedNumericFact
        );
        assert_eq!(
            infer_task_type_from_focus("深圳高考学生有多少人"),
            ResearchTaskType::SimpleLookup
        );
    }

    #[test]
    fn weak_numeric_signal_rejected() {
        assert!(!has_numeric_signal("参考往年约有众多考生报名"));
    }

    #[test]
    fn user_browser_request_detection() {
        assert!(!user_explicitly_requested_browser("深圳今天天气怎么样"));
        assert!(user_explicitly_requested_browser("请用 browser_navigate 打开这个页面"));
        assert!(user_explicitly_requested_browser("用浏览器打开官网"));
    }

    #[test]
    fn browser_disabled_until_user_requests() {
        let cfg = test_config();
        let ctrl = WebResearchController::with_user_message(cfg, "深圳今天天气怎么样");
        assert!(!ctrl.user_requested_browser);
        assert_eq!(ctrl.limits.browser_max, 0);
        let schemas = vec![
            ToolSchema::new("web_search", "", JsonSchema::new("object")),
            ToolSchema::new("browser_navigate", "", JsonSchema::new("object")),
        ];
        assert!(!ctrl.filter_tool_schemas(&schemas).iter().any(|s| s.name == "browser_navigate"));

        let ctrl = WebResearchController::with_user_message(test_config(), "请 browser_navigate 打开");
        assert!(ctrl.user_requested_browser);
        assert!(ctrl.limits.browser_max > 0);
        assert!(ctrl.filter_tool_schemas(&schemas).iter().any(|s| s.name == "browser_navigate"));
    }

    #[test]
    fn search_snippet_first_blocks_extract_for_weather_not_numeric() {
        let cfg = test_config();
        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("深圳今天天气怎么样", &cfg);
        ctrl.tasks[0].search_attempts = 1;
        ctrl.tasks[0]
            .allowed_urls
            .insert("https://weather.sz.gov.cn/".into());
        let mut weather_calls = vec![ToolCall {
            id: "e1".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://weather.sz.gov.cn/"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = ctrl.apply_task_policy(&mut weather_calls, 2);
        assert_eq!(blocked.len(), 1);
        assert!(weather_calls.is_empty());

        let mut ctrl = WebResearchController::new(cfg.clone());
        ctrl.tasks = decompose_research_tasks("2026年深圳高考报名人数", &cfg);
        ctrl.tasks[0].search_attempts = 1;
        ctrl.tasks[0]
            .allowed_urls
            .insert("https://www.sznews.com/news/content/2026-06/07/content_32081114.htm".into());
        let mut numeric_calls = vec![ToolCall {
            id: "e2".into(),
            function: FunctionCall {
                name: "web_extract".into(),
                arguments: r#"{"url":"https://www.sznews.com/news/content/2026-06/07/content_32081114.htm"}"#.into(),
            },
            extra_content: None,
        }];
        let blocked = ctrl.apply_task_policy(&mut numeric_calls, 2);
        assert!(blocked.is_empty());
        assert_eq!(numeric_calls.len(), 1);
    }

    #[test]
    fn parallel_cap_trims_excess_web_calls() {
        let mut calls: Vec<ToolCall> = (0..5)
            .map(|i| ToolCall {
                id: format!("s{i}"),
                function: FunctionCall {
                    name: "web_search".into(),
                    arguments: format!(r#"{{"query":"q{i}"}}"#),
                },
                extra_content: None,
            })
            .collect();
        trim_parallel_web_calls(&mut calls, 3);
        assert_eq!(calls.len(), 3);
    }

    #[test]
    fn parse_decomposer_json_builds_tasks() {
        let cfg = test_config();
        let raw = r#"{"tasks":[{"task_type":"realtime_weather","focus":"深圳今天天气"},{"task_type":"targeted_numeric_fact","focus":"深圳高考报名人数"}]}"#;
        let tasks = parse_decomposer_json(raw, "fallback", &cfg).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].task_type, ResearchTaskType::RealtimeWeather);
        assert_eq!(tasks[1].task_type, ResearchTaskType::TargetedNumericFact);
    }
}
