//! Bounded invariant coverage: toolset resolution dedup and completeness
//! **Validates: Requirement 5.2**
//!
//! For representative acyclic toolset dependency graphs,
//! resolve_toolset_unfiltered returns a list that is deduplicated, complete,
//! and sorted.

use std::collections::HashSet;
use std::sync::Arc;

use hermes_tools::{ToolRegistry, Toolset, ToolsetManager};

fn toolset_cases() -> Vec<(Vec<Toolset>, &'static str)> {
    vec![
        (
            vec![
                Toolset::new("ts_0", vec!["bash".to_string(), "read".to_string()]),
                Toolset::new_mixed("ts_1", vec!["write".to_string()], vec!["ts_0".to_string()]),
                Toolset::new_mixed("ts_2", vec!["edit".to_string()], vec!["ts_1".to_string()]),
                Toolset::new_mixed("ts_3", vec!["search".to_string()], vec!["ts_2".to_string()]),
            ],
            "ts_3",
        ),
        (
            vec![
                Toolset::new("ts_0", vec!["alpha".to_string()]),
                Toolset::new_mixed(
                    "ts_1",
                    vec!["alpha".to_string(), "beta".to_string()],
                    vec!["ts_0".to_string()],
                ),
                Toolset::new_mixed("ts_2", vec!["gamma".to_string()], vec!["ts_1".to_string()]),
                Toolset::new("ts_3", vec!["delta".to_string()]),
            ],
            "ts_2",
        ),
        (
            vec![
                Toolset::new("ts_0", vec!["root".to_string()]),
                Toolset::new("ts_1", vec!["standalone".to_string()]),
                Toolset::new_mixed("ts_2", vec!["leaf".to_string()], vec!["ts_0".to_string()]),
                Toolset::new_mixed("ts_3", vec!["target".to_string()], vec!["ts_2".to_string()]),
            ],
            "ts_3",
        ),
    ]
}

fn collect_all_tools(
    name: &str,
    toolsets: &[Toolset],
    visited: &mut HashSet<String>,
    result: &mut HashSet<String>,
) {
    if !visited.insert(name.to_string()) {
        return;
    }

    if let Some(ts) = toolsets.iter().find(|t| t.name == name) {
        for tool in &ts.tools {
            result.insert(tool.clone());
        }
        for inc in &ts.includes {
            collect_all_tools(inc, toolsets, visited, result);
        }
    }
}

#[test]
fn toolset_resolve_dedup_complete_sorted() {
    for (toolsets, target) in toolset_cases() {
        let registry = Arc::new(ToolRegistry::new());
        let mut manager = ToolsetManager::new(registry);

        for ts in &toolsets {
            manager.register(ts.clone());
        }

        let resolved = manager.resolve_toolset_unfiltered(target).unwrap();
        let unique: HashSet<String> = resolved.iter().cloned().collect();
        assert_eq!(
            resolved.len(),
            unique.len(),
            "Duplicates found in resolved toolset"
        );

        let mut expected = HashSet::new();
        let mut visited = HashSet::new();
        collect_all_tools(target, &toolsets, &mut visited, &mut expected);

        for tool in &expected {
            assert!(
                unique.contains(tool),
                "Missing tool '{}' in resolved set",
                tool
            );
        }
        for tool in &resolved {
            assert!(
                expected.contains(tool),
                "Unexpected tool '{}' in resolved set",
                tool
            );
        }

        let mut sorted = resolved.clone();
        sorted.sort();
        assert_eq!(&resolved, &sorted, "Resolved toolset is not sorted");
    }
}
