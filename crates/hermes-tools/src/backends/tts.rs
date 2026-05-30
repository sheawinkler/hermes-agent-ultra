//! TTS backends: ElevenLabs, OpenAI, MiniMax, Piper, Edge, Mistral, Gemini, xAI.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use hermes_config::voice::TtsConfig;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};

use crate::tools::tts::TtsBackend;
use crate::tts_streaming::minimax::MiniMaxTtsBackend;
use crate::voice_providers::{
    edge_tts_synthesize, gemini_tts_synthesize, mistral_tts_synthesize, tts_result_json,
    TtsSettings, xai_tts_synthesize,
};
use hermes_core::ToolError;

/// Multi-provider TTS backend driven by `config.yaml` `tts` block.
pub struct MultiTtsBackend {
    client: Client,
    settings: TtsSettings,
    elevenlabs_key: Option<String>,
    minimax: MiniMaxTtsBackend,
    minimax_available: bool,
}

impl MultiTtsBackend {
    pub fn new() -> Self {
        Self::with_config(None)
    }

    pub fn with_config(config: Option<TtsConfig>) -> Self {
        let minimax_available = std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
        Self {
            client: Client::new(),
            settings: TtsSettings::from_optional(config),
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY").ok(),
            minimax: MiniMaxTtsBackend::from_env(),
            minimax_available,
        }
    }

    async fn elevenlabs_tts(&self, text: &str, voice: &str) -> Result<String, ToolError> {
        let api_key = self
            .elevenlabs_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("ELEVENLABS_API_KEY not set".into()))?;

        let model_id = self
            .settings
            .config
            .elevenlabs
            .as_ref()
            .and_then(|c| c.model_id.as_deref())
            .unwrap_or("eleven_monolingual_v1");

        let body = json!({
            "text": text,
            "model_id": model_id,
        });

        let voice_id = self
            .settings
            .config
            .elevenlabs
            .as_ref()
            .and_then(|c| c.voice_id.as_deref())
            .unwrap_or(voice);

