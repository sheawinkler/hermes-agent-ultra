//! Premium TTS (ElevenLabs).
//!
//! Directly dispatches to [`crate::backends::tts::MultiTtsBackend`]'s
//! ElevenLabs HTTP path. Requires `ELEVENLABS_API_KEY` env var; if unset
//! the tool reports a clear actionable error instead of returning a
//! "queued" envelope.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use std::sync::Arc;

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::backends::tts::MultiTtsBackend;

/// Default ElevenLabs voice id (Rachel). Callers can override via the
/// `voice` parameter.
const DEFAULT_VOICE: &str = "21m00Tcm4TlvDq8ikWAM";

pub struct TtsPremiumHandler {
    backend: Arc<MultiTtsBackend>,
}

impl TtsPremiumHandler {
    pub fn new(backend: Arc<MultiTtsBackend>) -> Self {
        Self { backend }
    }
}

impl Default for TtsPremiumHandler {
    fn default() -> Self {
        Self::new(Arc::new(MultiTtsBackend::new()))
    }
}

#[async_trait]
impl ToolHandler for TtsPremiumHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let text = params.get("text").and_then(|v| v.as_str()).unwrap_or("");
        if text.trim().is_empty() {
            return Err(ToolError::InvalidParams("Missing 'text'".into()));
        }
        let voice = params
            .get("voice")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_VOICE);

        self.backend.synthesize_elevenlabs(text, voice).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "text".into(),
            json!({"type":"string","description":"Text to synthesize"}),
        );
        props.insert(
            "voice".into(),
            json!({
                "type":"string",
                "description":"ElevenLabs voice id (default: Rachel 21m00Tcm4TlvDq8ikWAM)"
            }),
        );
        tool_schema(
            "tts_premium",
            "Premium TTS via ElevenLabs (returns file path of synthesized MP3). \
             Requires ELEVENLABS_API_KEY.",
            JsonSchema::object(props, vec!["text".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_schema() {
        let handler = TtsPremiumHandler::default();
        let schema = handler.schema();
        assert_eq!(schema.name, "tts_premium");
    }

    #[tokio::test]
    async fn test_empty_text_rejected() {
        let handler = TtsPremiumHandler::default();
        let err = handler.execute(json!({"text": "   "})).await.unwrap_err();
        assert!(err.to_string().to_lowercase().contains("missing"));
    }
}
