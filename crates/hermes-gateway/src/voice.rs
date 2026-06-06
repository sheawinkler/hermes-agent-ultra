//! Voice mode management for the gateway.
//!
//! Handles voice message transcription (STT) and text-to-speech (TTS) responses.

use hermes_config::managed_gateway::resolve_openai_audio_api_key;
use hermes_core::AgentError;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::voice_mixer::{synth_ambient_pcm, VoiceMixer};

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
    pub voice_fx: VoiceFxConfig,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            state: VoiceState::Disabled,
            stt_provider: SttProvider::Whisper,
            tts_provider: TtsProvider::OpenAi,
            auto_detect_voice: false,
            language: None,
            voice_fx: VoiceFxConfig::default(),
        }
    }
}

/// Outgoing voice-channel effects: ambient bed plus ducked speech overlays.
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceFxConfig {
    pub enabled: bool,
    pub ambient_enabled: bool,
    pub ambient_gain: f32,
    pub duck_gain: f32,
    pub speech_gain: f32,
    pub ack_enabled: bool,
    pub ack_phrases: Vec<String>,
}

impl Default for VoiceFxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            ambient_enabled: true,
            ambient_gain: 0.18,
            duck_gain: 0.06,
            speech_gain: 1.0,
            ack_enabled: true,
            ack_phrases: vec![
                "Let me look into that.".to_string(),
                "One moment.".to_string(),
                "Checking on that now.".to_string(),
                "Give me a sec.".to_string(),
                "On it.".to_string(),
            ],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttProvider {
    Whisper,
    GroqWhisper,
    MistralVoxtral,
    DeepgramNova,
    Custom(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TtsProvider {
    OpenAi,
    ElevenLabs,
    Custom(String),
}

const OPENAI_STT_MODELS: &[&str] = &["whisper-1", "gpt-4o-mini-transcribe", "gpt-4o-transcribe"];
const GROQ_STT_MODELS: &[&str] = &[
    "whisper-large-v3",
    "whisper-large-v3-turbo",
    "distil-whisper-large-v3-en",
];
const MISTRAL_STT_MODELS: &[&str] = &[
    "voxtral-mini-latest",
    "voxtral-mini-2507",
    "voxtral-mini-2602",
];

fn env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_or_default(keys: &[&str], default: &str) -> String {
    keys.iter()
        .find_map(|key| env_trimmed(key))
        .unwrap_or_else(|| default.to_string())
}

fn endpoint_from_base(base: String) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/audio/transcriptions") {
        base.to_string()
    } else {
        format!("{base}/audio/transcriptions")
    }
}

fn known_model(model: &str, models: &[&str]) -> bool {
    models.iter().any(|m| model.eq_ignore_ascii_case(m))
}

fn normalize_gateway_stt_model(provider: &str, requested: Option<&str>) -> String {
    let requested = requested.map(str::trim).filter(|s| !s.is_empty());
    match provider {
        "groq" => {
            let default = env_or_default(&["STT_GROQ_MODEL"], "whisper-large-v3-turbo");
            match requested {
                Some(model)
                    if known_model(model, OPENAI_STT_MODELS)
                        || known_model(model, MISTRAL_STT_MODELS) =>
                {
                    default
                }
                Some(model) => model.to_string(),
                None => default,
            }
        }
        "mistral" => {
            let default = env_or_default(&["STT_MISTRAL_MODEL"], "voxtral-mini-latest");
            match requested {
                Some(model)
                    if known_model(model, OPENAI_STT_MODELS)
                        || known_model(model, GROQ_STT_MODELS) =>
                {
                    default
                }
                Some(model) => model.to_string(),
                None => default,
            }
        }
        _ => {
            let default = env_or_default(&["STT_OPENAI_MODEL"], "whisper-1");
            match requested {
                Some(model)
                    if known_model(model, GROQ_STT_MODELS)
                        || known_model(model, MISTRAL_STT_MODELS) =>
                {
                    default
                }
                Some(model) => model.to_string(),
                None => default,
            }
        }
    }
}

