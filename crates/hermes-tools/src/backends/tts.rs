//! Real TTS backends: ElevenLabs and OpenAI TTS.
//!
//! Zero-Python: Edge TTS (which required the `edge-tts` Python CLI) is no
//! longer supported. Callers that want free / no-key TTS should use local
//! ONNX models via the forthcoming `LocalOnnxTtsBackend` (Sprint 6) or
//! OpenAI's cheap `tts-1` endpoint.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::tools::tts::TtsBackend;
use crate::tts_streaming::minimax::MiniMaxTtsBackend;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};
use hermes_core::ToolError;

pub const FALLBACK_MAX_TEXT_LENGTH: usize = 4_000;
pub const DEFAULT_COMMAND_TTS_MAX_TEXT_LENGTH: usize = 15_000;
pub const DEFAULT_COMMAND_TTS_OUTPUT_FORMAT: &str = "mp3";
pub const DEFAULT_COMMAND_TTS_TIMEOUT_SECONDS: u64 = 60;
pub const COMMAND_TTS_OUTPUT_FORMATS: &[&str] = &["mp3", "wav", "ogg", "flac"];

const BUILTIN_TTS_PROVIDERS: &[&str] = &[
    "elevenlabs",
    "openai",
    "minimax",
    "piper",
    // Reserved upstream/Python provider names. Hermes Agent Ultra must not
    // let a command provider silently shadow names that are built in upstream
    // or intentionally unsupported by the Rust-only runtime.
    "edge",
    "edge_tts",
    "edge-tts",
    "neutts",
    "kittentts",
    "mistral",
    "gemini",
    "xai",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CommandTtsProviderConfig {
    pub name: String,
    pub command: String,
    pub config: Value,
}

fn normalize_provider_name(provider: Option<&str>) -> Option<String> {
    provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
}

fn find_object_key_case_insensitive<'a>(
    object: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Option<(&'a str, &'a Value)> {
    object
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(key))
        .map(|(k, v)| (k.as_str(), v))
}

fn named_tts_provider_config<'a>(tts_config: &'a Value, provider: &str) -> Option<&'a Value> {
    let object = tts_config.as_object()?;
    if let Some(providers) = object.get("providers").and_then(|v| v.as_object()) {
        if let Some((_name, value)) = find_object_key_case_insensitive(providers, provider) {
            return Some(value);
        }
    }
    find_object_key_case_insensitive(object, provider).map(|(_name, value)| value)
}

fn is_reserved_tts_provider(provider: &str) -> bool {
    BUILTIN_TTS_PROVIDERS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(provider))
}

pub(crate) fn is_command_provider_config(value: &Value) -> bool {
    let object = match value.as_object() {
        Some(object) => object,
        None => return false,
    };
    let command = object
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if command.is_empty() {
        return false;
    }
    let kind = object
        .get("type")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("command");
    kind.eq_ignore_ascii_case("command")
}

pub(crate) fn resolve_command_provider_config(
    provider: &str,
    tts_config: &Value,
) -> Option<CommandTtsProviderConfig> {
    if is_reserved_tts_provider(provider) {
        return None;
    }
    let (name, value) = {
        let object = tts_config.as_object()?;
        if let Some(providers) = object.get("providers").and_then(|v| v.as_object()) {
            if let Some((name, value)) = find_object_key_case_insensitive(providers, provider) {
                (name.to_string(), value)
            } else if let Some((name, value)) = find_object_key_case_insensitive(object, provider) {
                (name.to_string(), value)
            } else {
                return None;
            }
        } else if let Some((name, value)) = find_object_key_case_insensitive(object, provider) {
            (name.to_string(), value)
        } else {
            return None;
        }
    };
    if !is_command_provider_config(value) {
        return None;
    }
    let command = value
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    Some(CommandTtsProviderConfig {
        name,
        command,
        config: value.clone(),
    })
}

