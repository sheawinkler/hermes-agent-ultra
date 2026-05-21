//! Shared TTS/STT provider HTTP implementations (config-driven, Python parity).

use std::path::Path;
use std::process::Stdio;

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use hermes_config::voice::{SttConfig, TtsConfig};
use hermes_config::resolve_openai_audio_api_key;
use hermes_core::ToolError;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::process::Command;

const DEFAULT_EDGE_VOICE: &str = "en-US-AriaNeural";
const DEFAULT_OPENAI_TTS_MODEL: &str = "tts-1";
const DEFAULT_OPENAI_TTS_VOICE: &str = "alloy";
const GROQ_BASE_URL: &str = "https://api.groq.com/openai/v1";
const MISTRAL_API_BASE: &str = "https://api.mistral.ai/v1";
const GEMINI_TTS_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const XAI_DEFAULT_BASE: &str = "https://api.x.ai/v1";

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn audio_extension_format(path: &str) -> &'static str {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
        .map(|e| match e {
            "wav" => "wav",
            "mp3" => "mp3",
            "m4a" => "mp4",
            "webm" => "webm",
            "ogg" | "oga" => "ogg",
            "flac" => "flac",
            _ => "wav",
        })
        .unwrap_or("wav")
}

fn wrap_pcm_as_wav(pcm: &[u8], sample_rate: u32) -> Vec<u8> {
    let channels: u16 = 1;
    let sample_width: u16 = 2;
    let byte_rate = sample_rate * channels as u32 * sample_width as u32;
    let block_align = channels * sample_width;
    let data_size = pcm.len() as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&(sample_width * 8).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_size.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

async fn write_temp_audio(bytes: &[u8], ext: &str) -> Result<std::path::PathBuf, ToolError> {
    let path = std::env::temp_dir().join(format!("hermes_tts_{}.{}", uuid::Uuid::new_v4(), ext));
    tokio::fs::write(&path, bytes)
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write audio: {e}")))?;
    Ok(path)
}

// ---------------------------------------------------------------------------
// STT
// ---------------------------------------------------------------------------

pub struct SttEngine {
    pub config: SttConfig,
    pub client: Client,
}

impl SttEngine {
    pub fn new(config: SttConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    pub fn from_optional(config: Option<SttConfig>) -> Self {
        Self::new(config.unwrap_or_default())
    }

    pub fn check_enabled(&self) -> Result<(), ToolError> {
        if !self.config.is_enabled() {
            return Err(ToolError::ExecutionFailed(
                "STT is disabled in config (stt.enabled=false)".into(),
            ));
        }
        Ok(())
    }

    pub async fn transcribe_file(&self, path: &str) -> Result<String, ToolError> {
        self.check_enabled()?;
        let provider = self.config.default_provider();
        match provider {
            "groq" => self.transcribe_groq(path).await,
            "mistral" => self.transcribe_mistral(path).await,
            "xai" => self.transcribe_xai(path).await,
            "local_command" => self.transcribe_local_command(path).await,
            "openai" | _ => self.transcribe_openai(path).await,
        }
    }

    async fn transcribe_openai(&self, path: &str) -> Result<String, ToolError> {
        let api_key = self
            .config
            .openai
            .as_ref()
            .and_then(|c| c.api_key.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                let k = resolve_openai_audio_api_key();
                if k.trim().is_empty() {
                    None
                } else {
                    Some(k)
                }
            })
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "OpenAI STT requires VOICE_TOOLS_OPENAI_KEY, stt.openai.api_key, or OPENAI_API_KEY"
                        .into(),
                )
            })?;

        let base = self.config.openai_base_url();
        let endpoint = format!("{base}/audio/transcriptions");
        let model = self.config.openai_model().to_string();
        self.post_whisper_compatible(&endpoint, &api_key, path, &model).await
    }

    async fn transcribe_groq(&self, path: &str) -> Result<String, ToolError> {
        let api_key = env_nonempty("GROQ_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed("GROQ_API_KEY not set for Groq STT".into())
        })?;
        let base = env_nonempty("GROQ_BASE_URL").unwrap_or_else(|| GROQ_BASE_URL.to_string());
        let endpoint = format!("{}/audio/transcriptions", base.trim_end_matches('/'));
        let model = self.config.groq_model();
        self.post_whisper_compatible(&endpoint, &api_key, path, &model)
            .await
    }

    async fn post_whisper_compatible(
        &self,
        endpoint: &str,
        api_key: &str,
        path: &str,
        model: &str,
    ) -> Result<String, ToolError> {
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio: {e}")))?;
        let fmt = audio_extension_format(path);
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("audio.{fmt}"))
            .mime_str(&format!("audio/{fmt}"))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model.to_string());

        let resp = self
            .client
            .post(endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("STT request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "STT error {status}: {body}"
            )));
        }

        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let trimmed = text.trim();
        if trimmed.starts_with('{') {
            let json: Value = serde_json::from_str(trimmed)
                .map_err(|e| ToolError::ExecutionFailed(format!("STT JSON parse: {e}")))?;
            return Ok(json
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or(trimmed)
                .to_string());
        }
        Ok(trimmed.to_string())
    }

    async fn transcribe_mistral(&self, path: &str) -> Result<String, ToolError> {
        let api_key = env_nonempty("MISTRAL_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed("MISTRAL_API_KEY not set for Mistral STT".into())
        })?;
        let model = self.config.mistral_model().to_string();
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio: {e}")))?;
        let fmt = audio_extension_format(path);
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("audio.{fmt}"))
            .mime_str(&format!("audio/{fmt}"))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", model);

        let resp = self
            .client
            .post(format!("{MISTRAL_API_BASE}/audio/transcriptions"))
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Mistral STT failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Mistral STT error: {body}"
            )));
        }
        let json: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn transcribe_xai(&self, path: &str) -> Result<String, ToolError> {
        let api_key = env_nonempty("XAI_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed("XAI_API_KEY not set for xAI STT".into())
        })?;
        let base = self
            .config
            .xai
            .as_ref()
            .and_then(|c| c.base_url.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| env_nonempty("XAI_STT_BASE_URL"))
            .unwrap_or_else(|| XAI_DEFAULT_BASE.to_string());
        let endpoint = format!("{}/stt", base.trim_end_matches('/'));
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio: {e}")))?;
        let fmt = audio_extension_format(path);
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("audio.{fmt}"))
            .mime_str(&format!("audio/{fmt}"))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let form = reqwest::multipart::Form::new().part("file", part);

        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {api_key}"))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("xAI STT failed: {e}")))?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!("xAI STT error: {body}")));
        }
        let json: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        Ok(json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string())
    }

    async fn transcribe_local_command(&self, path: &str) -> Result<String, ToolError> {
        let template = env_nonempty("HERMES_LOCAL_STT_COMMAND").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "local_command STT requires HERMES_LOCAL_STT_COMMAND template".into(),
            )
        })?;
        let language = self
            .config
            .local
            .as_ref()
            .and_then(|c| c.language.as_deref())
            .unwrap_or("");
        let model = self
            .config
            .local
            .as_ref()
            .and_then(|c| c.model.as_deref())
            .unwrap_or("base");
        let output_dir = std::env::temp_dir().join(format!("hermes-stt-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir_all(&output_dir)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        let command = template
            .replace("{input_path}", path)
            .replace("{output_dir}", output_dir.display().to_string().as_str())
            .replace("{language}", language)
            .replace("{model}", model);

        let output = if template.contains('{') && !command.contains(' ') {
            Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
        } else {
            Command::new("sh")
                .arg("-c")
                .arg(&command)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output()
                .await
        }
        .map_err(|e| ToolError::ExecutionFailed(format!("local STT command failed: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed(format!(
                "local STT command error: {stderr}"
            )));
        }

        let mut entries = tokio::fs::read_dir(&output_dir)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
        {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("txt") {
                let text = tokio::fs::read_to_string(&p)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                let _ = tokio::fs::remove_dir_all(&output_dir).await;
                return Ok(text.trim().to_string());
            }
        }
        Err(ToolError::ExecutionFailed(
            "local STT command produced no .txt transcript".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// TTS helpers used by MultiTtsBackend
// ---------------------------------------------------------------------------

pub struct TtsSettings {
    pub config: TtsConfig,
}

impl TtsSettings {
    pub fn from_optional(config: Option<TtsConfig>) -> Self {
        Self {
            config: config.unwrap_or_default(),
        }
    }

    pub fn openai_model(&self) -> String {
        self.config
            .openai
            .as_ref()
            .and_then(|c| c.model.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_OPENAI_TTS_MODEL.to_string())
    }

    pub fn openai_voice<'a>(&'a self, override_voice: Option<&'a str>) -> String {
        override_voice
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                self.config
                    .openai
                    .as_ref()
                    .and_then(|c| c.voice.clone())
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_OPENAI_TTS_VOICE.to_string())
    }

    pub fn openai_base_url(&self) -> String {
        self.config
            .openai
            .as_ref()
            .and_then(|c| c.base_url.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches('/').to_string())
            .or_else(|| env_nonempty("OPENAI_BASE_URL"))
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string())
    }

    pub fn edge_voice<'a>(&'a self, override_voice: Option<&'a str>) -> String {
        override_voice
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                self.config
                    .edge
                    .as_ref()
                    .and_then(|c| c.voice.clone())
                    .filter(|s| !s.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_EDGE_VOICE.to_string())
    }
}

