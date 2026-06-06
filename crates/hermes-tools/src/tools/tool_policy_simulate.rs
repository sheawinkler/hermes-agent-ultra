//! Policy preview tool for simulating tool-governance decisions without
//! dispatching the target tool.

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::{ToolPolicyDecision, ToolRegistry};

pub const TOOL_POLICY_SIMULATE_TOOL_NAME: &str = "tool_policy_simulate";

#[derive(Clone)]
pub struct ToolPolicySimulateHandler {
    registry: Arc<ToolRegistry>,
}

impl ToolPolicySimulateHandler {
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl ToolHandler for ToolPolicySimulateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let target_tool = params
            .get("tool")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'tool'".into()))?;

        let args = params.get("args").cloned().unwrap_or_else(|| json!({}));
        let decision = self.registry.evaluate_policy_preview(target_tool, &args);
        let target = self.registry.get_tool(target_tool);
        let available = self.registry.is_available(target_tool);

        Ok(json!({
            "status": "preview",
            "tool": target_tool,
            "target": {
                "registered": target.is_some(),
                "available": available,
                "toolset": target.as_ref().map(|t| t.toolset.as_str()),
                "description": target.as_ref().map(|t| t.description.as_str()),
            },
            "args": summarize_value_for_output(&args),
            "policy": policy_decision_json(&decision),
            "will_execute": false,
            "counter_effect": "none",
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "tool".into(),
            json!({
                "type": "string",
                "description": "Name of the target tool to preview against the active policy."
            }),
        );
        props.insert(
            "args".into(),
            json!({
                "type": "object",
                "description": "Target tool arguments used for policy evaluation. Values are not echoed in the preview result."
            }),
        );
        tool_schema(
            TOOL_POLICY_SIMULATE_TOOL_NAME,
            "Preview the active tool-policy allow/deny decision for a target tool call without executing that target tool.",
            JsonSchema::object(props, vec!["tool".into()]),
        )
    }
}

pub(crate) fn dispatch_policy_params(params: &Value) -> Value {
    let target_tool = params
        .get("tool")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("<missing>");
    let args = params.get("args").unwrap_or(&Value::Null);
    json!({
        "tool": target_tool,
        "args": summarize_value_for_dispatch_policy(args),
        "policy_preview_wrapper": true,
    })
}

fn policy_decision_json(decision: &ToolPolicyDecision) -> Value {
    json!({
        "decision": if decision.allow { "allow" } else { "deny" },
        "allow": decision.allow,
        "mode": decision.mode.as_str(),
        "audited_only": decision.audited_only,
        "simulated": decision.simulated,
        "would_block": decision.would_block,
        "code": decision.code.as_deref(),
        "reason": decision.reason.as_deref(),
    })
}

fn summarize_value_for_output(value: &Value) -> Value {
    let json_bytes = serde_json::to_vec(value).map(|raw| raw.len()).unwrap_or(0);
    let mut summary = json!({
        "type": value_type(value),
        "json_bytes": json_bytes,
    });

    match value {
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_unstable();
            summary["key_count"] = json!(map.len());
            summary["keys"] = json!(keys);
        }
        Value::Array(values) => {
            summary["item_count"] = json!(values.len());
        }
        _ => {}
    }

    summary
}

fn summarize_value_for_dispatch_policy(value: &Value) -> Value {
    let json_bytes = serde_json::to_vec(value).map(|raw| raw.len()).unwrap_or(0);
    let mut summary = json!({
        "type": value_type(value),
        "json_bytes": json_bytes,
    });

    match value {
        Value::Object(map) => {
            summary["key_count"] = json!(map.len());
        }
        Value::Array(values) => {
            summary["item_count"] = json!(values.len());
        }
        _ => {}
    }

    summary
}

fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::tool_policy::{ToolPolicyEngine, ToolPolicyMode};

    struct CountingHandler {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ToolHandler for CountingHandler {
        async fn execute(&self, _params: Value) -> Result<String, ToolError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(json!({"executed": true}).to_string())
        }

        fn schema(&self) -> ToolSchema {
            tool_schema("terminal", "Counting target", JsonSchema::new("object"))
        }
    }

    fn register_counting_target(registry: &ToolRegistry, calls: Arc<AtomicUsize>) {
        let handler = Arc::new(CountingHandler { calls });
        registry.register(
            "terminal",
            "terminal",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Counting target",
            "",
            None,
        );
    }

    fn register_simulator(registry: &ToolRegistry) {
        let handler = Arc::new(ToolPolicySimulateHandler::new(Arc::new(registry.clone())));
        registry.register(
            TOOL_POLICY_SIMULATE_TOOL_NAME,
            "system",
            handler.schema(),
            handler,
            Arc::new(|| true),
            vec![],
            true,
            "Policy simulator",
            "",
            None,
        );
    }

    #[tokio::test]
    async fn preview_reports_target_policy_without_executing_target() {
        let registry = ToolRegistry::new();
        let calls = Arc::new(AtomicUsize::new(0));
        register_counting_target(&registry, calls.clone());
        register_simulator(&registry);
        registry.set_policy(
            ToolPolicyEngine::new(ToolPolicyMode::Enforce)
                .with_deny_param_patterns(&[r"api[_-]?key", r"rm\s+-rf"]),
        );

        let output = registry
            .dispatch_async(
                TOOL_POLICY_SIMULATE_TOOL_NAME,
                json!({
                    "tool": "terminal",
                    "args": {
                        "cmd": "rm -rf /tmp/demo",
                        "api_key": "sk-secret"
                    }
                }),
            )
            .await;
        let parsed: Value = serde_json::from_str(&output).expect("json preview");

        assert_eq!(parsed["status"], "preview");
        assert_eq!(parsed["tool"], "terminal");
        assert_eq!(parsed["target"]["registered"], true);
        assert_eq!(parsed["policy"]["decision"], "deny");
        assert_eq!(parsed["policy"]["code"], "params_pattern_denied");
        assert_eq!(parsed["will_execute"], false);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!output.contains("sk-secret"), "{output}");
        assert!(!output.contains("rm -rf /tmp/demo"), "{output}");
    }

    #[tokio::test]
    async fn preview_summarizes_argument_shape_without_values() {
        let registry = Arc::new(ToolRegistry::new());
        let handler = ToolPolicySimulateHandler::new(registry);

        let output = handler
            .execute(json!({
                "tool": "missing_tool",
                "args": {
                    "cmd": "rm -rf /tmp/demo",
                    "token": "secret-token"
                }
            }))
            .await
            .expect("preview");
        let parsed: Value = serde_json::from_str(&output).expect("json preview");

        assert_eq!(parsed["target"]["registered"], false);
        assert_eq!(parsed["args"]["type"], "object");
        assert_eq!(parsed["args"]["key_count"], 2);
        assert!(parsed["args"]["keys"]
            .as_array()
            .expect("keys")
            .contains(&json!("cmd")));
        assert!(!output.contains("secret-token"), "{output}");
        assert!(!output.contains("rm -rf /tmp/demo"), "{output}");
    }

    #[tokio::test]
    async fn missing_tool_param_is_invalid() {
        let handler = ToolPolicySimulateHandler::new(Arc::new(ToolRegistry::new()));
        let err = handler.execute(json!({})).await.expect_err("missing tool");
        match err {
            ToolError::InvalidParams(message) => assert!(message.contains("tool")),
            other => panic!("wrong error: {other:?}"),
        }
    }
}
