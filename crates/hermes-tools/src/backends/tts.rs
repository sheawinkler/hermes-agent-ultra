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

use crate::bounded_subprocess::{
    run_bounded_command_with_output_limit, run_bounded_command_with_stdin_and_output_limit,
    BoundedCommandError, BoundedOutputDirectoryLimit,
};

use crate::tools::tts::TtsBackend;
use crate::tts_audio::{
    commit_generated_audio, create_private_temp_file, default_tts_audio_path,
    generated_audio_workspace_path, read_bounded_error, read_bounded_response, validate_audio_file,
    validate_ogg_opus_file, validate_requested_output_format, write_validated_audio,
    MAX_TTS_AUDIO_BYTES,
};
use crate::tts_streaming::minimax::MiniMaxTtsBackend;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};
use hermes_core::{subprocess::CommandNoWindowExt, ToolError};

pub const FALLBACK_MAX_TEXT_LENGTH: usize = 4_000;
pub const DEFAULT_COMMAND_TTS_MAX_TEXT_LENGTH: usize = 15_000;
pub const DEFAULT_COMMAND_TTS_OUTPUT_FORMAT: &str = "mp3";
pub const DEFAULT_COMMAND_TTS_TIMEOUT_SECONDS: u64 = 60;
pub const MAX_COMMAND_TTS_TIMEOUT_SECONDS: f64 = 600.0;
pub const COMMAND_TTS_OUTPUT_FORMATS: &[&str] = &["mp3", "wav", "ogg", "flac"];
const DEFAULT_ELEVENLABS_MODEL_ID: &str = "eleven_multilingual_v2";
const DEFAULT_ELEVENLABS_VOICE_ID: &str = "21m00Tcm4TlvDq8ikWAM";
const DEFAULT_ELEVENLABS_BASE_URL: &str = "https://api.elevenlabs.io/v1";

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

#[derive(Clone, Copy)]
struct ResolvedElevenLabsConfig<'a> {
    model_id: &'a str,
    voice_id: &'a str,
    base_url: &'a str,
}

fn resolved_elevenlabs_config(tts_config: &Value) -> ResolvedElevenLabsConfig<'_> {
    let cfg = named_tts_provider_config(tts_config, "elevenlabs");
    let configured = |key| {
        cfg.and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    ResolvedElevenLabsConfig {
        model_id: configured("model_id").unwrap_or(DEFAULT_ELEVENLABS_MODEL_ID),
        voice_id: configured("voice_id").unwrap_or(DEFAULT_ELEVENLABS_VOICE_ID),
        base_url: configured("base_url").unwrap_or(DEFAULT_ELEVENLABS_BASE_URL),
    }
}

fn elevenlabs_text_to_speech_url(base_url: &str, voice: &str) -> Result<url::Url, ToolError> {
    let mut endpoint = url::Url::parse(base_url).map_err(|error| {
        ToolError::InvalidParams(format!("Invalid ElevenLabs base_url: {error}"))
    })?;
    let mut segments = endpoint.path_segments_mut().map_err(|()| {
        ToolError::InvalidParams("Invalid ElevenLabs base_url: cannot append a path".to_string())
    })?;
    segments.pop_if_empty();
    segments.push("text-to-speech");
    // `push` serializes this as one path segment. In particular, slash,
    // query, fragment, and dot-segment characters in a model-provided voice
    // never alter the authenticated endpoint.
    segments.push(voice);
    drop(segments);
    Ok(endpoint)
}

fn elevenlabs_max_text_length(tts_config: &Value) -> usize {
    let cfg = named_tts_provider_config(tts_config, "elevenlabs");
    if let Some(v) = cfg.and_then(|cfg| positive_usize(cfg.get("max_text_length"))) {
        return v;
    }
    let model = resolved_elevenlabs_config(tts_config).model_id;
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
    number_from_config(config.get("timeout_seconds"))
        .or_else(|| number_from_config(config.get("timeout")))
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .map(|seconds| seconds.min(MAX_COMMAND_TTS_TIMEOUT_SECONDS))
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .unwrap_or(Duration::from_secs(DEFAULT_COMMAND_TTS_TIMEOUT_SECONDS))
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

fn command_tts_ffmpeg_path(config: &Value) -> &str {
    config
        .get("ffmpeg_path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ffmpeg")
}

fn command_tts_voice_output_path(source: &Path) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("audio");
    let name = format!("{stem}-voice-{}.ogg", uuid::Uuid::new_v4());
    source.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

struct TempPathGuard(Option<PathBuf>);

impl TempPathGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0.take();
    }
}

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

struct TempDirectoryGuard(Option<PathBuf>);

impl TempDirectoryGuard {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }
}

