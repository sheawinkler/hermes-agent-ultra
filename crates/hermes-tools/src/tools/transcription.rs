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

use std::path::{Path, PathBuf};
use std::time::Duration;

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

use hermes_config::ValidatedExternalMediaUpload;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, resolve_openai_audio_api_key, ManagedToolGatewayConfig,
    ResolveOptions,
};
use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::tts_audio::{read_bounded_error, read_bounded_response};

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

const MAX_TRANSCRIPTION_AUDIO_BYTES: u64 = 96 * 1024 * 1024;
const OPENAI_GROQ_TRANSCRIPTION_MAX_AUDIO_BYTES: u64 = 25 * 1024 * 1024;
const MAX_TRANSCRIPTION_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const TRANSCRIPTION_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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

fn audio_extension_format(path: &str) -> Option<&'static str> {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
        .and_then(|e| match e {
            "wav" => Some("wav"),
            "mp3" => Some("mp3"),
            "m4a" => Some("mp4"),
            "webm" => Some("webm"),
            "ogg" | "oga" => Some("ogg"),
            "flac" => Some("flac"),
            _ => None,
        })
}

#[cfg(unix)]
fn open_validated_upload(upload: &ValidatedExternalMediaUpload) -> std::io::Result<std::fs::File> {
    use std::path::Component;

    fn component_c_string(component: &std::ffi::OsStr) -> std::io::Result<CString> {
        CString::new(component.as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))
    }

    let relative = upload
        .canonical_path
        .strip_prefix(&upload.canonical_root)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::PermissionDenied))?;
    let filesystem_root = CString::new("/").expect("static filesystem root has no NUL");
    let root_fd = unsafe {
        libc::open(
            filesystem_root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut directory = unsafe { std::fs::File::from_raw_fd(root_fd) };
    for component in upload.canonical_root.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => name,
            _ => return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        };
        let name = component_c_string(name)?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        directory = unsafe { std::fs::File::from_raw_fd(fd) };
    }
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        };
        let name = component_c_string(name)?;
        let is_file = components.peek().is_none();
        let flags = if is_file {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
        };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let opened = unsafe { std::fs::File::from_raw_fd(fd) };
        if is_file {
            if !opened.metadata()?.is_file() {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
            }
            return Ok(opened);
        }
        directory = opened;
    }
    Err(std::io::Error::from(std::io::ErrorKind::InvalidInput))
}

#[cfg(windows)]
fn open_validated_upload(upload: &ValidatedExternalMediaUpload) -> std::io::Result<std::fs::File> {
    use std::mem::size_of;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, GetFinalPathNameByHandleW,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    fn comparable_windows_path(path: &Path) -> String {
        let value = path.to_string_lossy();
        let value = value
            .strip_prefix(r"\\?\UNC\")
            .map(|rest| format!(r"\\{rest}"))
            .or_else(|| value.strip_prefix(r"\\?\").map(str::to_owned))
            .unwrap_or_else(|| value.into_owned());
        value.to_lowercase()
    }

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&upload.canonical_path)?;
    let handle = file.as_raw_handle();
    let mut attributes = FILE_ATTRIBUTE_TAG_INFO {
        FileAttributes: 0,
        ReparseTag: 0,
    };
    let info_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            (&mut attributes as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if info_ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }

    let required = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, 0) };
    if required == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut final_path = vec![0_u16; required as usize];
    let written =
        unsafe { GetFinalPathNameByHandleW(handle, final_path.as_mut_ptr(), required, 0) };
    if written == 0 || written >= required {
        return Err(std::io::Error::last_os_error());
    }
    let final_path = PathBuf::from(String::from_utf16_lossy(&final_path[..written as usize]));
    if comparable_windows_path(&final_path) != comparable_windows_path(&upload.canonical_path)
        || !file.metadata()?.is_file()
    {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    Ok(file)
}

#[cfg(all(not(unix), not(windows)))]
fn open_validated_upload(_upload: &ValidatedExternalMediaUpload) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "secure external media upload is unavailable on this platform",
    ))
}

fn transcription_audio_byte_limit(provider: &str) -> u64 {
    let provider_limit = match provider {
        "openai" | "groq" => OPENAI_GROQ_TRANSCRIPTION_MAX_AUDIO_BYTES,
        _ => MAX_TRANSCRIPTION_AUDIO_BYTES,
    };
    MAX_TRANSCRIPTION_AUDIO_BYTES.min(provider_limit)
}

fn transcription_audio_limit_error(provider: &str, max_bytes: u64) -> ToolError {
    ToolError::InvalidParams(format!(
        "Transcription audio exceeds the {max_bytes} byte limit for {provider}"
    ))
}