fn configured_gateway_stt_model(provider: &str) -> String {
    let provider_key = match provider {
        "groq" => "STT_GROQ_MODEL",
        "mistral" => "STT_MISTRAL_MODEL",
        _ => "STT_OPENAI_MODEL",
    };
    let requested = env_trimmed(provider_key)
        .or_else(|| env_trimmed("HERMES_STT_MODEL"))
        .or_else(|| env_trimmed("STT_MODEL"));
    normalize_gateway_stt_model(provider, requested.as_deref())
}

fn gateway_transcription_response_format(provider: &str, model: &str) -> &'static str {
    match provider {
        "groq" => "text",
        "mistral" => "json",
        _ if model.eq_ignore_ascii_case("whisper-1") => "text",
        _ => "json",
    }
}

/// Voice mode manager.
pub struct VoiceManager {
    config: VoiceConfig,
    joined_channels: Mutex<HashMap<String, HashSet<String>>>,
    voice_mixers: Mutex<HashMap<String, VoiceMixer>>,
    ack_phrase_cursor: Mutex<usize>,
}

impl VoiceManager {
    pub fn new(config: VoiceConfig) -> Self {
        Self {
            config,
            joined_channels: Mutex::new(HashMap::new()),
            voice_mixers: Mutex::new(HashMap::new()),
            ack_phrase_cursor: Mutex::new(0),
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
            SttProvider::GroqWhisper => self.transcribe_groq(audio_data, format).await,
            SttProvider::MistralVoxtral => self.transcribe_mistral(audio_data, format).await,
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
        drop(lock);
        self.install_voice_mixer_if_enabled(&platform, &channel_id)?;
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
        drop(lock);
        self.remove_voice_mixer(&platform, &channel_id);
        tracing::info!(
            platform = platform,
            channel_id = channel_id,
            "Left voice channel"
        );
        Ok(())
    }

    fn install_voice_mixer_if_enabled(
        &self,
        platform: &str,
        channel_id: &str,
    ) -> Result<(), AgentError> {
        if !self.config.voice_fx.enabled {
            return Ok(());
        }
        let mixer = VoiceMixer::new(
            self.config.voice_fx.ambient_gain,
            self.config.voice_fx.duck_gain,
            self.config.voice_fx.speech_gain,
            400,
        );
        if self.config.voice_fx.ambient_enabled {
            let ambient = synth_ambient_pcm(4.0);
            mixer.set_ambient(Some(&ambient), None);
        }
        let mut mixers = self
            .voice_mixers
            .lock()
            .map_err(|_| AgentError::Io("voice mixer lock poisoned".into()))?;
        mixers.insert(Self::voice_mixer_key(platform, channel_id), mixer);
        Ok(())
    }

    fn remove_voice_mixer(&self, platform: &str, channel_id: &str) {
        let Ok(mut mixers) = self.voice_mixers.lock() else {
            return;
        };
        if let Some(mixer) = mixers.remove(&Self::voice_mixer_key(platform, channel_id)) {
            mixer.cleanup();
        }
    }

    pub fn voice_mixer_active(&self, platform: &str, channel_id: &str) -> bool {
        let Ok(mixers) = self.voice_mixers.lock() else {
            return false;
        };
        mixers.contains_key(&Self::voice_mixer_key(platform, channel_id))
    }

    pub fn play_voice_speech_pcm(
        &self,
        platform: &str,
        channel_id: &str,
        pcm: &[u8],
    ) -> Result<(), AgentError> {
        let platform = Self::normalize_identifier(platform, "platform")?;
        let channel_id = Self::normalize_identifier(channel_id, "channel_id")?;
        let mixers = self
            .voice_mixers
            .lock()
            .map_err(|_| AgentError::Io("voice mixer lock poisoned".into()))?;
        let mixer = mixers
            .get(&Self::voice_mixer_key(&platform, &channel_id))
            .ok_or_else(|| {
                AgentError::Config(format!(
                    "Voice mixer is not active for '{}' channel '{}'",
                    platform, channel_id
                ))
            })?;
        mixer.play_speech(pcm, Some(self.config.voice_fx.speech_gain), 40);
        Ok(())
    }

    pub fn read_voice_mixer_frame(
        &self,
        platform: &str,
        channel_id: &str,
    ) -> Result<Vec<u8>, AgentError> {
        let platform = Self::normalize_identifier(platform, "platform")?;
        let channel_id = Self::normalize_identifier(channel_id, "channel_id")?;
        let mixers = self
            .voice_mixers
            .lock()
            .map_err(|_| AgentError::Io("voice mixer lock poisoned".into()))?;
        let mixer = mixers
            .get(&Self::voice_mixer_key(&platform, &channel_id))
            .ok_or_else(|| {
                AgentError::Config(format!(
                    "Voice mixer is not active for '{}' channel '{}'",
                    platform, channel_id
                ))
            })?;
        Ok(mixer.read())
    }

    pub fn voice_speech_active(&self, platform: &str, channel_id: &str) -> bool {
        let Ok(mixers) = self.voice_mixers.lock() else {
            return false;
        };
        mixers
            .get(&Self::voice_mixer_key(platform, channel_id))
            .map(VoiceMixer::speech_active)
            .unwrap_or(false)
    }

    pub fn stop_voice_speech(&self, platform: &str, channel_id: &str) -> bool {
        let Ok(mixers) = self.voice_mixers.lock() else {
            return false;
        };
        let Some(mixer) = mixers.get(&Self::voice_mixer_key(platform, channel_id)) else {
            return false;
        };
        mixer.stop_speech();
        true
    }

    pub fn voice_ack_phrase(&self, platform: &str, channel_id: &str) -> Option<String> {
        if !self.config.voice_fx.ack_enabled || !self.voice_mixer_active(platform, channel_id) {
            return None;
        }
        let phrases: Vec<_> = self
            .config
            .voice_fx
            .ack_phrases
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if phrases.is_empty() {
            return None;
        }
        let Ok(mut cursor) = self.ack_phrase_cursor.lock() else {
            return Some(phrases[0].to_string());
        };
        let phrase = phrases[*cursor % phrases.len()].to_string();
        *cursor = cursor.saturating_add(1);
        Some(phrase)
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

    fn voice_mixer_key(platform: &str, channel_id: &str) -> String {
        format!(
            "{}:{}",
            platform.trim().to_ascii_lowercase(),
            channel_id.trim()
        )
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
        drop(lock);
        if let Ok(mut mixers) = self.voice_mixers.lock() {
            for (_, mixer) in mixers.drain() {
                mixer.cleanup();
            }
        }
        count
    }

    async fn transcribe_whisper(
        &self,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        let api_key = resolve_openai_audio_api_key();
        if api_key.is_empty() {
            return Err(AgentError::Config(
                "VOICE_TOOLS_OPENAI_KEY / HERMES_OPENAI_API_KEY (or OPENAI_API_KEY) not set for Whisper STT"
                    .into(),
            ));
        }
        let endpoint = endpoint_from_base(env_or_default(
            &["STT_OPENAI_BASE_URL", "OPENAI_BASE_URL"],
            "https://api.openai.com/v1",
        ));
        let model = configured_gateway_stt_model("openai");
        let response_format = gateway_transcription_response_format("openai", &model);
        self.transcribe_multipart_endpoint(
            "OpenAI Whisper",
            &endpoint,
            &api_key,
            audio_data,
            format,
            &model,
            response_format,
            true,
        )
        .await
    }

    async fn transcribe_groq(&self, audio_data: &[u8], format: &str) -> Result<String, AgentError> {
        let api_key = env_trimmed("GROQ_API_KEY")
            .ok_or_else(|| AgentError::Config("GROQ_API_KEY not set for Groq STT".into()))?;
        let endpoint = endpoint_from_base(env_or_default(
            &["STT_GROQ_BASE_URL", "GROQ_BASE_URL"],
            "https://api.groq.com/openai/v1",
        ));
        let model = configured_gateway_stt_model("groq");
        let response_format = gateway_transcription_response_format("groq", &model);
        self.transcribe_multipart_endpoint(
            "Groq Whisper",
            &endpoint,
            &api_key,
            audio_data,
            format,
            &model,
            response_format,
            true,
        )
        .await
    }

    async fn transcribe_mistral(
        &self,
        audio_data: &[u8],
        format: &str,
    ) -> Result<String, AgentError> {
        let api_key = env_trimmed("MISTRAL_API_KEY")
            .ok_or_else(|| AgentError::Config("MISTRAL_API_KEY not set for Mistral STT".into()))?;
        let endpoint = endpoint_from_base(env_or_default(
            &["STT_MISTRAL_BASE_URL", "MISTRAL_BASE_URL"],
            "https://api.mistral.ai/v1",
        ));
        let model = configured_gateway_stt_model("mistral");
        let response_format = gateway_transcription_response_format("mistral", &model);
        self.transcribe_multipart_endpoint(
            "Mistral Voxtral",
            &endpoint,
            &api_key,
            audio_data,
            format,
            &model,
            response_format,
            false,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn transcribe_multipart_endpoint(
        &self,
        provider_label: &str,
        endpoint: &str,
        api_key: &str,
        audio_data: &[u8],
        format: &str,
        model: &str,
        response_format: &str,
        send_response_format: bool,
    ) -> Result<String, AgentError> {
        let client = reqwest::Client::new();
        let part = reqwest::multipart::Part::bytes(audio_data.to_vec())
            .file_name(format!("audio.{}", format))
            .mime_str(&format!("audio/{}", format))
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;

        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string());
        if send_response_format {
            form = form.text("response_format", response_format.to_string());
        }
        if let Some(ref lang) = self.config.language {
            form = form.text("language", lang.clone());
        }

        let resp = client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| AgentError::LlmApi(format!("{provider_label} API error: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::LlmApi(format!(
                "{provider_label} error: {body}"
            )));
        }

        if response_format.eq_ignore_ascii_case("text") {
            let text = resp
                .text()
                .await
                .map_err(|e| AgentError::LlmApi(format!("{provider_label} read body: {e}")))?;
            return Ok(text.trim().to_string());
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| AgentError::LlmApi(format!("{provider_label} parse error: {e}")))?;
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
        let api_key = resolve_openai_audio_api_key();
        if api_key.is_empty() {
            return Err(AgentError::Config(
                "VOICE_TOOLS_OPENAI_KEY / HERMES_OPENAI_API_KEY (or OPENAI_API_KEY) not set for TTS"
                    .into(),
            ));
        }

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
    use crate::voice_mixer::{FRAME_SIZE, SILENCE_FRAME};
    use std::f32::consts::PI;
    use std::sync::{Mutex as StdMutex, MutexGuard, OnceLock};

    static ENV_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| StdMutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    struct EnvGuard {
        original: Vec<(&'static str, Option<String>)>,
        _g: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let g = env_lock();
            let keys = [
                "VOICE_TOOLS_OPENAI_KEY",
                "HERMES_OPENAI_API_KEY",
                "OPENAI_API_KEY",
                "GROQ_API_KEY",
                "MISTRAL_API_KEY",
                "STT_MODEL",
                "HERMES_STT_MODEL",
                "STT_OPENAI_MODEL",
                "STT_GROQ_MODEL",
                "STT_MISTRAL_MODEL",
                "STT_OPENAI_BASE_URL",
                "OPENAI_BASE_URL",
                "STT_GROQ_BASE_URL",
                "GROQ_BASE_URL",
                "STT_MISTRAL_BASE_URL",
                "MISTRAL_BASE_URL",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { original, _g: g }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.original {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn test_voice_state_default() {
        let config = VoiceConfig::default();
        assert_eq!(config.state, VoiceState::Disabled);
        assert_eq!(config.stt_provider, SttProvider::Whisper);
        assert!(!config.voice_fx.enabled);
        assert_eq!(config.voice_fx.ack_phrases[0], "Let me look into that.");
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
    async fn voice_fx_installs_mixer_on_join_and_removes_on_leave() {
        let mut config = VoiceConfig::default();
        config.state = VoiceState::FullDuplex;
        config.voice_fx.enabled = true;
        config.voice_fx.ack_phrases = vec!["One moment.".to_string()];
        let mgr = VoiceManager::new(config);

        mgr.join_voice_channel("discord", "voice-1")
            .await
            .expect("join should install mixer");
        assert!(mgr.voice_mixer_active("DISCORD", "voice-1"));
        assert_eq!(
            mgr.voice_ack_phrase("discord", "voice-1").as_deref(),
            Some("One moment.")
        );

        let ambient_frame = mgr
            .read_voice_mixer_frame("discord", "voice-1")
            .expect("mixer frame");
        assert_eq!(ambient_frame.len(), FRAME_SIZE);
        assert_ne!(ambient_frame, SILENCE_FRAME.to_vec());

        mgr.leave_voice_channel("discord", "voice-1")
            .await
            .expect("leave should remove mixer");
        assert!(!mgr.voice_mixer_active("discord", "voice-1"));
        assert!(mgr.voice_ack_phrase("discord", "voice-1").is_none());
    }

    #[tokio::test]
    async fn voice_fx_ack_phrases_rotate_and_skip_empty_values() {
        let mut config = VoiceConfig::default();
        config.state = VoiceState::FullDuplex;
        config.voice_fx.enabled = true;
        config.voice_fx.ack_phrases = vec![
            " ".to_string(),
            "One moment.".to_string(),
            "Checking now.".to_string(),
        ];
        let mgr = VoiceManager::new(config);
        mgr.join_voice_channel("discord", "voice-ack")
            .await
            .expect("join should install mixer");

        assert_eq!(
            mgr.voice_ack_phrase("discord", "voice-ack").as_deref(),
            Some("One moment.")
        );
        assert_eq!(
            mgr.voice_ack_phrase("discord", "voice-ack").as_deref(),
            Some("Checking now.")
        );
        assert_eq!(
            mgr.voice_ack_phrase("discord", "voice-ack").as_deref(),
            Some("One moment.")
        );
    }

    #[tokio::test]
    async fn voice_fx_speech_layers_and_can_stop() {
        let mut config = VoiceConfig::default();
        config.state = VoiceState::FullDuplex;
        config.voice_fx.enabled = true;
        config.voice_fx.ambient_enabled = false;
        let mgr = VoiceManager::new(config);
        mgr.join_voice_channel("discord", "voice-2")
            .await
            .expect("join should install mixer");

        assert_eq!(
            mgr.read_voice_mixer_frame("discord", "voice-2").unwrap(),
            SILENCE_FRAME.to_vec()
        );
        let speech = constant_pcm(16_000, 10);
        mgr.play_voice_speech_pcm("discord", "voice-2", &speech)
            .expect("speech should enqueue");
        assert!(mgr.voice_speech_active("discord", "voice-2"));
        let frame = mgr.read_voice_mixer_frame("discord", "voice-2").unwrap();
        assert_eq!(frame.len(), FRAME_SIZE);
        assert_ne!(frame, SILENCE_FRAME.to_vec());
        assert!(mgr.stop_voice_speech("discord", "voice-2"));
        assert!(!mgr.voice_speech_active("discord", "voice-2"));
    }

    #[tokio::test]
    async fn leave_all_channels_cleans_voice_mixers() {
        let mut config = VoiceConfig::default();
        config.state = VoiceState::FullDuplex;
        config.voice_fx.enabled = true;
        let mgr = VoiceManager::new(config);
        mgr.join_voice_channel("discord", "voice-a").await.unwrap();
        mgr.join_voice_channel("discord", "voice-b").await.unwrap();
        assert!(mgr.voice_mixer_active("discord", "voice-a"));
        assert_eq!(mgr.leave_all_channels(), 2);
        assert!(!mgr.voice_mixer_active("discord", "voice-a"));
        assert!(!mgr.voice_mixer_active("discord", "voice-b"));
    }

    #[test]
    fn voice_fx_play_requires_active_mixer() {
        let mgr = VoiceManager::new(VoiceConfig::default());
        let err = mgr
            .play_voice_speech_pcm("discord", "missing", &[1, 2, 3])
            .unwrap_err();
        assert!(err.to_string().contains("Voice mixer is not active"));
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

    fn constant_pcm(sample: i16, frames: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..(crate::voice_mixer::SAMPLES_PER_FRAME * crate::voice_mixer::CHANNELS * frames)
        {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        out
    }

    #[test]
    fn gateway_stt_model_normalizes_provider_mismatches() {
        let _g = EnvGuard::new();
        assert_eq!(normalize_gateway_stt_model("openai", None), "whisper-1");
        assert_eq!(
            normalize_gateway_stt_model("groq", Some("whisper-1")),
            "whisper-large-v3-turbo"
        );
        assert_eq!(
            normalize_gateway_stt_model("mistral", Some("whisper-large-v3")),
            "voxtral-mini-latest"
        );
        assert_eq!(
            normalize_gateway_stt_model("openai", Some("gpt-4o-mini-transcribe")),
            "gpt-4o-mini-transcribe"
        );
    }

    #[test]
    fn gateway_stt_endpoint_builds_from_provider_base_urls() {
        let _g = EnvGuard::new();
        assert_eq!(
            endpoint_from_base("https://api.groq.com/openai/v1".into()),
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        assert_eq!(
            endpoint_from_base("https://api.mistral.ai/v1/audio/transcriptions".into()),
            "https://api.mistral.ai/v1/audio/transcriptions"
        );
    }

    #[tokio::test]
    async fn gateway_whisper_prefers_voice_tools_key_and_reports_missing_keys() {
        let _g = EnvGuard::new();
        let mgr = VoiceManager::new(VoiceConfig::default());
        let err = mgr.transcribe(&[1, 2, 3, 4], "wav").await.unwrap_err();
        assert!(err.to_string().contains("VOICE_TOOLS_OPENAI_KEY"));

        std::env::set_var("VOICE_TOOLS_OPENAI_KEY", "voice-key");
        std::env::set_var("HERMES_OPENAI_API_KEY", "hermes-key");
        assert_eq!(resolve_openai_audio_api_key(), "voice-key");
    }

    #[tokio::test]
    async fn gateway_groq_and_mistral_stt_report_provider_credentials() {
        let _g = EnvGuard::new();

        let mut cfg = VoiceConfig::default();
        cfg.stt_provider = SttProvider::GroqWhisper;
        let groq = VoiceManager::new(cfg)
            .transcribe(&[1, 2, 3, 4], "wav")
            .await
            .unwrap_err();
        assert!(groq.to_string().contains("GROQ_API_KEY"));

        let mut cfg = VoiceConfig::default();
        cfg.stt_provider = SttProvider::MistralVoxtral;
        let mistral = VoiceManager::new(cfg)
            .transcribe(&[1, 2, 3, 4], "wav")
            .await
            .unwrap_err();
        assert!(mistral.to_string().contains("MISTRAL_API_KEY"));
    }
}
