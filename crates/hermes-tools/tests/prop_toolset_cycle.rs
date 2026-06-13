//! Bounded invariant coverage: toolset cycle detection
//! **Validates: Requirement 5.3**
//!
//! Any toolset dependency graph containing a cycle returns CycleDetected rather
//! than infinite recursion or panic.

use std::sync::Arc;

use hermes_tools::{ToolRegistry, Toolset, ToolsetManager};

fn cyclic_toolsets(n: usize) -> (Vec<Toolset>, String) {
    let mut toolsets = Vec::new();
    for i in 0..n {
        let name = format!("cyc_{i}");
        let next = format!("cyc_{}", (i + 1) % n);
        toolsets.push(Toolset::with_includes(name, vec![next]));
    }
    (toolsets, "cyc_0".to_string())
}

#[test]
fn cycle_detected() {
    for n in [2, 3, 5] {
        let (toolsets, start) = cyclic_toolsets(n);
        let registry = Arc::new(ToolRegistry::new());
        let mut manager = ToolsetManager::new(registry);

        for ts in &toolsets {
            manager.register(ts.clone());
        }

        let result = manager.resolve_toolset_unfiltered(&start);
        assert!(
            result.is_err(),
            "Expected CycleDetected error for cyclic graph, got Ok"
        );

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Cycle detected"),
            "Error should mention cycle detection, got: {}",
            err_msg
        );
    }
}