        let resp = self
            .client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                voice_id
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
                "ElevenLabs error ({status}): {text}"
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        tts_result_json("elevenlabs", voice_id, &bytes, "mp3").await
    }

    async fn openai_tts(&self, text: &str, voice: Option<&str>) -> Result<String, ToolError> {
        let managed = resolve_managed_tool_gateway("openai-audio", ResolveOptions::default());
        let base_url = self.settings.openai_base_url();
        let (endpoint, bearer, transport) = match managed {
            Some(cfg) => Self::openai_audio_managed_endpoint(&cfg),
            None => {
                let key = resolve_openai_audio_api_key();
                if key.is_empty() {
                    return Err(ToolError::ExecutionFailed(
                        "HERMES_OPENAI_API_KEY (or OPENAI_API_KEY / VOICE_TOOLS_OPENAI_KEY) \
                         not set, and no managed openai-audio gateway is configured."
                            .into(),
                    ));
                }
                (
                    format!("{}/audio/speech", base_url),
                    key,
                    "direct",
                )
            }
        };

        let model = self.settings.openai_model();
        let voice_name = self.settings.openai_voice(voice);
        let speed = self.settings.config.openai.as_ref().and_then(|c| c.speed);

        let mut body = json!({
            "model": model,
            "input": text,
            "voice": voice_name,
        });
        if let Some(s) = speed {
            body["speed"] = json!(s);
        }

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
                "OpenAI TTS error ({status}): {text}"
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        let mut result = tts_result_json("openai", &voice_name, &bytes, "mp3").await?;
        if transport == "managed" {
            let parsed: serde_json::Value = serde_json::from_str(&result)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            let mut obj = parsed.as_object().cloned().unwrap_or_default();
            obj.insert("transport".into(), json!("managed"));
            result = serde_json::to_string(&obj)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        }
        Ok(result)
    }

    async fn edge_tts(&self, text: &str, voice: Option<&str>) -> Result<String, ToolError> {
        let voice_name = self.settings.edge_voice(voice);
        let speed = self.settings.config.edge.as_ref().and_then(|c| c.speed);
        let bytes = edge_tts_synthesize(&self.client, text, &voice_name, speed).await?;
        tts_result_json("edge", &voice_name, &bytes, "mp3").await
    }

    async fn piper_tts(&self, text: &str, voice: Option<&str>) -> Result<String, ToolError> {
        let binary = std::env::var("PIPER_BINARY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "piper".to_string());

        let model = voice
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .or_else(|| {
                self.settings
                    .config
                    .piper
                    .as_ref()
                    .and_then(|c| c.voice.clone())
            })
            .or_else(|| {
                std::env::var("PIPER_MODEL")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Piper requires a model. Set provider='piper' with voice='<model-path-or-name>' \
                     or set tts.piper.voice / PIPER_MODEL."
                        .into(),
                )
            })?;

        let output_path =
            std::env::temp_dir().join(format!("hermes_tts_{}.wav", uuid::Uuid::new_v4()));
        let mut cmd = Command::new(&binary);
        cmd.arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(&output_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        if let Ok(config) = std::env::var("PIPER_CONFIG") {
            let config = config.trim();
            if !config.is_empty() {
                cmd.arg("--config").arg(config);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to start piper binary '{}': {}", binary, e))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed writing to piper stdin: {}", e))
            })?;
            stdin.write_all(b"\n").await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed finalizing piper stdin: {}", e))
            })?;
            stdin.shutdown().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed closing piper stdin: {}", e))
            })?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("piper process failed: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ToolError::ExecutionFailed(format!(
                "piper exited with status {}{}",
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr)
                }
            )));
        }

        let bytes = tokio::fs::read(&output_path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "piper completed but failed reading output {}: {}",
                output_path.display(),
                e
            ))
        })?;

        tts_result_json("piper", &model, &bytes, "wav").await
    }

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

    pub async fn synthesize_elevenlabs(
        &self,
        text: &str,
        voice: &str,
    ) -> Result<String, ToolError> {
        self.elevenlabs_tts(text, voice).await
    }

    fn resolve_provider<'a>(&'a self, provider: Option<&'a str>) -> &'a str {
        provider
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.settings.config.default_provider())
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
        let resolved = self.resolve_provider(provider);
        match resolved {
            "elevenlabs" => {
                let v = voice.unwrap_or("21m00Tcm4TlvDq8ikWAM");
                self.elevenlabs_tts(text, v).await
            }
            "openai" => self.openai_tts(text, voice).await,
            "minimax" => {
                if !self.minimax_available {
                    return Err(ToolError::ExecutionFailed("MINIMAX_API_KEY not set".into()));
                }
                self.minimax.synthesize(text, voice, provider).await
            }
            "piper" => self.piper_tts(text, voice).await,
            "edge" | "edge_tts" | "edge-tts" => self.edge_tts(text, voice).await,
            "mistral" => {
                let bytes =
                    mistral_tts_synthesize(&self.client, text, &self.settings.config).await?;
                tts_result_json("mistral", voice.unwrap_or("default"), &bytes, "mp3").await
            }
            "gemini" => {
                let bytes =
                    gemini_tts_synthesize(&self.client, text, &self.settings.config).await?;
                tts_result_json("gemini", voice.unwrap_or("default"), &bytes, "wav").await
            }
            "xai" => {
                let bytes = xai_tts_synthesize(&self.client, text, &self.settings.config).await?;
                tts_result_json("xai", voice.unwrap_or("default"), &bytes, "mp3").await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown TTS provider: '{other}'. Supported: edge, openai, elevenlabs, minimax, \
                 piper, mistral, gemini, xai."
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
    async fn test_unknown_provider_errors() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hello", None, Some("bogus"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unknown"));
    }

    #[tokio::test]
    async fn test_piper_requires_model_hint() {
        let backend = MultiTtsBackend::new();
        let _guard = EnvVarGuard::new("PIPER_MODEL");
        hermes_core::test_env::remove_var("PIPER_MODEL");
        let err = backend
            .synthesize("hello", None, Some("piper"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("PIPER_MODEL") || err.to_string().contains("piper"));
    }

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                old: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(v) = &self.old {
                hermes_core::test_env::set_var(self.key, v);
            } else {
                hermes_core::test_env::remove_var(self.key);
            }
        }
    }
}
