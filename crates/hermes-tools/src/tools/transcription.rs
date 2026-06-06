//! Speech-to-text transcription through OpenAI-compatible and Voxtral
//! `/audio/transcriptions` endpoints, with optional Nous-managed gateway
//! routing for OpenAI audio.
//!
//! Resolution order at request time:
//!
//! 1. **Managed**: when `HERMES_ENABLE_NOUS_MANAGED_TOOLS` is on AND a
//!    Nous OAuth token resolves for the `openai-audio` vendor, the call
//!    is routed through `{gateway}/audio/transcriptions` with a Nous
//!    `Bearer` header.
//! 2. **Direct OpenAI**: otherwise, falls back to `VOICE_TOOLS_OPENAI_KEY`,
//!    then `HERMES_OPENAI_API_KEY`, then legacy `OPENAI_API_KEY`.
//! 3. **Direct Groq/Mistral**: explicit `provider` (or STT provider env)
//!    can route to Groq Whisper or Mistral Voxtral Transcribe.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TranscriptionEndpoint {
    pub endpoint: String,
    pub bearer: String,
    pub transport: &'static str,
    pub provider: &'static str,
    pub model: String,
    pub response_format: String,
    pub send_response_format: bool,
}

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

pub(crate) fn transcription_response_format_for_model(model: &str) -> &'static str {
    if model.trim().eq_ignore_ascii_case("whisper-1") {
        "text"
    } else {
        "json"
    }
}

fn known_model(model: &str, models: &[&str]) -> bool {
    models.iter().any(|m| model.eq_ignore_ascii_case(m))
}

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

fn normalize_provider_name(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "" => None,
        "openai" | "whisper" => Some("openai"),
        "groq" | "groq_whisper" | "groq-whisper" => Some("groq"),
        "mistral" | "voxtral" | "mistral_voxtral" | "mistral-voxtral" => Some("mistral"),
        _ => None,
    }
}

fn requested_provider_from_env() -> Option<String> {
    env_trimmed("HERMES_STT_PROVIDER").or_else(|| env_trimmed("STT_PROVIDER"))
}

pub(crate) fn normalize_stt_model(provider: &str, requested: Option<&str>) -> String {
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

pub(crate) fn transcription_response_format_for_provider_model(
    provider: &str,
    model: &str,
) -> &'static str {
    match provider {
        "groq" => "text",
        "mistral" => "json",
        _ => transcription_response_format_for_model(model),
    }
}