#[cfg(test)]
pub(crate) fn iter_command_providers(tts_config: &Value) -> Vec<CommandTtsProviderConfig> {
    let mut providers = BTreeMap::new();
    if let Some(object) = tts_config.as_object() {
        if let Some(block) = object.get("providers").and_then(|v| v.as_object()) {
            for (name, value) in block {
                if let Some(cfg) = resolve_command_provider_config(name, tts_config) {
                    providers.insert(name.to_ascii_lowercase(), cfg);
                } else if !is_reserved_tts_provider(name) && is_command_provider_config(value) {
                    // Unreachable for normal values, but keeps the helper
                    // robust if lookup semantics change.
                    let command = value["command"].as_str().unwrap_or("").trim().to_string();
                    providers.insert(
                        name.to_ascii_lowercase(),
                        CommandTtsProviderConfig {
                            name: name.clone(),
                            command,
                            config: value.clone(),
                        },
                    );
                }
            }
        }
        for (name, value) in object {
            if name == "providers"
                || is_reserved_tts_provider(name)
                || !is_command_provider_config(value)
            {
                continue;
            }
            providers
                .entry(name.to_ascii_lowercase())
                .or_insert_with(|| CommandTtsProviderConfig {
                    name: name.clone(),
                    command: value["command"].as_str().unwrap_or("").trim().to_string(),
                    config: value.clone(),
                });
        }
    }
    providers.into_values().collect()
}

fn positive_usize(value: Option<&Value>) -> Option<usize> {
    match value? {
        Value::Number(n) => n
            .as_u64()
            .and_then(|v| usize::try_from(v).ok())
            .filter(|v| *v > 0),
        _ => None,
    }
}

pub(crate) fn resolve_max_text_length(provider: Option<&str>, tts_config: &Value) -> usize {
    let provider = match normalize_provider_name(provider) {
        Some(provider) => provider,
        None => return FALLBACK_MAX_TEXT_LENGTH,
    };
    if let Some(cfg) = named_tts_provider_config(tts_config, &provider) {
        if let Some(v) = positive_usize(cfg.get("max_text_length")) {
            return v;
        }
    }
    if resolve_command_provider_config(&provider, tts_config).is_some() {
        return DEFAULT_COMMAND_TTS_MAX_TEXT_LENGTH;
    }
    match provider.as_str() {
        "openai" => 4_096,
        "xai" => 15_000,
        "minimax" => 10_000,
        "mistral" => 10_000,
        "gemini" => 32_000,
        "piper" => 5_000,
        "kittentts" => 5_000,
        "edge" | "edge_tts" | "edge-tts" | "neutts" => FALLBACK_MAX_TEXT_LENGTH,
        "elevenlabs" => elevenlabs_max_text_length(tts_config),
        _ => FALLBACK_MAX_TEXT_LENGTH,
    }
}