async fn read_transcription_audio(
    upload: ValidatedExternalMediaUpload,
    provider: &str,
) -> Result<Vec<u8>, ToolError> {
    let max_bytes = transcription_audio_byte_limit(provider);
    let file = tokio::task::spawn_blocking(move || {
        let file = open_validated_upload(&upload)?;
        hermes_config::validate_media_file_handle(&file)?;
        Ok::<_, std::io::Error>(file)
    })
    .await
    .map_err(|error| ToolError::ExecutionFailed(format!("Audio file open task failed: {error}")))?
    .map_err(|error| {
        ToolError::ExecutionFailed(format!(
            "Cannot open approved audio file without following symlinks: {error}"
        ))
    })?;
    let mut file = tokio::fs::File::from_std(file);
    let metadata = file.metadata().await.map_err(|error| {
        ToolError::ExecutionFailed(format!("Cannot inspect audio file: {error}"))
    })?;
    if metadata.len() == 0 {
        return Err(ToolError::ExecutionFailed(
            "Transcription audio file is empty".into(),
        ));
    }
    if metadata.len() > max_bytes {
        return Err(transcription_audio_limit_error(provider, max_bytes));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| ToolError::ExecutionFailed(format!("Cannot read audio file: {error}")))?;
    if bytes.len() as u64 > max_bytes {
        return Err(transcription_audio_limit_error(provider, max_bytes));
    }
    Ok(bytes)
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

fn transcription_http_client(timeout: Duration) -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|error| ToolError::ExecutionFailed(format!("Transcription HTTP client: {error}")))
}

impl TranscriptionHandler {
    pub(crate) async fn execute_with_trusted_roots(
        &self,
        params: Value,
        extra_trusted_roots: &[PathBuf],
    ) -> Result<String, ToolError> {
        let path = params
            .get("audio_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'audio_path'".into()));
        }
        let validated_upload =
            hermes_config::validate_external_media_upload_path(path, extra_trusted_roots)
                .ok_or_else(|| {
                    ToolError::InvalidParams(
                        "Refusing to upload audio outside configured media roots".into(),
                    )
                })?;
        let canonical_path = validated_upload.canonical_path.to_str().ok_or_else(|| {
            ToolError::InvalidParams("Transcription audio path is not valid UTF-8".into())
        })?;
        let fmt = audio_extension_format(canonical_path).ok_or_else(|| {
            ToolError::InvalidParams(
                "Unsupported transcription audio extension; expected wav, mp3, m4a, webm, ogg, oga, or flac"
                    .into(),
            )
        })?;

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

        let bytes = read_transcription_audio(validated_upload, request.provider).await?;
        let client = transcription_http_client(TRANSCRIPTION_REQUEST_TIMEOUT)?;
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
            let body = read_bounded_error(resp).await;
            return Err(ToolError::ExecutionFailed(format!(
                "Transcription error {status}: {body}"
            )));
        }

        let body = read_bounded_response(
            resp,
            MAX_TRANSCRIPTION_RESPONSE_BYTES,
            "Transcription response",
        )
        .await?;
        let body = String::from_utf8(body).map_err(|error| {
            ToolError::ExecutionFailed(format!("Transcription response is not UTF-8: {error}"))
        })?;
        let text = parse_transcription_response(&body, &request.response_format)?;
        if text.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "Transcription provider returned an empty transcript".into(),
            ));
        }

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
}

