//! Whisper-style transcription via OpenAI's `/audio/transcriptions`
//! endpoint, with optional Nous-managed gateway routing.
//!
//! Resolution order at request time:
//!
//! 1. **Managed**: when `HERMES_ENABLE_NOUS_MANAGED_TOOLS` is on AND a
//!    Nous OAuth token resolves for the `openai-audio` vendor, the call
//!    is routed through `{gateway}/audio/transcriptions` with a Nous
//!    `Bearer` header.
//! 2. **Direct**: otherwise, falls back to `VOICE_TOOLS_OPENAI_KEY`,
//!    then `OPENAI_API_KEY`, calling `https://api.openai.com/v1/audio/...`.
//!
//! The output JSON includes `transport: "managed" | "direct"` for
//! observability.

use std::path::Path;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

pub struct TranscriptionHandler;

fn audio_extension_format(path: &str) -> &'static str {
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

/// Compose the (endpoint, bearer, transport_label) tuple for a Whisper
/// request. Pure helper so it can be unit-tested without touching the
/// network.
pub(crate) fn resolve_transcription_endpoint(
    managed: Option<&ManagedToolGatewayConfig>,
) -> Option<(String, String, &'static str)> {
    if let Some(cfg) = managed {
        let base = cfg.gateway_origin.trim_end_matches('/');
        return Some((
            format!("{base}/audio/transcriptions"),
            cfg.nous_user_token.clone(),
            "managed",
        ));
    }

    let key = resolve_openai_audio_api_key();
    if key.is_empty() {
        return None;
    }
    Some((
        "https://api.openai.com/v1/audio/transcriptions".into(),
        key,
        "direct",
    ))
}

#[async_trait]
impl ToolHandler for TranscriptionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params
            .get("audio_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'audio_path'".into()));
        }

        let managed = resolve_managed_tool_gateway("openai-audio", ResolveOptions::default());
        let (endpoint, bearer, transport) = resolve_transcription_endpoint(managed.as_ref())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Whisper transcription requires either VOICE_TOOLS_OPENAI_KEY / \
                     OPENAI_API_KEY for direct mode, or a Nous-managed openai-audio gateway."
                        .into(),
                )
            })?;

        let bytes = tokio::fs::read(path)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Cannot read audio file: {e}")))?;

        let fmt = audio_extension_format(path);
        let client = reqwest::Client::new();
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(format!("audio.{fmt}"))
            .mime_str(&format!("audio/{fmt}"))
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", "whisper-1");

        let resp = client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", bearer))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Whisper API: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Whisper error {status}: {body}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Whisper JSON: {e}")))?;
        let text = json
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        Ok(json!({
            "audio_path": path,
            "text": text,
            "transport": transport,
            "status": "transcribed",
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "audio_path".into(),
            json!({"type":"string","description":"Path to audio file"}),
        );
        tool_schema(
            "transcription",
            "Transcribe audio into text via OpenAI Whisper. Honors VOICE_TOOLS_OPENAI_KEY / \
             OPENAI_API_KEY for direct mode and routes through Nous-managed openai-audio \
             gateway when HERMES_ENABLE_NOUS_MANAGED_TOOLS is enabled.",
            JsonSchema::object(props, vec!["audio_path".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvScope {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "OPENAI_API_KEY",
                "VOICE_TOOLS_OPENAI_KEY",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn audio_extension_known_formats() {
        assert_eq!(audio_extension_format("a.wav"), "wav");
        assert_eq!(audio_extension_format("a.mp3"), "mp3");
        assert_eq!(audio_extension_format("a.m4a"), "mp4");
        assert_eq!(audio_extension_format("a.webm"), "webm");
        assert_eq!(audio_extension_format("a.ogg"), "ogg");
        assert_eq!(audio_extension_format("a.oga"), "ogg");
        assert_eq!(audio_extension_format("a.flac"), "flac");
    }

    #[test]
    fn audio_extension_unknown_falls_back_to_wav() {
        assert_eq!(audio_extension_format("a.xyz"), "wav");
        assert_eq!(audio_extension_format("noext"), "wav");
        assert_eq!(audio_extension_format(""), "wav");
    }

    #[test]
    fn resolve_endpoint_prefers_managed_when_provided() {
        let _g = EnvScope::new();
        // Even with a direct key set, the explicit managed cfg wins because
        // the resolver already decided.
        std::env::set_var("OPENAI_API_KEY", "should-be-ignored");
        let cfg = ManagedToolGatewayConfig {
            vendor: "openai-audio".into(),
            gateway_origin: "https://oa.gw.example.com/".into(),
            nous_user_token: "nous-tok".into(),
            managed_mode: true,
        };
        let (endpoint, bearer, label) = resolve_transcription_endpoint(Some(&cfg)).unwrap();
        assert_eq!(endpoint, "https://oa.gw.example.com/audio/transcriptions");
        assert_eq!(bearer, "nous-tok");
        assert_eq!(label, "managed");
    }

    #[test]
    fn resolve_endpoint_uses_voice_key_first_in_direct_mode() {
        let _g = EnvScope::new();
        std::env::set_var("VOICE_TOOLS_OPENAI_KEY", "voice-key");
        std::env::set_var("OPENAI_API_KEY", "main-key");
        let (endpoint, bearer, label) = resolve_transcription_endpoint(None).unwrap();
        assert_eq!(endpoint, "https://api.openai.com/v1/audio/transcriptions");
        assert_eq!(bearer, "voice-key");
        assert_eq!(label, "direct");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_main_key() {
        let _g = EnvScope::new();
        std::env::set_var("OPENAI_API_KEY", "main-key");
        let (_endpoint, bearer, label) = resolve_transcription_endpoint(None).unwrap();
        assert_eq!(bearer, "main-key");
        assert_eq!(label, "direct");
    }

    #[test]
    fn resolve_endpoint_returns_none_when_unconfigured() {
        let _g = EnvScope::new();
        assert!(resolve_transcription_endpoint(None).is_none());
    }

    #[tokio::test]
    async fn execute_errors_when_no_credentials_or_gateway() {
        let _g = EnvScope::new();
        let h = TranscriptionHandler;
        let err = h
            .execute(json!({"audio_path": "/tmp/nope.wav"}))
            .await
            .unwrap_err();
        let s = err.to_string();
        assert!(s.contains("VOICE_TOOLS_OPENAI_KEY") || s.contains("OPENAI_API_KEY"));
        assert!(s.contains("openai-audio gateway"));
    }

    #[tokio::test]
    async fn execute_errors_on_missing_path_param() {
        let _g = EnvScope::new();
        std::env::set_var("OPENAI_API_KEY", "k");
        let h = TranscriptionHandler;
        let err = h.execute(json!({})).await.unwrap_err();
        assert!(matches!(err, ToolError::InvalidParams(_)));
    }

    #[test]
    fn schema_advertises_managed_routing() {
        let h = TranscriptionHandler;
        let s = h.schema();
        let desc = serde_json::to_string(&s).unwrap();
        assert!(desc.contains("VOICE_TOOLS_OPENAI_KEY"));
        assert!(desc.contains("HERMES_ENABLE_NOUS_MANAGED_TOOLS"));
    }
}
