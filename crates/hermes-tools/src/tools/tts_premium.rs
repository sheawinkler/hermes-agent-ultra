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
use crate::tools::tts::normalize_tts_tool_result;

fn canonical_premium_tts_result(raw: String) -> String {
    normalize_tts_tool_result(raw)
}

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
            .map(str::trim)
            .filter(|voice| !voice.is_empty());

        let result = self.backend.synthesize_elevenlabs(text, voice).await?;
        Ok(canonical_premium_tts_result(result))
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
                "description":"ElevenLabs voice id (default: tts.elevenlabs.voice_id, then Rachel)"
            }),
        );
        tool_schema(
            "tts_premium",
            "Premium TTS via ElevenLabs. Returns a gateway-deliverable audio attachment. \
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

    #[test]
    fn premium_result_uses_canonical_media_tag_shape() {
        let result = canonical_premium_tts_result(
            json!({
                "provider": "elevenlabs",
                "file": "/private/audio.mp3",
                "format": "mp3"
            })
            .to_string(),
        );
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["media_tag"], "MEDIA:/private/audio.mp3");
    }
}