pub async fn edge_tts_synthesize(
    client: &Client,
    text: &str,
    voice: &str,
    speed: Option<f32>,
) -> Result<Vec<u8>, ToolError> {
    // Microsoft Edge Read Aloud API (parity with Python edge-tts, no Python runtime).
    let rate = speed
        .map(|s| format!("+{}%", ((s - 1.0).max(-0.5).min(1.0) * 100.0) as i32))
        .unwrap_or_else(|| "+0%".to_string());
    let ssml = format!(
        "<speak version='1.0' xml:lang='en-US'><voice name='{voice}'><prosody rate='{rate}'>{text}</prosody></voice></speak>"
    );
    let url = format!(
        "https://speech.platform.bing.com/consumer/speech/synthesize/readaloud/edge/v1?TrustedClientToken=6A5AA1D4EAFF4E9FB37E23D68491D6F4"
    );
    let resp = client
        .post(&url)
        .header("Content-Type", "application/ssml+xml")
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
        )
        .body(ssml)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Edge TTS failed: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!(
            "Edge TTS HTTP {status}: {body}"
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
        .to_vec();
    Ok(bytes)
}

pub async fn mistral_tts_synthesize(
    client: &Client,
    text: &str,
    cfg: &TtsConfig,
) -> Result<Vec<u8>, ToolError> {
    let api_key = env_nonempty("MISTRAL_API_KEY").ok_or_else(|| {
        ToolError::ExecutionFailed("MISTRAL_API_KEY not set for Mistral TTS".into())
    })?;
    let mi = cfg.mistral.as_ref();
    let model = mi
        .and_then(|c| c.model.as_deref())
        .unwrap_or("mistral-tts");
    let voice_id = mi
        .and_then(|c| c.voice_id.as_deref())
        .unwrap_or("default");
    let body = json!({
        "model": model,
        "input": text,
        "voice_id": voice_id,
        "response_format": "mp3",
    });
    let resp = client
        .post(format!("{MISTRAL_API_BASE}/audio/speech"))
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Mistral TTS: {e}")))?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("Mistral TTS error: {err}")));
    }
    let json: Value = resp
        .json()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    if let Some(b64) = json.get("audio").and_then(|v| v.as_str()) {
        return B64.decode(b64).map_err(|e| ToolError::ExecutionFailed(e.to_string()));
    }
    if let Some(b64) = json.get("audio_data").and_then(|v| v.as_str()) {
        return B64.decode(b64).map_err(|e| ToolError::ExecutionFailed(e.to_string()));
    }
    Err(ToolError::ExecutionFailed(
        "Mistral TTS: response missing audio payload".into(),
    ))
}

