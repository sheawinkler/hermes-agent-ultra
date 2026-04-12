//! Self-evolution policy engine.
//!
//! Provides a bounded adaptive layer for:
//! - L1: model/tool/retry strategy tuning (short cycle)
//! - L2: long-task execution planning (parallel/split/checkpoint)
//! - L3: prompt and memory shaping (mid cycle)

use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionConfig {
    pub enabled_l1: bool,
    pub enabled_l2: bool,
    pub enabled_l3: bool,
    pub exploration_ratio: f64,
    pub max_prompt_prefix_chars: usize,
    pub max_memory_chars: usize,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            enabled_l1: true,
            enabled_l2: true,
            enabled_l3: true,
            exploration_ratio: 0.15,
            max_prompt_prefix_chars: 512,
            max_memory_chars: 4_000,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutcomeSignals {
    pub success: bool,
    pub latency_ms: u64,
    pub cost_usd: f64,
    pub errors: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelStats {
    pub attempts: u64,
    pub successes: u64,
    pub total_latency_ms: u64,
    pub total_cost_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolStats {
    pub calls: u64,
    pub successes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LongTaskPlan {
    pub parallelism: u32,
    pub split_subtasks: u32,
    pub checkpoint_interval_turns: u32,
    pub retry_boost: u32,
}

impl Default for LongTaskPlan {
    fn default() -> Self {
        Self {
            parallelism: 1,
            split_subtasks: 1,
            checkpoint_interval_turns: 3,
            retry_boost: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptivePolicyEngine {
    pub config: EvolutionConfig,
    pub model_stats: HashMap<String, ModelStats>,
    pub tool_stats: HashMap<String, ToolStats>,
    pub memory_provider_weights: HashMap<String, f64>,
}

impl Default for AdaptivePolicyEngine {
    fn default() -> Self {
        Self {
            config: EvolutionConfig::default(),
            model_stats: HashMap::new(),
            tool_stats: HashMap::new(),
            memory_provider_weights: HashMap::new(),
        }
    }
}

impl AdaptivePolicyEngine {
    pub fn record_model_outcome(&mut self, model: &str, outcome: &OutcomeSignals) {
        let stat = self.model_stats.entry(model.to_string()).or_default();
        stat.attempts += 1;
        if outcome.success {
            stat.successes += 1;
        }
        stat.total_latency_ms = stat.total_latency_ms.saturating_add(outcome.latency_ms);
        stat.total_cost_usd += outcome.cost_usd.max(0.0);
    }

    pub fn record_tool_outcome(&mut self, tool: &str, success: bool) {
        let stat = self.tool_stats.entry(tool.to_string()).or_default();
        stat.calls += 1;
        if success {
            stat.successes += 1;
        }
    }

    pub fn set_memory_weight(&mut self, provider: &str, weight: f64) {
        self.memory_provider_weights
            .insert(provider.to_string(), weight.clamp(0.1, 5.0));
    }

    pub fn recommend_model_for_text(&self, text: &str) -> Option<String> {
        if !self.config.enabled_l1 {
            return None;
        }
        if let Some(model) = self.recommend_model_via_bandit(text) {
            return Some(model);
        }
        let lower = text.to_lowercase();
        if lower.contains("analyze") || lower.contains("design") || lower.contains("refactor") {
            return Some("openai:gpt-4o".to_string());
        }
        if lower.contains("quick") || lower.contains("summary") || lower.contains("simple") {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
    }

    pub fn recommend_model_via_bandit(&self, text: &str) -> Option<String> {
        if self.model_stats.is_empty() {
            return None;
        }
        let mut ranked: Vec<(String, f64)> = self
            .model_stats
            .iter()
            .map(|(model, stat)| {
                let attempts = stat.attempts.max(1) as f64;
                let success_rate = stat.successes as f64 / attempts;
                let avg_latency_s = (stat.total_latency_ms as f64 / attempts) / 1000.0;
                let avg_cost = stat.total_cost_usd / attempts;
                let score = success_rate - (0.08 * avg_latency_s) - (16.0 * avg_cost);
                (model.clone(), score)
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let best = ranked.first().map(|(m, _)| m.clone())?;
        if ranked.len() == 1 {
            return Some(best);
        }
        if should_explore(text, self.config.exploration_ratio) {
            return ranked.get(1).map(|(m, _)| m.clone()).or(Some(best));
        }
        Some(best)
    }

    pub fn recommend_retry(
        &self,
        base_retries: u32,
        base_delay_ms: u64,
        is_long_task: bool,
    ) -> (u32, u64) {
        if !self.config.enabled_l1 {
            return (base_retries, base_delay_ms);
        }
        if is_long_task {
            return (
                base_retries.saturating_add(2).min(8),
                base_delay_ms.saturating_add(500),
            );
        }
        (base_retries, base_delay_ms)
    }

    pub fn recommend_long_task_plan(&self, prompt: &str) -> LongTaskPlan {
        if !self.config.enabled_l2 {
            return LongTaskPlan::default();
        }
        let mut plan = LongTaskPlan::default();
        let lower = prompt.to_lowercase();
        let is_complex = prompt.len() > 1200
            || lower.contains("multi")
            || lower.contains("parallel")
            || lower.contains("pipeline")
            || lower.contains("step");
        if is_complex {
            plan.parallelism = 2;
            plan.split_subtasks = 3;
            plan.checkpoint_interval_turns = 2;
            plan.retry_boost = 1;
        }
        plan
    }

    pub fn optimize_prompt_template(&self, base_prompt: &str, task_hint: &str) -> String {
        if !self.config.enabled_l3 {
            return base_prompt.to_string();
        }
        let prefix = format!(
            "[adaptive_prompt]\nFocus on correctness, explicit milestones, and bounded tool use.\nTask hint: {}\n",
            task_hint
        );
        let clipped_prefix: String = prefix
            .chars()
            .take(self.config.max_prompt_prefix_chars)
            .collect();
        format!("{}\n{}", clipped_prefix, base_prompt)
    }

    pub fn optimize_memory_context(&self, memory_context: &str) -> String {
        if !self.config.enabled_l3 {
            return memory_context.to_string();
        }
        memory_context
            .chars()
            .take(self.config.max_memory_chars)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardGateConfig {
    pub max_error_rate: f64,
    pub max_avg_latency_ms: u64,
    pub max_avg_cost_usd: f64,
}

impl Default for HardGateConfig {
    fn default() -> Self {
        Self {
            max_error_rate: 0.25,
            max_avg_latency_ms: 15_000,
            max_avg_cost_usd: 0.08,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyVersion {
    pub version: String,
    pub created_at_epoch_secs: u64,
    pub rollout_ratio: f64,
    pub engine: AdaptivePolicyEngine,
}

impl PolicyVersion {
    pub fn from_engine(engine: AdaptivePolicyEngine, rollout_ratio: f64) -> Self {
        let now = now_epoch_secs();
        let version = next_version_tag();
        Self {
            version,
            created_at_epoch_secs: now,
            rollout_ratio: rollout_ratio.clamp(0.0, 1.0),
            engine,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyStore {
    pub active: PolicyVersion,
    pub stable: PolicyVersion,
    #[serde(default)]
    pub candidate: Option<PolicyVersion>,
    #[serde(default)]
    pub history: Vec<PolicyVersion>,
    #[serde(default)]
    pub hard_gate: HardGateConfig,
    #[serde(default)]
    pub audit_log: Vec<PolicyAuditEvent>,
}

impl PolicyStore {
    pub fn new(initial_engine: AdaptivePolicyEngine) -> Self {
        let initial = PolicyVersion::from_engine(initial_engine, 1.0);
        Self {
            active: initial.clone(),
            stable: initial,
            candidate: None,
            history: Vec::new(),
            hard_gate: HardGateConfig::default(),
            audit_log: Vec::new(),
        }
    }

    pub fn active_engine(&self) -> &AdaptivePolicyEngine {
        &self.active.engine
    }

    pub fn active_engine_mut(&mut self) -> &mut AdaptivePolicyEngine {
        &mut self.active.engine
    }

    pub fn promote_candidate(
        &mut self,
        candidate: AdaptivePolicyEngine,
        rollout_ratio: f64,
    ) -> String {
        let previous = self.active.clone();
        self.history.push(previous);
        self.active = PolicyVersion::from_engine(candidate, rollout_ratio);
        self.candidate = Some(self.active.clone());
        self.active.version.clone()
    }

    pub fn mark_active_stable(&mut self) {
        self.stable = self.active.clone();
        self.candidate = None;
    }

    pub fn evaluate_hard_gate(&self) -> Option<String> {
        let mut attempts_total = 0_u64;
        let mut failures_total = 0_u64;
        let mut latency_total = 0_u64;
        let mut cost_total = 0.0_f64;

        for stat in self.active.engine.model_stats.values() {
            attempts_total += stat.attempts;
            failures_total += stat.attempts.saturating_sub(stat.successes);
            latency_total = latency_total.saturating_add(stat.total_latency_ms);
            cost_total += stat.total_cost_usd;
        }
        if attempts_total == 0 {
            return None;
        }

        let attempts = attempts_total as f64;
        let error_rate = failures_total as f64 / attempts;
        if error_rate > self.hard_gate.max_error_rate {
            return Some(format!(
                "error rate {:.2}% > {:.2}%",
                error_rate * 100.0,
                self.hard_gate.max_error_rate * 100.0
            ));
        }

        let avg_latency = latency_total / attempts_total.max(1);
        if avg_latency > self.hard_gate.max_avg_latency_ms {
            return Some(format!(
                "avg latency {}ms > {}ms",
                avg_latency, self.hard_gate.max_avg_latency_ms
            ));
        }

        let avg_cost = cost_total / attempts;
        if avg_cost > self.hard_gate.max_avg_cost_usd {
            return Some(format!(
                "avg cost ${:.5} > ${:.5}",
                avg_cost, self.hard_gate.max_avg_cost_usd
            ));
        }
        None
    }

    pub fn rollback_if_needed(&mut self) -> Option<String> {
        let reason = self.evaluate_hard_gate()?;
        if self.active.version != self.stable.version {
            self.history.push(self.active.clone());
            self.audit_log.push(PolicyAuditEvent {
                at_epoch_secs: now_epoch_secs(),
                actor: "system".to_string(),
                action: "rollback".to_string(),
                reason: reason.clone(),
                from_version: self.active.version.clone(),
                to_version: self.stable.version.clone(),
                rollout_ratio: self.active.rollout_ratio,
                delta_summary: "hard_gate_violation".to_string(),
            });
            self.active = self.stable.clone();
            self.candidate = None;
            return Some(format!(
                "hard gate triggered ({reason}), rolled back to {}",
                self.active.version
            ));
        }
        Some(format!(
            "hard gate triggered ({reason}), already on stable {}",
            self.active.version
        ))
    }

    pub fn save_to_path(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&raw).map_err(|e| e.to_string())
    }

    pub fn save_audit_log(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.audit_log).map_err(|e| e.to_string())?;
        fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn apply_update<U: PolicyUpdater>(
        &mut self,
        updater: &U,
        req: PolicyUpdateRequest,
    ) -> Result<String, String> {
        let base = self.active.engine.clone();
        let next = updater.derive_candidate(&base)?;
        let from_version = self.active.version.clone();
        let new_version = self.promote_candidate(next, req.rollout_ratio);
        self.audit_log.push(PolicyAuditEvent {
            at_epoch_secs: now_epoch_secs(),
            actor: req.actor,
            action: "promote_candidate".to_string(),
            reason: req.reason,
            from_version,
            to_version: new_version.clone(),
            rollout_ratio: req.rollout_ratio,
            delta_summary: updater.name().to_string(),
        });
        Ok(new_version)
    }

    pub fn promote_active_to_stable(&mut self, actor: &str, reason: &str) -> Option<String> {
        if self.active.version == self.stable.version {
            return None;
        }
        let from = self.stable.version.clone();
        let to = self.active.version.clone();
        self.mark_active_stable();
        self.audit_log.push(PolicyAuditEvent {
            at_epoch_secs: now_epoch_secs(),
            actor: actor.to_string(),
            action: "promote_stable".to_string(),
            reason: reason.to_string(),
            from_version: from,
            to_version: to.clone(),
            rollout_ratio: 1.0,
            delta_summary: "candidate_graduated".to_string(),
        });
        Some(to)
    }

    pub fn auto_promote_if_healthy(&mut self, min_attempts: u64) -> Option<String> {
        if self.active.version == self.stable.version {
            return None;
        }
        if self.evaluate_hard_gate().is_some() {
            return None;
        }
        let attempts = self
            .active
            .engine
            .model_stats
            .values()
            .map(|s| s.attempts)
            .sum::<u64>();
        if attempts < min_attempts {
            return None;
        }
        self.promote_active_to_stable("system", "healthy_canary_threshold_reached")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyAuditEvent {
    pub at_epoch_secs: u64,
    pub actor: String,
    pub action: String,
    pub reason: String,
    pub from_version: String,
    pub to_version: String,
    pub rollout_ratio: f64,
    pub delta_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyUpdateRequest {
    pub actor: String,
    pub reason: String,
    pub rollout_ratio: f64,
}

pub trait PolicyUpdater {
    fn name(&self) -> &'static str;
    fn derive_candidate(
        &self,
        current: &AdaptivePolicyEngine,
    ) -> Result<AdaptivePolicyEngine, String>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutcomeDrivenUpdater {
    pub exploration_delta: f64,
    pub max_prompt_prefix_delta: i32,
    pub max_memory_chars_delta: i32,
}

impl Default for OutcomeDrivenUpdater {
    fn default() -> Self {
        Self {
            exploration_delta: 0.02,
            max_prompt_prefix_delta: 64,
            max_memory_chars_delta: 256,
        }
    }
}

impl PolicyUpdater for OutcomeDrivenUpdater {
    fn name(&self) -> &'static str {
        "outcome_driven_updater"
    }

    fn derive_candidate(
        &self,
        current: &AdaptivePolicyEngine,
    ) -> Result<AdaptivePolicyEngine, String> {
        let mut next = current.clone();
        next.config.exploration_ratio =
            (next.config.exploration_ratio + self.exploration_delta).clamp(0.01, 0.45);
        let next_prefix =
            next.config.max_prompt_prefix_chars as i64 + self.max_prompt_prefix_delta as i64;
        let next_memory = next.config.max_memory_chars as i64 + self.max_memory_chars_delta as i64;
        next.config.max_prompt_prefix_chars = next_prefix.clamp(128, 4096) as usize;
        next.config.max_memory_chars = next_memory.clamp(512, 24_000) as usize;
        Ok(next)
    }
}

fn now_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Monotonically increasing version counter to avoid collisions within the same millisecond.
static VERSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_version_tag() -> String {
    let secs = now_epoch_secs();
    let seq = VERSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("policy-{}-{}", secs, seq)
}

fn should_explore(seed: &str, ratio: f64) -> bool {
    if ratio <= 0.0 {
        return false;
    }
    if ratio >= 1.0 {
        return true;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    seed.hash(&mut hasher);
    let bucket = (hasher.finish() % 10_000) as f64 / 10_000.0;
    bucket < ratio
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recommend_long_task_plan_for_complex_prompt() {
        let engine = AdaptivePolicyEngine::default();
        let plan = engine.recommend_long_task_plan(
            "Please build a multi-step parallel pipeline and split work.",
        );
        assert!(plan.parallelism >= 2);
        assert!(plan.split_subtasks >= 2);
    }

    #[test]
    fn test_optimize_memory_context_respects_cap() {
        let mut engine = AdaptivePolicyEngine::default();
        engine.config.max_memory_chars = 8;
        let out = engine.optimize_memory_context("1234567890");
        assert_eq!(out, "12345678");
    }

    #[test]
    fn test_bandit_prefers_higher_score_model() {
        let mut engine = AdaptivePolicyEngine::default();
        engine.config.exploration_ratio = 0.0;
        engine.model_stats.insert(
            "openai:gpt-4o".to_string(),
            ModelStats {
                attempts: 100,
                successes: 92,
                total_latency_ms: 180_000,
                total_cost_usd: 0.22,
            },
        );
        engine.model_stats.insert(
            "openai:gpt-4o-mini".to_string(),
            ModelStats {
                attempts: 100,
                successes: 85,
                total_latency_ms: 120_000,
                total_cost_usd: 0.03,
            },
        );
        let selected = engine.recommend_model_via_bandit("deep analysis task");
        assert_eq!(selected.as_deref(), Some("openai:gpt-4o-mini"));
    }

    #[test]
    fn test_policy_store_rollback_when_non_stable_policy_bad() {
        let base = AdaptivePolicyEngine::default();
        let mut store = PolicyStore::new(base.clone());
        let mut candidate = base;
        candidate.model_stats.insert(
            "openai:gpt-4o".to_string(),
            ModelStats {
                attempts: 20,
                successes: 2,
                total_latency_ms: 100_000,
                total_cost_usd: 5.0,
            },
        );
        store.promote_candidate(candidate, 0.2);
        let msg = store.rollback_if_needed();
        assert!(msg.is_some());
        assert_eq!(store.active.version, store.stable.version);
    }

    #[test]
    fn test_apply_update_creates_candidate_and_audit_event() {
        let mut store = PolicyStore::new(AdaptivePolicyEngine::default());
        let req = PolicyUpdateRequest {
            actor: "trainer".to_string(),
            reason: "weekly_online_update".to_string(),
            rollout_ratio: 0.15,
        };
        let version = store
            .apply_update(&OutcomeDrivenUpdater::default(), req)
            .expect("update should succeed");
        assert_eq!(store.active.version, version);
        assert!(store.candidate.is_some());
        assert!(!store.audit_log.is_empty());
        assert_eq!(
            store.audit_log.last().map(|e| e.action.as_str()),
            Some("promote_candidate")
        );
    }

    #[test]
    fn test_auto_promote_if_healthy() {
        let mut store = PolicyStore::new(AdaptivePolicyEngine::default());
        let mut next = store.active.engine.clone();
        next.model_stats.insert(
            "openai:gpt-4o-mini".to_string(),
            ModelStats {
                attempts: 80,
                successes: 79,
                total_latency_ms: 20_000,
                total_cost_usd: 0.01,
            },
        );
        store.promote_candidate(next, 0.2);
        let promoted = store.auto_promote_if_healthy(50);
        assert!(promoted.is_some());
        assert_eq!(store.active.version, store.stable.version);
    }
}
