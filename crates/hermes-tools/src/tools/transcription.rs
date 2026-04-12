use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct TranscriptionHandler;

#[async_trait]
impl ToolHandler for TranscriptionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params.get("audio_path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'audio_path'".into()));
        }
        Ok(json!({"audio_path": path, "text": "", "status":"transcribed_stub"}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("audio_path".into(), json!({"type":"string","description":"Path to audio file"}));
        tool_schema("transcription", "Transcribe audio into text.", JsonSchema::object(props, vec!["audio_path".into()]))
    }
}
