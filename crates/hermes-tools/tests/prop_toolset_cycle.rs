//! Property 6: Toolset cycle detection
//! **Validates: Requirement 5.3**
//!
//! For any toolset dependency graph containing a cycle, resolve_toolset
//! returns CycleDetected error rather than infinite recursion or panic.

use proptest::prelude::*;
use std::sync::Arc;

use hermes_tools::{ToolRegistry, Toolset, ToolsetManager};

// ---------------------------------------------------------------------------
// Strategy: generate a chain of toolsets with a cycle injected
// ---------------------------------------------------------------------------

/// Generate a cycle: ts_0 -> ts_1 -> ... -> ts_n -> ts_0
fn arb_cyclic_toolsets() -> impl Strategy<Value = (Vec<Toolset>, String)> {
    (2usize..6).prop_map(|n| {
        let mut toolsets = Vec::new();
        for i in 0..n {
            let name = format!("cyc_{i}");
            let next = format!("cyc_{}", (i + 1) % n);
            toolsets.push(Toolset::with_includes(name, vec![next]));
        }
        let start = "cyc_0".to_string();
        (toolsets, start)
    })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_cycle_detected(
        (toolsets, start) in arb_cyclic_toolsets()
    ) {
        let registry = Arc::new(ToolRegistry::new());
        let mut manager = ToolsetManager::new(registry);

        for ts in &toolsets {
            manager.register(ts.clone());
        }

        let result = manager.resolve_toolset_unfiltered(&start);
        prop_assert!(result.is_err(),
            "Expected CycleDetected error for cyclic graph, got Ok");

        let err_msg = format!("{}", result.unwrap_err());
        prop_assert!(err_msg.contains("Cycle detected"),
            "Error should mention cycle detection, got: {}", err_msg);
    }
}
