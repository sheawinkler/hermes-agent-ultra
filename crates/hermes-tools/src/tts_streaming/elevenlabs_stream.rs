//! ElevenLabs streaming synthesis.
//!
//! Python's `stream_tts_to_speaker` uses the ElevenLabs SDK
//! `client.text_to_speech.convert(..., output_format="pcm_24000")` which
//! returns an iterator of raw PCM chunks (signed 16-bit, little-endian,
//! mono, 24 kHz). This module exposes the same data shape via the public
//! `text-to-speech/<voice_id>/stream` REST endpoint so callers can pipe
//! chunks directly to whatever audio sink they own.
//!
//! The module deliberately does not care *what* happens to the PCM bytes —
//! the CLI can feed them to `sounddevice` via a thin wrapper; the gateway
//! can re-encode them to Opus for Telegram; tests can count them.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use reqwest::Client;
use serde_json::json;

use hermes_core::ToolError;

/// ElevenLabs defaults.
pub const DEFAULT_ELEVENLABS_VOICE_ID: &str = "pNInz6obpgDQGcFmaJgB"; // Adam
pub const DEFAULT_ELEVENLABS_STREAMING_MODEL_ID: &str = "eleven_flash_v2_5";
pub const DEFAULT_ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io/v1";

/// Supported PCM output formats. ElevenLabs lets us pick the sample rate.
#[derive(Debug, Clone, Copy)]
pub enum PcmFormat {
    Pcm16000,
    Pcm22050,
    Pcm24000,
    Pcm44100,
}

impl PcmFormat {
    pub fn as_query(self) -> &'static str {
        match self {
            Self::Pcm16000 => "pcm_16000",
            Self::Pcm22050 => "pcm_22050",
            Self::Pcm24000 => "pcm_24000",
            Self::Pcm44100 => "pcm_44100",
        }
    }

    pub fn sample_rate_hz(self) -> u32 {
        match self {
            Self::Pcm16000 => 16_000,
            Self::Pcm22050 => 22_050,
            Self::Pcm24000 => 24_000,
            Self::Pcm44100 => 44_100,
        }
    }
}

/// Per-call configuration for the streaming endpoint.
#[derive(Debug, Clone)]
pub struct ElevenLabsStreamConfig {
    pub voice_id: String,
    pub model_id: String,
    pub output_format: PcmFormat,
    pub optimize_streaming_latency: Option<u8>,
    pub base_url: String,
}

impl Default for ElevenLabsStreamConfig {
    fn default() -> Self {
        Self {
            voice_id: DEFAULT_ELEVENLABS_VOICE_ID.to_string(),
            model_id: DEFAULT_ELEVENLABS_STREAMING_MODEL_ID.to_string(),
            output_format: PcmFormat::Pcm24000,
            optimize_streaming_latency: Some(3),
            base_url: DEFAULT_ELEVENLABS_BASE_URL.to_string(),
        }
    }
}

/// Async chunk stream: each `Result<Bytes, ToolError>` is a PCM slice.
pub type PcmChunkStream = Pin<Box<dyn Stream<Item = Result<Bytes, ToolError>> + Send>>;

/// Trait narrow enough for the pipeline to depend on without knowing about
/// the HTTP client. Makes mocking trivial.
#[async_trait]
pub trait StreamingTtsBackend: Send + Sync {
    /// Stream PCM chunks for a single sentence.
    async fn stream_sentence(
        &self,
        text: &str,
        config: &ElevenLabsStreamConfig,
    ) -> Result<PcmChunkStream, ToolError>;
}

// ---------------------------------------------------------------------------
// HTTP implementation
// ---------------------------------------------------------------------------

pub struct ElevenLabsStreamingClient {
    client: Client,
    api_key: Option<String>,
}

impl ElevenLabsStreamingClient {
    pub fn from_env() -> Self {
        Self {
            client: Client::new(),
            api_key: std::env::var("ELEVENLABS_API_KEY").ok(),
        }
    }

    pub fn with_client(client: Client, api_key: impl Into<String>) -> Self {
        Self {
            client,
            api_key: Some(api_key.into()),
        }
    }
}

impl Default for ElevenLabsStreamingClient {
    fn default() -> Self {
        Self::from_env()
    }
}

#[async_trait]
impl StreamingTtsBackend for ElevenLabsStreamingClient {
    async fn stream_sentence(
        &self,
        text: &str,
        config: &ElevenLabsStreamConfig,
    ) -> Result<PcmChunkStream, ToolError> {
        let api_key = self
            .api_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("ELEVENLABS_API_KEY not set".into()))?;

