use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct EnvPassthroughHandler;

#[async_trait]
impl ToolHandler for EnvPassthroughHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let keys = params
            .get("keys")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'keys'".into()))?;

        let mut out = serde_json::Map::new();
        for key in keys.iter().filter_map(|v| v.as_str()) {
            if let Ok(value) = std::env::var(key) {
                out.insert(key.to_string(), json!(value));
            }
        }
        Ok(Value::Object(out).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("keys".into(), json!({"type":"array","items":{"type":"string"}}));
        tool_schema("env_passthrough", "Expose selected env vars to tool workflows.", JsonSchema::object(props, vec!["keys".into()]))
    }
}
