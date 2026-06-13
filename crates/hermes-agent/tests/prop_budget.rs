//! Bounded invariant coverage: budget truncation preserves invariants
//! **Validates: Requirement 3.9**
//!
//! For representative BudgetConfig and tool result lists, after enforce_budget:
//! - Each result's content length <= max_result_size_chars plus truncation suffix
//! - Total content length is bounded by the original content plus suffix overhead

use hermes_agent::budget::enforce_budget;
use hermes_core::{BudgetConfig, ToolResult};

fn budget_cases() -> Vec<BudgetConfig> {
    vec![
        BudgetConfig {
            max_result_size_chars: 100,
            max_aggregate_chars: 500,
        },
        BudgetConfig {
            max_result_size_chars: 100,
            max_aggregate_chars: 50_000,
        },
        BudgetConfig {
            max_result_size_chars: 512,
            max_aggregate_chars: 1_024,
        },
        BudgetConfig {
            max_result_size_chars: 9_999,
            max_aggregate_chars: 500,
        },
        BudgetConfig {
            max_result_size_chars: 10_000,
            max_aggregate_chars: 50_000,
        },
    ]
}

fn tool_result(id: &str, content: String, is_error: bool) -> ToolResult {
    ToolResult {
        tool_call_id: id.to_string(),
        content,
        is_error,
    }
}

fn tool_result_cases() -> Vec<Vec<ToolResult>> {
    vec![
        vec![tool_result("empty", String::new(), false)],
        vec![tool_result("small", "abc".repeat(8), false)],
        vec![tool_result("large", "x".repeat(20_000), false)],
        vec![tool_result("large-error", "error".repeat(4_000), true)],
        vec![
            tool_result("first", "a".repeat(99), false),
            tool_result("second", "b".repeat(100), false),
            tool_result("third", "c".repeat(101), true),
        ],
        vec![
            tool_result("one", "a".repeat(5_000), false),
            tool_result("two", "b".repeat(10_000), false),
            tool_result("three", "x".repeat(20_000), true),
            tool_result("four", "mixed".repeat(1_000), false),
            tool_result("five", "edge".repeat(2_500), true),
        ],
    ]
}

#[test]
fn budget_per_result_invariant() {
    for budget in budget_cases() {
        for mut results in tool_result_cases() {
            enforce_budget(&mut results, &budget);

            for result in &results {
                let max_with_suffix = budget.max_result_size_chars + 100;
                assert!(
                    result.content.len() <= max_with_suffix,
                    "Result '{}' has {} chars, exceeds max {} + suffix",
                    result.tool_call_id,
                    result.content.len(),
                    budget.max_result_size_chars
                );
            }
        }
    }
}

#[test]
fn budget_does_not_grow_content() {
    for budget in budget_cases() {
        for results in tool_result_cases() {
            let original_total: usize = results.iter().map(|r| r.content.len()).sum();
            let mut results = results;
            enforce_budget(&mut results, &budget);
            let after_total: usize = results.iter().map(|r| r.content.len()).sum();
            let max_suffix_overhead = results.len() * 100;

            assert!(
                after_total <= original_total + max_suffix_overhead,
                "Budget enforcement grew content from {} to {} (overhead limit {})",
                original_total,
                after_total,
                max_suffix_overhead
            );
        }
    }
}
