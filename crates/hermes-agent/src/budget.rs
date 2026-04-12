//! Budget enforcement for tool results.
//!
//! Ensures that tool output doesn't overwhelm the context window by
//! truncating individual results and enforcing aggregate size limits.

use hermes_core::{BudgetConfig, ToolResult};

/// Truncate a single result string to `max_chars`.
///
/// If the content exceeds `max_chars`, it is truncated and a sentinel
/// message is appended indicating how many characters were removed.
pub fn truncate_result(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let truncated = &content[..max_chars];
    let removed = content.len() - max_chars;
    format!(
        "{truncated}\n\n[... truncated {removed} characters ...]"
    )
}

/// Enforce budget on a batch of tool results.
///
/// - Each result is individually truncated to `budget.max_result_size_chars`.
/// - If the aggregate size exceeds `budget.max_aggregate_chars`, results are
///   further truncated proportionally until the total fits.
pub fn enforce_budget(results: &mut [ToolResult], budget: &BudgetConfig) {
    // Step 1: Per-result truncation
    for result in results.iter_mut() {
        result.content = truncate_result(&result.content, budget.max_result_size_chars);
    }

    // Step 2: Aggregate truncation
    let total_chars: usize = results.iter().map(|r| r.content.len()).sum();
    if total_chars <= budget.max_aggregate_chars {
        return;
    }

    let over_budget = total_chars - budget.max_aggregate_chars;
    let ratio = budget.max_aggregate_chars as f64 / total_chars as f64;

    for result in results.iter_mut() {
        let target_len = ((result.content.len() as f64) * ratio) as usize;
        result.content = truncate_result(&result.content, target_len.max(200));
    }
}

/// Check whether the aggregate size of tool results is within budget.
///
/// Returns `true` if the total character count of all results does not
/// exceed `max_aggregate`.
pub fn check_aggregate_budget(results: &[ToolResult], max_aggregate: usize) -> bool {
    let total: usize = results.iter().map(|r| r.content.len()).sum();
    total <= max_aggregate
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_result_under_limit() {
        let content = "hello world";
        let truncated = truncate_result(content, 100);
        assert_eq!(truncated, "hello world");
    }

    #[test]
    fn test_truncate_result_over_limit() {
        let content = "a".repeat(200);
        let truncated = truncate_result(&content, 100);
        assert!(truncated.len() < 300); // original + suffix
        assert!(truncated.contains("truncated"));
        assert!(truncated.starts_with(&"a".repeat(100)));
    }

    #[test]
    fn test_enforce_budget_per_result() {
        let mut results = vec![
            ToolResult::ok("1", "x".repeat(500)),
            ToolResult::ok("2", "y".repeat(200)),
        ];
        let budget = BudgetConfig {
            max_result_size_chars: 300,
            max_aggregate_chars: 1_000_000,
        };
        enforce_budget(&mut results, &budget);
        assert!(results[0].content.len() <= 350);
        assert_eq!(results[1].content.len(), 200);
    }

    #[test]
    fn test_check_aggregate_budget_pass() {
        let results = vec![
            ToolResult::ok("1", "hello"),
            ToolResult::ok("2", "world"),
        ];
        assert!(check_aggregate_budget(&results, 100));
    }

    #[test]
    fn test_check_aggregate_budget_fail() {
        let results = vec![
            ToolResult::ok("1", "x".repeat(500)),
        ];
        assert!(!check_aggregate_budget(&results, 100));
    }
}