//! Property 3: Tool registry consistency
//! **Validates: Requirements 4.1, 4.2, 4.3**
//!
//! For any sequence of register/deregister operations, get_definitions returns
//! the tool set that matches currently registered tools with check_fn == true.

use proptest::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use hermes_tools::ToolRegistry;
use serde_json::Value;

// ---------------------------------------------------------------------------
// Stub handler
// ---------------------------------------------------------------------------

struct StubHandler {
    name: String,
}

#[async_trait]
impl ToolHandler for StubHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        Ok("ok".to_string())
    }
    fn schema(&self) -> ToolSchema {
        tool_schema(&self.name, "stub", JsonSchema::new("object"))
    }
}

// ---------------------------------------------------------------------------
// Operation enum for generating random sequences
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Register(String),
    Deregister(String),
}

fn arb_op() -> impl Strategy<Value = Op> {
    let name = "[a-z]{2,8}";
    prop_oneof![
        name.prop_map(Op::Register),
        name.prop_map(Op::Deregister),
    ]
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_registry_consistency(ops in proptest::collection::vec(arb_op(), 1..20)) {
        let registry = ToolRegistry::new();
        let mut expected: HashSet<String> = HashSet::new();

        for op in &ops {
            match op {
                Op::Register(name) => {
                    let handler = Arc::new(StubHandler { name: name.clone() });
                    let schema = handler.schema();
                    registry.register(
                        name.clone(),
                        "test",
                        schema,
                        handler,
                        Arc::new(|| true),
                        vec![],
                        false,
                        "stub",
                        "🔧",
                        None,
                    );
                    expected.insert(name.clone());
                }
                Op::Deregister(name) => {
                    registry.deregister(name);
                    expected.remove(name);
                }
            }
        }

        let defs = registry.get_definitions();
        let def_names: HashSet<String> = defs.iter().map(|d| d.name.clone()).collect();

        prop_assert_eq!(
            expected, def_names,
            "Registry definitions don't match expected set"
        );
    }
}
