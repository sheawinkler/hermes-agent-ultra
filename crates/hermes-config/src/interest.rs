//! Local user interest (POI) summarization configuration.

use serde::{Deserialize, Serialize};

/// Controls local topic-of-interest extraction and prompt injection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterestConfig {
    /// Master switch for interest store, prefetch, and session-end ingestion.
    #[serde(default = "default_interest_enabled")]
    pub enabled: bool,

    /// Maximum topics retained after consolidation.
    #[serde(default = "default_interest_max_topics")]
    pub max_topics: u32,

    /// Topics injected into the frozen system prompt at session start.
    #[serde(default = "default_interest_snapshot_top_k")]
    pub snapshot_top_k: u32,

    /// Topics returned from per-turn prefetch.
    #[serde(default = "default_interest_prefetch_top_k")]
    pub prefetch_top_k: u32,

    /// Character budget for the frozen USER INTERESTS block.
    #[serde(default = "default_interest_char_budget_snapshot")]
    pub char_budget_snapshot: usize,

    /// Character budget for prefetch interest lines.
    #[serde(default = "default_interest_char_budget_prefetch")]
    pub char_budget_prefetch: usize,

    /// Extraction mode: `llm` (default, semantic session-end LLM), `hybrid` (LLM + rule supplement), or `rules` (legacy).
    #[serde(default = "default_interest_extract_mode")]
    pub extract_mode: String,

    /// Half-life in days for exponential weight decay.
    #[serde(default = "default_interest_decay_half_life_days")]
    pub decay_half_life_days: f64,

    /// Run LLM topic extraction at session end when mode is `llm` or `hybrid`.
    #[serde(default = "default_interest_llm_on_session_end")]
    pub llm_on_session_end: bool,

    /// Accumulate high-confidence per-turn signals in memory (no DB write until session end).
    #[serde(default = "default_interest_per_turn_buffer")]
    pub per_turn_buffer: bool,

    /// Persist POI to SQLite on every user message (legacy; not recommended).
    #[serde(default = "default_interest_per_turn_persist")]
    pub per_turn_persist: bool,

    /// Evidence hits required to promote `candidate` → `active`.
    #[serde(default = "default_interest_promote_min_evidence")]
    pub promote_min_evidence: u32,

    /// Minimum extractor confidence to insert as `active` immediately (else `candidate`).
    #[serde(default = "default_interest_promote_min_confidence")]
    pub promote_min_confidence: f64,

    /// Minimum user message length (chars) before rule extraction runs.
    #[serde(default = "default_interest_min_turn_chars")]
    pub min_turn_chars: u32,
}

fn default_interest_enabled() -> bool {
    true
}

fn default_interest_max_topics() -> u32 {
    40
}

fn default_interest_snapshot_top_k() -> u32 {
    5
}

fn default_interest_prefetch_top_k() -> u32 {
    3
}

fn default_interest_char_budget_snapshot() -> usize {
    600
}

fn default_interest_char_budget_prefetch() -> usize {
    400
}

fn default_interest_extract_mode() -> String {
    "llm".to_string()
}

fn default_interest_decay_half_life_days() -> f64 {
    30.0
}

fn default_interest_llm_on_session_end() -> bool {
    true
}

fn default_interest_per_turn_buffer() -> bool {
    true
}

fn default_interest_per_turn_persist() -> bool {
    false
}

fn default_interest_promote_min_evidence() -> u32 {
    2
}

fn default_interest_promote_min_confidence() -> f64 {
    0.55
}

fn default_interest_min_turn_chars() -> u32 {
    12
}

impl Default for InterestConfig {
    fn default() -> Self {
        Self {
            enabled: default_interest_enabled(),
            max_topics: default_interest_max_topics(),
            snapshot_top_k: default_interest_snapshot_top_k(),
            prefetch_top_k: default_interest_prefetch_top_k(),
            char_budget_snapshot: default_interest_char_budget_snapshot(),
            char_budget_prefetch: default_interest_char_budget_prefetch(),
            extract_mode: default_interest_extract_mode(),
            decay_half_life_days: default_interest_decay_half_life_days(),
            llm_on_session_end: default_interest_llm_on_session_end(),
            per_turn_buffer: default_interest_per_turn_buffer(),
            per_turn_persist: default_interest_per_turn_persist(),
            promote_min_evidence: default_interest_promote_min_evidence(),
            promote_min_confidence: default_interest_promote_min_confidence(),
            min_turn_chars: default_interest_min_turn_chars(),
        }
    }
}

impl InterestConfig {
    pub fn uses_llm(&self) -> bool {
        matches!(
            self.extract_mode.trim().to_ascii_lowercase().as_str(),
            "llm" | "hybrid"
        )
    }

    pub fn uses_rules(&self) -> bool {
        matches!(
            self.extract_mode.trim().to_ascii_lowercase().as_str(),
            "rules" | "hybrid"
        )
    }

    /// Whether session-end cloud LLM extraction is allowed.
    ///
    /// Requires `extract_mode` of `llm` or `hybrid`, plus either
    /// `llm_on_session_end: true` in config or `HERMES_INTEREST_LLM=1`.
    pub fn session_end_llm_enabled(&self) -> bool {
        if !self.enabled || !self.uses_llm() {
            return false;
        }
        self.llm_on_session_end || crate::managed_gateway::env_var_enabled("HERMES_INTEREST_LLM")
    }

    /// Production default: session-end commit, optional per-turn buffer only.
    pub fn session_end_persist_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_prefer_llm_semantic_extract() {
        let cfg = InterestConfig::default();
        assert!(cfg.enabled);
        assert_eq!(cfg.extract_mode, "llm");
        assert!(cfg.llm_on_session_end);
        assert!(cfg.session_end_llm_enabled());
        assert!(cfg.per_turn_buffer);
        assert!(!cfg.per_turn_persist);
        assert!(!cfg.uses_rules());
        assert!(cfg.uses_llm());
    }

    #[test]
    fn hybrid_enables_rules_supplement_and_llm() {
        let mut cfg = InterestConfig::default();
        cfg.extract_mode = "hybrid".to_string();
        assert!(cfg.uses_rules());
        assert!(cfg.session_end_llm_enabled());
    }

    #[test]
    fn legacy_rules_mode_skips_llm_without_opt_in() {
        let mut cfg = InterestConfig::default();
        cfg.extract_mode = "rules".to_string();
        cfg.llm_on_session_end = false;
        assert!(cfg.uses_rules());
        assert!(!cfg.session_end_llm_enabled());
    }
}
