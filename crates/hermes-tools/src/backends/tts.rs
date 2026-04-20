//! Real TTS backends: ElevenLabs and OpenAI TTS.
//!
//! Zero-Python: Edge TTS (which required the `edge-tts` Python CLI) is no
//! longer supported. Callers that want free / no-key TTS should use local
//! ONNX models via the forthcoming `LocalOnnxTtsBackend` (Sprint 6) or
//! OpenAI's cheap `tts-1` endpoint.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;

use crate::tools::tts::TtsBackend;
use crate::tts_streaming::minimax::MiniMaxTtsBackend;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};
use hermes_core::ToolError;

/// TTS backend that dispatches to ElevenLabs, OpenAI, or MiniMax based on
/// the `provider` argument. Defaults to `openai` when no API keys hint at a
/// preferred provider.
pub struct MultiTtsBackend {
    client: Client,
    elevenlabs_key: Option<String>,
    openai_base_url: String,
    minimax: MiniMaxTtsBackend,
    minimax_available: bool,
}

impl MultiTtsBackend {
    pub fn new() -> Self {
        let minimax_available = std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
        Self {
            client: Client::new(),
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY").ok(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            minimax: MiniMaxTtsBackend::from_env(),
            minimax_available,
        }
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
            "bytes": bytes.len(),
        })
        .to_string())
    }

    async fn openai_tts(&self, text: &str, voice: &str) -> Result<String, ToolError> {
        // Resolve transport in priority order:
        // 1. Managed Nous gateway (HERMES_ENABLE_NOUS_MANAGED_TOOLS + Nous token)
        // 2. Direct OpenAI with VOICE_TOOLS_OPENAI_KEY override or OPENAI_API_KEY
        let managed = resolve_managed_tool_gateway("openai-audio", ResolveOptions::default());
        let (endpoint, bearer, transport) = match managed {
            Some(cfg) => Self::openai_audio_managed_endpoint(&cfg),
            None => {
                let key = resolve_openai_audio_api_key();
                if key.is_empty() {
                    return Err(ToolError::ExecutionFailed(
                        "OPENAI_API_KEY (or VOICE_TOOLS_OPENAI_KEY) not set, and no managed \
                         openai-audio gateway is configured."
                            .into(),
                    ));
                }
                (
                    format!("{}/audio/speech", self.openai_base_url),
                    key,
                    "direct",
                )
            }
        };

        let body = json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
        });

        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", bearer))
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
            "transport": transport,
            "file": output_path.display().to_string(),
            "voice": voice,
            "bytes": bytes.len(),
        })
        .to_string())
    }

    /// Compose the OpenAI-audio gateway endpoint + bearer for a resolved
    /// managed config. Public visibility kept tight (`pub(crate)`) so the
    /// `tts_premium` handler can reuse it later if needed.
    pub(crate) fn openai_audio_managed_endpoint(
        cfg: &ManagedToolGatewayConfig,
    ) -> (String, String, &'static str) {
        let base = cfg.gateway_origin.trim_end_matches('/');
        (
            format!("{base}/audio/speech"),
            cfg.nous_user_token.clone(),
            "managed",
        )
    }

    /// Public accessor so other tool handlers (e.g. `tts_premium`) can reuse
    /// the ElevenLabs HTTP path without instantiating a second client.
    pub async fn synthesize_elevenlabs(
        &self,
        text: &str,
        voice: &str,
    ) -> Result<String, ToolError> {
        self.elevenlabs_tts(text, voice).await
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
        // Default provider preference:
        // 1. ELEVENLABS_API_KEY set → elevenlabs (highest quality)
        // 2. Otherwise OpenAI (cheapest HTTP-only path)
        // Zero-Python: edge_tts removed entirely — callers asking for
        // "edge_tts" receive a clear migration error.
        let resolved_provider = provider.unwrap_or_else(|| {
            if self.elevenlabs_key.is_some() {
                "elevenlabs"
            } else {
                "openai"
            }
        });

        match resolved_provider {
            "elevenlabs" => {
                let voice = voice.unwrap_or("21m00Tcm4TlvDq8ikWAM"); // Rachel
                self.elevenlabs_tts(text, voice).await
            }
            "openai" => {
                let voice = voice.unwrap_or("alloy");
                self.openai_tts(text, voice).await
            }
            "minimax" => {
                if !self.minimax_available {
                    return Err(ToolError::ExecutionFailed("MINIMAX_API_KEY not set".into()));
                }
                self.minimax.synthesize(text, voice, provider).await
            }
            "edge_tts" | "edge-tts" | "neutts" => Err(ToolError::InvalidParams(format!(
                "{resolved_provider} is not supported in hermes-agent-rust (zero-Python). \
                 Use provider='openai' (OPENAI_API_KEY), 'elevenlabs' (ELEVENLABS_API_KEY), \
                 or 'minimax' (MINIMAX_API_KEY)."
            ))),
            other => Err(ToolError::InvalidParams(format!(
                "Unknown TTS provider: '{other}'. Use 'openai', 'elevenlabs', or 'minimax'.",
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_audio_managed_endpoint_appends_audio_speech() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "openai-audio".into(),
            gateway_origin: "https://openai-audio-gateway.example.com/".into(),
            nous_user_token: "tok-xyz".into(),
            managed_mode: true,
        };
        let (endpoint, bearer, transport) = MultiTtsBackend::openai_audio_managed_endpoint(&cfg);
        assert_eq!(
            endpoint,
            "https://openai-audio-gateway.example.com/audio/speech"
        );
        assert_eq!(bearer, "tok-xyz");
        assert_eq!(transport, "managed");
    }

    #[tokio::test]
    async fn test_edge_tts_returns_migration_error() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hello", None, Some("edge_tts"))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not supported") || msg.contains("zero-Python"));
        assert!(msg.contains("openai") || msg.contains("elevenlabs"));
    }

    #[tokio::test]
    async fn test_neutts_returns_migration_error() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hi", None, Some("neutts"))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not supported") || msg.contains("zero-Python"));
    }

    #[tokio::test]
    async fn test_unknown_provider_errors() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hello", None, Some("bogus"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unknown"));
    }
}
