use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct MixtureOfAgentsHandler;

#[async_trait]
impl ToolHandler for MixtureOfAgentsHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let prompt = params.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'prompt'".into()));
        }
        Ok(json!({"status":"planned","strategy":"majority_vote","agents":params.get("agents").cloned().unwrap_or(json!([]))}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("prompt".into(), json!({"type":"string"}));
        props.insert("agents".into(), json!({"type":"array","items":{"type":"string"}}));
        tool_schema("mixture_of_agents", "Run a mixture-of-agents voting workflow.", JsonSchema::object(props, vec!["prompt".into()]))
    }
}