fn elevenlabs_max_text_length(tts_config: &Value) -> usize {
    let cfg = named_tts_provider_config(tts_config, "elevenlabs");
    if let Some(v) = cfg.and_then(|cfg| positive_usize(cfg.get("max_text_length"))) {
        return v;
    }
    let model = cfg
        .and_then(|cfg| cfg.get("model_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("eleven_multilingual_v2");
    match model {
        "eleven_flash_v2_5" => 40_000,
        "eleven_flash_v2" => 30_000,
        "eleven_v3" => 5_000,
        "eleven_multilingual_v2" => 10_000,
        _ => 10_000,
    }
}

fn number_from_config(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

pub(crate) fn tts_speed_for_provider(
    provider: &str,
    tts_config: &Value,
    explicit_speed: Option<f64>,
) -> Option<f64> {
    let raw = explicit_speed
        .or_else(|| {
            named_tts_provider_config(tts_config, provider)
                .and_then(|cfg| number_from_config(cfg.get("speed")))
        })
        .or_else(|| number_from_config(tts_config.get("speed")));
    raw.map(|speed| speed.clamp(0.25, 4.0))
}

fn resolve_tts_provider_name(
    explicit_provider: Option<&str>,
    tts_config: &Value,
    elevenlabs_available: bool,
) -> String {
    let configured_provider = tts_config
        .get("provider")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    explicit_provider
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or(configured_provider)
        .unwrap_or(if elevenlabs_available {
            "elevenlabs"
        } else {
            "openai"
        })
        .to_ascii_lowercase()
}

fn command_tts_timeout(config: &Value) -> Duration {
    let seconds = number_from_config(config.get("timeout_seconds"))
        .or_else(|| number_from_config(config.get("timeout")))
        .filter(|v| *v > 0.0)
        .unwrap_or(DEFAULT_COMMAND_TTS_TIMEOUT_SECONDS as f64);
    Duration::from_secs_f64(seconds)
}

fn command_tts_output_format(config: &Value, output_path: Option<&Path>) -> &'static str {
    if let Some(path) = output_path {
        if let Some(ext) = path
            .extension()
            .and_then(|v| v.to_str())
            .map(|v| v.to_ascii_lowercase())
        {
            if let Some(format) = COMMAND_TTS_OUTPUT_FORMATS
                .iter()
                .copied()
                .find(|format| *format == ext)
            {
                return format;
            }
        }
    }
    config
        .get("output_format")
        .or_else(|| config.get("format"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .and_then(|v| {
            COMMAND_TTS_OUTPUT_FORMATS
                .iter()
                .copied()
                .find(|format| format.eq_ignore_ascii_case(v))
        })
        .unwrap_or(DEFAULT_COMMAND_TTS_OUTPUT_FORMAT)
}

fn command_tts_voice_compatible(config: &Value) -> bool {
    match config.get("voice_compatible") {
        Some(Value::Bool(v)) => *v,
        Some(Value::String(s)) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

pub(crate) fn shell_quote_context(template: &str, placeholder_start: usize) -> Option<char> {
    let mut single = false;
    let mut double = false;
    let mut escape = false;
    for (idx, ch) in template.char_indices() {
        if idx >= placeholder_start {
            break;
        }
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' && double {
            escape = true;
            continue;
        }
        match ch {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            _ => {}
        }
    }
    if single {
        Some('\'')
    } else if double {
        Some('"')
    } else {
        None
    }
}

fn posix_shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let safe = value.bytes().all(|b| {
        b.is_ascii_alphanumeric()
            || matches!(b, b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b',' | b'@')
    });
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn quote_placeholder_for_context(template: &str, start: usize, value: &str) -> String {
    match shell_quote_context(template, start) {
        Some('\'') => value.replace('\'', "'\\''"),
        Some('"') => value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('`', "\\`"),
        _ => posix_shell_quote(value),
    }
}

pub(crate) fn render_command_tts_template(
    template: &str,
    placeholders: &BTreeMap<&str, String>,
) -> String {
    let mut out = String::with_capacity(template.len());
    let mut idx = 0;
    while idx < template.len() {
        let rest = &template[idx..];
        if rest.starts_with("{{") {
            out.push('{');
            idx += 2;
            continue;
        }
        if rest.starts_with("}}") {
            out.push('}');
            idx += 2;
            continue;
        }
        if rest.starts_with('{') {
            if let Some(close) = rest.find('}') {
                let key = &rest[1..close];
                if let Some(value) = placeholders.get(key) {
                    out.push_str(&quote_placeholder_for_context(template, idx, value));
                    idx += close + 1;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().expect("non-empty rest");
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn load_tts_config_from_env_or_file() -> Value {
    if let Ok(raw) = std::env::var("HERMES_TTS_CONFIG_JSON") {
        if let Ok(value) = serde_json::from_str::<Value>(&raw) {
            return value;
        }
    }
    hermes_config::load_config(None)
        .map(|cfg| cfg.tts)
        .unwrap_or(Value::Null)
}

/// TTS backend that dispatches to ElevenLabs, OpenAI, or MiniMax based on
/// the `provider` argument. Defaults to `openai` when no API keys hint at a
/// preferred provider.
pub struct MultiTtsBackend {
    client: Client,
    elevenlabs_key: Option<String>,
    openai_base_url: String,
    minimax: MiniMaxTtsBackend,
    minimax_available: bool,
    tts_config: Value,
}

impl MultiTtsBackend {
    pub fn new() -> Self {
        let minimax_available = std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_some();
        Self {
            client: Client::new(),
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY").ok(),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            minimax: MiniMaxTtsBackend::from_env(),
            minimax_available,
            tts_config: load_tts_config_from_env_or_file(),
        }
    }

    #[cfg(test)]
    fn with_tts_config(tts_config: Value) -> Self {
        Self {
            tts_config,
            ..Self::new()
        }
    }

    async fn elevenlabs_tts(
        &self,
        text: &str,
        voice: &str,
        output_path: Option<&str>,
    ) -> Result<String, ToolError> {
        let api_key = self
            .elevenlabs_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("ELEVENLABS_API_KEY not set".into()))?;

        let body = json!({
            "text": text,
            "model_id": "eleven_monolingual_v1",
        });

        let resp = self
            .client
            .post(format!(
                "https://api.elevenlabs.io/v1/text-to-speech/{}",
                voice
            ))
            .header("xi-api-key", api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("ElevenLabs API failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "ElevenLabs error ({}): {}",
                status, text
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        let output_path = output_path.map(PathBuf::from).unwrap_or_else(|| {
            std::env::temp_dir().join(format!("hermes_tts_{}.mp3", uuid::Uuid::new_v4()))
        });
        tokio::fs::write(&output_path, &bytes)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write audio: {}", e)))?;

        Ok(json!({
            "provider": "elevenlabs",
            "file": output_path.display().to_string(),
            "voice": voice,
            "bytes": bytes.len(),
        })
        .to_string())
    }

    async fn openai_tts(
        &self,
        text: &str,
        voice: &str,
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError> {
        // Resolve transport in priority order:
        // 1. Managed Nous gateway (HERMES_ENABLE_NOUS_MANAGED_TOOLS + Nous token)
        // 2. Direct OpenAI with VOICE_TOOLS_OPENAI_KEY override, then
        //    HERMES_OPENAI_API_KEY, then legacy OPENAI_API_KEY.
        let managed = resolve_managed_tool_gateway("openai-audio", ResolveOptions::default());
        let (endpoint, bearer, transport) = match managed {
            Some(cfg) => Self::openai_audio_managed_endpoint(&cfg),
            None => {
                let key = resolve_openai_audio_api_key();
                if key.is_empty() {
                    return Err(ToolError::ExecutionFailed(
                        "HERMES_OPENAI_API_KEY (or OPENAI_API_KEY / VOICE_TOOLS_OPENAI_KEY) \
                         not set, and no managed openai-audio gateway is configured."
                            .into(),
                    ));
                }
                (
                    format!("{}/audio/speech", self.openai_base_url),
                    key,
                    "direct",
                )
            }
        };

        let mut body = json!({
            "model": "tts-1",
            "input": text,
            "voice": voice,
        });
        if let Some(speed) = tts_speed_for_provider("openai", &self.tts_config, speed) {
            body["speed"] = json!(speed);
        }

        let resp = self
            .client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", bearer))
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("OpenAI TTS API failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ToolError::ExecutionFailed(format!(
                "OpenAI TTS error ({}): {}",
                status, text
            )));
        }

        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read audio: {}", e)))?;

        let output_path = output_path.map(PathBuf::from).unwrap_or_else(|| {
            std::env::temp_dir().join(format!("hermes_tts_{}.mp3", uuid::Uuid::new_v4()))
        });
        tokio::fs::write(&output_path, &bytes)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write audio: {}", e)))?;

        Ok(json!({
            "provider": "openai",
            "transport": transport,
            "file": output_path.display().to_string(),
            "voice": voice,
            "bytes": bytes.len(),
        })
        .to_string())
    }

    async fn piper_tts(
        &self,
        text: &str,
        voice: Option<&str>,
        output_path: Option<&str>,
    ) -> Result<String, ToolError> {
        let binary = std::env::var("PIPER_BINARY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "piper".to_string());

        let model = voice
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
            .or_else(|| {
                std::env::var("PIPER_MODEL")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "Piper requires a model. Set provider='piper' with voice='<model-path-or-name>' \
                     or set PIPER_MODEL."
                        .into(),
                )
            })?;

        let output_path = output_path.map(PathBuf::from).unwrap_or_else(|| {
            std::env::temp_dir().join(format!("hermes_tts_{}.wav", uuid::Uuid::new_v4()))
        });
        let mut cmd = Command::new(&binary);
        cmd.arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(&output_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        if let Ok(config) = std::env::var("PIPER_CONFIG") {
            let config = config.trim();
            if !config.is_empty() {
                cmd.arg("--config").arg(config);
            }
        }
        if let Ok(v) = std::env::var("PIPER_SPEAKER") {
            let v = v.trim();
            if !v.is_empty() {
                cmd.arg("--speaker").arg(v);
            }
        }
        if let Ok(v) = std::env::var("PIPER_LENGTH_SCALE") {
            let v = v.trim();
            if !v.is_empty() {
                cmd.arg("--length_scale").arg(v);
            }
        }
        if let Ok(v) = std::env::var("PIPER_NOISE_SCALE") {
            let v = v.trim();
            if !v.is_empty() {
                cmd.arg("--noise_scale").arg(v);
            }
        }
        if let Ok(v) = std::env::var("PIPER_NOISE_W") {
            let v = v.trim();
            if !v.is_empty() {
                cmd.arg("--noise_w").arg(v);
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to start piper binary '{}': {}", binary, e))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes()).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed writing to piper stdin: {}", e))
            })?;
            stdin.write_all(b"\n").await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed finalizing piper stdin: {}", e))
            })?;
            stdin.shutdown().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed closing piper stdin: {}", e))
            })?;
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("piper process failed: {}", e)))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ToolError::ExecutionFailed(format!(
                "piper exited with status {}{}",
                output.status,
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr)
                }
            )));
        }

        let bytes = tokio::fs::read(&output_path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "piper completed but failed reading output {}: {}",
                output_path.display(),
                e
            ))
        })?;

        Ok(json!({
            "provider": "piper",
            "transport": "local",
            "file": output_path.display().to_string(),
            "voice": model,
            "bytes": bytes.len(),
        })
        .to_string())
    }

    async fn command_tts(
        &self,
        text: &str,
        voice: Option<&str>,
        provider: &CommandTtsProviderConfig,
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError> {
        let output_hint = output_path.map(PathBuf::from);
        let format = command_tts_output_format(&provider.config, output_hint.as_deref());
        let output_path = output_hint.unwrap_or_else(|| {
            std::env::temp_dir().join(format!("hermes_tts_{}.{}", uuid::Uuid::new_v4(), format))
        });
        let input_path =
            std::env::temp_dir().join(format!("hermes_tts_input_{}.txt", uuid::Uuid::new_v4()));
        tokio::fs::write(&input_path, text)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("write TTS command input: {e}")))?;

        let mut placeholders = BTreeMap::new();
        placeholders.insert("input_path", input_path.display().to_string());
        placeholders.insert("text_path", input_path.display().to_string());
        placeholders.insert("output_path", output_path.display().to_string());
        placeholders.insert("format", format.to_string());
        placeholders.insert("voice", voice.unwrap_or("").to_string());
        placeholders.insert(
            "model",
            provider
                .config
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        );
        placeholders.insert(
            "speed",
            speed
                .or_else(|| tts_speed_for_provider(&provider.name, &self.tts_config, None))
                .unwrap_or(1.0)
                .to_string(),
        );

        let rendered = render_command_tts_template(&provider.command, &placeholders);
        let mut cmd = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(&rendered);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&rendered);
            cmd
        };
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());

        let duration = command_tts_timeout(&provider.config);
        let output = match timeout(duration, cmd.output()).await {
            Ok(result) => result.map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "TTS command provider '{}' failed to start: {e}",
                    provider.name
                ))
            })?,
            Err(_) => {
                let _ = tokio::fs::remove_file(&input_path).await;
                return Err(ToolError::ExecutionFailed(format!(
                    "TTS command provider '{}' timed out after {:.1}s",
                    provider.name,
                    duration.as_secs_f64()
                )));
            }
        };
        let _ = tokio::fs::remove_file(&input_path).await;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ToolError::ExecutionFailed(format!(
                "TTS command provider '{}' exited with code {}{}",
                provider.name,
                output.status.code().unwrap_or(-1),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr)
                }
            )));
        }

        let metadata = tokio::fs::metadata(&output_path).await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "TTS command provider '{}' produced no output at {}: {e}",
                provider.name,
                output_path.display()
            ))
        })?;
        if metadata.len() == 0 {
            return Err(ToolError::ExecutionFailed(format!(
                "TTS command provider '{}' produced no output",
                provider.name
            )));
        }

        let voice_compatible = command_tts_voice_compatible(&provider.config);
        Ok(json!({
            "provider": provider.name,
            "transport": "command",
            "file": output_path.display().to_string(),
            "file_path": output_path.display().to_string(),
            "format": format,
            "voice": voice.unwrap_or(""),
            "bytes": metadata.len(),
            "voice_compatible": voice_compatible,
            "media_tag": if voice_compatible {
                format!("[[audio_as_voice]]{}", output_path.display())
            } else {
                format!("[[audio]]{}", output_path.display())
            },
        })
        .to_string())
    }

    /// Compose the OpenAI-audio gateway endpoint + bearer for a resolved
    /// managed config. Public visibility kept tight (`pub(crate)`) so the
    /// `tts_premium` handler can reuse it later if needed.
    pub(crate) fn openai_audio_managed_endpoint(
        cfg: &ManagedToolGatewayConfig,
    ) -> (String, String, &'static str) {
        let base = cfg.gateway_origin.trim_end_matches('/');
        (
            format!("{base}/audio/speech"),
            cfg.nous_user_token.clone(),
            "managed",
        )
    }

    /// Public accessor so other tool handlers (e.g. `tts_premium`) can reuse
    /// the ElevenLabs HTTP path without instantiating a second client.
    pub async fn synthesize_elevenlabs(
        &self,
        text: &str,
        voice: &str,
    ) -> Result<String, ToolError> {
        self.elevenlabs_tts(text, voice, None).await
    }
}

