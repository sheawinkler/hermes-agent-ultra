//! Voice mode management for the gateway.
//!
//! Handles voice message transcription (STT) and text-to-speech (TTS) responses.

use hermes_core::AgentError;

/// Voice mode state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceState {
    Disabled,
    ListenOnly,
    FullDuplex,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self::Disabled
    }
}

/// Voice mode configuration.
#[derive(Debug, Clone)]
pub struct VoiceConfig {
    pub state: VoiceState,
    pub stt_provider: SttProvider,
    pub tts_provider: TtsProvider,
    pub auto_detect_voice: bool,
    pub language: Option<String>,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            state: VoiceState::Disabled,
            stt_provider: SttProvider::Whisper,
            tts_provider: TtsProvider::OpenAi,
            auto_detect_voice: false,
            language: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttProvider {
    Whisper,
    DeepgramNova,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtsProvider {
    OpenAi,
    ElevenLabs,
    Custom(String),
}

/// Voice mode manager.
pub struct VoiceManager {
    config: VoiceConfig,
}

impl VoiceManager {
    pub fn new(config: VoiceConfig) -> Self {
        Self { config }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.state != VoiceState::Disabled
    }

    pub fn toggle(&mut self) -> VoiceState {
        self.config.state = match self.config.state {
            VoiceState::Disabled => VoiceState::FullDuplex,
            VoiceState::ListenOnly => VoiceState::Disabled,
            VoiceState::FullDuplex => VoiceState::Disabled,
        };
        self.config.state.clone()
    }

    /// Transcribe an audio file to text (STT).
    pub async fn transcribe(&self, audio_data: &[u8], format: &str) -> Result<String, AgentError> {
        match &self.config.stt_provider {
            SttProvider::Whisper => self.transcribe_whisper(audio_data, format).await,
            SttProvider::DeepgramNova => self.transcribe_deepgram(audio_data, format).await,
            SttProvider::Custom(url) => self.transcribe_custom(url, audio_data, format).await,
        }
    }

    /// Synthesize text to speech (TTS).
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>, AgentError> {
        match &self.config.tts_provider {
            TtsProvider::OpenAi => self.tts_openai(text).await,
            TtsProvider::ElevenLabs => self.tts_elevenlabs(text).await,
            TtsProvider::Custom(url) => self.tts_custom(url, text).await,
        }
    }

    async fn transcribe_whisper(&self, audio_data: &[u8], format: &str) -> Result<String, AgentError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| AgentError::Config("OPENAI_API_KEY not set for Whisper STT".into()))?;

        let client = reqwest::Client::new();
        let part = reqwest::multipart::Part::bytes(audio_data.to_vec())
            .file_name(format!("audio.{}", format))
            .mime_str(&format!("audio/{}", format))
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1");

        let form = if let Some(ref lang) = self.config.language {
            form.text("language", lang.clone())
        } else {
            form
        };

        let resp = client
            .post("https://api.openai.com/v1/audio/transcriptions")
            .header("Authorization", format!("Bearer {}", api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Whisper API error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("Whisper error: {body}")));
        }

        let json: serde_json::Value = resp.json().await
            .map_err(|e| AgentError::LlmApi(format!("Parse error: {e}")))?;
        Ok(json.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string())
    }

    async fn transcribe_deepgram(&self, _audio_data: &[u8], _format: &str) -> Result<String, AgentError> {
        Err(AgentError::Config("Deepgram STT not yet implemented".into()))
    }

    async fn transcribe_custom(&self, _url: &str, _audio_data: &[u8], _format: &str) -> Result<String, AgentError> {
        Err(AgentError::Config("Custom STT not yet implemented".into()))
    }

    async fn tts_openai(&self, text: &str) -> Result<Vec<u8>, AgentError> {
        let api_key = std::env::var("OPENAI_API_KEY")
            .map_err(|_| AgentError::Config("OPENAI_API_KEY not set for TTS".into()))?;

        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "model": "tts-1",
            "input": text,
            "voice": "alloy",
        });

        let resp = client
            .post("https://api.openai.com/v1/audio/speech")
            .header("Authorization", format!("Bearer {}", api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("TTS API error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("TTS error: {body}")));
        }

        let bytes = resp.bytes().await
            .map_err(|e| AgentError::LlmApi(format!("TTS read error: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn tts_elevenlabs(&self, _text: &str) -> Result<Vec<u8>, AgentError> {
        Err(AgentError::Config("ElevenLabs TTS not yet implemented".into()))
    }

    async fn tts_custom(&self, _url: &str, _text: &str) -> Result<Vec<u8>, AgentError> {
        Err(AgentError::Config("Custom TTS not yet implemented".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_voice_state_default() {
        let config = VoiceConfig::default();
        assert_eq!(config.state, VoiceState::Disabled);
    }

    #[test]
    fn test_voice_toggle() {
        let mut mgr = VoiceManager::new(VoiceConfig::default());
        assert!(!mgr.is_enabled());
        let state = mgr.toggle();
        assert_eq!(state, VoiceState::FullDuplex);
        assert!(mgr.is_enabled());
        let state = mgr.toggle();
        assert_eq!(state, VoiceState::Disabled);
        assert!(!mgr.is_enabled());
    }
}