        let url = format!(
            "{}/text-to-speech/{}/stream",
            config.base_url.trim_end_matches('/'),
            config.voice_id
        );

        let mut req = self
            .client
            .post(url)
            .header("xi-api-key", api_key)
            .header("accept", "audio/pcm")
            .query(&[("output_format", config.output_format.as_query())]);
        if let Some(latency) = config.optimize_streaming_latency {
            req = req.query(&[("optimize_streaming_latency", latency.to_string())]);
        }

        let body = json!({
            "text": text,
            "model_id": config.model_id,
        });

        let resp = req
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("ElevenLabs request: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "ElevenLabs stream HTTP {status}: {body}"
            )));
        }

        let stream = resp
            .bytes_stream()
            .map(|item| item.map_err(|e| ToolError::ExecutionFailed(format!("stream: {e}"))));

        Ok(Box::pin(stream))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures::stream;

    #[test]
    fn pcm_format_round_trip() {
        assert_eq!(PcmFormat::Pcm16000.as_query(), "pcm_16000");
        assert_eq!(PcmFormat::Pcm22050.sample_rate_hz(), 22_050);
        assert_eq!(PcmFormat::Pcm24000.sample_rate_hz(), 24_000);
        assert_eq!(PcmFormat::Pcm44100.as_query(), "pcm_44100");
    }

    #[test]
    fn default_config_matches_python() {
        let cfg = ElevenLabsStreamConfig::default();
        assert_eq!(cfg.voice_id, DEFAULT_ELEVENLABS_VOICE_ID);
        assert_eq!(cfg.model_id, DEFAULT_ELEVENLABS_STREAMING_MODEL_ID);
        assert!(matches!(cfg.output_format, PcmFormat::Pcm24000));
    }

    // Provides a scripted streaming backend for pipeline tests. Kept in
    // this module's tests because the mock is useful as a reference impl
    // of the trait.
    pub struct FakeStreamingBackend {
        pub script: Vec<Vec<Bytes>>,
        pub seen_texts: std::sync::Mutex<Vec<String>>,
        pub cursor: std::sync::atomic::AtomicUsize,
    }

    impl FakeStreamingBackend {
        pub fn with_chunks(chunks_per_call: Vec<Vec<Bytes>>) -> Self {
            Self {
                script: chunks_per_call,
                seen_texts: std::sync::Mutex::new(Vec::new()),
                cursor: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl StreamingTtsBackend for FakeStreamingBackend {
        async fn stream_sentence(
            &self,
            text: &str,
            _config: &ElevenLabsStreamConfig,
        ) -> Result<PcmChunkStream, ToolError> {
            self.seen_texts.lock().unwrap().push(text.to_string());
            let idx = self
                .cursor
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let chunks = self
                .script
                .get(idx)
                .cloned()
                .unwrap_or_else(|| vec![Bytes::from_static(&[0u8; 4])]);
            let s = stream::iter(chunks.into_iter().map(Ok));
            Ok(Box::pin(s))
        }
    }

    #[tokio::test]
    async fn fake_backend_emits_scripted_chunks() {
        let backend = FakeStreamingBackend::with_chunks(vec![vec![
            Bytes::from_static(b"aaaa"),
            Bytes::from_static(b"bbbb"),
        ]]);
        let cfg = ElevenLabsStreamConfig::default();
        let mut s = backend
            .stream_sentence("hello", &cfg)
            .await
            .expect("stream");
        let first = s.next().await.unwrap().unwrap();
        let second = s.next().await.unwrap().unwrap();
        assert_eq!(first, Bytes::from_static(b"aaaa"));
        assert_eq!(second, Bytes::from_static(b"bbbb"));
        assert!(s.next().await.is_none());
        assert_eq!(backend.seen_texts.lock().unwrap().as_slice(), ["hello"]);
    }

    #[tokio::test]
    async fn streaming_client_without_api_key_errors() {
        let client = ElevenLabsStreamingClient {
            client: Client::new(),
            api_key: None,
        };
        let res = client
            .stream_sentence("hi", &ElevenLabsStreamConfig::default())
            .await;
        match res {
            Err(e) => assert!(e.to_string().contains("ELEVENLABS_API_KEY")),
            Ok(_) => panic!("expected ELEVENLABS_API_KEY error"),
        }
    }
}
