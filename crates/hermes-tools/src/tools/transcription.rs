use std::path::Path;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

pub struct TranscriptionHandler;

fn audio_extension_format(path: &str) -> &'static str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
        .map(|e| match e {
            "wav" => "wav",
            "mp3" => "mp3",
            "m4a" => "mp4",
            "webm" => "webm",
            "ogg" | "oga" => "ogg",
            "flac" => "flac",
            _ => "wav",
        })
        .unwrap_or("wav")
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

        let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| {
            ToolError::ExecutionFailed(
                "OPENAI_API_KEY is required for transcription (Whisper)".into(),
            )
        })?;

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio file: {e}")))?;

        let fmt = audio_extension_format(path);
        let client = reqwest::Client::new();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("audio.{fmt}"))
            .mime_str(&format!("audio/{fmt}"))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1");

        let resp = client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Whisper API: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Whisper error {status}: {body}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Whisper JSON: {e}")))?;
        let text = json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        Ok(json!({
            "audio_path": path,
            "text": text,
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
            "Transcribe audio into text via OpenAI Whisper (requires OPENAI_API_KEY).",
            JsonSchema::object(props, vec!["audio_path".into()]),
        )
    }
}
