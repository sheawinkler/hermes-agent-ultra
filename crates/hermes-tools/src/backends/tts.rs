//! Real TTS backends: Edge TTS, ElevenLabs, and OpenAI TTS.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use crate::tools::tts::TtsBackend;
use hermes_core::ToolError;

/// TTS backend that dispatches to Edge TTS, ElevenLabs, or OpenAI based on provider.
pub struct MultiTtsBackend {
    client: Client,
    elevenlabs_key: Option<String>,
    openai_key: Option<String>,
    openai_base_url: String,
}

impl MultiTtsBackend {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY").ok(),
            openai_key: std::env::var("OPENAI_API_KEY").ok(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
        }
    }

    async fn edge_tts(&self, text: &str, voice: &str) -> Result<String, ToolError> {
        // Edge TTS uses a WebSocket connection to Microsoft's speech service.
        // For simplicity, we shell out to the edge-tts CLI if available,
        // or return a signal for the caller to handle.
        let output_path =
            std::env::temp_dir().join(format!("hermes_tts_{}.mp3", uuid::Uuid::new_v4()));

        let output = tokio::process::Command::new("edge-tts")
            .arg("--voice")
            .arg(voice)
            .arg("--text")
            .arg(text)
            .arg("--write-media")
            .arg(output_path.to_str().unwrap_or("output.mp3"))
            .output()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "edge-tts command failed (is it installed? pip install edge-tts): {}",
                    e
                ))
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed(format!(
                "edge-tts error: {}",
                stderr
            )));
        }

        Ok(json!({
            "provider": "edge_tts",
            "file": output_path.display().to_string(),
            "voice": voice,
        })
        .to_string())
    }

    async fn elevenlabs_tts(&self, text: &str, voice: &str) -> Result<String, ToolError> {
        let api_key = self
            .elevenlabs_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("ELEVENLABS_API_KEY not set".into()))?;

        let body = json!({
            "text": text,
            "model_id": "eleven_monolingual_v1",
        });

        let resp = self
            .client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                voice
            ))
            .header("xi-api-key", api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("ElevenLabs API failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "ElevenLabs error ({}): {}",
                status, text
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        let output_path =
            std::env::temp_dir().join(format!("hermes_tts_{}.mp3", uuid::Uuid::new_v4()));
        tokio::fs::write(&output_path, &bytes)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write audio: {}", e)))?;

        Ok(json!({
            "provider": "elevenlabs",
            "file": output_path.display().to_string(),
            "voice": voice,
        })
        .to_string())
    }

    async fn openai_tts(&self, text: &str, voice: &str) -> Result<String, ToolError> {
        let api_key = self
            .openai_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("OPENAI_API_KEY not set".into()))?;

        let body = json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
        });

        let resp = self
            .client
            .post(format!("{}/audio/speech", self.openai_base_url))
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("OpenAI TTS API failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "OpenAI TTS error ({}): {}",
                status, text
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        let output_path =
            std::env::temp_dir().join(format!("hermes_tts_{}.mp3", uuid::Uuid::new_v4()));
        tokio::fs::write(&output_path, &bytes)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write audio: {}", e)))?;

        Ok(json!({
            "provider": "openai",
            "file": output_path.display().to_string(),
            "voice": voice,
        })
        .to_string())
    }
}

impl Default for MultiTtsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TtsBackend for MultiTtsBackend {
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        provider: Option<&str>,
    ) -> Result<String, ToolError> {
        let provider = provider.unwrap_or("edge_tts");

        match provider {
            "edge_tts" => {
                let voice = voice.unwrap_or("en-US-AriaNeural");
                self.edge_tts(text, voice).await
            }
            "elevenlabs" => {
                let voice = voice.unwrap_or("21m00Tcm4TlvDq8ikWAM"); // Rachel
                self.elevenlabs_tts(text, voice).await
            }
            "openai" => {
                let voice = voice.unwrap_or("alloy");
                self.openai_tts(text, voice).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown TTS provider: '{}'. Use 'edge_tts', 'elevenlabs', or 'openai'.",
                other
            ))),
        }
    }
}
