//! Multi-provider STT (OpenAI, Groq, Mistral, xAI, local_command).

use std::sync::Arc;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_config::voice::SttConfig;
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::voice_providers::SttEngine;

pub struct TranscriptionHandler {
    engine: Arc<SttEngine>,
}

impl TranscriptionHandler {
    pub fn new() -> Self {
        Self::with_config(None)
    }

    pub fn with_config(config: Option<SttConfig>) -> Self {
        Self {
            engine: Arc::new(SttEngine::from_optional(config)),
        }
    }
}

impl Default for TranscriptionHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ToolHandler for TranscriptionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("audio_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'audio_path'".into()));
        }

        let text = self.engine.transcribe_file(path).await?;
        Ok(json!({
            "audio_path": path,
            "text": text,
            "provider": self.engine.config.default_provider(),
            "status": "transcribed",
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "audio_path".into(),
            json!({"type":"string","description":"Path to audio file"}),
        );
        tool_schema(
            "transcription",
            "Transcribe audio into text. Provider from config stt.provider (openai, groq, mistral, xai, local_command). \
             Honors stt.enabled, stt.openai.model/base_url, STT_OPENAI_BASE_URL, and VOICE_TOOLS_OPENAI_KEY.",
            JsonSchema::object(props, vec!["audio_path".into()]),
        )
    }
}

/// Gateway / shared inbound STT entry (reads file from disk).
pub async fn transcribe_audio_file(
    config: Option<SttConfig>,
    path: &str,
) -> Result<String, ToolError> {
    SttEngine::from_optional(config).transcribe_file(path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice_providers::audio_extension_format;
    use hermes_config::managed_gateway::test_lock;

    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "HERMES_OPENAI_API_KEY",
                "OPENAI_API_KEY",
                "VOICE_TOOLS_OPENAI_KEY",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                hermes_core::test_env::remove_var(k);
            }
            hermes_core::test_env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => hermes_core::test_env::set_var(k, val),
                    None => hermes_core::test_env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn audio_extension_known_formats() {
        assert_eq!(audio_extension_format("a.wav"), "wav");
        assert_eq!(audio_extension_format("a.mp3"), "mp3");
        assert_eq!(audio_extension_format("a.m4a"), "mp4");
        assert_eq!(audio_extension_format("a.webm"), "webm");
        assert_eq!(audio_extension_format("a.ogg"), "ogg");
        assert_eq!(audio_extension_format("a.flac"), "flac");
    }

    #[test]
    fn stt_disabled_returns_error() {
        let mut cfg = SttConfig::default();
        cfg.enabled = Some(false);
        let engine = SttEngine::new(cfg);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let err = rt
            .block_on(engine.transcribe_file("/tmp/x.wav"))
            .unwrap_err();
        assert!(err.to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn execute_errors_on_missing_path_param() {
        let _g = EnvScope::new();
        hermes_core::test_env::set_var("OPENAI_API_KEY", "k");
        let h = TranscriptionHandler::new();
        let err = h.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }
}