impl Default for MultiTtsBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TtsBackend for MultiTtsBackend {
    async fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        provider: Option<&str>,
        output_path: Option<&str>,
        speed: Option<f64>,
    ) -> Result<String, ToolError> {
        // Default provider preference:
        // 1. ELEVENLABS_API_KEY set → elevenlabs (highest quality)
        // 2. Otherwise OpenAI (cheapest HTTP-only path)
        // Zero-Python: edge_tts removed entirely — callers asking for
        // "edge_tts" receive a clear migration error.
        let resolved_provider_key =
            resolve_tts_provider_name(provider, &self.tts_config, self.elevenlabs_key.is_some());
        let resolved_provider = resolved_provider_key.as_str();

        let max_len = resolve_max_text_length(Some(resolved_provider), &self.tts_config);
        let text_buf;
        let text = if text.chars().count() > max_len {
            tracing::warn!(
                provider = resolved_provider,
                max_text_length = max_len,
                "truncating TTS input to provider limit"
            );
            text_buf = text.chars().take(max_len).collect::<String>();
            text_buf.as_str()
        } else {
            text
        };

        if let Some(command_provider) =
            resolve_command_provider_config(resolved_provider, &self.tts_config)
        {
            return self
                .command_tts(text, voice, &command_provider, output_path, speed)
                .await;
        }

        match resolved_provider {
            "elevenlabs" => {
                let voice = voice.unwrap_or("21m00Tcm4TlvDq8ikWAM"); // Rachel
                self.elevenlabs_tts(text, voice, output_path).await
            }
            "openai" => {
                let voice = voice.unwrap_or("alloy");
                self.openai_tts(text, voice, output_path, speed).await
            }
            "minimax" => {
                if !self.minimax_available {
                    return Err(ToolError::ExecutionFailed("MINIMAX_API_KEY not set".into()));
                }
                self.minimax
                    .synthesize(text, voice, Some(resolved_provider), output_path, speed)
                    .await
            }
            "piper" => self.piper_tts(text, voice, output_path).await,
            "edge" | "edge_tts" | "edge-tts" | "neutts" | "kittentts" => Err(
                ToolError::InvalidParams(format!(
                "{resolved_provider} is not supported in hermes-agent-rust (zero-Python). \
                 Use provider='openai' (HERMES_OPENAI_API_KEY or OPENAI_API_KEY), \
                 'elevenlabs' (ELEVENLABS_API_KEY), 'minimax' (MINIMAX_API_KEY), \
                 or 'piper' (local piper binary + PIPER_MODEL)."
            )),
            ),
            other => Err(ToolError::InvalidParams(format!(
                "Unknown TTS provider: '{other}'. Use 'openai', 'elevenlabs', 'minimax', or 'piper'.",
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_audio_managed_endpoint_appends_audio_speech() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "openai-audio".into(),
            gateway_origin: "https://openai-audio-gateway.example.com/".into(),
            nous_user_token: "tok-xyz".into(),
            managed_mode: true,
        };
        let (endpoint, bearer, transport) = MultiTtsBackend::openai_audio_managed_endpoint(&cfg);
        assert_eq!(
            endpoint,
            "https://openai-audio-gateway.example.com/audio/speech"
        );
        assert_eq!(bearer, "tok-xyz");
        assert_eq!(transport, "managed");
    }

    #[test]
    fn command_provider_resolution_matches_upstream_contracts() {
        let cfg = json!({
            "providers": {
                "openai": {"type": "command", "command": "echo no"},
                "piper": {"type": "command", "command": "echo no"},
                "piper-cli": {"command": "piper-cli foo"},
                "broken": {"type": "command", "command": "   "}
            },
            "voxcpm": {"type": "command", "command": "voxcpm"}
        });

        assert!(resolve_command_provider_config("openai", &cfg).is_none());
        assert!(resolve_command_provider_config("piper", &cfg).is_none());
        assert!(resolve_command_provider_config("broken", &cfg).is_none());
        assert_eq!(
            resolve_command_provider_config("PIPER-CLI", &cfg)
                .unwrap()
                .command,
            "piper-cli foo"
        );
        let names: Vec<_> = iter_command_providers(&cfg)
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["piper-cli".to_string(), "voxcpm".to_string()]);
    }

    #[test]
    fn tts_max_text_length_is_provider_and_model_specific() {
        assert_eq!(resolve_max_text_length(Some("OpenAI"), &json!({})), 4_096);
        assert_eq!(resolve_max_text_length(Some("xai"), &json!({})), 15_000);
        assert_eq!(resolve_max_text_length(Some("minimax"), &json!({})), 10_000);
        assert_eq!(
            resolve_max_text_length(
                Some("elevenlabs"),
                &json!({"elevenlabs": {"model_id": "eleven_flash_v2_5"}})
            ),
            40_000
        );
        assert_eq!(
            resolve_max_text_length(
                Some("elevenlabs"),
                &json!({"elevenlabs": {"model_id": "eleven_v3"}})
            ),
            5_000
        );
        assert_eq!(
            resolve_max_text_length(Some("openai"), &json!({"openai": {"max_text_length": 99}})),
            99
        );
        assert_eq!(
            resolve_max_text_length(
                Some("cmd"),
                &json!({"providers": {"cmd": {"command": "cp {input_path} {output_path}"}}})
            ),
            DEFAULT_COMMAND_TTS_MAX_TEXT_LENGTH
        );
        assert_eq!(
            resolve_max_text_length(None, &json!({})),
            FALLBACK_MAX_TEXT_LENGTH
        );
    }

    #[test]
    fn command_template_rendering_quotes_placeholders_by_context() {
        let mut placeholders = BTreeMap::new();
        placeholders.insert("input_path", "/tmp/Jane Doe/in.txt".to_string());
        placeholders.insert("output_path", "/tmp/out; rm -rf /".to_string());
        placeholders.insert("voice", "$(whoami)".to_string());

        assert_eq!(shell_quote_context("tts '{output_path}'", 5), Some('\''));
        assert_eq!(shell_quote_context("tts \"{output_path}\"", 5), Some('"'));

        let rendered = render_command_tts_template(
            "tts --voice {voice} --in {input_path} --out {output_path} '{{literal}}'",
            &placeholders,
        );
        assert!(rendered.contains("'$(whoami)'"));
        assert!(rendered.contains("'/tmp/Jane Doe/in.txt'"));
        assert!(rendered.contains("'/tmp/out; rm -rf /'"));
        assert!(rendered.contains("'{literal}'"));

        let rendered = render_command_tts_template("tts --voice \"{voice}\"", &placeholders);
        assert!(rendered.contains("\"\\$(whoami)\""));
    }

    #[tokio::test]
    async fn command_provider_writes_output_and_reports_voice_compatibility() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("clip.ogg");
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {input_path} {output_path}",
                    "output_format": "ogg",
                    "voice_compatible": true
                }
            }
        }));

        let out = backend
            .synthesize(
                "hello command",
                Some("voice-a"),
                None,
                Some(output.to_str().unwrap()),
                Some(1.2),
            )
            .await
            .unwrap();
        let data: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(data["provider"], "copy-tts");
        assert_eq!(data["transport"], "command");
        assert_eq!(data["voice_compatible"], true);
        assert!(data["media_tag"]
            .as_str()
            .unwrap()
            .starts_with("[[audio_as_voice]]"));
        assert_eq!(std::fs::read_to_string(output).unwrap(), "hello command");
    }

    #[test]
    fn tts_speed_prefers_provider_then_global_and_clamps() {
        let cfg = json!({"speed": 1.5, "openai": {"speed": 10.0}});
        assert_eq!(tts_speed_for_provider("openai", &cfg, None), Some(4.0));
        assert_eq!(tts_speed_for_provider("piper", &cfg, None), Some(1.5));
        assert_eq!(
            tts_speed_for_provider("openai", &cfg, Some(0.1)),
            Some(0.25)
        );
    }

    #[test]
    fn tts_provider_resolution_treats_null_and_missing_as_default() {
        assert_eq!(
            resolve_tts_provider_name(None, &json!({"provider": null}), false),
            "openai"
        );
        assert_eq!(
            resolve_tts_provider_name(None, &json!({}), true),
            "elevenlabs"
        );
        assert_eq!(
            resolve_tts_provider_name(Some(" OPENAI "), &json!({"provider": "piper"}), true),
            "openai"
        );
        assert_eq!(
            resolve_tts_provider_name(None, &json!({"provider": " PIPER "}), false),
            "piper"
        );
    }

    #[tokio::test]
    async fn test_edge_tts_returns_migration_error() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hello", None, Some("edge_tts"), None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not supported") || msg.contains("zero-Python"));
        assert!(msg.contains("openai") || msg.contains("elevenlabs"));
    }

    #[tokio::test]
    async fn test_neutts_returns_migration_error() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hi", None, Some("neutts"), None, None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not supported") || msg.contains("zero-Python"));
    }

    #[tokio::test]
    async fn test_unknown_provider_errors() {
        let backend = MultiTtsBackend::new();
        let err = backend
            .synthesize("hello", None, Some("bogus"), None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Unknown"));
    }

    #[tokio::test]
    async fn test_piper_requires_model_hint() {
        let backend = MultiTtsBackend::new();
        let _guard = EnvVarGuard::new("PIPER_MODEL");
        std::env::remove_var("PIPER_MODEL");
        let err = backend
            .synthesize("hello", None, Some("piper"), None, None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("PIPER_MODEL"));
    }

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn new(key: &'static str) -> Self {
            Self {
                key,
                old: std::env::var(key).ok(),
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(v) = &self.old {
                std::env::set_var(self.key, v);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}