pub async fn gemini_tts_synthesize(
    client: &Client,
    text: &str,
    cfg: &TtsConfig,
) -> Result<Vec<u8>, ToolError> {
    let api_key = env_nonempty("GEMINI_API_KEY")
        .or_else(|| env_nonempty("GOOGLE_API_KEY"))
        .ok_or_else(|| {
            ToolError::ExecutionFailed("GEMINI_API_KEY or GOOGLE_API_KEY not set".into())
        })?;
    let g = cfg.gemini.as_ref();
    let model = g
        .and_then(|c| c.model.as_deref())
        .unwrap_or("gemini-2.5-flash-preview-tts");
    let voice = g
        .and_then(|c| c.voice.as_deref())
        .unwrap_or("Kore");
    let base = g
        .and_then(|c| c.base_url.as_deref())
        .unwrap_or(GEMINI_TTS_BASE)
        .trim_end_matches('/');
    let endpoint = format!("{base}/models/{model}:generateContent");
    let body = json!({
        "contents": [{"parts": [{"text": text}]}],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {"prebuiltVoiceConfig": {"voiceName": voice}}
            }
        }
    });
    let resp = client
        .post(&endpoint)
        .query(&[("key", api_key.as_str())])
        .json(&body)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Gemini TTS: {e}")))?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("Gemini TTS error: {err}")));
    }
    let data: Value = resp
        .json()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    let parts = data
        .pointer("/candidates/0/content/parts")
        .and_then(|p| p.as_array())
        .ok_or_else(|| ToolError::ExecutionFailed("Gemini TTS: no audio in response".into()))?;
    let inline = parts.iter().find_map(|p| {
        p.get("inlineData")
            .or_else(|| p.get("inline_data"))
            .and_then(|v| v.get("data").and_then(|d| d.as_str()))
    });
    let b64 = inline.ok_or_else(|| {
        ToolError::ExecutionFailed("Gemini TTS: missing inline audio data".into())
    })?;
    let pcm = B64
        .decode(b64)
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
    Ok(wrap_pcm_as_wav(&pcm, 24_000))
}

