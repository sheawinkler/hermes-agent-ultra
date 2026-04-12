//! Property 5: Toolset resolution dedup and completeness
//! **Validates: Requirement 5.2**
//!
//! For any acyclic DAG of toolset dependencies, resolve_toolset_unfiltered
//! returns a list that is: (a) deduplicated, (b) complete, (c) sorted.

use proptest::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;

use hermes_tools::{ToolRegistry, Toolset, ToolsetManager};

// ---------------------------------------------------------------------------
// Strategy: generate a tree-shaped toolset hierarchy (no diamond deps)
// Each toolset includes at most one parent to avoid false cycle detection
// in the shared-visited-set implementation.
// ---------------------------------------------------------------------------

fn arb_tool_name() -> impl Strategy<Value = String> {
    "[a-z]{3,8}"
}

fn arb_tree_toolsets() -> impl Strategy<Value = Vec<Toolset>> {
    (
        proptest::collection::vec(arb_tool_name(), 1..4),
        proptest::collection::vec(arb_tool_name(), 1..4),
        proptest::collection::vec(arb_tool_name(), 1..4),
        proptest::collection::vec(arb_tool_name(), 1..4),
        // Each toolset includes at most one earlier toolset (tree shape)
        proptest::option::of(0usize..1), // ts_1 parent
        proptest::option::of(0usize..2), // ts_2 parent
        proptest::option::of(0usize..3), // ts_3 parent
    )
        .prop_map(|(t0, t1, t2, t3, p1, p2, p3)| {
            let mut toolsets = Vec::new();

            toolsets.push(Toolset::new("ts_0", t0));

            let inc1: Vec<String> = p1.map(|i| vec![format!("ts_{i}")]).unwrap_or_default();
            toolsets.push(Toolset::new_mixed("ts_1", t1, inc1));

            let inc2: Vec<String> = p2.map(|i| vec![format!("ts_{i}")]).unwrap_or_default();
            toolsets.push(Toolset::new_mixed("ts_2", t2, inc2));

            let inc3: Vec<String> = p3.map(|i| vec![format!("ts_{i}")]).unwrap_or_default();
            toolsets.push(Toolset::new_mixed("ts_3", t3, inc3));

            toolsets
        })
}

// ---------------------------------------------------------------------------
// Helper: compute expected tools by manual DFS
// ---------------------------------------------------------------------------

fn collect_all_tools(
    name: &str,
    toolsets: &[Toolset],
    visited: &mut HashSet<String>,
    result: &mut HashSet<String>,
) {
    if visited.contains(name) {
        return;
    }
    visited.insert(name.to_string());
    if let Some(ts) = toolsets.iter().find(|t| t.name == name) {
        for tool in &ts.tools {
            result.insert(tool.clone());
        }
        for inc in &ts.includes {
            collect_all_tools(inc, toolsets, visited, result);
        }
    }
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_toolset_resolve_dedup_complete_sorted(toolsets in arb_tree_toolsets()) {
        let registry = Arc::new(ToolRegistry::new());
        let mut manager = ToolsetManager::new(registry);

        // Register all generated toolsets (overwrite defaults)
        for ts in &toolsets {
            manager.register(ts.clone());
        }

        // Test resolution of the last toolset (which may include earlier ones)
        let last_name = "ts_3";
        let resolved = manager.resolve_toolset_unfiltered(last_name).unwrap();

        // (a) No duplicates
        let unique: HashSet<String> = resolved.iter().cloned().collect();
        prop_assert_eq!(resolved.len(), unique.len(), "Duplicates found in resolved toolset");

        // (b) Complete — contains all expected tools
        let mut expected = HashSet::new();
        let mut visited = HashSet::new();
        collect_all_tools(last_name, &toolsets, &mut visited, &mut expected);
        for tool in &expected {
            prop_assert!(unique.contains(tool),
                "Missing tool '{}' in resolved set", tool);
        }
        for tool in &resolved {
            prop_assert!(expected.contains(tool),
                "Unexpected tool '{}' in resolved set", tool);
        }

        // (c) Sorted
        let mut sorted = resolved.clone();
        sorted.sort();
        prop_assert_eq!(&resolved, &sorted, "Resolved toolset is not sorted");
    }
}