impl Drop for TempDirectoryGuard {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

async fn transcode_command_tts_voice(
    source: &Path,
    config: &Value,
    duration: Duration,
    remove_source_on_success: bool,
) -> Result<PathBuf, ToolError> {
    let target = command_tts_voice_output_path(source);
    let (target_workspace, generated_target) = generated_audio_workspace_path(&target, "ogg")?;
    let _target_workspace_guard = TempDirectoryGuard::new(target_workspace.clone());
    let mut command = Command::new(command_tts_ffmpeg_path(config));
    command
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(source)
        .arg("-vn")
        .arg("-c:a")
        .arg("libopus")
        .arg("-f")
        .arg("ogg")
        .arg(&generated_target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    command.suppress_windows_console();

    let output = match run_bounded_command_with_output_limit(
        command,
        duration,
        true,
        BoundedOutputDirectoryLimit::new(target_workspace, 1, MAX_TTS_AUDIO_BYTES as u64),
    )
    .await
    {
        Ok(output) => output,
        Err(error) => {
            let detail = match error {
                BoundedCommandError::Start(error) => format!(
                    "failed to start voice-compatible TTS transcoder '{}': {error}",
                    command_tts_ffmpeg_path(config)
                ),
                BoundedCommandError::Stdin(error) => {
                    format!("voice-compatible TTS transcoder rejected stdin: {error}")
                }
                BoundedCommandError::Wait(error) => {
                    format!("failed waiting for voice-compatible TTS transcoder: {error}")
                }
                BoundedCommandError::Stderr(error) => {
                    format!("failed capturing voice-compatible TTS diagnostics: {error}")
                }
                BoundedCommandError::OutputLimit(error) => {
                    format!("voice-compatible TTS transcoder exceeded its output limit: {error}")
                }
                BoundedCommandError::TimedOut => format!(
                    "voice-compatible TTS transcoding timed out after {:.1}s",
                    duration.as_secs_f64()
                ),
            };
            return Err(ToolError::ExecutionFailed(detail));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(ToolError::ExecutionFailed(format!(
            "voice-compatible TTS transcoding failed with code {}{}",
            output.status.code().unwrap_or(-1),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {stderr}")
            }
        )));
    }
    validate_audio_file(&generated_target, "ogg", "voice-compatible TTS transcoder").await?;
    validate_ogg_opus_file(&generated_target, "voice-compatible TTS transcoder").await?;
    commit_generated_audio(
        &generated_target,
        &target,
        "ogg",
        "voice-compatible TTS transcoder",
    )
    .await?;
    if remove_source_on_success {
        if let Err(error) = tokio::fs::remove_file(source).await {
            return Err(ToolError::ExecutionFailed(format!(
                "voice-compatible TTS transcoding could not remove temporary source {}: {error}",
                source.display()
            )));
        }
    }
    Ok(target)
}

const COMMAND_TTS_TEMPLATE_PLACEHOLDERS: &[&str] = &[
    "input_path",
    "text_path",
    "output_path",
    "format",
    "voice",
    "model",
    "speed",
];

fn invalid_command_tts_template(reason: &str) -> ToolError {
    ToolError::InvalidParams(format!(
        "Unsupported command TTS argv template: {reason}. \
         Use whitespace-separated argv words with a fixed literal executable."
    ))
}

fn is_command_tts_literal_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b'-' | b'.' | b'/' | b':' | b'=' | b',' | b'@' | b'+' | b'%'
        )
}

fn finish_command_tts_argv_word(
    argv: &mut Vec<String>,
    word: &mut String,
    word_started: &mut bool,
    contains_placeholder: &mut bool,
) -> Result<(), ToolError> {
    if !*word_started {
        return Ok(());
    }
    if argv.is_empty() && *contains_placeholder {
        return Err(invalid_command_tts_template(
            "the executable must be a fixed literal, not a placeholder",
        ));
    }
    argv.push(std::mem::take(word));
    *word_started = false;
    *contains_placeholder = false;
    Ok(())
}

/// Parse a deliberately small argv-template grammar. Shell syntax is rejected
/// rather than quoted heuristically: placeholder values become direct process
/// arguments and are never interpreted by `sh -c`.
fn parse_command_tts_argv_template(
    template: &str,
    placeholders: Option<&BTreeMap<&str, String>>,
) -> Result<Vec<String>, ToolError> {
    let mut argv = Vec::new();
    let mut word = String::new();
    let mut word_started = false;
    let mut contains_placeholder = false;
    let bytes = template.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' => {
                finish_command_tts_argv_word(
                    &mut argv,
                    &mut word,
                    &mut word_started,
                    &mut contains_placeholder,
                )?;
                index += 1;
            }
            b'\r' | b'\n' => {
                return Err(invalid_command_tts_template(
                    "newlines and heredocs are not supported",
                ));
            }
            b'{' => {
                let remaining = &template[index + 1..];
                let Some(close) = remaining.find('}') else {
                    return Err(invalid_command_tts_template("an unclosed placeholder"));
                };
                let key = &remaining[..close];
                if !COMMAND_TTS_TEMPLATE_PLACEHOLDERS.contains(&key) {
                    return Err(invalid_command_tts_template("an unknown placeholder"));
                }
                if let Some(placeholders) = placeholders {
                    let value = placeholders.get(key).ok_or_else(|| {
                        invalid_command_tts_template("a placeholder without a value")
                    })?;
                    word.push_str(value);
                }
                word_started = true;
                contains_placeholder = true;
                index += close + 2;
            }
            b'}' => {
                return Err(invalid_command_tts_template("an unmatched closing brace"));
            }
            byte if is_command_tts_literal_byte(byte) => {
                word.push(byte as char);
                word_started = true;
                index += 1;
            }
            _ => {
                return Err(invalid_command_tts_template(
                    "quotes, escapes, substitutions, redirects, and shell operators are not supported",
                ));
            }
        }
    }
    finish_command_tts_argv_word(
        &mut argv,
        &mut word,
        &mut word_started,
        &mut contains_placeholder,
    )?;
    if argv.is_empty() {
        return Err(invalid_command_tts_template("the command is empty"));
    }
    Ok(argv)
}

fn validate_command_tts_argv_template(template: &str) -> Result<(), ToolError> {
    parse_command_tts_argv_template(template, None).map(|_| ())
}

