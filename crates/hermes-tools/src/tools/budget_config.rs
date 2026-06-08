//! Configurable budget constants for tool result persistence.
//!
//! Corresponds to `hermes-agent/tools/budget_config.py`.

use std::collections::HashMap;
use std::sync::LazyLock;

pub const DEFAULT_RESULT_SIZE_CHARS: usize = 100_000;
pub const DEFAULT_TURN_BUDGET_CHARS: usize = 200_000;
pub const DEFAULT_PREVIEW_SIZE_CHARS: usize = 1_500;

pub static PINNED_THRESHOLDS: LazyLock<HashMap<&'static str, BudgetThreshold>> =
    LazyLock::new(|| HashMap::from([("read_file", BudgetThreshold::Infinite)]));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetThreshold {
    Chars(usize),
    Infinite,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetConfig {
    pub default_result_size: usize,
    pub turn_budget: usize,
    pub preview_size: usize,
    pub tool_overrides: HashMap<String, usize>,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            default_result_size: DEFAULT_RESULT_SIZE_CHARS,
            turn_budget: DEFAULT_TURN_BUDGET_CHARS,
            preview_size: DEFAULT_PREVIEW_SIZE_CHARS,
            tool_overrides: HashMap::new(),
        }
    }
}

impl BudgetConfig {
    pub fn resolve_threshold(&self, tool_name: &str) -> BudgetThreshold {
        self.resolve_threshold_with_registry(tool_name, |_, default| default)
    }

    pub fn resolve_threshold_with_registry(
        &self,
        tool_name: &str,
        registry_lookup: impl FnOnce(&str, usize) -> usize,
    ) -> BudgetThreshold {
        if let Some(threshold) = PINNED_THRESHOLDS.get(tool_name) {
            return *threshold;
        }
        if let Some(threshold) = self.tool_overrides.get(tool_name) {
            return BudgetThreshold::Chars(*threshold);
        }
        BudgetThreshold::Chars(registry_lookup(tool_name, self.default_result_size))
    }
}

pub fn default_budget() -> BudgetConfig {
    BudgetConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_python() {
        let budget = BudgetConfig::default();
        assert_eq!(budget.default_result_size, 100_000);
        assert_eq!(budget.turn_budget, 200_000);
        assert_eq!(budget.preview_size, 1_500);
        assert!(budget.tool_overrides.is_empty());
    }

    #[test]
    fn read_file_threshold_is_pinned_to_infinite() {
        let mut budget = BudgetConfig::default();
        budget.tool_overrides.insert("read_file".to_string(), 1);

        let threshold = budget.resolve_threshold_with_registry("read_file", |_, _| 2);
        assert_eq!(threshold, BudgetThreshold::Infinite);
    }

    #[test]
    fn tool_override_wins_over_registry_default() {
        let mut budget = BudgetConfig::default();
        budget.tool_overrides.insert("terminal".to_string(), 42);

        let threshold = budget.resolve_threshold_with_registry("terminal", |_, _| 99);
        assert_eq!(threshold, BudgetThreshold::Chars(42));
    }

    #[test]
    fn registry_lookup_wins_over_default_for_unpinned_tools() {
        let budget = BudgetConfig::default();

        let threshold = budget.resolve_threshold_with_registry("web_search", |name, default| {
            assert_eq!(name, "web_search");
            assert_eq!(default, DEFAULT_RESULT_SIZE_CHARS);
            12_345
        });
        assert_eq!(threshold, BudgetThreshold::Chars(12_345));
    }

    #[test]
    fn resolve_threshold_without_registry_returns_default() {
        let budget = BudgetConfig::default();
        assert_eq!(
            budget.resolve_threshold("unknown_tool"),
            BudgetThreshold::Chars(DEFAULT_RESULT_SIZE_CHARS)
        );
    }
}
