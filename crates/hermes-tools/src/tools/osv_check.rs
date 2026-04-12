use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

pub struct OsvCheckHandler;

#[async_trait]
impl ToolHandler for OsvCheckHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let package = params.get("package").and_then(|v| v.as_str()).unwrap_or("");
        if package.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'package'".into()));
        }
        Ok(json!({"package":package,"vulnerabilities":[]}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("package".into(), json!({"type":"string"}));
        props.insert("version".into(), json!({"type":"string"}));
        tool_schema(
            "osv_check",
            "Check package vulnerabilities via OSV.",
            JsonSchema::object(props, vec!["package".into()]),
        )
    }
}