fn render_command_tts_argv_template(
    template: &str,
    placeholders: &BTreeMap<&str, String>,
) -> Result<Vec<String>, ToolError> {
    parse_command_tts_argv_template(template, Some(placeholders))
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
    elevenlabs_base_url: String,
    openai_base_url: String,
    minimax: MiniMaxTtsBackend,
    minimax_available: bool,
    tts_config: Value,
}

impl MultiTtsBackend {
    pub fn new() -> Self {
        Self::with_tts_config(load_tts_config_from_env_or_file())
    }

    pub fn with_tts_config(tts_config: Value) -> Self {
        let minimax = MiniMaxTtsBackend::from_tts_config(&tts_config);
        let minimax_available = minimax.has_api_key();
        let elevenlabs_base_url = resolved_elevenlabs_config(&tts_config)
            .base_url
            .trim_end_matches('/')
            .to_string();
        Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(60))
                .build()
                .expect("valid TTS HTTP client"),
            elevenlabs_key: std::env::var("ELEVENLABS_API_KEY")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            elevenlabs_base_url,
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string()),
            minimax,
            minimax_available,
            tts_config,
        }
    }

    async fn elevenlabs_tts(
        &self,
        text: &str,
        voice: &str,
        output_path: Option<&str>,
    ) -> Result<String, ToolError> {
        validate_requested_output_format(output_path, "mp3", "elevenlabs")?;
        let api_key = self
            .elevenlabs_key
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed("ELEVENLABS_API_KEY not set".into()))?;

        let model_id = resolved_elevenlabs_config(&self.tts_config).model_id;
        let body = json!({
            "text": text,
            "model_id": model_id,
        });

        let endpoint = elevenlabs_text_to_speech_url(&self.elevenlabs_base_url, voice)?;
        let resp = self
            .client
            .post(endpoint)
            .header("xi-api-key", api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("ElevenLabs API failed: {}", e)))?;

        let status = resp.status();
        if !status.is_success() {
            let detail = read_bounded_error(resp).await;
            return Err(ToolError::ExecutionFailed(format!(
                "ElevenLabs error ({}): {}",
                status, detail
            )));
        }

        let bytes = read_bounded_response(resp, MAX_TTS_AUDIO_BYTES, "ElevenLabs audio").await?;

        let output_path = match output_path {
            Some(path) => PathBuf::from(path),
            None => default_tts_audio_path("hermes_tts", "mp3")?,
        };
        let written = write_validated_audio(&output_path, &bytes, "mp3", "elevenlabs").await?;

        Ok(json!({
            "provider": "elevenlabs",
            "file": output_path.display().to_string(),
            "voice": voice,
            "bytes": written,
            "format": "mp3",
            "model": model_id,
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
        validate_requested_output_format(output_path, "mp3", "openai")?;
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
            let detail = read_bounded_error(resp).await;
            return Err(ToolError::ExecutionFailed(format!(
                "OpenAI TTS error ({}): {}",
                status, detail
            )));
        }

        let bytes = read_bounded_response(resp, MAX_TTS_AUDIO_BYTES, "OpenAI TTS audio").await?;

        let output_path = match output_path {
            Some(path) => PathBuf::from(path),
            None => default_tts_audio_path("hermes_tts", "mp3")?,
        };
        let written = write_validated_audio(&output_path, &bytes, "mp3", "openai").await?;

        Ok(json!({
            "provider": "openai",
            "transport": transport,
            "file": output_path.display().to_string(),
            "voice": voice,
            "bytes": written,
            "format": "mp3",
        })
        .to_string())
    }

    async fn piper_tts(
        &self,
        text: &str,
        voice: Option<&str>,
        output_path: Option<&str>,
    ) -> Result<String, ToolError> {
        #[cfg(windows)]
        return Err(ToolError::ExecutionFailed(
            "Piper local TTS is unavailable on Windows until private subprocess workspaces can be contained safely."
                .to_string(),
        ));

        validate_requested_output_format(output_path, "wav", "piper")?;
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

        let output_path = match output_path {
            Some(path) => PathBuf::from(path),
            None => default_tts_audio_path("hermes_tts", "wav")?,
        };
        let (generated_workspace, generated_path) =
            generated_audio_workspace_path(&output_path, "wav")?;
        let _generated_workspace_guard = TempDirectoryGuard::new(generated_workspace.clone());
        let mut cmd = Command::new(&binary);
        cmd.arg("--model")
            .arg(&model)
            .arg("--output_file")
            .arg(&generated_path)
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
        cmd.suppress_windows_console();

        let mut input = Vec::with_capacity(text.len() + 1);
        input.extend_from_slice(text.as_bytes());
        input.push(b'\n');
        let timeout_config =
            named_tts_provider_config(&self.tts_config, "piper").unwrap_or(&self.tts_config);
        let duration = command_tts_timeout(timeout_config);
        let output = run_bounded_command_with_stdin_and_output_limit(
            cmd,
            duration,
            true,
            &input,
            BoundedOutputDirectoryLimit::new(generated_workspace, 1, MAX_TTS_AUDIO_BYTES as u64),
        )
        .await
        .map_err(|error| {
            let detail = match error {
                BoundedCommandError::Start(error) => {
                    format!("Failed to start piper binary '{binary}': {error}")
                }
                BoundedCommandError::Stdin(error) => {
                    format!("Failed writing to piper stdin: {error}")
                }
                BoundedCommandError::Wait(error) => {
                    format!("piper process failed while waiting: {error}")
                }
                BoundedCommandError::Stderr(error) => {
                    format!("piper diagnostics failed: {error}")
                }
                BoundedCommandError::OutputLimit(error) => {
                    format!("piper exceeded its output limit: {error}")
                }
                BoundedCommandError::TimedOut => {
                    format!("piper timed out after {:.1}s", duration.as_secs_f64())
                }
            };
            ToolError::ExecutionFailed(detail)
        })?;
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
        let bytes = commit_generated_audio(&generated_path, &output_path, "wav", "piper").await?;

        Ok(json!({
            "provider": "piper",
            "transport": "local",
            "file": output_path.display().to_string(),
            "voice": model,
            "bytes": bytes,
            "format": "wav",
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
        #[cfg(windows)]
        return Err(ToolError::ExecutionFailed(
            "Custom command TTS providers are unavailable on Windows; use a built-in provider."
                .to_string(),
        ));

        // Reject shell syntax before allocating any input or output artifact.
        validate_command_tts_argv_template(&provider.command)?;

        let output_hint = output_path.map(PathBuf::from);
        let output_is_temporary = output_hint.is_none();
        let format = command_tts_output_format(&provider.config, output_hint.as_deref());
        let output_path = match output_hint {
            Some(path) => path,
            None => default_tts_audio_path("hermes_tts", format)?,
        };
        let (generated_workspace, generated_path) =
            generated_audio_workspace_path(&output_path, format)?;
        let _generated_workspace_guard = TempDirectoryGuard::new(generated_workspace.clone());
        let mut output_guard = output_is_temporary.then(|| TempPathGuard::new(output_path.clone()));
        let input_path = generated_workspace.join("input.txt");
        let input_file =
            create_private_temp_file(&input_path, &input_path, false).map_err(|error| {
                ToolError::ExecutionFailed(format!("create private TTS command input: {error}"))
            })?;
        let _input_guard = TempPathGuard::new(input_path.clone());
        let mut input_file = tokio::fs::File::from_std(input_file);
        input_file
            .write_all(text.as_bytes())
            .await
            .map_err(|error| {
                ToolError::ExecutionFailed(format!("write TTS command input: {error}"))
            })?;
        input_file.sync_all().await.map_err(|error| {
            ToolError::ExecutionFailed(format!("sync TTS command input: {error}"))
        })?;
        drop(input_file);

        let mut placeholders = BTreeMap::new();
        placeholders.insert("input_path", input_path.display().to_string());
        placeholders.insert("text_path", input_path.display().to_string());
        placeholders.insert("output_path", generated_path.display().to_string());
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

        let argv = render_command_tts_argv_template(&provider.command, &placeholders)?;
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| invalid_command_tts_template("the command is empty"))?;
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        cmd.suppress_windows_console();

        let duration = command_tts_timeout(&provider.config);
        let command_result = run_bounded_command_with_output_limit(
            cmd,
            duration,
            true,
            BoundedOutputDirectoryLimit::new(
                generated_workspace,
                2,
                MAX_TTS_AUDIO_BYTES.saturating_add(text.len()) as u64,
            ),
        )
        .await;
        let _ = tokio::fs::remove_file(&input_path).await;
        let output = match command_result {
            Ok(output) => output,
            Err(error) => {
                let detail = match error {
                    BoundedCommandError::Start(error) => format!(
                        "TTS command provider '{}' failed to start: {error}",
                        provider.name
                    ),
                    BoundedCommandError::Stdin(error) => format!(
                        "TTS command provider '{}' failed writing stdin: {error}",
                        provider.name
                    ),
                    BoundedCommandError::Wait(error) => format!(
                        "TTS command provider '{}' failed while waiting: {error}",
                        provider.name
                    ),
                    BoundedCommandError::Stderr(error) => format!(
                        "TTS command provider '{}' failed capturing diagnostics: {error}",
                        provider.name
                    ),
                    BoundedCommandError::OutputLimit(error) => format!(
                        "TTS command provider '{}' exceeded its output limit: {error}",
                        provider.name
                    ),
                    BoundedCommandError::TimedOut => format!(
                        "TTS command provider '{}' timed out after {:.1}s",
                        provider.name,
                        duration.as_secs_f64()
                    ),
                };
                return Err(ToolError::ExecutionFailed(detail));
            }
        };

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
        let voice_compatible = command_tts_voice_compatible(&provider.config);
        if voice_compatible && format == "ogg" {
            validate_ogg_opus_file(&generated_path, &provider.name).await?;
        }
        let source_bytes =
            commit_generated_audio(&generated_path, &output_path, format, &provider.name).await?;
        let mut delivered_guard = None;
        let (delivered_path, delivered_format, delivered_bytes) =
            if voice_compatible && format != "ogg" {
                let path = match transcode_command_tts_voice(
                    &output_path,
                    &provider.config,
                    duration,
                    output_is_temporary,
                )
                .await
                {
                    Ok(path) => path,
                    Err(error) => {
                        if output_is_temporary {
                            let _ = tokio::fs::remove_file(&output_path).await;
                        }
                        return Err(error);
                    }
                };
                delivered_guard = Some(TempPathGuard::new(path.clone()));
                let bytes =
                    validate_audio_file(&path, "ogg", "voice-compatible TTS transcoder").await?;
                (path, "ogg", bytes)
            } else {
                (output_path.clone(), format, source_bytes)
            };
        let media_tag = if voice_compatible {
            format!("[[audio_as_voice]]\nMEDIA:{}", delivered_path.display())
        } else {
            format!("MEDIA:{}", delivered_path.display())
        };

        let result = json!({
            "provider": provider.name,
            "transport": "command",
            "file": delivered_path.display().to_string(),
            "file_path": delivered_path.display().to_string(),
            "format": delivered_format,
            "voice": voice.unwrap_or(""),
            "bytes": delivered_bytes,
            "voice_compatible": voice_compatible,
            "media_tag": media_tag,
        })
        .to_string();
        if delivered_path == output_path {
            if let Some(guard) = output_guard.as_mut() {
                guard.disarm();
            }
        }
        if let Some(guard) = delivered_guard.as_mut() {
            guard.disarm();
        }
        Ok(result)
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
        voice: Option<&str>,
    ) -> Result<String, ToolError> {
        let max_len = resolve_max_text_length(Some("elevenlabs"), &self.tts_config);
        let text_buf;
        let text = if text.chars().count() > max_len {
            tracing::warn!(
                provider = "elevenlabs",
                max_text_length = max_len,
                "truncating TTS input to provider limit"
            );
            text_buf = text.chars().take(max_len).collect::<String>();
            text_buf.as_str()
        } else {
            text
        };
        let voice = voice
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| resolved_elevenlabs_config(&self.tts_config).voice_id);
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
                let voice = voice
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| resolved_elevenlabs_config(&self.tts_config).voice_id);
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
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[cfg(unix)]
    struct UmaskGuard(libc::mode_t);

    #[cfg(unix)]
    impl UmaskGuard {
        fn set(mode: libc::mode_t) -> Self {
            Self(unsafe { libc::umask(mode) })
        }
    }

    #[cfg(unix)]
    impl Drop for UmaskGuard {
        fn drop(&mut self) {
            unsafe {
                libc::umask(self.0);
            }
        }
    }

    #[cfg(unix)]
    fn shell_quote_path(path: &std::path::Path) -> String {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\"'\"'"))
    }

    #[cfg(unix)]
    fn command_template_path(path: &std::path::Path) -> String {
        let value = path.to_string_lossy().into_owned();
        assert!(
            value.bytes().all(is_command_tts_literal_byte),
            "test executable path must be a valid argv-template literal: {value}"
        );
        value
    }

    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    async fn assert_malicious_command_template_has_no_side_effect(template: &str) {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("command-substitution-side-effect");
        let output = tmp.path().join("output.mp3");
        let voice = format!("$(touch {})", sentinel.display());
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "unsafe-template",
            "providers": {
                "unsafe-template": {
                    "type": "command",
                    "command": template
                }
            }
        }));

        let error = backend
            .synthesize(
                "untrusted text",
                Some(&voice),
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .expect_err("shell syntax must be rejected before command execution");
        assert!(matches!(error, ToolError::InvalidParams(_)));
        assert!(error
            .to_string()
            .contains("Unsupported command TTS argv template"));
        assert!(!sentinel.exists(), "voice text must not execute");
        assert!(!output.exists(), "rejected templates must create no output");
    }

    fn tiny_mp3_frame() -> Vec<u8> {
        let mut frame = vec![0_u8; 417];
        frame[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x64]);
        frame
    }

    fn tiny_ogg_page(payload: &[u8]) -> Vec<u8> {
        assert!(!payload.is_empty() && payload.len() <= u8::MAX as usize);
        let mut page = vec![0_u8; 28];
        page[..4].copy_from_slice(b"OggS");
        page[4] = 0;
        page[5] = 2;
        page[26] = 1;
        page[27] = payload.len() as u8;
        page.extend_from_slice(payload);
        page
    }

    fn tiny_opus_ogg_page() -> Vec<u8> {
        tiny_ogg_page(b"OpusHead\x01\x01\0\0\0\0\0\0\0\0\0")
    }

    #[test]
    fn configured_minimax_settings_wire_into_direct_tool_backend() {
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "minimax",
            "minimax": {
                "api_key": "test-key",
                "voice_id": "Configured_Custom_Voice",
                "model": "configured-model",
                "speed": 1.25,
                "vol": 1.5,
                "pitch": 2,
                "emotion": "happy"
            }
        }));
        assert!(backend.minimax_available);
        let payload = backend.minimax.build_payload("hello", None);
        assert_eq!(
            payload["voice_setting"]["voice_id"],
            "Configured_Custom_Voice"
        );
        assert_eq!(payload["voice_setting"]["speed"], 1.25);
        assert_eq!(payload["voice_setting"]["vol"], 1.5);
        assert_eq!(payload["voice_setting"]["pitch"], 2);
        assert_eq!(payload["voice_setting"]["emotion"], "happy");
    }
    #[tokio::test]
    async fn configured_elevenlabs_model_and_voice_reach_mocked_request() {
        let server = MockServer::start().await;
        let mp3 = tiny_mp3_frame();
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/configured-voice"))
            .and(body_json(json!({
                "text": "configured speech",
                "model_id": "eleven_flash_v2_5"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(mp3.clone()),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/configured-voice"))
            .and(body_json(json!({
                "text": "empty voice",
                "model_id": "eleven_flash_v2_5"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(mp3.clone()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "elevenlabs",
            "elevenlabs": {
                "base_url": format!("{}/v1", server.uri()),
                "model_id": "eleven_flash_v2_5",
                "voice_id": "configured-voice"
            }
        }));
        backend.elevenlabs_key = Some("test-key".to_string());
        let temp = tempfile::tempdir().unwrap();
        let result = backend
            .synthesize_elevenlabs("configured speech", None)
            .await
            .unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(result["voice"], "configured-voice");
        assert_eq!(result["model"], "eleven_flash_v2_5");
        let rendered = result["file"].as_str().unwrap();
        assert_eq!(tokio::fs::read(rendered).await.unwrap(), mp3);
        tokio::fs::remove_file(rendered).await.unwrap();
        let empty_voice_output = temp.path().join("empty-voice.mp3");
        let empty_voice = backend
            .synthesize(
                "empty voice",
                Some("  "),
                Some("elevenlabs"),
                Some(empty_voice_output.to_str().unwrap()),
                None,
            )
            .await
            .unwrap();
        let empty_voice: Value = serde_json::from_str(&empty_voice).unwrap();
        assert_eq!(empty_voice["voice"], "configured-voice");
        assert_eq!(tokio::fs::read(empty_voice_output).await.unwrap(), mp3);

        let mismatch = temp.path().join("configured.wav");
        let error = backend
            .synthesize(
                "wrong extension",
                None,
                Some("elevenlabs"),
                Some(mismatch.to_str().unwrap()),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("must end in .mp3"));
        assert!(!mismatch.exists());
    }

    #[tokio::test]
    async fn direct_elevenlabs_path_applies_configured_text_limit() {
        let server = MockServer::start().await;
        let mp3 = tiny_mp3_frame();
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/limited-voice"))
            .and(body_json(json!({
                "text": "abc",
                "model_id": "eleven_flash_v2_5"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(mp3),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut backend = MultiTtsBackend::with_tts_config(json!({
            "elevenlabs": {
                "base_url": format!("{}/v1", server.uri()),
                "model_id": "eleven_flash_v2_5",
                "voice_id": "limited-voice",
                "max_text_length": 3
            }
        }));
        backend.elevenlabs_key = Some("test-key".to_string());

        let result = backend.synthesize_elevenlabs("abcdef", None).await.unwrap();
        let result: Value = serde_json::from_str(&result).unwrap();
        tokio::fs::remove_file(result["file"].as_str().unwrap())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn elevenlabs_voice_is_encoded_as_one_authenticated_path_segment() {
        let server = MockServer::start().await;
        let mp3 = tiny_mp3_frame();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "audio/mpeg")
                    .set_body_bytes(mp3),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut backend = MultiTtsBackend::with_tts_config(json!({
            "elevenlabs": {
                "base_url": format!("{}/v1", server.uri())
            }
        }));
        backend.elevenlabs_key = Some("test-key".to_string());
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("voice.mp3");
        backend
            .elevenlabs_tts(
                "safe speech",
                "voice/../?query#fragment",
                Some(output.to_str().unwrap()),
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].url.path(),
            "/v1/text-to-speech/voice%2F..%2F%3Fquery%23fragment"
        );
        assert!(requests[0].url.query().is_none());
    }

    #[tokio::test]
    async fn elevenlabs_rejects_cross_origin_redirect_without_leaking_key_or_text() {
        let source = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/text-to-speech/redirect-voice"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("Location", format!("{}/capture", destination.uri()))
                    .set_body_string("redirect blocked"),
            )
            .expect(1)
            .mount(&source)
            .await;
        let output = tempfile::tempdir().unwrap();
        let output_path = output.path().join("redirect.mp3");
        let mut backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "elevenlabs",
            "elevenlabs": {"base_url": format!("{}/v1", source.uri())}
        }));
        backend.elevenlabs_key = Some("eleven-secret".to_string());

        let error = backend
            .synthesize(
                "private ElevenLabs text",
                Some("redirect-voice"),
                Some("elevenlabs"),
                Some(output_path.to_str().unwrap()),
                None,
            )
            .await
            .expect_err("redirect responses must fail closed");
        assert!(error.to_string().contains("ElevenLabs error (307"));
        assert!(error.to_string().contains("redirect blocked"));
        assert!(
            destination.received_requests().await.unwrap().is_empty(),
            "redirect destination must receive neither xi-api-key nor request body"
        );
        assert!(!output_path.exists());
    }

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
    fn command_template_renders_placeholder_values_as_literal_argv() {
        let mut placeholders = BTreeMap::new();
        placeholders.insert("input_path", "/tmp/Jane Doe/in.txt".to_string());
        placeholders.insert("output_path", "/tmp/out; rm -rf /".to_string());
        placeholders.insert("voice", "$(whoami)".to_string());

        let argv = render_command_tts_argv_template(
            "tts --voice {voice} --in {input_path} --out {output_path}",
            &placeholders,
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                "tts",
                "--voice",
                "$(whoami)",
                "--in",
                "/tmp/Jane Doe/in.txt",
                "--out",
                "/tmp/out; rm -rf /",
            ]
        );
    }

    #[test]
    fn command_template_rejects_shell_syntax_without_fallback() {
        for template in [
            "wrapper \"$(synth --voice {voice})\"",
            "wrapper --voice \\\"{voice}\\\"",
            "synth < {input_path}",
            "synth {input_path} | tee {output_path}",
            "synth `echo {voice}`",
            "synth $((1 + 1))",
        ] {
            let error = validate_command_tts_argv_template(template).unwrap_err();
            assert!(matches!(error, ToolError::InvalidParams(_)), "{template}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_rejects_command_substitution_voice_without_side_effect() {
        assert_malicious_command_template_has_no_side_effect(
            "wrapper \"$(synth --voice {voice})\"",
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_rejects_escaped_quote_voice_without_side_effect() {
        assert_malicious_command_template_has_no_side_effect("wrapper --voice \\\"{voice}\\\"")
            .await;
    }

    #[test]
    fn command_timeout_rejects_non_finite_and_clamps_huge_values() {
        let fallback = Duration::from_secs(DEFAULT_COMMAND_TTS_TIMEOUT_SECONDS);
        assert_eq!(command_tts_timeout(&json!({"timeout": "inf"})), fallback);
        assert_eq!(command_tts_timeout(&json!({"timeout": "NaN"})), fallback);
        assert_eq!(command_tts_timeout(&json!({"timeout": 0})), fallback);
        assert_eq!(
            command_tts_timeout(&json!({"timeout": "1e308"})),
            Duration::from_secs_f64(MAX_COMMAND_TTS_TIMEOUT_SECONDS)
        );
        assert_eq!(
            command_tts_timeout(&json!({"timeout_seconds": 1.5})),
            Duration::from_secs_f64(1.5)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_writes_output_and_reports_voice_compatibility() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("clip.ogg");
        let fixture = tmp.path().join("fixture.ogg");
        let ogg = tiny_opus_ogg_page();
        std::fs::write(&fixture, &ogg).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {model} {output_path}",
                    "model": fixture,
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
        assert_eq!(
            data["media_tag"],
            format!("[[audio_as_voice]]\nMEDIA:{}", output.display())
        );
        assert_eq!(std::fs::read(&output).unwrap(), ogg);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_rejects_non_opus_ogg_before_voice_delivery() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("non-opus.ogg");
        let output = tmp.path().join("clip.ogg");
        std::fs::write(&fixture, tiny_ogg_page(b"not-opus")).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {model} {output_path}",
                    "model": fixture,
                    "output_format": "ogg",
                    "voice_compatible": true
                }
            }
        }));

        let error = backend
            .synthesize(
                "hello command",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without an Opus stream"));
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_transcodes_voice_output_to_ogg_and_cleans_owned_source() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let ffmpeg = tmp.path().join("fake-ffmpeg");
        let args_log = tmp.path().join("ffmpeg-args");
        let source_log = tmp.path().join("ffmpeg-source");
        let source_audio = tmp.path().join("source.mp3");
        std::fs::write(&source_audio, tiny_mp3_frame()).unwrap();
        let ogg_fixture = tmp.path().join("fixture.ogg");
        let ogg = tiny_opus_ogg_page();
        std::fs::write(&ogg_fixture, &ogg).unwrap();
        std::fs::write(
            &ffmpeg,
            format!(
                "#!/bin/sh\ninput=''\nprevious=''\nlast=''\nprintf '%s\\n' \"$@\" > \"{}\"\nfor arg in \"$@\"; do\n  if [ \"$previous\" = '-i' ]; then input=\"$arg\"; fi\n  previous=\"$arg\"\n  last=\"$arg\"\ndone\nprintf '%s' \"$input\" > \"{}\"\ncp \"{}\" \"$last\"\n",
                args_log.display(),
                source_log.display(),
                ogg_fixture.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {model} {output_path}",
                    "model": source_audio,
                    "voice_compatible": true,
                    "ffmpeg_path": ffmpeg
                }
            }
        }));

        let out = backend
            .synthesize("hello converted", None, None, None, None)
            .await
            .unwrap();
        let data: Value = serde_json::from_str(&out).unwrap();
        let delivered = PathBuf::from(data["file"].as_str().unwrap());
        let source = PathBuf::from(std::fs::read_to_string(&source_log).unwrap());
        let args = std::fs::read_to_string(args_log).unwrap();

        assert_eq!(data["format"], "ogg");
        assert_eq!(
            delivered.extension().and_then(|value| value.to_str()),
            Some("ogg")
        );
        assert_eq!(
            data["media_tag"],
            format!("[[audio_as_voice]]\nMEDIA:{}", delivered.display())
        );
        assert!(args.lines().any(|arg| arg == "libopus"));
        assert!(args.lines().any(|arg| arg == "ogg"));
        assert!(!source.exists(), "agent-owned source must be removed");
        assert_eq!(std::fs::read(&delivered).unwrap(), ogg);
        std::fs::remove_file(delivered).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_keeps_text_and_transcoded_ogg_private_under_umask_022() {
        use std::os::unix::fs::PermissionsExt;

        let _global_test_lock = hermes_config::managed_gateway::test_lock::lock();
        let _umask = UmaskGuard::set(0o022);
        let tmp = tempfile::tempdir().unwrap();
        let command_probe = tmp.path().join("command-probe");
        let ffmpeg = tmp.path().join("fake-ffmpeg");
        let source_fixture = tmp.path().join("fixture.mp3");
        let ogg_fixture = tmp.path().join("fixture.ogg");
        let explicit_source = tmp.path().join("source.mp3");
        let mp3 = tiny_mp3_frame();
        let ogg = tiny_opus_ogg_page();
        std::fs::write(&source_fixture, &mp3).unwrap();
        std::fs::write(&ogg_fixture, &ogg).unwrap();

        std::fs::write(
            &command_probe,
            r#"#!/bin/sh
set -eu
mode() {
    case "$(uname -s)" in
        Darwin) stat -f '%Lp' "$1" ;;
        *) stat -c '%a' "$1" ;;
    esac
}
directory=$(dirname "$2")
log_directory=$(dirname "$directory")
printf '%s\n' "$1" > "$log_directory/input-path"
mode "$1" > "$log_directory/input-mode-before"
cat "$1" > "$log_directory/input-text"
printf '%s\n' "$2" > "$log_directory/command-output-path"
mode "$2" > "$log_directory/command-output-mode-before"
cp "$3" "$2"
mode "$2" > "$log_directory/command-output-mode-after"
"#,
        )
        .unwrap();
        std::fs::set_permissions(&command_probe, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::fs::write(
            &ffmpeg,
            format!(
                r#"#!/bin/sh
set -eu
mode() {{
    case "$(uname -s)" in
        Darwin) stat -f '%Lp' "$1" ;;
        *) stat -c '%a' "$1" ;;
    esac
}}
previous=''
last=''
for arg in "$@"; do
    if [ "$previous" = '-i' ]; then input="$arg"; fi
    previous="$arg"
    last="$arg"
done
directory=$(dirname "$last")
log_directory=$(dirname "$directory")
printf '%s\n' "$last" > "$log_directory/transcode-output-path"
mode "$last" > "$log_directory/transcode-output-mode-before"
cp {} "$last"
mode "$last" > "$log_directory/transcode-output-mode-after"
"#,
                shell_quote_path(&ogg_fixture)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();

        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "probe-tts",
            "providers": {
                "probe-tts": {
                    "type": "command",
                    "command": format!(
                        "{} {{input_path}} {{output_path}} {{model}}",
                        command_template_path(&command_probe)
                    ),
                    "model": source_fixture,
                    "voice_compatible": true,
                    "ffmpeg_path": ffmpeg
                }
            }
        }));

        let out = backend
            .synthesize(
                "sensitive command TTS input",
                None,
                None,
                Some(explicit_source.to_str().unwrap()),
                None,
            )
            .await
            .unwrap();
        let data: Value = serde_json::from_str(&out).unwrap();
        let delivered = PathBuf::from(data["file"].as_str().unwrap());

        for name in [
            "input-mode-before",
            "command-output-mode-before",
            "command-output-mode-after",
            "transcode-output-mode-before",
            "transcode-output-mode-after",
        ] {
            assert_eq!(
                std::fs::read_to_string(tmp.path().join(name))
                    .unwrap()
                    .trim(),
                "600",
                "{name} must observe an owner-only artifact"
            );
        }
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("input-text")).unwrap(),
            "sensitive command TTS input"
        );
        assert_eq!(unix_mode(&explicit_source), 0o600);
        assert_eq!(unix_mode(&delivered), 0o600);
        assert_eq!(std::fs::read(&delivered).unwrap(), ogg);

        for (name, description) in [
            ("input-path", "command input"),
            ("command-output-path", "command temporary output"),
            ("transcode-output-path", "transcoder temporary output"),
        ] {
            let path = PathBuf::from(
                std::fs::read_to_string(tmp.path().join(name))
                    .unwrap()
                    .trim(),
            );
            assert!(!path.exists(), "{description} must be cleaned");
        }
        std::fs::remove_file(delivered).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_preserves_explicit_source_when_voice_transcoding_fails() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let ffmpeg = tmp.path().join("failing-ffmpeg");
        let output = tmp.path().join("explicit.mp3");
        let source_audio = tmp.path().join("source.mp3");
        let mp3 = tiny_mp3_frame();
        std::fs::write(&source_audio, &mp3).unwrap();
        std::fs::write(&ffmpeg, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "copy-tts",
            "providers": {
                "copy-tts": {
                    "type": "command",
                    "command": "cp {model} {output_path}",
                    "model": source_audio,
                    "voice_compatible": true,
                    "ffmpeg_path": ffmpeg
                }
            }
        }));

        let error = backend
            .synthesize(
                "preserve me",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .expect_err("failed transcoding must be reported");

        assert!(error.to_string().contains("transcoding failed"));
        assert_eq!(std::fs::read(output).unwrap(), mp3);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_rejects_non_audio_without_replacing_destination() {
        let tmp = tempfile::tempdir().unwrap();
        let output = tmp.path().join("explicit.mp3");
        std::fs::write(&output, b"existing audio destination").unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "invalid-tts",
            "providers": {
                "invalid-tts": {
                    "type": "command",
                    "command": "cp {input_path} {output_path}",
                    "output_format": "mp3"
                }
            }
        }));

        let error = backend
            .synthesize(
                "not audio",
                None,
                None,
                Some(output.to_str().unwrap()),
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("valid mp3 audio container"));
        assert_eq!(
            std::fs::read(&output).unwrap(),
            b"existing audio destination"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_timeout_kills_descendant_processes() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("late-write");
        let script = tmp.path().join("slow-tts");
        std::fs::write(
            &script,
            "#!/bin/sh\n(sleep 0.4; printf leaked > \"$1\") & wait\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "slow-tts",
            "providers": {
                "slow-tts": {
                    "type": "command",
                    "command": format!(
                        "{} {}",
                        command_template_path(&script),
                        command_template_path(&sentinel)
                    ),
                    "timeout_seconds": 0.05
                }
            }
        }));

        let error = backend
            .synthesize("timeout", None, None, None, None)
            .await
            .expect_err("slow command must time out");
        assert!(error.to_string().contains("timed out"));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            !sentinel.exists(),
            "timed-out provider descendant must not survive"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_provider_bounds_stderr_while_retaining_diagnostic_tail() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("noisy-tts");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf EARLY_SENTINEL >&2\ndd if=/dev/zero bs=70000 count=1 1>&2 2>/dev/null\nprintf FINAL_SENTINEL >&2\nexit 7\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let backend = MultiTtsBackend::with_tts_config(json!({
            "provider": "noisy-tts",
            "providers": {
                "noisy-tts": {
                    "type": "command",
                    "command": command_template_path(&script)
                }
            }
        }));

        let error = backend
            .synthesize("noise", None, None, None, None)
            .await
            .expect_err("nonzero noisy provider must fail");
        let message = error.to_string();
        assert!(message.contains("[...stderr truncated; showing tail...]"));
        assert!(message.contains("FINAL_SENTINEL"));
        assert!(!message.contains("EARLY_SENTINEL"));
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
