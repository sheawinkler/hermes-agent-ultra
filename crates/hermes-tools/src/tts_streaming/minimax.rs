//! MiniMax `t2a_v2` HTTP backend.
//!
//! The response body contains hex-encoded audio bytes under
//! `data.audio`, plus a `base_resp.status_code` field that is zero on
//! success. We mirror Python's handling byte-for-byte.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::tts::TtsBackend;
use hermes_core::ToolError;

/// Default endpoint for MiniMax's `t2a_v2` (text-to-audio) API.
pub const DEFAULT_MINIMAX_BASE_URL: &str = "https://api.minimax.io/v1/t2a_v2";
/// Default MiniMax model (high-definition, multilingual).
pub const DEFAULT_MINIMAX_MODEL: &str = "speech-2.8-hd";
/// Default voice identifier.
pub const DEFAULT_MINIMAX_VOICE_ID: &str = "English_Graceful_Lady";

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

impl MiniMaxTtsBackend {
    /// Construct from environment:
    /// * `MINIMAX_API_KEY` — required for runtime calls
    /// * `MINIMAX_BASE_URL` — optional override (default: `DEFAULT_MINIMAX_BASE_URL`)
    /// * `MINIMAX_MODEL` — optional override (default: `DEFAULT_MINIMAX_MODEL`)
    pub fn from_env() -> Self {
        Self::from_env_with(MiniMaxVoiceSettings::default())
    }

    pub fn from_env_with(settings: MiniMaxVoiceSettings) -> Self {
        Self {
            client: Client::new(),
            api_key: std::env::var("MINIMAX_API_KEY").ok(),
            base_url: std::env::var("MINIMAX_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_MINIMAX_BASE_URL.to_string()),
            model: std::env::var("MINIMAX_MODEL")
                .unwrap_or_else(|_| DEFAULT_MINIMAX_MODEL.to_string()),
            settings,
        }
    }

    /// Construct with explicit config (useful for tests).
    pub fn with_config(
        client: Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
        settings: MiniMaxVoiceSettings,
    ) -> Self {
        Self {
            client,
            api_key: Some(api_key.into()),
            base_url: base_url.into(),
            model: model.into(),
            settings,
        }
    }

    /// Build the JSON body Python's `_generate_minimax_tts` sends. Exposed
    /// for testing; the main request path builds it inline.
    pub fn build_payload(&self, text: &str, voice: Option<&str>) -> Value {
        let voice_id = voice.unwrap_or(&self.settings.voice_id);
        json!({
            "model": self.model,
            "text": text,
            "stream": false,
            "voice_setting": {
                "voice_id": voice_id,
                "speed": self.settings.speed,
                "vol": self.settings.vol,
                "pitch": self.settings.pitch,
            },
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
    ) -> Result<Vec<u8>, ToolError> {
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("MINIMAX_API_KEY not set".into()))?;
        let payload = self.build_payload(text, voice);

        let resp = self
            .client
            .post(&self.base_url)
            .header("Content-Type", "application/json")
            .bearer_auth(api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("MiniMax request failed: {e}")))?;

        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("MiniMax JSON decode failed: {e}")))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "MiniMax HTTP {status}: {body}"
            )));
        }

        parse_response_audio(&body)
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
    ) -> Result<String, ToolError> {
        let audio = self.synthesize_to_bytes(text, voice).await?;
        let suffix = match self.settings.format.as_str() {
            "wav" => "wav",
            "flac" => "flac",
            _ => "mp3",
        };
        let path =
            std::env::temp_dir().join(format!("hermes_minimax_{}.{suffix}", uuid::Uuid::new_v4()));
        tokio::fs::write(&path, &audio)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("write audio: {e}")))?;
        Ok(json!({
            "provider": "minimax",
            "file": path.display().to_string(),
            "voice": voice.unwrap_or(&self.settings.voice_id),
            "model": self.model,
            "bytes": audio.len(),
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

    #[test]
    fn payload_shape_matches_python() {
        let backend = MiniMaxTtsBackend::with_config(
            Client::new(),
            "key",
            "https://example/v1/t2a_v2",
            "speech-2.8-hd",
            MiniMaxVoiceSettings {
                voice_id: "English_Graceful_Lady".into(),
                speed: 1.0,
                vol: 1.0,
                pitch: 0,
                sample_rate: 32_000,
                bitrate: 128_000,
                format: "mp3".into(),
                channel: 1,
            },
        );
        let p = backend.build_payload("hello", None);
        assert_eq!(p["model"], "speech-2.8-hd");
        assert_eq!(p["text"], "hello");
        assert_eq!(p["stream"], false);
        assert_eq!(p["voice_setting"]["voice_id"], "English_Graceful_Lady");
        assert_eq!(p["voice_setting"]["speed"], 1.0);
        assert_eq!(p["audio_setting"]["sample_rate"], 32_000);
        assert_eq!(p["audio_setting"]["bitrate"], 128_000);
        assert_eq!(p["audio_setting"]["format"], "mp3");
        assert_eq!(p["audio_setting"]["channel"], 1);
    }

    #[test]
    fn voice_override_applies_to_payload() {
        let backend = MiniMaxTtsBackend::with_config(
            Client::new(),
            "k",
            "https://x",
            "m",
            MiniMaxVoiceSettings::default(),
        );
        let p = backend.build_payload("hi", Some("Custom_Voice"));
        assert_eq!(p["voice_setting"]["voice_id"], "Custom_Voice");
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
            client: Client::new(),
            api_key: None,
            base_url: "https://example".into(),
            model: "m".into(),
            settings: MiniMaxVoiceSettings::default(),
        };
        let err = backend.synthesize("hello", None, None).await.unwrap_err();
        assert!(err.to_string().contains("MINIMAX_API_KEY"));
    }
}
