//! Smart-tier multi-pass verification: plan → critique → revise → final.

#[derive(Debug, Clone)]
pub struct DeepThinkingMode {
    pub enabled: bool,
    pub max_rounds: u32,
}

impl Default for DeepThinkingMode {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rounds: 3,
        }
    }
}

pub fn should_enable_deep_thinking(tier: &str, vertical: Option<&str>) -> DeepThinkingMode {
    let enabled = tier.eq_ignore_ascii_case("smart")
        && vertical.is_some_and(|v| v == "trader" || v == "computer-use");
    DeepThinkingMode {
        enabled,
        max_rounds: if enabled { 3 } else { 1 },
    }
}
