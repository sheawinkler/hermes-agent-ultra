//! Voice mode management for the gateway.
//!
//! Handles voice message transcription (STT) and text-to-speech (TTS) responses.

use hermes_core::AgentError;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

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
    joined_channels: Mutex<HashMap<String, HashSet<String>>>,
}

impl VoiceManager {
    pub fn new(config: VoiceConfig) -> Self {
        Self {
            config,
            joined_channels: Mutex::new(HashMap::new()),
        }
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

    /// Handle voice input: transcribe audio data and return text.
    ///
    /// Wraps the STT pipeline with format normalization and optional
    /// voice activity detection.
    pub async fn handle_voice_input(
        &self,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        if audio_data.is_empty() {
            return Ok(String::new());
        }

        // Run voice activity detection if enabled
        if self.config.auto_detect_voice {
            if !self.detect_voice_activity(audio_data) {
                return Ok(String::new());
            }
        }

        self.transcribe(audio_data, format).await
    }

    /// Handle voice output: synthesize text into audio bytes.
    ///
    /// Takes a TTS backend override or uses the configured default.
    pub async fn handle_voice_output(
        &self,
        text: &str,
        tts_backend: Option<&TtsProvider>,
    ) -> Result<Vec<u8>, AgentError> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }

        let provider = tts_backend.unwrap_or(&self.config.tts_provider);
        match provider {
            TtsProvider::OpenAi => self.tts_openai(text).await,
            TtsProvider::ElevenLabs => self.tts_elevenlabs(text).await,
            TtsProvider::Custom(url) => self.tts_custom(url, text).await,
        }
    }

    /// Join a voice channel on a platform (e.g., Discord voice channel).
    pub async fn join_voice_channel(
        &self,
        platform: &str,
        channel_id: &str,
    ) -> Result<(), AgentError> {
        if self.config.state == VoiceState::Disabled {
            return Err(AgentError::Config(
                "Voice mode is disabled; enable it before joining a channel".into(),
            ));
        }
        let platform = Self::normalize_identifier(platform, "platform")?;
        let channel_id = Self::normalize_identifier(channel_id, "channel_id")?;

        let mut lock = self
            .joined_channels
            .lock()
            .map_err(|_| AgentError::Io("voice join lock poisoned".into()))?;
        let entry = lock.entry(platform.clone()).or_default();
        if !entry.insert(channel_id.clone()) {
            tracing::debug!(
                platform = platform,
                channel_id = channel_id,
                "Voice channel already joined"
            );
            return Ok(());
        }
        tracing::info!(
            platform = platform,
            channel_id = channel_id,
            "Joined voice channel"
        );
        Ok(())
    }

    /// Leave a voice channel on a platform.
    pub async fn leave_voice_channel(
        &self,
        platform: &str,
        channel_id: &str,
    ) -> Result<(), AgentError> {
        let platform = Self::normalize_identifier(platform, "platform")?;
        let channel_id = Self::normalize_identifier(channel_id, "channel_id")?;

        let mut lock = self
            .joined_channels
            .lock()
            .map_err(|_| AgentError::Io("voice leave lock poisoned".into()))?;
        let Some(channels) = lock.get_mut(&platform) else {
            return Err(AgentError::Config(format!(
                "Voice channel is not joined for platform '{}'",
                platform
            )));
        };
        if !channels.remove(&channel_id) {
            return Err(AgentError::Config(format!(
                "Voice channel '{}' is not currently joined on '{}'",
                channel_id, platform
            )));
        }
        if channels.is_empty() {
            lock.remove(&platform);
        }
        tracing::info!(
            platform = platform,
            channel_id = channel_id,
            "Left voice channel"
        );
        Ok(())
    }

    /// Lightweight voice activity detection (VAD) using RMS energy.
    ///
    /// Uses 16-bit PCM little-endian RMS when possible and falls back to
    /// byte-level average amplitude otherwise.
    fn detect_voice_activity(&self, audio_data: &[u8]) -> bool {
        if audio_data.len() < 320 {
            return false;
        }

        // Prefer 16-bit PCM RMS when byte alignment suggests PCM frames.
        if audio_data.len() % 2 == 0 {
            let mut sum_sq = 0.0_f64;
            let mut n = 0usize;
            for frame in audio_data.chunks_exact(2) {
                let sample = i16::from_le_bytes([frame[0], frame[1]]) as f64 / 32768.0;
                sum_sq += sample * sample;
                n += 1;
            }
            if n > 0 {
                let rms = (sum_sq / n as f64).sqrt();
                let threshold = std::env::var("HERMES_VAD_RMS_THRESHOLD")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.01)
                    .clamp(0.001, 0.25);
                return rms >= threshold;
            }
        }

        let avg: f64 = audio_data
            .iter()
            .map(|&b| {
                let signed = b as i8;
                signed.unsigned_abs() as f64
            })
            .sum::<f64>()
            / audio_data.len() as f64;
        avg >= 12.0
    }

    fn normalize_identifier(value: &str, field: &str) -> Result<String, AgentError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(AgentError::Config(format!(
                "Voice {} must be non-empty",
                field
            )));
        }
        if trimmed.len() > 256 {
            return Err(AgentError::Config(format!(
                "Voice {} exceeds maximum length",
                field
            )));
        }
        Ok(trimmed.to_string())
    }

    pub fn is_joined(&self, platform: &str, channel_id: &str) -> bool {
        let lock = match self.joined_channels.lock() {
            Ok(l) => l,
            Err(_) => return false,
        };
        lock.get(platform)
            .map(|channels| channels.contains(channel_id))
            .unwrap_or(false)
    }

    pub fn joined_channel_count(&self) -> usize {
        let lock = match self.joined_channels.lock() {
            Ok(l) => l,
            Err(_) => return 0,
        };
        lock.values().map(|channels| channels.len()).sum()
    }

    pub fn leave_all_channels(&self) -> usize {
        let mut lock = match self.joined_channels.lock() {
            Ok(l) => l,
            Err(_) => return 0,
        };
        let count: usize = lock.values().map(|channels| channels.len()).sum();
        lock.clear();
        count
    }

    async fn transcribe_whisper(
        &self,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        let api_key = std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| {
                AgentError::Config(
                    "HERMES_OPENAI_API_KEY (or OPENAI_API_KEY) not set for Whisper STT".into(),
                )
            })?;

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

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Parse error: {e}")))?;
        Ok(json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn transcribe_deepgram(
        &self,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        let api_key = std::env::var("DEEPGRAM_API_KEY")
            .map_err(|_| AgentError::Config("DEEPGRAM_API_KEY not set for Deepgram STT".into()))?;

        let mime = match format.to_ascii_lowercase().as_str() {
            "wav" => "audio/wav",
            "mp3" => "audio/mpeg",
            "ogg" | "oga" => "audio/ogg",
            "webm" => "audio/webm",
            "flac" => "audio/flac",
            other => {
                return Err(AgentError::Config(format!(
                "Unsupported audio format for Deepgram: '{other}' (try wav, mp3, webm, flac, ogg)"
            )))
            }
        };

        let model = std::env::var("DEEPGRAM_MODEL").unwrap_or_else(|_| "nova-2".to_string());
        if !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AgentError::Config(
                "DEEPGRAM_MODEL must be alphanumeric (plus '-' or '_')".into(),
            ));
        }
        let url = format!("https://api.deepgram.com/v1/listen?model={}", model);

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Token {}", api_key))
            .header("Content-Type", mime)
            .body(audio_data.to_vec())
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Deepgram request error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("Deepgram error: {body}")));
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Deepgram parse error: {e}")))?;

        let transcript = json
            .pointer("/results/channels/0/alternatives/0/transcript")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        Ok(transcript)
    }

    async fn transcribe_custom(
        &self,
        url: &str,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        let mime = format!("audio/{}", format);
        let client = reqwest::Client::new();
        let mut req = client
            .post(url)
            .header("Content-Type", &mime)
            .body(audio_data.to_vec());

        if let Ok(h) = std::env::var("HERMES_CUSTOM_STT_AUTH_HEADER") {
            req = req.header("Authorization", h);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Custom STT request error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("Custom STT error: {body}")));
        }

        let ct = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();

        if ct.contains("json") {
            let json: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AgentError::LlmApi(format!("Custom STT JSON parse: {e}")))?;
            let text = json
                .get("text")
                .or_else(|| json.pointer("/transcript"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            Ok(text)
        } else {
            let text = resp
                .text()
                .await
                .map_err(|e| AgentError::LlmApi(format!("Custom STT read body: {e}")))?;
            Ok(text)
        }
    }

    async fn tts_openai(&self, text: &str) -> Result<Vec<u8>, AgentError> {
        let api_key = std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| {
                AgentError::Config(
                    "HERMES_OPENAI_API_KEY (or OPENAI_API_KEY) not set for TTS".into(),
                )
            })?;

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

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::LlmApi(format!("TTS read error: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn tts_elevenlabs(&self, text: &str) -> Result<Vec<u8>, AgentError> {
        let api_key = std::env::var("ELEVENLABS_API_KEY").map_err(|_| {
            AgentError::Config("ELEVENLABS_API_KEY not set for ElevenLabs TTS".into())
        })?;

        let voice_id = std::env::var("ELEVENLABS_VOICE_ID")
            .unwrap_or_else(|_| "21m00Tcm4TlvDq8ikWAM".to_string());
        if !voice_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AgentError::Config(
                "ELEVENLABS_VOICE_ID must be alphanumeric (plus '-' or '_')".into(),
            ));
        }

        let url = format!("https://api.elevenlabs.io/v1/text-to-speech/{}", voice_id);

        let client = reqwest::Client::new();
        let model_id = std::env::var("ELEVENLABS_MODEL_ID")
            .unwrap_or_else(|_| "eleven_turbo_v2_5".to_string());
        if !model_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(AgentError::Config(
                "ELEVENLABS_MODEL_ID must be alphanumeric (plus '-' or '_')".into(),
            ));
        }

        let body = serde_json::json!({
            "text": text,
            "model_id": model_id,
        });

        let resp = client
            .post(&url)
            .header("xi-api-key", api_key)
            .header("Accept", "audio/mpeg")
            .json(&body)
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("ElevenLabs TTS error: {e}")))?;

        if !resp.status().is_success() {
            let err_body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("ElevenLabs API: {err_body}")));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::LlmApi(format!("ElevenLabs read body: {e}")))?;
        Ok(bytes.to_vec())
    }

    async fn tts_custom(&self, url: &str, text: &str) -> Result<Vec<u8>, AgentError> {
        let client = reqwest::Client::new();
        let payload = serde_json::json!({ "input": text });
        let mut req = client.post(url).json(&payload);
        if let Ok(h) = std::env::var("HERMES_CUSTOM_TTS_AUTH_HEADER") {
            req = req.header("Authorization", h);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Custom TTS request error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!("Custom TTS error: {body}")));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Custom TTS read body: {e}")))?;
        Ok(bytes.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

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

    #[tokio::test]
    async fn test_voice_join_leave_lifecycle() {
        let mut config = VoiceConfig::default();
        config.state = VoiceState::FullDuplex;
        let mgr = VoiceManager::new(config);

        mgr.join_voice_channel("discord", "room-1")
            .await
            .expect("join should succeed");
        assert!(mgr.is_joined("discord", "room-1"));
        assert_eq!(mgr.joined_channel_count(), 1);

        // Idempotent join
        mgr.join_voice_channel("discord", "room-1")
            .await
            .expect("duplicate join should be no-op");
        assert_eq!(mgr.joined_channel_count(), 1);

        mgr.leave_voice_channel("discord", "room-1")
            .await
            .expect("leave should succeed");
        assert_eq!(mgr.joined_channel_count(), 0);
    }

    #[tokio::test]
    async fn test_join_requires_voice_enabled() {
        let mgr = VoiceManager::new(VoiceConfig::default());
        let err = mgr
            .join_voice_channel("discord", "room-1")
            .await
            .expect_err("disabled voice should reject join");
        assert!(err.to_string().contains("Voice mode is disabled"));
    }

    #[test]
    fn test_vad_detects_rms_energy() {
        let mut cfg = VoiceConfig::default();
        cfg.auto_detect_voice = true;
        let mgr = VoiceManager::new(cfg);

        // 200ms at 16 kHz sine wave with moderate amplitude.
        let mut pcm = Vec::new();
        let freq = 440.0_f32;
        let sample_rate = 16_000.0_f32;
        for i in 0..3200 {
            let t = i as f32 / sample_rate;
            let sample = (0.2 * (2.0 * PI * freq * t).sin() * i16::MAX as f32) as i16;
            pcm.extend_from_slice(&sample.to_le_bytes());
        }
        assert!(mgr.detect_voice_activity(&pcm));
    }
}
