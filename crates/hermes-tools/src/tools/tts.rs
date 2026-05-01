//! Text-to-speech tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// TtsBackend trait
// ---------------------------------------------------------------------------

/// Backend for text-to-speech operations.
#[async_trait]
pub trait TtsBackend: Send + Sync {
    /// Convert text to speech.
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        provider: Option<&str>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// TextToSpeechHandler
// ---------------------------------------------------------------------------

/// Tool for converting text to speech using various TTS providers.
pub struct TextToSpeechHandler {
    backend: Arc<dyn TtsBackend>,
}

impl TextToSpeechHandler {
    pub fn new(backend: Arc<dyn TtsBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for TextToSpeechHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let text = params
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'text' parameter".into()))?;

        let voice = params.get("voice").and_then(|v| v.as_str());
        let provider = params.get("provider").and_then(|v| v.as_str());

        self.backend.synthesize(text, voice, provider).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "text".into(),
            json!({
                "type": "string",
                "description": "Text to convert to speech"
            }),
        );
        props.insert(
            "voice".into(),
            json!({
                "type": "string",
                "description": "Voice to use (provider-specific)"
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "TTS provider to use",
                "enum": ["elevenlabs", "openai", "minimax", "piper"],
                "default": "openai"
            }),
        );

        tool_schema(
            "text_to_speech",
            "Convert text to speech audio using ElevenLabs, OpenAI, MiniMax, or local Piper. HTTP providers use direct API calls; Piper uses a local binary (no Python runtime required).",
            JsonSchema::object(props, vec!["text".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockTtsBackend;
    #[async_trait]
    impl TtsBackend for MockTtsBackend {
        async fn synthesize(
            &self,
            text: &str,
            _voice: Option<&str>,
            _provider: Option<&str>,
        ) -> Result<String, ToolError> {
            Ok(format!("Audio for: {}", text))
        }
    }

    #[tokio::test]
    async fn test_tts_schema() {
        let handler = TextToSpeechHandler::new(Arc::new(MockTtsBackend));
        assert_eq!(handler.schema().name, "text_to_speech");
    }

    #[tokio::test]
    async fn test_tts_execute() {
        let handler = TextToSpeechHandler::new(Arc::new(MockTtsBackend));
        let result = handler
            .execute(json!({"text": "Hello world"}))
            .await
            .unwrap();
        assert!(result.contains("Hello world"));
    }
}
