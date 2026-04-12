use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct ManagedToolGatewayHandler;

#[async_trait]
impl ToolHandler for ManagedToolGatewayHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let target_tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        if target_tool.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'tool'".into()));
        }
        Ok(json!({"status":"delegated_stub","tool":target_tool}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("tool".into(), json!({"type":"string"}));
        props.insert("args".into(), json!({"type":"object"}));
        tool_schema("managed_tool_gateway", "Dispatch a managed tool call through gateway controls.", JsonSchema::object(props, vec!["tool".into()]))
    }
}
