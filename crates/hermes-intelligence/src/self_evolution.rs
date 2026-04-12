//! Self-evolution policy engine.
//!
//! Provides a bounded adaptive layer for:
//! - L1: model/tool/retry strategy tuning (short cycle)
//! - L2: long-task execution planning (parallel/split/checkpoint)
//! - L3: prompt and memory shaping (mid cycle)

use std::collections::HashMap;

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
        let lower = text.to_lowercase();
        if lower.contains("analyze") || lower.contains("design") || lower.contains("refactor") {
            return Some("openai:gpt-4o".to_string());
        }
        if lower.contains("quick") || lower.contains("summary") || lower.contains("simple") {
            return Some("openai:gpt-4o-mini".to_string());
        }
        None
    }

    pub fn recommend_retry(&self, base_retries: u32, base_delay_ms: u64, is_long_task: bool) -> (u32, u64) {
        if !self.config.enabled_l1 {
            return (base_retries, base_delay_ms);
        }
        if is_long_task {
            return (base_retries.saturating_add(2).min(8), base_delay_ms.saturating_add(500));
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
        let clipped_prefix: String = prefix.chars().take(self.config.max_prompt_prefix_chars).collect();
        format!("{}\n{}", clipped_prefix, base_prompt)
    }

    pub fn optimize_memory_context(&self, memory_context: &str) -> String {
        if !self.config.enabled_l3 {
            return memory_context.to_string();
        }
        memory_context.chars().take(self.config.max_memory_chars).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recommend_long_task_plan_for_complex_prompt() {
        let engine = AdaptivePolicyEngine::default();
        let plan = engine.recommend_long_task_plan("Please build a multi-step parallel pipeline and split work.");
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
}

