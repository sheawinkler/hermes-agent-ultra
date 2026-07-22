//! Text-to-speech tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::path::PathBuf;
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
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError>;
}

pub(crate) fn normalize_tts_tool_result(raw: String) -> String {
    let Ok(mut value) = serde_json::from_str::<Value>(&raw) else {
        return raw;
    };
    let Some(object) = value.as_object_mut() else {
        return raw;
    };
    let file_path = object
        .get("file_path")
        .or_else(|| object.get("file"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty() && !path.contains('\r') && !path.contains('\n'))
        .map(str::to_string);
    let Some(file_path) = file_path else {
        return raw;
    };
    let voice_compatible = match object.get("voice_compatible") {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    };
    object.insert(
        "media_tag".to_string(),
        Value::String(if voice_compatible {
            format!("[[audio_as_voice]]\nMEDIA:{file_path}")
        } else {
            format!("MEDIA:{file_path}")
        }),
    );
    serde_json::to_string(&value).unwrap_or(raw)
}

fn resolve_tts_output_path(raw: Option<&str>) -> Result<Option<String>, ToolError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.contains('\r') || raw.contains('\n') {
        return Err(ToolError::InvalidParams(
            "output_path must not contain CR or LF".to_string(),
        ));
    }
    if raw.trim().is_empty() {
        return Err(ToolError::InvalidParams(
            "output_path must not be empty".to_string(),
        ));
    }
    let path = PathBuf::from(raw);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "resolve relative TTS output_path from current directory: {error}"
                ))
            })?
            .join(path)
    };
    absolute
        .into_os_string()
        .into_string()
        .map(Some)
        .map_err(|_| {
            ToolError::InvalidParams(
                "output_path resolves through a non-UTF-8 directory".to_string(),
            )
        })
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
        let output_path =
            resolve_tts_output_path(params.get("output_path").and_then(|value| value.as_str()))?;
        let speed = params.get("speed").and_then(|v| v.as_f64());

        let result = self
            .backend
            .synthesize(text, voice, provider, output_path.as_deref(), speed)
            .await?;
        Ok(normalize_tts_tool_result(result))
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
                "description": "Optional TTS provider name. Omit to use tts.provider. Accepts built-in providers and names configured under tts.providers."
            }),
        );
        props.insert(
            "output_path".into(),
            json!({
                "type": "string",
                "description": "Optional output file path. Defaults to a temporary audio file."
            }),
        );
        props.insert(
            "speed".into(),
            json!({
                "type": "number",
                "description": "Optional provider-specific speech speed multiplier."
            }),
        );

        tool_schema(
            "text_to_speech",
            "Convert text to speech audio using a built-in provider or a configured command provider. Omit provider to honor the configured tts.provider default.",
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
            _output_path: Option<&str>,
            _speed: Option<f64>,
        ) -> Result<String, ToolError> {
            Ok(format!("Audio for: {}", text))
        }
    }

    struct CountingTtsBackend {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl TtsBackend for CountingTtsBackend {
        async fn synthesize(
            &self,
            _text: &str,
            _voice: Option<&str>,
            _provider: Option<&str>,
            _output_path: Option<&str>,
            _speed: Option<f64>,
        ) -> Result<String, ToolError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok("unreachable".to_string())
        }
    }

    #[test]
    fn test_tts_schema_accepts_configured_provider_names_without_overriding_default() {
        let handler = TextToSpeechHandler::new(Arc::new(MockTtsBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "text_to_speech");
        let properties = schema.parameters.properties.expect("schema properties");
        let provider = properties.get("provider").expect("provider property");
        assert!(provider.get("enum").is_none());
        assert!(provider.get("default").is_none());
        assert!(provider
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|description| description.contains("tts.providers")));
    }

    #[test]
    fn relative_output_path_resolves_to_absolute_gateway_deliverable() {
        let resolved = resolve_tts_output_path(Some("clip.mp3"))
            .unwrap()
            .expect("resolved path");
        let path = PathBuf::from(resolved);
        assert!(path.is_absolute());
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("clip.mp3")
        );
    }

    #[test]
    fn builtin_result_is_normalized_to_structured_media_tag() {
        let normalized = normalize_tts_tool_result(
            json!({"provider": "openai", "file": "/tmp/audio.mp3"}).to_string(),
        );
        let value: Value = serde_json::from_str(&normalized).unwrap();
        assert_eq!(value["media_tag"], "MEDIA:/tmp/audio.mp3");
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
    #[tokio::test]
    async fn crlf_output_path_is_rejected_before_backend_invocation() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler = TextToSpeechHandler::new(Arc::new(CountingTtsBackend {
            calls: Arc::clone(&calls),
        }));
        let output = tempfile::tempdir().unwrap().path().join("speech\r\n.mp3");
        let error = handler
            .execute(json!({
                "text": "Hello world",
                "output_path": output.to_string_lossy(),
            }))
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidParams(_)));
        assert!(error.to_string().contains("CR or LF"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(!output.exists());
    }
}
