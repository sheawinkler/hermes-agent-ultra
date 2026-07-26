//! MiniMax TTS backend for the current `t2a_v2` endpoint and the legacy
//! `text_to_speech` endpoint. The current endpoint returns JSON containing
//! hex-encoded audio; the legacy endpoint returns raw audio bytes.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::tts::TtsBackend;
use crate::tts_audio::{
    default_tts_audio_path, read_bounded_error, read_bounded_response,
    validate_requested_output_format, write_validated_audio, MAX_TTS_AUDIO_BYTES,
    MAX_TTS_JSON_BYTES,
};
use hermes_core::ToolError;

/// Default endpoint for MiniMax's `t2a_v2` (text-to-audio) API.
pub const DEFAULT_MINIMAX_BASE_URL: &str = "https://api.minimax.io/v1/t2a_v2";
/// Default MiniMax model (high-definition, multilingual).
pub const DEFAULT_MINIMAX_MODEL: &str = "speech-02-hd";
/// Default voice identifier.
pub const DEFAULT_MINIMAX_VOICE_ID: &str = "English_expressive_narrator";

/// The only emotion values accepted by the live MiniMax `t2a_v2` API.
pub const MINIMAX_T2A_V2_EMOTIONS: &[&str] = &[
    "happy",
    "sad",
    "angry",
    "fearful",
    "disgusted",
    "surprised",
    "calm",
    "fluent",
    "whisper",
];

/// Voice / audio settings mirroring the Python config keys.
#[derive(Debug, Clone)]
pub struct MiniMaxVoiceSettings {
    pub voice_id: String,
    /// 0.5 – 2.0. Default: 1.
    pub speed: f32,
    /// 0.1 – 10. Default: 1.
    pub vol: f32,
    /// -12 – 12. Default: 0.
    pub pitch: i32,
    /// Optional MiniMax `t2a_v2` speaking emotion. When absent, the field is
    /// omitted so MiniMax selects its own default.
    pub emotion: Option<String>,
    /// Desired sample rate in Hz. Default: 32000.
    pub sample_rate: u32,
    /// Desired bitrate in bps. Default: 128000.
    pub bitrate: u32,
    /// One of `"mp3"`, `"wav"`, `"flac"` (lower case).
    pub format: String,
    pub channel: u8,
}

