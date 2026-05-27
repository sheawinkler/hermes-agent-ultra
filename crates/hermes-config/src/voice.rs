//! TTS / STT configuration blocks (parity with Python `config.yaml`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsOpenAiConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsElevenLabsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming_model_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsMiniMaxConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsMistralConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsGeminiConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsXaiConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bit_rate: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsEdgeConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsPiperConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsProviderEntry {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TtsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<TtsOpenAiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elevenlabs: Option<TtsElevenLabsConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimax: Option<TtsMiniMaxConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mistral: Option<TtsMistralConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<TtsGeminiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xai: Option<TtsXaiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<TtsEdgeConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piper: Option<TtsPiperConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<HashMap<String, TtsProviderEntry>>,
}

impl TtsConfig {
    /// Default TTS provider name (Python: `edge`).
    pub fn default_provider(&self) -> &str {
        self.provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("edge")
    }
}

// ---------------------------------------------------------------------------
// STT
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttOpenAiConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttLocalConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttGroqConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttMistralConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttXaiConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SttConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<SttOpenAiConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local: Option<SttLocalConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groq: Option<SttGroqConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mistral: Option<SttMistralConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xai: Option<SttXaiConfig>,
}

impl SttConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn default_provider(&self) -> &str {
        self.provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("openai")
    }

    pub fn openai_model(&self) -> &str {
        self.openai
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or("whisper-1")
    }

    pub fn openai_base_url(&self) -> String {
        if let Some(url) = self
            .openai
            .as_ref()
            .and_then(|c| c.base_url.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return url.trim_end_matches('/').to_string();
        }
        std::env::var("STT_OPENAI_BASE_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    pub fn groq_model(&self) -> String {
        self.groq
            .as_ref()
            .and_then(|c| c.model.clone())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("STT_GROQ_MODEL")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .unwrap_or_else(|| "whisper-large-v3-turbo".to_string())
    }

    pub fn mistral_model(&self) -> &str {
        self.mistral
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .filter(|s| !s.is_empty())
            .unwrap_or("voxtral-mini-latest")
    }
}

// ---------------------------------------------------------------------------
// Meeting recorder configuration
// ---------------------------------------------------------------------------

/// Transcription mode for the meeting recorder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTranscriptionMode {
    /// Real-time streaming ASR (e.g. Deepgram WebSocket).  Lower latency,
    /// shows live captions during the meeting.
    Realtime,
    /// Offline batch ASR (e.g. faster-whisper large-v3).  Higher accuracy,
    /// processed after the meeting ends.
    #[default]
    Offline,
}

/// Optional diarization backend for single-file offline transcription.
/// Not needed when dual-track (mic + loopback) capture is available.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiarizationProvider {
    /// Disable diarization (use channel labels from dual-track capture).
    #[default]
    None,
    /// pyannote-audio via HTTP sidecar (requires `PYANNOTE_TOKEN`).
    Pyannote,
    /// Local command (path configured via `diarization_command`).
    LocalCommand,
}

/// Configuration for the meeting recorder pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MeetingConfig {
    /// Transcription strategy: `realtime` or `offline` (default: `offline`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_mode: Option<MeetingTranscriptionMode>,

    /// Speaker diarization backend (default: `none`, relies on dual-track).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization_provider: Option<DiarizationProvider>,

    /// HTTP endpoint for pyannote sidecar (e.g. `http://localhost:8765`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pyannote_endpoint: Option<String>,

    /// Shell command template for local diarization.
    /// `{audio}` is replaced with the input WAV path.
    /// `{output}` is replaced with the RTTM output path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diarization_command: Option<String>,

    /// Minutes of transcript per LLM chunk summary (default: 10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_chunk_minutes: Option<u32>,

    /// Write structured notes into `holographic` memory_store.db (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_sink_enabled: Option<bool>,

    /// Directory for storing raw transcript files (default: `$HERMES_HOME/meetings/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcripts_dir: Option<String>,
}

impl MeetingConfig {
    pub fn transcription_mode(&self) -> MeetingTranscriptionMode {
        self.transcription_mode.clone().unwrap_or_default()
    }

    pub fn diarization_provider(&self) -> DiarizationProvider {
        self.diarization_provider.clone().unwrap_or_default()
    }

    pub fn summary_chunk_minutes(&self) -> u32 {
        self.summary_chunk_minutes.unwrap_or(10)
    }

    pub fn memory_sink_enabled(&self) -> bool {
        self.memory_sink_enabled.unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_defaults_enabled() {
        let cfg = SttConfig::default();
        assert!(cfg.is_enabled());
    }

    #[test]
    fn tts_default_provider_edge() {
        let cfg = TtsConfig::default();
        assert_eq!(cfg.default_provider(), "edge");
    }

    #[test]
    fn meeting_defaults() {
        let cfg = MeetingConfig::default();
        assert_eq!(cfg.transcription_mode(), MeetingTranscriptionMode::Offline);
        assert_eq!(cfg.diarization_provider(), DiarizationProvider::None);
        assert_eq!(cfg.summary_chunk_minutes(), 10);
        assert!(cfg.memory_sink_enabled());
    }
}
