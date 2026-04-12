use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

pub struct VoiceModeHandler;

#[async_trait]
impl ToolHandler for VoiceModeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let enabled = params
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        Ok(json!({"voice_mode": enabled, "status": "updated"}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "enabled".into(),
            json!({"type":"boolean","description":"Enable or disable voice mode"}),
        );
        tool_schema(
            "voice_mode",
            "Toggle voice mode (STT/TTS).",
            JsonSchema::object(props, vec![]),
        )
    }
}