impl Default for MiniMaxVoiceSettings {
    fn default() -> Self {
        Self {
            voice_id: DEFAULT_MINIMAX_VOICE_ID.to_string(),
            speed: 1.0,
            vol: 1.0,
            pitch: 0,
            emotion: None,
            sample_rate: 32_000,
            bitrate: 128_000,
            format: "mp3".to_string(),
            channel: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// MiniMax TTS backend. Stateless except for the shared reqwest client.
pub struct MiniMaxTtsBackend {
    client: Client,
    api_key: Option<String>,
    base_url: String,
    model: String,
    settings: MiniMaxVoiceSettings,
}

fn config_string(config: &Value, key: &str) -> Option<String> {
    config
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn config_f32(config: &Value, key: &str) -> Option<f32> {
    config
        .get(key)
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value as f32)
}

fn normalize_minimax_tts_endpoint(raw: &str) -> String {
    let raw = raw.trim();
    let Ok(mut url) = url::Url::parse(raw) else {
        return raw.to_string();
    };
    let path = url.path().trim_end_matches('/');
    let endpoint_path = if path.ends_with("/t2a_v2") {
        return url.to_string();
    } else if path.is_empty() || path == "/" {
        Some("/v1/t2a_v2".to_string())
    } else if path.ends_with("/v1") {
        Some(format!("{path}/t2a_v2"))
    } else if path == "/anthropic"
        || path.starts_with("/anthropic/")
        || path.contains("chatcompletion")
        || path.contains("/text/")
    {
        Some("/v1/t2a_v2".to_string())
    } else {
        None
    };
    if let Some(endpoint_path) = endpoint_path {
        url.set_path(&endpoint_path);
    }
    url.to_string()
}

fn with_minimax_group_id(endpoint: String, group_id: Option<&str>) -> String {
    let Some(group_id) = group_id.map(str::trim).filter(|value| !value.is_empty()) else {
        return endpoint;
    };
    let Ok(mut url) = url::Url::parse(&endpoint) else {
        return endpoint;
    };
    if !url.query_pairs().any(|(key, _)| key == "GroupId") {
        url.query_pairs_mut().append_pair("GroupId", group_id);
    }
    url.to_string()
}

fn is_legacy_text_to_speech_endpoint(endpoint: &str) -> bool {
    url::Url::parse(endpoint).ok().is_some_and(|url| {
        url.path()
            .trim_end_matches('/')
            .ends_with("/text_to_speech")
    })
}

fn minimax_http_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("valid MiniMax TTS HTTP client")
}

impl MiniMaxTtsBackend {
    /// Construct from environment variables and default voice settings.
    pub fn from_env() -> Self {
        Self::from_tts_config(&Value::Null)
    }

    pub fn from_env_with(settings: MiniMaxVoiceSettings) -> Self {
        let base_url = std::env::var("MINIMAX_TTS_BASE_URL")
            .or_else(|_| std::env::var("MINIMAX_BASE_URL"))
            .unwrap_or_else(|_| DEFAULT_MINIMAX_BASE_URL.to_string());
        let group_id = std::env::var("MINIMAX_GROUP_ID")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        Self {
            client: minimax_http_client(),
            api_key: std::env::var("MINIMAX_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            base_url: with_minimax_group_id(
                normalize_minimax_tts_endpoint(&base_url),
                group_id.as_deref(),
            ),
            model: std::env::var("MINIMAX_TTS_MODEL")
                .or_else(|_| std::env::var("MINIMAX_MODEL"))
                .unwrap_or_else(|_| DEFAULT_MINIMAX_MODEL.to_string()),
            settings,
        }
    }

    pub fn from_tts_config(tts_config: &Value) -> Self {
        let null = Value::Null;
        let minimax = tts_config.get("minimax").unwrap_or(&null);
        let mut settings = MiniMaxVoiceSettings::default();
        if let Some(voice_id) = config_string(minimax, "voice_id") {
            settings.voice_id = voice_id;
        }
        if let Some(speed) =
            config_f32(minimax, "speed").or_else(|| config_f32(tts_config, "speed"))
        {
            settings.speed = speed.clamp(0.5, 2.0);
        }
        if let Some(vol) = config_f32(minimax, "vol") {
            settings.vol = vol.clamp(0.1, 10.0);
        }
        if let Some(pitch) = minimax.get("pitch").and_then(Value::as_i64) {
            settings.pitch = pitch.clamp(-12, 12) as i32;
        }
        if let Some(emotion) = config_string(minimax, "emotion") {
            settings.emotion = Some(emotion);
        }
        if let Some(sample_rate) = minimax
            .get("sample_rate")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        {
            settings.sample_rate = sample_rate.min(u32::MAX as u64) as u32;
        }
        if let Some(bitrate) = minimax
            .get("bitrate")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
        {
            settings.bitrate = bitrate.min(u32::MAX as u64) as u32;
        }
        if let Some(format) = config_string(minimax, "format")
            .map(|value| value.to_ascii_lowercase())
            .filter(|value| matches!(value.as_str(), "mp3" | "wav" | "flac"))
        {
            settings.format = format;
        }
        if let Some(channel) = minimax
            .get("channel")
            .and_then(Value::as_u64)
            .filter(|value| matches!(*value, 1 | 2))
        {
            settings.channel = channel as u8;
        }

        let base_url = std::env::var("MINIMAX_TTS_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| config_string(minimax, "base_url"))
            .or_else(|| {
                std::env::var("MINIMAX_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_MINIMAX_BASE_URL.to_string());
        let model = std::env::var("MINIMAX_TTS_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| config_string(minimax, "model"))
            .or_else(|| {
                std::env::var("MINIMAX_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_MINIMAX_MODEL.to_string());
        let api_key = std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| config_string(minimax, "api_key"));
        let group_id = config_string(minimax, "group_id").or_else(|| {
            std::env::var("MINIMAX_GROUP_ID")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        });

        Self {
            client: minimax_http_client(),
            api_key,
            base_url: with_minimax_group_id(
                normalize_minimax_tts_endpoint(&base_url),
                group_id.as_deref(),
            ),
            model,
            settings,
        }
    }

    pub fn has_api_key(&self) -> bool {
        self.api_key.is_some()
    }
    fn output_format(&self) -> &'static str {
        if is_legacy_text_to_speech_endpoint(&self.base_url) {
            "mp3"
        } else {
            match self.settings.format.as_str() {
                "wav" => "wav",
                "flac" => "flac",
                _ => "mp3",
            }
        }
    }

    /// Construct with explicit config for focused tests.
    #[cfg(test)]
    fn with_config(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        settings: MiniMaxVoiceSettings,
    ) -> Self {
        Self {
            client: minimax_http_client(),
            api_key: Some(api_key.into()),
            base_url: normalize_minimax_tts_endpoint(&base_url.into()),
            model: model.into(),
            settings,
        }
    }

    /// Build the MiniMax `t2a_v2` JSON body. An explicit voice overrides the
    /// configured voice; all other settings come from `tts.minimax`.
    ///
    /// This compatibility helper never serializes an unsupported emotion.
    /// Request paths must call `try_build_payload` (or the private
    /// speed-aware variant) so an invalid configuration is reported instead
    /// of silently being ignored.
    pub fn build_payload(&self, text: &str, voice: Option<&str>) -> Value {
        self.build_payload_unchecked(text, voice, None)
    }

    /// Build a request payload only when the optional configured emotion is
    /// accepted by MiniMax's live `t2a_v2` enum.
    pub fn try_build_payload(&self, text: &str, voice: Option<&str>) -> Result<Value, ToolError> {
        self.validate_configured_emotion()?;
        Ok(self.build_payload_unchecked(text, voice, None))
    }

    fn validate_configured_emotion(&self) -> Result<(), ToolError> {
        let Some(emotion) = self.settings.emotion.as_deref() else {
            return Ok(());
        };
        if MINIMAX_T2A_V2_EMOTIONS.contains(&emotion) {
            return Ok(());
        }
        Err(ToolError::InvalidParams(format!(
            "Unsupported MiniMax t2a_v2 emotion '{emotion}'. Allowed values: {}",
            MINIMAX_T2A_V2_EMOTIONS.join("|")
        )))
    }

    fn build_payload_with_speed(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
    ) -> Result<Value, ToolError> {
        self.validate_configured_emotion()?;
        Ok(self.build_payload_unchecked(text, voice, speed))
    }

    fn build_payload_unchecked(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
    ) -> Value {
        let voice_id = voice.unwrap_or(&self.settings.voice_id);
        let speed = speed
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.5, 2.0) as f32)
            .unwrap_or(self.settings.speed);
        if is_legacy_text_to_speech_endpoint(&self.base_url) {
            return json!({
                "model": self.model,
                "text": text,
                "voice_id": voice_id,
            });
        }

        let mut voice_setting = json!({
            "voice_id": voice_id,
            "speed": speed,
            "vol": self.settings.vol,
            "pitch": self.settings.pitch,
        });
        if let Some(emotion) = self
            .settings
            .emotion
            .as_deref()
            .filter(|emotion| MINIMAX_T2A_V2_EMOTIONS.contains(emotion))
        {
            voice_setting["emotion"] = json!(emotion);
        }
        json!({
            "model": self.model,
            "text": text,
            "stream": false,
            "voice_setting": voice_setting,
            "audio_setting": {
                "sample_rate": self.settings.sample_rate,
                "bitrate": self.settings.bitrate,
                "format": self.settings.format,
                "channel": self.settings.channel,
            },
        })
    }

    async fn synthesize_to_bytes(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f64>,
    ) -> Result<Vec<u8>, ToolError> {
        let payload = self.build_payload_with_speed(text, voice, speed)?;
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("MINIMAX_API_KEY not set".into()))?;

        let resp = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json")
            .bearer_auth(api_key)
            .json(&payload)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("MiniMax request failed: {e}")))?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !status.is_success() {
            let detail = read_bounded_error(resp).await;
            return Err(ToolError::ExecutionFailed(format!(
                "MiniMax HTTP {status}: {detail}"
            )));
        }

        if !is_legacy_text_to_speech_endpoint(&self.base_url) {
            let body =
                read_bounded_response(resp, MAX_TTS_JSON_BYTES, "MiniMax JSON response").await?;
            let body: Value = serde_json::from_slice(&body).map_err(|e| {
                ToolError::ExecutionFailed(format!("MiniMax JSON decode failed: {e}"))
            })?;
            return parse_response_audio(&body);
        }

        if content_type.starts_with("audio/") {
            return read_bounded_response(resp, MAX_TTS_AUDIO_BYTES, "MiniMax audio").await;
        }
        let body = read_bounded_response(resp, MAX_TTS_JSON_BYTES, "MiniMax JSON response").await?;
        if let Ok(value) = serde_json::from_slice::<Value>(&body) {
            let status_code = value
                .get("base_resp")
                .and_then(|base| base.get("status_code"))
                .and_then(Value::as_i64)
                .unwrap_or(-1);
            if status_code != 0 {
                let status_msg = value
                    .get("base_resp")
                    .and_then(|base| base.get("status_msg"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(ToolError::ExecutionFailed(format!(
                    "MiniMax TTS API error (code {status_code}): {status_msg}"
                )));
            }
        }
        Err(ToolError::ExecutionFailed(format!(
            "MiniMax TTS returned unexpected Content-Type '{content_type}' ({} bytes)",
            body.len()
        )))
    }
}

impl Default for MiniMaxTtsBackend {
    fn default() -> Self {
        Self::from_env()
    }
}

#[async_trait]
impl TtsBackend for MiniMaxTtsBackend {
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        _provider: Option<&str>,
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError> {
        let suffix = self.output_format();
        validate_requested_output_format(output_path, suffix, "minimax")?;
        let audio = self.synthesize_to_bytes(text, voice, speed).await?;
        let path = match output_path {
            Some(path) => std::path::PathBuf::from(path),
            None => default_tts_audio_path("hermes_minimax", suffix)?,
        };
        let written = write_validated_audio(&path, &audio, suffix, "minimax").await?;
        Ok(json!({
            "provider": "minimax",
            "file": path.display().to_string(),
            "voice": voice.unwrap_or(&self.settings.voice_id),
            "model": self.model,
            "bytes": written,
            "format": suffix,
        })
        .to_string())
    }
}

// ---------------------------------------------------------------------------
// Response parsing (shared with tests)
// ---------------------------------------------------------------------------

/// Extract and hex-decode the audio bytes from a successful MiniMax JSON
/// body. Returns `ToolError::ExecutionFailed` on any shape mismatch,
/// non-zero `base_resp.status_code`, or invalid hex.
pub fn parse_response_audio(body: &Value) -> Result<Vec<u8>, ToolError> {
    let base_resp = body.get("base_resp").cloned().unwrap_or(Value::Null);
    let status_code = base_resp
        .get("status_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    if status_code != 0 {
        let msg = base_resp
            .get("status_msg")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(ToolError::ExecutionFailed(format!(
            "MiniMax TTS error (code {status_code}): {msg}"
        )));
    }
    let hex_audio = body
        .get("data")
        .and_then(|d| d.get("audio"))
        .and_then(|a| a.as_str())
        .unwrap_or("");
    if hex_audio.is_empty() {
        return Err(ToolError::ExecutionFailed(
            "MiniMax TTS returned empty audio data".into(),
        ));
    }
    decode_hex(hex_audio)
        .map_err(|e| ToolError::ExecutionFailed(format!("MiniMax TTS returned malformed hex: {e}")))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("odd number of hex digits".into());
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Result<u8, String> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(format!("invalid hex char '{}'", c as char)),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    fn tiny_wav() -> Vec<u8> {
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&38_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&1_u16.to_le_bytes());
        wav.extend_from_slice(&8_000_u32.to_le_bytes());
        wav.extend_from_slice(&16_000_u32.to_le_bytes());
        wav.extend_from_slice(&2_u16.to_le_bytes());
        wav.extend_from_slice(&16_u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&2_u32.to_le_bytes());
        wav.extend_from_slice(&0_i16.to_le_bytes());
        wav
    }
    fn tiny_mp3_frame() -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        frame
    }

    #[test]
    fn endpoint_normalization_rewrites_known_roots_and_preserves_query() {
        assert_eq!(
            normalize_minimax_tts_endpoint("https://api.minimax.io"),
            "https://api.minimax.io/v1/t2a_v2"
        );
        assert_eq!(
            normalize_minimax_tts_endpoint("https://api.minimax.io/anthropic?GroupId=group-123"),
            "https://api.minimax.io/v1/t2a_v2?GroupId=group-123"
        );
        assert_eq!(
            normalize_minimax_tts_endpoint("https://proxy.example/custom/minimax-tts?tenant=alpha"),
            "https://proxy.example/custom/minimax-tts?tenant=alpha"
        );
        assert_eq!(
            normalize_minimax_tts_endpoint("not a valid URL"),
            "not a valid URL"
        );
    }

    #[test]
    fn custom_proxy_path_defaults_to_current_payload_shape() {
        let backend = MiniMaxTtsBackend::with_config(
            "key",
            "https://proxy.example/custom/minimax-tts?tenant=alpha",
            DEFAULT_MINIMAX_MODEL,
            MiniMaxVoiceSettings::default(),
        );
        let payload = backend.build_payload("custom proxy", None);
        assert!(payload.get("voice_setting").is_some());
        assert!(payload.get("audio_setting").is_some());
        assert!(payload.get("voice_id").is_none());
    }

    #[test]
    fn group_id_is_added_once_without_losing_existing_query() {
        let endpoint = with_minimax_group_id(
            "https://api.minimax.io/v1/t2a_v2?tenant=alpha".to_string(),
            Some("group 123"),
        );
        assert_eq!(
            endpoint,
            "https://api.minimax.io/v1/t2a_v2?tenant=alpha&GroupId=group+123"
        );
        let preset = with_minimax_group_id(
            "https://api.minimax.io/v1/t2a_v2?GroupId=preset".to_string(),
            Some("ignored"),
        );
        assert_eq!(preset, "https://api.minimax.io/v1/t2a_v2?GroupId=preset");
    }

    #[test]
    fn tts_config_voice_and_audio_settings_reach_payload_without_overrides() {
        let backend = MiniMaxTtsBackend::from_tts_config(&json!({
            "speed": 1.1,
            "minimax": {
                "voice_id": "Configured_Custom_Voice",
                "speed": 1.25,
                "vol": 2.0,
                "pitch": -3,
                "sample_rate": 44_100,
                "bitrate": 192_000,
                "format": "wav",
                "channel": 2
            }
        }));
        let payload = backend.build_payload("configured speech", None);
        assert_eq!(
            payload["voice_setting"]["voice_id"],
            "Configured_Custom_Voice"
        );
        assert_eq!(payload["voice_setting"]["speed"], 1.25);
        assert_eq!(payload["voice_setting"]["vol"], 2.0);
        assert_eq!(payload["voice_setting"]["pitch"], -3);
        assert_eq!(payload["audio_setting"]["sample_rate"], 44_100);
        assert_eq!(payload["audio_setting"]["bitrate"], 192_000);
        assert_eq!(payload["audio_setting"]["format"], "wav");
        assert_eq!(payload["audio_setting"]["channel"], 2);
    }

    #[tokio::test]
    async fn legacy_text_to_speech_uses_flat_payload_and_raw_audio() {
        let server = MockServer::start().await;
        let mp3 = tiny_mp3_frame();
        Mock::given(method("POST"))
            .and(path("/v1/text_to_speech"))
            .and(body_json(json!({
                "model": DEFAULT_MINIMAX_MODEL,
                "text": "legacy speech",
                "voice_id": DEFAULT_MINIMAX_VOICE_ID
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(mp3.clone()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut settings = MiniMaxVoiceSettings::default();
        settings.format = "wav".to_string();
        let backend = MiniMaxTtsBackend::with_config(
            "key",
            format!("{}/v1/text_to_speech", server.uri()),
            DEFAULT_MINIMAX_MODEL,
            settings,
        );
        let result = backend
            .synthesize("legacy speech", None, None, None, None)
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        let path = result["file"].as_str().unwrap();
        assert!(path.ends_with(".mp3"));
        assert_eq!(tokio::fs::read(path).await.unwrap(), mp3);
        tokio::fs::remove_file(path).await.unwrap();
    }
    #[tokio::test]
    async fn modern_synthesize_validates_audio_and_requested_extension() {
        let server = MockServer::start().await;
        let wav = tiny_wav();
        let hex_audio = wav
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Mock::given(method("POST"))
            .and(path("/v1/t2a_v2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "base_resp": {"status_code": 0},
                "data": {"audio": hex_audio}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let mut settings = MiniMaxVoiceSettings::default();
        settings.format = "wav".to_string();
        let backend = MiniMaxTtsBackend::with_config(
            "key",
            format!("{}/v1/t2a_v2", server.uri()),
            DEFAULT_MINIMAX_MODEL,
            settings,
        );
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path().join("speech.wav");
        let result = backend
            .synthesize(
                "modern speech",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["format"], "wav");
        assert_eq!(tokio::fs::read(&output).await.unwrap(), wav);

        let mismatch = temp.path().join("speech.mp3");
        let error = backend
            .synthesize(
                "must fail before request",
                None,
                None,
                Some(mismatch.to_str().unwrap()),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("must end in .wav"));
        assert!(!mismatch.exists());
    }

    #[tokio::test]
    async fn minimax_rejects_cross_origin_redirect_without_leaking_bearer_or_text() {
        let source = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/t2a_v2"))
            .respond_with(
                ResponseTemplate::new(308)
                    .insert_header("Location", format!("{}/capture", destination.uri()))
                    .set_body_string("redirect blocked"),
            )
            .expect(1)
            .mount(&source)
            .await;
        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("redirect.mp3");
        let backend = MiniMaxTtsBackend::with_config(
            "minimax-secret",
            format!("{}/v1/t2a_v2", source.uri()),
            DEFAULT_MINIMAX_MODEL,
            MiniMaxVoiceSettings::default(),
        );

        let error = backend
            .synthesize(
                "private MiniMax text",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .expect_err("redirect responses must fail closed");
        assert!(error.to_string().contains("MiniMax HTTP 308"));
        assert!(error.to_string().contains("redirect blocked"));
        assert!(
            destination.received_requests().await.unwrap().is_empty(),
            "redirect destination must receive neither bearer token nor request body"
        );
        assert!(!output.exists());
    }

    #[test]
    fn payload_shape_matches_python() {
        let backend = MiniMaxTtsBackend::with_config(
            "key",
            "https://example/v1/t2a_v2",
            DEFAULT_MINIMAX_MODEL,
            MiniMaxVoiceSettings::default(),
        );
        let p = backend.build_payload("hello", None);
        assert_eq!(p["model"], DEFAULT_MINIMAX_MODEL);
        assert_eq!(p["text"], "hello");
        assert_eq!(p["stream"], false);
        assert_eq!(p["voice_setting"]["voice_id"], DEFAULT_MINIMAX_VOICE_ID);
        assert_eq!(p["voice_setting"]["speed"], 1.0);
        assert!(p["voice_setting"].get("emotion").is_none());
        assert_eq!(p["audio_setting"]["sample_rate"], 32_000);
        assert_eq!(p["audio_setting"]["bitrate"], 128_000);
        assert_eq!(p["audio_setting"]["format"], "mp3");
        assert_eq!(p["audio_setting"]["channel"], 1);
    }

    #[test]
    fn voice_override_applies_to_payload() {
        let backend = MiniMaxTtsBackend::with_config(
            "k",
            "https://x/v1/t2a_v2",
            "m",
            MiniMaxVoiceSettings::default(),
        );
        let p = backend.build_payload("hi", Some("Custom_Voice"));
        assert_eq!(p["voice_setting"]["voice_id"], "Custom_Voice");
    }

    #[test]
    fn every_live_t2a_v2_emotion_is_serialized_when_configured() {
        for emotion in MINIMAX_T2A_V2_EMOTIONS {
            let backend = MiniMaxTtsBackend::from_tts_config(&json!({
                "minimax": {"emotion": emotion}
            }));
            let payload = backend.try_build_payload("hello", None).unwrap();
            assert_eq!(payload["voice_setting"]["emotion"], *emotion);
        }
    }

    #[test]
    fn unsupported_t2a_v2_emotions_are_rejected() {
        for emotion in ["neutral", "joyful", "Happy"] {
            let backend = MiniMaxTtsBackend::from_tts_config(&json!({
                "minimax": {"emotion": emotion}
            }));
            let error = backend.try_build_payload("hello", None).unwrap_err();
            assert!(matches!(error, ToolError::InvalidParams(_)));
            assert!(error
                .to_string()
                .contains("Unsupported MiniMax t2a_v2 emotion"));
        }
    }

    #[tokio::test]
    async fn invalid_emotion_fails_before_making_a_minimax_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let output_dir = tempfile::tempdir().unwrap();
        for emotion in ["neutral", "unknown"] {
            let mut settings = MiniMaxVoiceSettings::default();
            settings.emotion = Some(emotion.to_string());
            let backend = MiniMaxTtsBackend::with_config(
                "key",
                format!("{}/v1/t2a_v2", server.uri()),
                DEFAULT_MINIMAX_MODEL,
                settings,
            );
            let output = output_dir.path().join(format!("{emotion}.mp3"));
            let error = backend
                .synthesize(
                    "must not reach MiniMax",
                    None,
                    None,
                    Some(output.to_str().unwrap()),
                    None,
                )
                .await
                .unwrap_err();

            assert!(matches!(error, ToolError::InvalidParams(_)));
            assert!(error.to_string().contains(emotion));
            assert!(!output.exists());
        }
        server.verify().await;
    }

    #[tokio::test]
    async fn crlf_output_path_fails_before_making_a_minimax_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        let mut settings = MiniMaxVoiceSettings::default();
        settings.format = "wav".to_string();
        let backend = MiniMaxTtsBackend::with_config(
            "key",
            format!("{}/v1/t2a_v2", server.uri()),
            DEFAULT_MINIMAX_MODEL,
            settings,
        );
        let output_dir = tempfile::tempdir().unwrap();
        let output = output_dir.path().join("speech\r\n.wav");
        let error = backend
            .synthesize(
                "must not reach MiniMax",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, ToolError::InvalidParams(_)));
        assert!(error.to_string().contains("CR or LF"));
        assert!(!output.exists());
        server.verify().await;
    }

    #[test]
    fn parse_success_decodes_hex() {
        let hex = "48656c6c6f"; // "Hello"
        let body = json!({
            "base_resp": {"status_code": 0, "status_msg": "ok"},
            "data": {"audio": hex}
        });
        let out = parse_response_audio(&body).unwrap();
        assert_eq!(out, b"Hello");
    }

    #[test]
    fn parse_nonzero_status_returns_error() {
        let body = json!({
            "base_resp": {"status_code": 1000, "status_msg": "quota exhausted"},
            "data": {"audio": "00"}
        });
        let err = parse_response_audio(&body).unwrap_err();
        assert!(err.to_string().contains("code 1000"));
        assert!(err.to_string().contains("quota exhausted"));
    }

    #[test]
    fn parse_empty_audio_returns_error() {
        let body = json!({
            "base_resp": {"status_code": 0},
            "data": {"audio": ""}
        });
        let err = parse_response_audio(&body).unwrap_err();
        assert!(err.to_string().contains("empty audio"));
    }

    #[test]
    fn parse_missing_base_resp_treated_as_failure() {
        let body = json!({"data": {"audio": "aa"}});
        let err = parse_response_audio(&body).unwrap_err();
        assert!(err.to_string().contains("code -1"));
    }

    #[test]
    fn hex_decode_rejects_invalid_chars() {
        assert!(decode_hex("zz").is_err());
        assert!(decode_hex("abc").is_err()); // odd length
        assert_eq!(decode_hex("ff").unwrap(), vec![255]);
        assert_eq!(decode_hex("00AB").unwrap(), vec![0, 171]);
    }

    #[tokio::test]
    async fn synthesize_without_api_key_errors() {
        // Bypass from_env by constructing manually with None key.
        let backend = MiniMaxTtsBackend {
            client: minimax_http_client(),
            api_key: None,
            base_url: "https://example".into(),
            model: "m".into(),
            settings: MiniMaxVoiceSettings::default(),
        };
        let err = backend
            .synthesize("hello", None, None, None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MINIMAX_API_KEY"));
    }
}
