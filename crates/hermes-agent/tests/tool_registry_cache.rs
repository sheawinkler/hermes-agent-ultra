//! ToolRegistry schema cache — shared `Arc` across calls.

use std::sync::Arc;

use hermes_agent::agent_loop::ToolRegistry;
use hermes_core::{JsonSchema, ToolSchema};

#[test]
fn schemas_cache_returns_same_arc_until_register() {
    let mut reg = ToolRegistry::new();
    reg.register(
        "alpha",
        ToolSchema::new("alpha", "A", JsonSchema::new("object")),
        Arc::new(|_| Ok("ok".into())),
    );
    reg.register(
        "beta",
        ToolSchema::new("beta", "B", JsonSchema::new("object")),
        Arc::new(|_| Ok("ok".into())),
    );

    let first = reg.schemas();
    let second = reg.schemas();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(first.len(), 2);
    assert_eq!(first[0].name, "alpha");
    assert_eq!(first[1].name, "beta");

    reg.register(
        "gamma",
        ToolSchema::new("gamma", "G", JsonSchema::new("object")),
        Arc::new(|_| Ok("ok".into())),
    );
    let third = reg.schemas();
    assert!(!Arc::ptr_eq(&first, &third));
    assert_eq!(third.len(), 3);
}