#[async_trait]
impl ToolHandler for TranscriptionHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.execute_with_trusted_roots(params, &[]).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "audio_path".into(),
            json!({"type":"string","description":"Absolute audio path under a configured media cache/allow root; external uploads outside those roots are rejected"}),
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
            "Transcribe an approved local audio file via OpenAI, Groq Whisper, or Mistral Voxtral. \
             The path must be under a configured media root (including HERMES_MEDIA_ALLOW_DIRS). \
             Honors VOICE_TOOLS_OPENAI_KEY / HERMES_OPENAI_API_KEY (or OPENAI_API_KEY), \
             GROQ_API_KEY, MISTRAL_API_KEY, provider/model overrides, and Nous-managed OpenAI audio \
             when HERMES_ENABLE_NOUS_MANAGED_TOOLS is enabled.",
            JsonSchema::object(props, vec!["audio_path".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
                "HERMES_MEDIA_ALLOW_DIRS",
                "HERMES_MEDIA_CACHE_DIR",
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
        assert_eq!(audio_extension_format("a.wav"), Some("wav"));
        assert_eq!(audio_extension_format("a.mp3"), Some("mp3"));
        assert_eq!(audio_extension_format("a.m4a"), Some("mp4"));
        assert_eq!(audio_extension_format("a.webm"), Some("webm"));
        assert_eq!(audio_extension_format("a.ogg"), Some("ogg"));
        assert_eq!(audio_extension_format("a.oga"), Some("ogg"));
        assert_eq!(audio_extension_format("a.flac"), Some("flac"));
    }

    #[test]
    fn audio_extension_unknown_fails_closed() {
        assert_eq!(audio_extension_format("a.xyz"), None);
        assert_eq!(audio_extension_format("noext"), None);
        assert_eq!(audio_extension_format(""), None);
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
    async fn execute_errors_when_authorized_path_has_no_credentials_or_gateway() {
        let scope = EnvScope::new();
        let audio_root = scope._tmp.path().join("audio_cache");
        std::fs::create_dir_all(&audio_root).unwrap();
        let audio = audio_root.join("input.wav");
        std::fs::write(&audio, b"authorized audio fixture").unwrap();
        let h = TranscriptionHandler;
        let err = h.execute(json!({"audio_path": audio})).await.unwrap_err();
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

    #[tokio::test]
    async fn execute_rejects_arbitrary_readable_path_before_network_or_credentials() {
        let _g = EnvScope::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("STT_OPENAI_BASE_URL", server.uri());
        let arbitrary = tempfile::tempdir().unwrap();
        let audio = arbitrary.path().join("renamed-secret.wav");
        std::fs::write(&audio, b"not approved for upload").unwrap();

        let error = TranscriptionHandler
            .execute(json!({"audio_path": audio, "provider": "openai"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("configured media roots"));
    }

    #[test]
    fn transcription_audio_byte_limit_caps_openai_and_groq() {
        assert_eq!(
            transcription_audio_byte_limit("openai"),
            OPENAI_GROQ_TRANSCRIPTION_MAX_AUDIO_BYTES
        );
        assert_eq!(
            transcription_audio_byte_limit("groq"),
            OPENAI_GROQ_TRANSCRIPTION_MAX_AUDIO_BYTES
        );
    }

    #[tokio::test]
    async fn approved_upload_handle_reads_single_link_audio() {
        let scope = EnvScope::new();
        let approved = scope._tmp.path().join("approved");
        std::fs::create_dir_all(&approved).unwrap();
        let audio = approved.join("sample.wav");
        std::fs::write(&audio, b"approved bytes").unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", &approved);
        let upload =
            hermes_config::validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .unwrap();

        assert_eq!(
            read_transcription_audio(upload, "openai").await.unwrap(),
            b"approved bytes".to_vec()
        );
    }
    #[tokio::test]
    async fn approved_upload_handle_accepts_exact_openai_provider_limit() {
        let scope = EnvScope::new();
        let approved = scope._tmp.path().join("approved");
        std::fs::create_dir_all(&approved).unwrap();
        let audio = approved.join("sample.wav");
        let limit = transcription_audio_byte_limit("openai");
        std::fs::File::create(&audio)
            .unwrap()
            .set_len(limit)
            .unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", &approved);
        let upload =
            hermes_config::validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .unwrap();

        assert_eq!(
            read_transcription_audio(upload, "openai")
                .await
                .unwrap()
                .len() as u64,
            limit
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn execute_rejects_hard_linked_audio_before_network_send() {
        let scope = EnvScope::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("STT_OPENAI_BASE_URL", server.uri());
        let approved = scope._tmp.path().join("approved");
        std::fs::create_dir_all(&approved).unwrap();
        let credential = scope._tmp.path().join("credential.wav");
        std::fs::write(&credential, b"same-user credential bytes").unwrap();
        let audio = approved.join("audio.wav");
        std::fs::hard_link(&credential, &audio).unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", &approved);

        let error = TranscriptionHandler
            .execute(json!({"audio_path": audio, "provider": "openai"}))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::ExecutionFailed(_)), "{error}");
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn approved_upload_handle_rejects_ancestor_swapped_to_symlink() {
        use std::os::unix::fs::symlink;

        let scope = EnvScope::new();
        let container = scope._tmp.path().join("container");
        let approved = container.join("approved");
        let displaced_container = scope._tmp.path().join("container-original");
        let outside_container = scope._tmp.path().join("outside-container");
        let outside_approved = outside_container.join("approved");
        std::fs::create_dir_all(&approved).unwrap();
        std::fs::create_dir_all(&outside_approved).unwrap();
        let audio = approved.join("sample.wav");
        std::fs::write(&audio, b"approved bytes").unwrap();
        std::fs::write(outside_approved.join("sample.wav"), b"outside bytes").unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", &approved);
        let upload =
            hermes_config::validate_external_media_upload_path(&audio.to_string_lossy(), &[])
                .unwrap();

        std::fs::rename(&container, &displaced_container).unwrap();
        symlink(&outside_container, &container).unwrap();

        let error = read_transcription_audio(upload, "openai")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("without following symlinks"));
    }

    #[tokio::test]
    async fn execute_posts_bounded_audio_and_returns_nonempty_transcript() {
        let _g = EnvScope::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_string("hello from mock"))
            .expect(1)
            .mount(&server)
            .await;
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("STT_OPENAI_BASE_URL", server.uri());
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", temp.path());
        let audio = temp.path().join("sample.wav");
        std::fs::write(&audio, b"bounded audio fixture").unwrap();

        let response = TranscriptionHandler
            .execute(json!({"audio_path": audio, "provider": "openai"}))
            .await
            .unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["status"], "transcribed");
        assert_eq!(response["text"], "hello from mock");
        assert_eq!(response["provider"], "openai");
    }

    #[tokio::test]
    async fn execute_rejects_audio_over_openai_provider_limit_before_network_send() {
        let _g = EnvScope::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("STT_OPENAI_BASE_URL", server.uri());
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", temp.path());
        let audio = temp.path().join("oversized.wav");
        let file = std::fs::File::create(&audio).unwrap();
        file.set_len(transcription_audio_byte_limit("openai") + 1)
            .unwrap();

        let error = TranscriptionHandler
            .execute(json!({"audio_path": audio, "provider": "openai"}))
            .await
            .unwrap_err();
        match error {
            ToolError::InvalidParams(message) => assert_eq!(
                message,
                format!(
                    "Transcription audio exceeds the {} byte limit for openai",
                    transcription_audio_byte_limit("openai")
                )
            ),
            _ => panic!("expected provider limit preflight rejection"),
        }
    }

    #[tokio::test]
    async fn execute_rejects_oversized_provider_response() {
        let _g = EnvScope::new();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![
                b'x';
                MAX_TRANSCRIPTION_RESPONSE_BYTES
                    + 1
            ]))
            .expect(1)
            .mount(&server)
            .await;
        std::env::set_var("OPENAI_API_KEY", "test-key");
        std::env::set_var("STT_OPENAI_BASE_URL", server.uri());
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", temp.path());
        let audio = temp.path().join("sample.wav");
        std::fs::write(&audio, b"bounded audio fixture").unwrap();

        let error = TranscriptionHandler
            .execute(json!({"audio_path": audio, "provider": "openai"}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("byte limit"));
    }

    #[tokio::test]
    async fn execute_refuses_redirects_without_replaying_transcription_upload() {
        let _g = EnvScope::new();
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_MEDIA_ALLOW_DIRS", temp.path());
        std::env::set_var("OPENAI_API_KEY", "test-key");
        let audio = temp.path().join("sample.wav");
        std::fs::write(&audio, b"redirect audio fixture").unwrap();

        for redirect_status in [307, 308] {
            let redirect_target = MockServer::start().await;
            let redirect_source = MockServer::start().await;
            let redirect_uri =
                redirect_target
                    .uri()
                    .replacen("://", "://redirect-user:redirect-password@", 1);
            Mock::given(method("POST"))
                .and(path("/audio/transcriptions"))
                .and(header("authorization", "Bearer test-key"))
                .respond_with(
                    ResponseTemplate::new(redirect_status)
                        .insert_header("location", format!("{redirect_uri}/redirected")),
                )
                .expect(1)
                .mount(&redirect_source)
                .await;
            std::env::set_var("STT_OPENAI_BASE_URL", redirect_source.uri());

            let error = TranscriptionHandler
                .execute(json!({"audio_path": audio, "provider": "openai"}))
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains(&format!("Transcription error {redirect_status}")),
                "{error}"
            );

            let received = redirect_target.received_requests().await.unwrap();
            assert!(
                received.is_empty(),
                "redirect target received a request that could expose auth, URL credentials, multipart headers, filename, or audio bytes: {received:?}"
            );
        }
    }

    #[tokio::test]
    async fn transcription_client_enforces_request_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&server)
            .await;

        let client = transcription_http_client(Duration::from_millis(10)).unwrap();
        let error = client
            .get(format!("{}/slow", server.uri()))
            .send()
            .await
            .unwrap_err();
        assert!(error.is_timeout(), "{error}");
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