pub(crate) fn parse_transcription_response(
    body: &str,
    response_format: &str,
) -> Result<String, ToolError> {
    if response_format.eq_ignore_ascii_case("text") {
        return Ok(body.trim().to_string());
    }
    let json: Value = serde_json::from_str(body)
        .map_err(|e| ToolError::ExecutionFailed(format!("Whisper JSON: {e}")))?;
    Ok(json
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

/// Compose the endpoint metadata for a transcription request. Pure helper so
/// it can be unit-tested without touching the network.
pub(crate) fn resolve_transcription_endpoint(
    managed: Option<&ManagedToolGatewayConfig>,
    requested_provider: Option<&str>,
    requested_model: Option<&str>,
    requested_response_format: Option<&str>,
) -> Option<TranscriptionEndpoint> {
    let raw_provider = requested_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(requested_provider_from_env);
    let provider = match raw_provider.as_deref() {
        Some(provider) => Some(normalize_provider_name(provider)?),
        None => None,
    };

    if provider.is_none() || provider == Some("openai") {
        if let Some(cfg) = managed {
            let base = cfg.gateway_origin.trim_end_matches('/');
            let model = normalize_stt_model("openai", requested_model);
            let response_format = requested_response_format
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    transcription_response_format_for_provider_model("openai", &model).to_string()
                });
            return Some(TranscriptionEndpoint {
                endpoint: format!("{base}/audio/transcriptions"),
                bearer: cfg.nous_user_token.clone(),
                transport: "managed",
                provider: "openai",
                model,
                response_format,
                send_response_format: true,
            });
        }
    }

    let provider = provider.unwrap_or_else(|| {
        if !resolve_openai_audio_api_key().is_empty() {
            "openai"
        } else if env_trimmed("GROQ_API_KEY").is_some() {
            "groq"
        } else if env_trimmed("MISTRAL_API_KEY").is_some() {
            "mistral"
        } else {
            "openai"
        }
    });

    match provider {
        "openai" => {
            let key = resolve_openai_audio_api_key();
            if key.is_empty() {
                return None;
            }
            let model = normalize_stt_model("openai", requested_model);
            let response_format = requested_response_format
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    transcription_response_format_for_provider_model("openai", &model).to_string()
                });
            let base = env_or_default(
                &["STT_OPENAI_BASE_URL", "OPENAI_BASE_URL"],
                "https://api.openai.com/v1",
            );
            Some(TranscriptionEndpoint {
                endpoint: endpoint_from_base(base),
                bearer: key,
                transport: "direct",
                provider: "openai",
                model,
                response_format,
                send_response_format: true,
            })
        }
        "groq" => {
            let key = env_trimmed("GROQ_API_KEY")?;
            let model = normalize_stt_model("groq", requested_model);
            let response_format = requested_response_format
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    transcription_response_format_for_provider_model("groq", &model).to_string()
                });
            let base = env_or_default(
                &["STT_GROQ_BASE_URL", "GROQ_BASE_URL"],
                "https://api.groq.com/openai/v1",
            );
            Some(TranscriptionEndpoint {
                endpoint: endpoint_from_base(base),
                bearer: key,
                transport: "direct",
                provider: "groq",
                model,
                response_format,
                send_response_format: true,
            })
        }
        "mistral" => {
            let key = env_trimmed("MISTRAL_API_KEY")?;
            let model = normalize_stt_model("mistral", requested_model);
            let response_format = requested_response_format
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| {
                    transcription_response_format_for_provider_model("mistral", &model).to_string()
                });
            let base = env_or_default(
                &["STT_MISTRAL_BASE_URL", "MISTRAL_BASE_URL"],
                "https://api.mistral.ai/v1",
            );
            Some(TranscriptionEndpoint {
                endpoint: endpoint_from_base(base),
                bearer: key,
                transport: "direct",
                provider: "mistral",
                model,
                response_format,
                send_response_format: false,
            })
        }
        _ => None,
    }
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

        let provider = params
            .get("provider")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let model = params
            .get("model")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let response_format = params
            .get("response_format")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let managed = resolve_managed_tool_gateway("openai-audio", ResolveOptions::default());
        let request =
            resolve_transcription_endpoint(managed.as_ref(), provider, model, response_format)
                .ok_or_else(|| {
                    ToolError::ExecutionFailed(
                        "Transcription requires a configured STT provider: \
                         VOICE_TOOLS_OPENAI_KEY / HERMES_OPENAI_API_KEY / OPENAI_API_KEY \
                         for OpenAI, GROQ_API_KEY for Groq, MISTRAL_API_KEY for Mistral, \
                         or a Nous-managed openai-audio gateway."
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

        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", request.model.clone());
        if request.send_response_format {
            form = form.text("response_format", request.response_format.clone());
        }
        if let Some(language) = params
            .get("language")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            form = form.text("language", language.to_string());
        }

        let resp = client
            .post(&request.endpoint)
            .header("Authorization", format!("Bearer {}", request.bearer))
            .multipart(form)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Transcription API: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "Transcription error {status}: {body}"
            )));
        }

        let body = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Transcription read body: {e}")))?;
        let text = parse_transcription_response(&body, &request.response_format)?;

        Ok(json!({
            "audio_path": path,
            "model": request.model,
            "provider": request.provider,
            "text": text,
            "response_format": request.response_format,
            "transport": request.transport,
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
        props.insert(
            "provider".into(),
            json!({
                "type":"string",
                "description":"STT provider: openai, groq, or mistral/voxtral. Defaults to managed/OpenAI, then Groq, then Mistral based on available credentials.",
                "enum":["openai","groq","mistral","voxtral"]
            }),
        );
        props.insert(
            "model".into(),
            json!({"type":"string","description":"Provider-specific STT model override"}),
        );
        props.insert(
            "language".into(),
            json!({"type":"string","description":"Optional ISO language hint"}),
        );
        props.insert(
            "response_format".into(),
            json!({"type":"string","description":"Optional response format for OpenAI-compatible providers", "enum":["text","json","verbose_json"]}),
        );
        tool_schema(
            "transcription",
            "Transcribe audio into text via OpenAI, Groq Whisper, or Mistral Voxtral. \
             Honors VOICE_TOOLS_OPENAI_KEY / HERMES_OPENAI_API_KEY (or OPENAI_API_KEY), \
             GROQ_API_KEY, MISTRAL_API_KEY, provider/model overrides, and routes OpenAI audio \
             through the Nous-managed openai-audio gateway when HERMES_ENABLE_NOUS_MANAGED_TOOLS is enabled.",
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
                "HERMES_OPENAI_API_KEY",
                "OPENAI_API_KEY",
                "VOICE_TOOLS_OPENAI_KEY",
                "GROQ_API_KEY",
                "MISTRAL_API_KEY",
                "HERMES_STT_PROVIDER",
                "STT_PROVIDER",
                "STT_OPENAI_MODEL",
                "STT_GROQ_MODEL",
                "STT_MISTRAL_MODEL",
                "STT_OPENAI_BASE_URL",
                "OPENAI_BASE_URL",
                "STT_GROQ_BASE_URL",
                "GROQ_BASE_URL",
                "STT_MISTRAL_BASE_URL",
                "MISTRAL_BASE_URL",
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
    fn response_format_matches_openai_audio_model_contract() {
        assert_eq!(transcription_response_format_for_model("whisper-1"), "text");
        assert_eq!(
            transcription_response_format_for_model("gpt-4o-mini-transcribe"),
            "json"
        );
        assert_eq!(
            transcription_response_format_for_provider_model("groq", "whisper-large-v3-turbo"),
            "text"
        );
        assert_eq!(
            transcription_response_format_for_provider_model("mistral", "voxtral-mini-latest"),
            "json"
        );
    }

    #[test]
    fn normalize_stt_model_keeps_provider_compatible_defaults() {
        let _g = EnvScope::new();
        assert_eq!(normalize_stt_model("openai", None), "whisper-1");
        assert_eq!(
            normalize_stt_model("groq", Some("whisper-1")),
            "whisper-large-v3-turbo"
        );
        assert_eq!(
            normalize_stt_model("mistral", Some("whisper-large-v3")),
            "voxtral-mini-latest"
        );
        assert_eq!(
            normalize_stt_model("openai", Some("gpt-4o-transcribe")),
            "gpt-4o-transcribe"
        );
    }

    #[test]
    fn parse_transcription_response_handles_text_and_json_shapes() {
        assert_eq!(
            parse_transcription_response("  hello from whisper\n", "text").unwrap(),
            "hello from whisper"
        );
        assert_eq!(
            parse_transcription_response(r#"{"text":"hello from gpt-4o"}"#, "json").unwrap(),
            "hello from gpt-4o"
        );
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
        let resolved = resolve_transcription_endpoint(Some(&cfg), None, None, None).unwrap();
        assert_eq!(
            resolved.endpoint,
            "https://oa.gw.example.com/audio/transcriptions"
        );
        assert_eq!(resolved.bearer, "nous-tok");
        assert_eq!(resolved.transport, "managed");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "whisper-1");
    }

    #[test]
    fn resolve_endpoint_uses_voice_key_first_in_direct_mode() {
        let _g = EnvScope::new();
        std::env::set_var("VOICE_TOOLS_OPENAI_KEY", "voice-key");
        std::env::set_var("OPENAI_API_KEY", "main-key");
        let resolved = resolve_transcription_endpoint(None, Some("openai"), None, None).unwrap();
        assert_eq!(
            resolved.endpoint,
            "https://api.openai.com/v1/audio/transcriptions"
        );
        assert_eq!(resolved.bearer, "voice-key");
        assert_eq!(resolved.transport, "direct");
        assert_eq!(resolved.provider, "openai");
    }

    #[test]
    fn resolve_endpoint_falls_back_to_main_key() {
        let _g = EnvScope::new();
        std::env::set_var("OPENAI_API_KEY", "main-key");
        let resolved = resolve_transcription_endpoint(None, None, None, None).unwrap();
        assert_eq!(resolved.bearer, "main-key");
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.transport, "direct");
    }

    #[test]
    fn resolve_endpoint_supports_explicit_groq_provider() {
        let _g = EnvScope::new();
        std::env::set_var("GROQ_API_KEY", "groq-key");
        let resolved =
            resolve_transcription_endpoint(None, Some("groq"), Some("whisper-1"), None).unwrap();
        assert_eq!(
            resolved.endpoint,
            "https://api.groq.com/openai/v1/audio/transcriptions"
        );
        assert_eq!(resolved.bearer, "groq-key");
        assert_eq!(resolved.provider, "groq");
        assert_eq!(resolved.model, "whisper-large-v3-turbo");
        assert_eq!(resolved.response_format, "text");
        assert!(resolved.send_response_format);
    }

    #[test]
    fn resolve_endpoint_supports_explicit_mistral_provider() {
        let _g = EnvScope::new();
        std::env::set_var("MISTRAL_API_KEY", "mistral-key");
        let resolved =
            resolve_transcription_endpoint(None, Some("voxtral"), Some("whisper-1"), None).unwrap();
        assert_eq!(
            resolved.endpoint,
            "https://api.mistral.ai/v1/audio/transcriptions"
        );
        assert_eq!(resolved.bearer, "mistral-key");
        assert_eq!(resolved.provider, "mistral");
        assert_eq!(resolved.model, "voxtral-mini-latest");
        assert_eq!(resolved.response_format, "json");
        assert!(!resolved.send_response_format);
    }

    #[test]
    fn resolve_endpoint_auto_falls_back_to_groq_when_openai_unconfigured() {
        let _g = EnvScope::new();
        std::env::set_var("GROQ_API_KEY", "groq-key");
        let resolved = resolve_transcription_endpoint(None, None, None, None).unwrap();
        assert_eq!(resolved.provider, "groq");
        assert_eq!(resolved.bearer, "groq-key");
    }

    #[test]
    fn resolve_endpoint_rejects_unknown_provider_instead_of_falling_back() {
        let _g = EnvScope::new();
        std::env::set_var("VOICE_TOOLS_OPENAI_KEY", "voice-key");
        assert!(resolve_transcription_endpoint(None, Some("bogus"), None, None).is_none());
    }

    #[test]
    fn resolve_endpoint_returns_none_when_unconfigured() {
        let _g = EnvScope::new();
        assert!(resolve_transcription_endpoint(None, None, None, None).is_none());
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
        assert!(s.contains("VOICE_TOOLS_OPENAI_KEY"));
        assert!(s.contains("HERMES_OPENAI_API_KEY") || s.contains("OPENAI_API_KEY"));
        assert!(s.contains("GROQ_API_KEY"));
        assert!(s.contains("MISTRAL_API_KEY"));
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
        assert!(desc.contains("HERMES_OPENAI_API_KEY"));
        assert!(desc.contains("GROQ_API_KEY"));
        assert!(desc.contains("MISTRAL_API_KEY"));
        assert!(desc.contains("provider"));
        assert!(desc.contains("model"));
        assert!(desc.contains("HERMES_ENABLE_NOUS_MANAGED_TOOLS"));
    }
}
