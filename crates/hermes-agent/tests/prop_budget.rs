//! Property 8: Budget truncation preserves invariants
//! **Validates: Requirement 3.9**
//!
//! For any BudgetConfig and tool result list, after enforce_budget:
//! - Each result's content length <= max_result_size_chars (plus truncation suffix)
//! - Total content length is bounded

use proptest::prelude::*;

use hermes_agent::budget::enforce_budget;
use hermes_core::{BudgetConfig, ToolResult};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn arb_budget_config() -> impl Strategy<Value = BudgetConfig> {
    (100usize..10_000, 500usize..50_000).prop_map(|(max_result, max_agg)| BudgetConfig {
        max_result_size_chars: max_result,
        max_aggregate_chars: max_agg,
    })
}

fn arb_tool_result() -> impl Strategy<Value = ToolResult> {
    (
        "[a-z]{3,8}",
        proptest::collection::vec(prop_oneof![Just('a'), Just('b'), Just('x')], 0..20_000),
        proptest::bool::ANY,
    )
        .prop_map(|(id, chars, is_error)| {
            let content: String = chars.into_iter().collect();
            ToolResult {
                tool_call_id: id,
                content,
                is_error,
            }
        })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_budget_per_result_invariant(
        budget in arb_budget_config(),
        results in proptest::collection::vec(arb_tool_result(), 1..6),
    ) {
        let mut results = results;
        enforce_budget(&mut results, &budget);

        for result in &results {
            // After per-result truncation, the content should be bounded.
            // The truncation adds a suffix like "\n\n[... truncated N characters ...]"
            // which is at most ~50 chars, so we allow some slack.
            let max_with_suffix = budget.max_result_size_chars + 100;
            prop_assert!(
                result.content.len() <= max_with_suffix,
                "Result '{}' has {} chars, exceeds max {} + suffix",
                result.tool_call_id,
                result.content.len(),
                budget.max_result_size_chars
            );
        }
    }

    #[test]
    fn prop_budget_does_not_grow_content(
        budget in arb_budget_config(),
        results in proptest::collection::vec(arb_tool_result(), 1..4),
    ) {
        let original_total: usize = results.iter().map(|r| r.content.len()).sum();
        let mut results = results;
        enforce_budget(&mut results, &budget);
        let after_total: usize = results.iter().map(|r| r.content.len()).sum();

        // Budget enforcement should never increase total content beyond
        // original + truncation suffixes
        let max_suffix_overhead = results.len() * 100;
        prop_assert!(
            after_total <= original_total + max_suffix_overhead,
            "Budget enforcement grew content from {} to {} (overhead limit {})",
            original_total, after_total, max_suffix_overhead
        );
    }
}
