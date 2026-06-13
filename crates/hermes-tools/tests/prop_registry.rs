//! Bounded invariant coverage: tool registry consistency
//! **Validates: Requirements 4.1, 4.2, 4.3**
//!
//! For representative register/deregister operation sequences,
//! get_definitions returns the tool set that matches currently registered
//! tools with check_fn == true.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};
use hermes_tools::ToolRegistry;
use serde_json::Value;

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

#[derive(Debug, Clone)]
enum Op {
    Register(&'static str),
    Deregister(&'static str),
}

fn operation_cases() -> Vec<Vec<Op>> {
    vec![
        vec![Op::Register("aa")],
        vec![Op::Deregister("missing"), Op::Register("aa")],
        vec![Op::Register("aa"), Op::Register("bb"), Op::Deregister("aa")],
        vec![
            Op::Register("aa"),
            Op::Register("aa"),
            Op::Deregister("aa"),
            Op::Register("cc"),
        ],
        vec![
            Op::Register("aa"),
            Op::Register("bb"),
            Op::Deregister("missing"),
            Op::Register("dd"),
            Op::Deregister("bb"),
            Op::Register("ee"),
        ],
    ]
}

#[test]
fn registry_consistency() {
    for ops in operation_cases() {
        let registry = ToolRegistry::new();
        let mut expected: HashSet<String> = HashSet::new();

        for op in &ops {
            match op {
                Op::Register(name) => {
                    let handler = Arc::new(StubHandler {
                        name: (*name).to_string(),
                    });
                    let schema = handler.schema();
                    registry.register(
                        (*name).to_string(),
                        "test",
                        schema,
                        handler,
                        Arc::new(|| true),
                        vec![],
                        false,
                        "stub",
                        "tool",
                        None,
                    );
                    expected.insert((*name).to_string());
                }
                Op::Deregister(name) => {
                    registry.deregister(name);
                    expected.remove(*name);
                }
            }
        }

        let defs = registry.get_definitions();
        let def_names: HashSet<String> = defs.iter().map(|d| d.name.clone()).collect();

        assert_eq!(
            expected, def_names,
            "Registry definitions don't match expected set"
        );
    }
}