pub async fn xai_tts_synthesize(
    client: &Client,
    text: &str,
    cfg: &TtsConfig,
) -> Result<Vec<u8>, ToolError> {
    let api_key = env_nonempty("XAI_API_KEY").ok_or_else(|| {
        ToolError::ExecutionFailed("XAI_API_KEY not set for xAI TTS".into())
    })?;
    let x = cfg.xai.as_ref();
    let voice_id = x
        .and_then(|c| c.voice_id.as_deref())
        .unwrap_or("default");
    let language = x.and_then(|c| c.language.as_deref()).unwrap_or("en");
    let base = x
        .and_then(|c| c.base_url.as_deref())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| env_nonempty("XAI_BASE_URL"))
        .unwrap_or_else(|| XAI_DEFAULT_BASE.to_string());
    let url = format!("{}/tts", base.trim_end_matches('/'));
    let body = json!({
        "text": text,
        "voice_id": voice_id,
        "language": language,
    });
    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("xAI TTS: {e}")))?;
    if !resp.status().is_success() {
        let err = resp.text().await.unwrap_or_default();
        return Err(ToolError::ExecutionFailed(format!("xAI TTS error: {err}")));
    }
    Ok(resp
        .bytes()
        .await
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
        .to_vec())
}

pub async fn tts_result_json(
    provider: &str,
    voice: &str,
    bytes: &[u8],
    ext: &str,
) -> Result<String, ToolError> {
    let path = write_temp_audio(bytes, ext).await?;
    Ok(json!({
        "provider": provider,
        "file": path.display().to_string(),
        "voice": voice,
        "bytes": bytes.len(),
    })
    .to_string())
}
