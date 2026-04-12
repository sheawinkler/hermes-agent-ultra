use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct RlTrainingHandler;

#[async_trait]
impl ToolHandler for RlTrainingHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let dataset = params.get("dataset").and_then(|v| v.as_str()).unwrap_or("");
        if dataset.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'dataset'".into()));
        }
        Ok(json!({"status":"training_started_stub","dataset":dataset}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("dataset".into(), json!({"type":"string"}));
        props.insert("algo".into(), json!({"type":"string","default":"ppo"}));
        tool_schema("rl_training", "Start RL training job from trajectory dataset.", JsonSchema::object(props, vec!["dataset".into()]))
    }
}
