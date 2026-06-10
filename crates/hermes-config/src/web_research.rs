//! Adaptive web research loop settings (`agent.web_research` in gateway YAML).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebResearchTaskProfile {
    #[serde(default)]
    pub max_search: u32,
    #[serde(default)]
    pub max_extract: u32,
    #[serde(default)]
    pub max_latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebResearchTaskProfiles {
    #[serde(default = "default_realtime_weather_profile")]
    pub realtime_weather: WebResearchTaskProfile,
    #[serde(default = "default_simple_lookup_profile")]
    pub simple_lookup: WebResearchTaskProfile,
    #[serde(default = "default_targeted_numeric_fact_profile")]
    pub targeted_numeric_fact: WebResearchTaskProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebResearchMessageCaps {
    #[serde(default = "default_message_max_total_search")]
    pub max_total_search: u32,
    #[serde(default = "default_message_max_total_extract")]
    pub max_total_extract: u32,
    #[serde(default = "default_message_max_attempt_total")]
    pub max_attempt_total: u32,
    #[serde(default = "default_message_max_latency_ms")]
    pub max_latency_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebSourceClassConfig {
    #[serde(default)]
    pub domain_patterns: Vec<String>,
    #[serde(default)]
    pub weight: i32,
}

/// Runtime caps and planner/evaluator toggles for per-user-message web research.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebResearchConfig {
    #[serde(default = "default_web_research_enabled")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub planner_enabled: bool,
    #[serde(default = "default_true")]
    pub evaluator_enabled: bool,
    #[serde(default = "default_max_search")]
    pub max_search: u32,
    #[serde(default = "default_max_extract")]
    pub max_extract: u32,
    #[serde(default = "default_max_browser")]
    pub max_browser: u32,
    #[serde(default = "default_max_total")]
    pub max_total: u32,
    #[serde(default = "default_fallback_search")]
    pub fallback_search: u32,
    #[serde(default = "default_fallback_extract")]
    pub fallback_extract: u32,
    #[serde(default = "default_fallback_browser")]
    pub fallback_browser: u32,
    #[serde(default = "default_max_consecutive_errors")]
    pub max_consecutive_errors: u32,
    #[serde(default)]
    pub task_profiles: WebResearchTaskProfiles,
    #[serde(default)]
    pub message_caps: WebResearchMessageCaps,
    #[serde(default = "default_source_classes")]
    pub source_classes: HashMap<String, WebSourceClassConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner_prompt_path: Option<String>,
}

fn default_web_research_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_max_search() -> u32 {
    5
}

fn default_max_extract() -> u32 {
    5
}

fn default_max_browser() -> u32 {
    2
}

fn default_max_total() -> u32 {
    8
}

fn default_fallback_search() -> u32 {
    5
}

fn default_fallback_extract() -> u32 {
    5
}

fn default_fallback_browser() -> u32 {
    2
}

fn default_max_consecutive_errors() -> u32 {
    2
}

fn profile(max_search: u32, max_extract: u32, max_latency_ms: u64) -> WebResearchTaskProfile {
    WebResearchTaskProfile {
        max_search,
        max_extract,
        max_latency_ms,
    }
}

fn default_realtime_weather_profile() -> WebResearchTaskProfile {
    profile(2, 1, 8_000)
}

fn default_simple_lookup_profile() -> WebResearchTaskProfile {
    profile(2, 1, 12_000)
}

fn default_targeted_numeric_fact_profile() -> WebResearchTaskProfile {
    profile(6, 3, 25_000)
}

fn default_message_max_total_search() -> u32 {
    10
}

fn default_message_max_total_extract() -> u32 {
    5
}

fn default_message_max_attempt_total() -> u32 {
    16
}

fn default_message_max_latency_ms() -> u64 {
    45_000
}

fn default_source_classes() -> HashMap<String, WebSourceClassConfig> {
    HashMap::new()
}

impl Default for WebResearchTaskProfiles {
    fn default() -> Self {
        Self {
            realtime_weather: default_realtime_weather_profile(),
            simple_lookup: default_simple_lookup_profile(),
            targeted_numeric_fact: default_targeted_numeric_fact_profile(),
        }
    }
}

impl Default for WebResearchMessageCaps {
    fn default() -> Self {
        Self {
            max_total_search: default_message_max_total_search(),
            max_total_extract: default_message_max_total_extract(),
            max_attempt_total: default_message_max_attempt_total(),
            max_latency_ms: default_message_max_latency_ms(),
        }
    }
}

impl Default for WebResearchConfig {
    fn default() -> Self {
        Self {
            enabled: default_web_research_enabled(),
            planner_enabled: default_true(),
            evaluator_enabled: default_true(),
            max_search: default_max_search(),
            max_extract: default_max_extract(),
            max_browser: default_max_browser(),
            max_total: default_max_total(),
            fallback_search: default_fallback_search(),
            fallback_extract: default_fallback_extract(),
            fallback_browser: default_fallback_browser(),
            max_consecutive_errors: default_max_consecutive_errors(),
            task_profiles: WebResearchTaskProfiles::default(),
            message_caps: WebResearchMessageCaps::default(),
            source_classes: default_source_classes(),
            planner_prompt_path: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_research_yaml_deserializes_with_defaults() {
        let cfg: WebResearchConfig = serde_yaml::from_str("enabled: true\n").unwrap();
        assert!(cfg.enabled);
        assert!(cfg.planner_enabled);
        assert_eq!(cfg.max_search, 5);
        assert_eq!(cfg.fallback_search, 5);
        assert_eq!(cfg.task_profiles.targeted_numeric_fact.max_search, 6);
        assert_eq!(cfg.message_caps.max_total_search, 10);
        assert!(cfg.source_classes.is_empty());
    }

    #[test]
    fn web_research_nested_under_agent_block() {
        #[derive(Deserialize)]
        struct Agent {
            #[serde(default)]
            web_research: WebResearchConfig,
        }
        let agent: Agent = serde_yaml::from_str(
            r#"
web_research:
  enabled: false
  max_search: 6
  fallback_search: 1
"#,
        )
        .unwrap();
        assert!(!agent.web_research.enabled);
        assert_eq!(agent.web_research.max_search, 6);
        assert_eq!(agent.web_research.fallback_search, 1);
    }
}
