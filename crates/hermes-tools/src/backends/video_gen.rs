//! Rust-native FAL video generation backend.
//!
//! This ports the FAL video plugin surface into the built-in Rust tool
//! runtime. Direct mode uses FAL's queue HTTP API, matching
//! `fal_client.subscribe`; managed mode routes through the existing Nous
//! `fal-queue` gateway resolver.

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

use crate::tools::video::{VideoGenerateBackend, VideoGenerateRequest};
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

const DEFAULT_FAL_VIDEO_MODEL: &str = "pixverse-v6";
const DEFAULT_TIMEOUT_SECONDS: u64 = 600;
const DEFAULT_POLL_INTERVAL_SECONDS: u64 = 2;

const DEFAULT_XAI_BASE_URL: &str = "https://api.x.ai/v1";
const DEFAULT_XAI_TEXT_TO_VIDEO_MODEL: &str = "grok-imagine-video";
const DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL: &str = "grok-imagine-video-1.5-preview";
const XAI_IMAGE_TO_VIDEO_MODEL_ALIAS: &str = "grok-imagine-video-1.5-2026-05-30";
const DEFAULT_XAI_DURATION: u32 = 8;
const DEFAULT_XAI_ASPECT_RATIO: &str = "16:9";
const DEFAULT_XAI_RESOLUTION: &str = "720p";
const DEFAULT_XAI_TIMEOUT_SECONDS: u64 = 240;
const DEFAULT_XAI_POLL_INTERVAL_SECONDS: u64 = 5;
const XAI_MAX_REFERENCE_IMAGES: usize = 7;
const XAI_VALID_ASPECT_RATIOS: &[&str] = &["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"];
const XAI_VALID_RESOLUTIONS: &[&str] = &["480p", "720p"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DurationSpec {
    Range(u32, u32),
    Enum(&'static [u32]),
}

#[derive(Debug, Clone, Copy)]
struct FalVideoFamily {
    id: &'static str,
    text_endpoint: &'static str,
    image_endpoint: &'static str,
    image_param_key: &'static str,
    aspect_ratios: &'static [&'static str],
    resolutions: &'static [&'static str],
    durations: Option<DurationSpec>,
    audio: bool,
    negative: bool,
}

const VEO_DURATIONS: &[u32] = &[4, 6, 8];

const FAL_VIDEO_FAMILIES: &[FalVideoFamily] = &[
    FalVideoFamily {
        id: "ltx-2.3",
        text_endpoint: "fal-ai/ltx-2.3-22b/text-to-video",
        image_endpoint: "fal-ai/ltx-2.3-22b/image-to-video",
        image_param_key: "image_url",
        aspect_ratios: &[],
        resolutions: &[],
        durations: None,
        audio: true,
        negative: true,
    },
    FalVideoFamily {
        id: "pixverse-v6",
        text_endpoint: "fal-ai/pixverse/v6/text-to-video",
        image_endpoint: "fal-ai/pixverse/v6/image-to-video",
        image_param_key: "image_url",
        aspect_ratios: &[],
        resolutions: &["360p", "540p", "720p", "1080p"],
        durations: Some(DurationSpec::Range(1, 15)),
        audio: true,
        negative: true,
    },
    FalVideoFamily {
        id: "veo3.1",
        text_endpoint: "fal-ai/veo3.1",
        image_endpoint: "fal-ai/veo3.1/image-to-video",
        image_param_key: "image_url",
        aspect_ratios: &["16:9", "9:16"],
        resolutions: &["720p", "1080p"],
        durations: Some(DurationSpec::Enum(VEO_DURATIONS)),
        audio: true,
        negative: true,
    },
    FalVideoFamily {
        id: "seedance-2.0",
        text_endpoint: "bytedance/seedance-2.0/text-to-video",
        image_endpoint: "bytedance/seedance-2.0/image-to-video",
        image_param_key: "image_url",
        aspect_ratios: &["21:9", "16:9", "4:3", "1:1", "3:4", "9:16"],
        resolutions: &["480p", "720p", "1080p"],
        durations: Some(DurationSpec::Range(4, 15)),
        audio: true,
        negative: false,
    },
    FalVideoFamily {
        id: "kling-v3-4k",
        text_endpoint: "fal-ai/kling-video/v3/4k/text-to-video",
        image_endpoint: "fal-ai/kling-video/v3/4k/image-to-video",
        image_param_key: "start_image_url",
        aspect_ratios: &["16:9", "9:16", "1:1"],
        resolutions: &[],
        durations: Some(DurationSpec::Range(3, 15)),
        audio: true,
        negative: true,
    },
    FalVideoFamily {
        id: "happy-horse",
        text_endpoint: "fal-ai/happy-horse/text-to-video",
        image_endpoint: "fal-ai/happy-horse/image-to-video",
        image_param_key: "image_url",
        aspect_ratios: &[],
        resolutions: &[],
        durations: None,
        audio: false,
        negative: false,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum FalVideoTransport {
    Direct {
        api_key: String,
    },
    Managed {
        gateway_origin: String,
        nous_token: String,
    },
    Unconfigured,
}

impl FalVideoTransport {
    fn label(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Managed { .. } => "managed",
            Self::Unconfigured => "unconfigured",
        }
    }

    fn submit_url(&self, endpoint: &str) -> Result<String, ToolError> {
        match self {
            Self::Direct { .. } => Ok(format!("https://queue.fal.run/{endpoint}")),
            Self::Managed { gateway_origin, .. } => {
                let root = gateway_origin.trim_end_matches('/');
                Ok(format!("{root}/run/{endpoint}"))
            }
            Self::Unconfigured => Err(ToolError::ExecutionFailed(
                "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.".into(),
            )),
        }
    }

    fn auth_header(&self) -> Result<(String, String), ToolError> {
        match self {
            Self::Direct { api_key } => Ok(("Authorization".into(), format!("Key {api_key}"))),
            Self::Managed { nous_token, .. } => {
                Ok(("Authorization".into(), format!("Bearer {nous_token}")))
            }
            Self::Unconfigured => Err(ToolError::ExecutionFailed(
                "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.".into(),
            )),
        }
    }
}

/// FAL video generation backend using direct FAL queue API or the
/// Nous-managed fal-queue gateway.
#[derive(Debug)]
pub struct FalVideoGenBackend {
    client: Client,
    transport: FalVideoTransport,
}

impl FalVideoGenBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            transport: FalVideoTransport::Direct { api_key },
        }
    }

    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            client: Client::new(),
            transport: FalVideoTransport::Managed {
                gateway_origin: cfg.gateway_origin.clone(),
                nous_token: cfg.nous_user_token.clone(),
            },
        }
    }

    pub fn unconfigured() -> Self {
        Self {
            client: Client::new(),
            transport: FalVideoTransport::Unconfigured,
        }
    }

    /// Priority: direct `FAL_KEY` -> Nous-managed `fal-queue` -> error.
    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        if let Ok(key) = std::env::var("FAL_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed.to_string()));
            }
        }
        if let Some(cfg) = resolve_managed_tool_gateway("fal-queue", ResolveOptions::default()) {
            return Ok(Self::from_managed(&cfg));
        }
        Err(ToolError::ExecutionFailed(
            "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.".into(),
        ))
    }

    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }

    async fn submit_managed(&self, endpoint: &str, payload: &Value) -> Result<Value, ToolError> {
        let url = self.transport.submit_url(endpoint)?;
        let (auth_name, auth_value) = self.transport.auth_header()?;
        let resp = self
            .client
            .post(url)
            .header(auth_name, auth_value)
            .header("Content-Type", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("FAL video request failed: {e}")))?;
        read_json_response(resp, "FAL video generation").await
    }

    async fn submit_direct_queue(
        &self,
        endpoint: &str,
        payload: &Value,
    ) -> Result<Value, ToolError> {
        let url = self.transport.submit_url(endpoint)?;
        let (auth_name, auth_value) = self.transport.auth_header()?;
        let submit = self
            .client
            .post(url)
            .header(auth_name, auth_value.clone())
            .header("Content-Type", "application/json")
            .json(payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("FAL queue submit failed: {e}")))?;
        let submitted = read_json_response(submit, "FAL queue submit").await?;
        if extract_video(&submitted).is_some() {
            return Ok(submitted);
        }

        let status_url = submitted
            .get("status_url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let response_url = submitted
            .get("response_url")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let request_id = submitted
            .get("request_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        let timeout = env_u64("FAL_VIDEO_TIMEOUT_SECONDS").unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        let poll_interval =
            env_u64("FAL_VIDEO_POLL_INTERVAL_SECONDS").unwrap_or(DEFAULT_POLL_INTERVAL_SECONDS);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);

        if let Some(status_url) = status_url.as_deref() {
            loop {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Timed out waiting for FAL video generation after {timeout}s"
                    )));
                }
                let resp = self
                    .client
                    .get(status_url)
                    .header("Authorization", auth_value.clone())
                    .send()
                    .await
                    .map_err(|e| {
                        ToolError::ExecutionFailed(format!("FAL queue status failed: {e}"))
                    })?;
                let status = read_json_response(resp, "FAL queue status").await?;
                match status
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "COMPLETED" | "OK" => break,
                    "FAILED" | "ERROR" => {
                        return Err(ToolError::ExecutionFailed(format!(
                            "FAL video generation failed: {status}"
                        )));
                    }
                    _ => tokio::time::sleep(Duration::from_secs(poll_interval.max(1))).await,
                }
            }
        }

        let response_url = response_url
            .or_else(|| {
                request_id.map(|id| format!("https://queue.fal.run/{endpoint}/requests/{id}"))
            })
            .ok_or_else(|| {
                ToolError::ExecutionFailed("FAL queue response omitted request URL".into())
            })?;
        let resp = self
            .client
            .get(response_url)
            .header("Authorization", auth_value)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("FAL queue response failed: {e}")))?;
        read_json_response(resp, "FAL queue response").await
    }
}

/// xAI Grok Imagine video credentials resolved from env or Hermes auth store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XaiVideoCredentials {
    pub api_key: String,
    pub base_url: String,
    pub source: String,
}

/// xAI video generation backend using `/videos/generations`.
#[derive(Debug)]
pub struct XaiVideoGenBackend {
    client: Client,
    credentials: Option<XaiVideoCredentials>,
}

impl XaiVideoGenBackend {
    pub fn new(credentials: XaiVideoCredentials) -> Self {
        Self {
            client: Client::new(),
            credentials: Some(credentials),
        }
    }

    pub fn unconfigured() -> Self {
        Self {
            client: Client::new(),
            credentials: None,
        }
    }

    pub fn from_env_or_auth_store() -> Result<Self, ToolError> {
        Ok(Self::new(resolve_xai_video_credentials()?))
    }

    pub fn credentials(&self) -> Option<&XaiVideoCredentials> {
        self.credentials.as_ref()
    }

    pub fn transport_label(&self) -> &'static str {
        match self.credentials.as_ref().map(|creds| creds.source.as_str()) {
            Some("env") => "direct",
            Some(_) => "auth-store",
            None => "unconfigured",
        }
    }

    async fn submit_xai_generation(
        &self,
        credentials: &XaiVideoCredentials,
        payload: &Value,
    ) -> Result<String, ToolError> {
        let url = format!("{}/videos/generations", credentials.base_url);
        let resp = self
            .client
            .post(url)
            .bearer_auth(&credentials.api_key)
            .header("Content-Type", "application/json")
            .header("User-Agent", xai_user_agent())
            .header("x-idempotency-key", Uuid::new_v4().to_string())
            .json(payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("xAI video submit failed: {e}")))?;
        let body = read_json_response(resp, "xAI video submit").await?;
        body.get("request_id")
            .or_else(|| body.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                ToolError::ExecutionFailed("xAI video response did not include request_id".into())
            })
    }

    async fn poll_xai_generation(
        &self,
        credentials: &XaiVideoCredentials,
        request_id: &str,
    ) -> Result<Value, ToolError> {
        let timeout = env_u64("XAI_VIDEO_TIMEOUT_SECONDS").unwrap_or(DEFAULT_XAI_TIMEOUT_SECONDS);
        let poll_interval =
            env_u64("XAI_VIDEO_POLL_INTERVAL_SECONDS").unwrap_or(DEFAULT_XAI_POLL_INTERVAL_SECONDS);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout);
        let url = format!("{}/videos/{request_id}", credentials.base_url);
        let mut last_status = String::from("queued");

        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(ToolError::ExecutionFailed(format!(
                    "Timed out waiting for xAI video generation after {timeout}s; last status: {last_status}"
                )));
            }

            let resp = self
                .client
                .get(&url)
                .bearer_auth(&credentials.api_key)
                .header("User-Agent", xai_user_agent())
                .send()
                .await
                .map_err(|e| {
                    ToolError::ExecutionFailed(format!("xAI video status request failed: {e}"))
                })?;
            let body = read_json_response(resp, "xAI video status").await?;
            last_status = body
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_lowercase();

            match last_status.as_str() {
                "done" | "completed" | "succeeded" => return Ok(body),
                "failed" | "error" | "expired" | "cancelled" | "canceled" => {
                    let message = extract_xai_failure_message(&body)
                        .unwrap_or_else(|| format!("xAI video generation failed: {last_status}"));
                    return Err(ToolError::ExecutionFailed(message));
                }
                _ => tokio::time::sleep(Duration::from_secs(poll_interval.max(1))).await,
            }
        }
    }
}

#[async_trait]
impl VideoGenerateBackend for XaiVideoGenBackend {
    async fn generate_video(&self, request: VideoGenerateRequest) -> Result<String, ToolError> {
        let credentials = self.credentials.as_ref().ok_or_else(|| {
            ToolError::ExecutionFailed(
                "No xAI credentials found. Sign in via `hermes auth add xai-oauth` or set XAI_API_KEY.".into(),
            )
        })?;

        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "prompt is required for xAI video generation.".into(),
            ));
        }

        let xai_payload = build_xai_payload(&request)?;
        let payload = Value::Object(xai_payload.payload);
        let request_id = self.submit_xai_generation(credentials, &payload).await?;
        let response = self
            .poll_xai_generation(credentials, request_id.as_str())
            .await?;
        let video = extract_video(&response).ok_or_else(|| {
            ToolError::ExecutionFailed("xAI returned no video URL in response".into())
        })?;

        let mut out = Map::new();
        out.insert("success".into(), Value::Bool(true));
        out.insert("video".into(), Value::String(video.url));
        out.insert("model".into(), Value::String(xai_payload.model));
        out.insert("prompt".into(), Value::String(prompt.to_string()));
        out.insert("modality".into(), Value::String(xai_payload.modality));
        out.insert(
            "aspect_ratio".into(),
            Value::String(xai_payload.aspect_ratio),
        );
        out.insert(
            "duration".into(),
            Value::Number(xai_payload.duration.into()),
        );
        out.insert("provider".into(), Value::String("xai".into()));
        out.insert("request_id".into(), Value::String(request_id));
        out.insert("resolution".into(), Value::String(xai_payload.resolution));
        out.insert(
            "transport".into(),
            Value::String(self.transport_label().to_string()),
        );
        if let Some(file_size) = video.file_size {
            out.insert("file_size".into(), Value::Number(file_size.into()));
        }
        if let Some(content_type) = video.content_type {
            out.insert("content_type".into(), Value::String(content_type));
        }
        if let Some(usage) = response.get("usage").cloned() {
            out.insert("usage".into(), usage);
        }

        Ok(Value::Object(out).to_string())
    }
}

/// Configured built-in video generation backend.
#[derive(Debug)]
pub enum VideoGenBackend {
    Fal(FalVideoGenBackend),
    Xai(XaiVideoGenBackend),
}

impl VideoGenBackend {
    pub fn from_env_or_managed() -> Self {
        match selected_video_provider() {
            Some("xai") => XaiVideoGenBackend::from_env_or_auth_store()
                .unwrap_or_else(|_| XaiVideoGenBackend::unconfigured())
                .into(),
            _ => FalVideoGenBackend::from_env_or_managed()
                .unwrap_or_else(|_| FalVideoGenBackend::unconfigured())
                .into(),
        }
    }

    pub fn provider_label(&self) -> &'static str {
        match self {
            Self::Fal(_) => "fal",
            Self::Xai(_) => "xai",
        }
    }

    pub fn required_env_vars(&self) -> Vec<String> {
        match self {
            Self::Fal(_) => vec!["FAL_KEY".into()],
            Self::Xai(_) => vec!["XAI_API_KEY".into()],
        }
    }
}

impl From<FalVideoGenBackend> for VideoGenBackend {
    fn from(value: FalVideoGenBackend) -> Self {
        Self::Fal(value)
    }
}

impl From<XaiVideoGenBackend> for VideoGenBackend {
    fn from(value: XaiVideoGenBackend) -> Self {
        Self::Xai(value)
    }
}

#[async_trait]
impl VideoGenerateBackend for VideoGenBackend {
    async fn generate_video(&self, request: VideoGenerateRequest) -> Result<String, ToolError> {
        match self {
            Self::Fal(backend) => backend.generate_video(request).await,
            Self::Xai(backend) => backend.generate_video(request).await,
        }
    }
}

#[async_trait]
impl VideoGenerateBackend for FalVideoGenBackend {
    async fn generate_video(&self, request: VideoGenerateRequest) -> Result<String, ToolError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams("prompt is required.".into()));
        }

        let family = resolve_family(request.model.as_deref());
        let image_url = request
            .image_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let (endpoint, modality) = if image_url.is_some() {
            (family.image_endpoint, "image")
        } else {
            (family.text_endpoint, "text")
        };
        let payload_meta = build_payload(family, &request);
        let payload = Value::Object(payload_meta.payload);
        let response = match self.transport {
            FalVideoTransport::Direct { .. } => {
                self.submit_direct_queue(endpoint, &payload).await?
            }
            FalVideoTransport::Managed { .. } => self.submit_managed(endpoint, &payload).await?,
            FalVideoTransport::Unconfigured => {
                return Err(ToolError::ExecutionFailed(
                    "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.".into(),
                ));
            }
        };

        let video = extract_video(&response).ok_or_else(|| {
            ToolError::ExecutionFailed("FAL returned no video URL in response".into())
        })?;

        let mut out = Map::new();
        out.insert("success".into(), Value::Bool(true));
        out.insert("video".into(), Value::String(video.url));
        out.insert("model".into(), Value::String(family.id.to_string()));
        out.insert("prompt".into(), Value::String(prompt.to_string()));
        out.insert("modality".into(), Value::String(modality.to_string()));
        out.insert(
            "aspect_ratio".into(),
            Value::String(payload_meta.aspect_ratio.unwrap_or_default()),
        );
        out.insert(
            "duration".into(),
            payload_meta
                .duration
                .map(|d| Value::Number(d.into()))
                .unwrap_or_else(|| Value::Number(0.into())),
        );
        out.insert("provider".into(), Value::String("fal".into()));
        out.insert("endpoint".into(), Value::String(endpoint.to_string()));
        out.insert(
            "transport".into(),
            Value::String(self.transport.label().to_string()),
        );
        if let Some(file_size) = video.file_size {
            out.insert("file_size".into(), Value::Number(file_size.into()));
        }
        if let Some(content_type) = video.content_type {
            out.insert("content_type".into(), Value::String(content_type));
        }

        Ok(Value::Object(out).to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PayloadMeta {
    payload: Map<String, Value>,
    aspect_ratio: Option<String>,
    duration: Option<u32>,
}

fn build_payload(family: &FalVideoFamily, request: &VideoGenerateRequest) -> PayloadMeta {
    let mut payload = Map::new();
    payload.insert(
        "prompt".into(),
        Value::String(request.prompt.trim().to_string()),
    );
    if let Some(image_url) = request
        .image_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        payload.insert(
            family.image_param_key.into(),
            Value::String(image_url.to_string()),
        );
    }
    if let Some(seed) = request.seed {
        payload.insert("seed".into(), Value::Number(seed.into()));
    }

    let mut sent_aspect_ratio = None;
    if !family.aspect_ratios.is_empty()
        && family
            .aspect_ratios
            .contains(&request.aspect_ratio.as_str())
    {
        payload.insert(
            "aspect_ratio".into(),
            Value::String(request.aspect_ratio.clone()),
        );
        sent_aspect_ratio = Some(request.aspect_ratio.clone());
    }

    if !family.resolutions.is_empty() && family.resolutions.contains(&request.resolution.as_str()) {
        payload.insert(
            "resolution".into(),
            Value::String(request.resolution.clone()),
        );
    }

    let duration = clamp_duration(family.durations, request.duration);
    if let Some(duration) = duration {
        payload.insert("duration".into(), Value::String(duration.to_string()));
    }

    if family.audio {
        if let Some(audio) = request.audio {
            payload.insert("generate_audio".into(), Value::Bool(audio));
        }
    }

    if family.negative {
        if let Some(negative_prompt) = request.negative_prompt.as_deref() {
            payload.insert(
                "negative_prompt".into(),
                Value::String(negative_prompt.to_string()),
            );
        }
    }

    PayloadMeta {
        payload,
        aspect_ratio: sent_aspect_ratio,
        duration,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct XaiPayloadMeta {
    payload: Map<String, Value>,
    model: String,
    modality: String,
    aspect_ratio: String,
    resolution: String,
    duration: u32,
}

fn build_xai_payload(request: &VideoGenerateRequest) -> Result<XaiPayloadMeta, ToolError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(ToolError::InvalidParams(
            "prompt is required for xAI video generation.".into(),
        ));
    }

    let image_url = request
        .image_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reference_images = normalize_xai_reference_images(&request.reference_image_urls);
    if image_url.is_some() && !reference_images.is_empty() {
        return Err(ToolError::InvalidParams(
            "image_url and reference_image_urls cannot be combined on xAI.".into(),
        ));
    }
    if reference_images.len() > XAI_MAX_REFERENCE_IMAGES {
        return Err(ToolError::InvalidParams(format!(
            "reference_image_urls supports at most {XAI_MAX_REFERENCE_IMAGES} images on xAI."
        )));
    }

    let modality = if image_url.is_some() || !reference_images.is_empty() {
        "image"
    } else {
        "text"
    };
    let model =
        resolve_xai_model_for_modality(request.model.as_deref(), modality, request.model_explicit);
    let aspect_ratio = normalize_xai_choice(
        request.aspect_ratio.as_str(),
        XAI_VALID_ASPECT_RATIOS,
        DEFAULT_XAI_ASPECT_RATIO,
    );
    let resolution = normalize_xai_choice(
        request.resolution.as_str(),
        XAI_VALID_RESOLUTIONS,
        DEFAULT_XAI_RESOLUTION,
    )
    .to_ascii_lowercase();
    let duration = clamp_xai_duration(request.duration, !reference_images.is_empty());

    let mut payload = Map::new();
    payload.insert("model".into(), Value::String(model.clone()));
    payload.insert("prompt".into(), Value::String(prompt.to_string()));
    payload.insert("duration".into(), Value::Number(duration.into()));
    payload.insert("aspect_ratio".into(), Value::String(aspect_ratio.clone()));
    payload.insert("resolution".into(), Value::String(resolution.clone()));
    if let Some(image_url) = image_url {
        let mut image = Map::new();
        image.insert("url".into(), Value::String(image_ref_to_xai_url(image_url)));
        payload.insert("image".into(), Value::Object(image));
    }
    if !reference_images.is_empty() {
        payload.insert(
            "reference_images".into(),
            Value::Array(
                reference_images
                    .into_iter()
                    .map(|url| {
                        let mut image = Map::new();
                        image.insert("url".into(), Value::String(url));
                        Value::Object(image)
                    })
                    .collect(),
            ),
        );
    }

    Ok(XaiPayloadMeta {
        payload,
        model,
        modality: modality.to_string(),
        aspect_ratio,
        resolution,
        duration,
    })
}

fn normalize_xai_reference_images(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| image_ref_to_xai_url(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn image_ref_to_xai_url(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:image/")
    {
        return trimmed.to_string();
    }

    let expanded = expand_user_path(trimmed);
    if !expanded.is_file() {
        return trimmed.to_string();
    }
    let Some(mime) = image_mime_for_path(&expanded).filter(|mime| mime.starts_with("image/"))
    else {
        return trimmed.to_string();
    };
    let Ok(bytes) = std::fs::read(&expanded) else {
        return trimmed.to_string();
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    format!("data:{mime};base64,{encoded}")
}

fn expand_user_path(value: &str) -> PathBuf {
    if value == "~" {
        return user_home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(stripped) = value.strip_prefix("~/") {
        if let Some(home) = user_home_dir() {
            return home.join(stripped);
        }
    }
    PathBuf::from(value)
}

fn image_mime_for_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("png") => Some("image/png"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        Some("avif") => Some("image/avif"),
        Some("svg") => Some("image/svg+xml"),
        _ => None,
    }
}

fn clamp_xai_duration(duration: Option<u32>, has_reference_images: bool) -> u32 {
    let mut value = duration.unwrap_or(DEFAULT_XAI_DURATION).clamp(1, 15);
    if has_reference_images {
        value = value.min(10);
    }
    value
}

fn normalize_xai_choice(value: &str, valid: &[&str], default: &str) -> String {
    let trimmed = value.trim();
    if valid.contains(&trimmed) {
        trimmed.to_string()
    } else {
        default.to_string()
    }
}

fn resolve_xai_model_for_modality(
    model: Option<&str>,
    modality: &str,
    explicit_model: bool,
) -> String {
    let requested = model.map(str::trim).filter(|value| !value.is_empty());
    if explicit_model {
        if let Some(requested) = requested {
            return requested.to_string();
        }
    }
    if modality == "image" {
        return DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL.to_string();
    }
    match requested {
        Some(DEFAULT_XAI_IMAGE_TO_VIDEO_MODEL | XAI_IMAGE_TO_VIDEO_MODEL_ALIAS) => {
            DEFAULT_XAI_TEXT_TO_VIDEO_MODEL.to_string()
        }
        Some(requested) => requested.to_string(),
        None => DEFAULT_XAI_TEXT_TO_VIDEO_MODEL.to_string(),
    }
}

fn clamp_duration(spec: Option<DurationSpec>, duration: Option<u32>) -> Option<u32> {
    match spec {
        None => None,
        Some(DurationSpec::Range(lo, hi)) => Some(duration.unwrap_or(lo).clamp(lo, hi)),
        Some(DurationSpec::Enum(values)) => {
            let requested = duration.unwrap_or_else(|| values[0]);
            values
                .iter()
                .copied()
                .min_by_key(|candidate| candidate.abs_diff(requested))
        }
    }
}

fn resolve_family(explicit: Option<&str>) -> &'static FalVideoFamily {
    let candidates = explicit
        .into_iter()
        .map(ToOwned::to_owned)
        .chain(std::env::var("FAL_VIDEO_MODEL").ok())
        .chain(configured_video_model_candidates());
    for candidate in candidates {
        if let Some(family) = family_by_id(candidate.trim()) {
            return family;
        }
    }
    family_by_id(DEFAULT_FAL_VIDEO_MODEL).expect("default FAL video family exists")
}

fn family_by_id(id: &str) -> Option<&'static FalVideoFamily> {
    FAL_VIDEO_FAMILIES.iter().find(|family| family.id == id)
}

fn configured_video_model_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for path in [
        hermes_config::cli_config_path(),
        hermes_config::config_path(),
    ] {
        collect_video_model_candidates(&path, &mut out);
    }
    out
}

fn selected_video_provider() -> Option<&'static str> {
    let mut candidates = Vec::new();
    if let Some(provider) =
        env_string("HERMES_VIDEO_GEN_BACKEND").or_else(|| env_string("VIDEO_GEN_BACKEND"))
    {
        candidates.push(provider);
    }
    candidates.extend(configured_video_provider_candidates());

    candidates
        .into_iter()
        .find_map(|candidate| normalize_video_provider(candidate.as_str()))
}

fn configured_video_provider_candidates() -> Vec<String> {
    let mut out = Vec::new();
    for path in [
        hermes_config::cli_config_path(),
        hermes_config::config_path(),
    ] {
        collect_video_provider_candidates(&path, &mut out);
    }
    out
}

fn collect_video_model_candidates(path: &Path, out: &mut Vec<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return;
    };
    let Some(video_gen) = root.get("video_gen") else {
        return;
    };
    if let Some(model) = video_gen
        .get("fal")
        .and_then(|fal| fal.get("model"))
        .and_then(serde_yaml::Value::as_str)
    {
        out.push(model.to_string());
    }
    if let Some(model) = video_gen.get("model").and_then(serde_yaml::Value::as_str) {
        out.push(model.to_string());
    }
}

fn collect_video_provider_candidates(path: &Path, out: &mut Vec<String>) {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(&raw) else {
        return;
    };
    let Some(video_gen) = root.get("video_gen") else {
        return;
    };
    for key in ["provider", "backend"] {
        if let Some(provider) = video_gen.get(key).and_then(serde_yaml::Value::as_str) {
            out.push(provider.to_string());
        }
    }
}

fn normalize_video_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "xai" | "x.ai" | "grok" | "grok-imagine" | "grok-imagine-video" => Some("xai"),
        "fal" | "fal-ai" | "fal_ai" | "fal-queue" => Some("fal"),
        _ => None,
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok()
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_xai_video_credentials() -> Result<XaiVideoCredentials, ToolError> {
    if let Some(api_key) = env_string("HERMES_XAI_API_KEY").or_else(|| env_string("XAI_API_KEY")) {
        return Ok(XaiVideoCredentials {
            api_key,
            base_url: env_string("HERMES_XAI_BASE_URL")
                .or_else(|| env_string("XAI_BASE_URL"))
                .unwrap_or_else(|| DEFAULT_XAI_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            source: "env".to_string(),
        });
    }

    for path in auth_store_candidates() {
        if let Some(credentials) = read_xai_credentials_from_auth_store(&path) {
            return Ok(credentials);
        }
    }

    Err(ToolError::ExecutionFailed(
        "No xAI credentials found. Sign in via `hermes auth add xai-oauth` or set XAI_API_KEY."
            .into(),
    ))
}

fn auth_store_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_string("HERMES_AUTH_FILE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(hermes_config::paths::auth_json_path());
    if let Some(home) = user_home_dir() {
        candidates.push(home.join(".hermes-agent-ultra").join("auth.json"));
        candidates.push(home.join(".hermes").join("auth.json"));
    }

    let mut seen = Vec::<PathBuf>::new();
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| {
            if seen.contains(path) {
                false
            } else {
                seen.push(path.clone());
                true
            }
        })
        .collect()
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn read_xai_credentials_from_auth_store(path: &PathBuf) -> Option<XaiVideoCredentials> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    let providers = value.get("providers").and_then(Value::as_object)?;
    let provider = ["xai", "xai-oauth", "xai_oauth"]
        .into_iter()
        .find_map(|name| providers.get(name).and_then(Value::as_object))?;
    let api_key = provider
        .get("api_key")
        .or_else(|| provider.get("access_token"))
        .or_else(|| provider.get("token"))
        .or_else(|| provider.get("bearer_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let base_url = provider
        .get("base_url")
        .or_else(|| provider.get("api_base_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_XAI_BASE_URL)
        .trim_end_matches('/')
        .to_string();
    Some(XaiVideoCredentials {
        api_key,
        base_url,
        source: path.display().to_string(),
    })
}

fn xai_user_agent() -> &'static str {
    "hermes-agent/video_gen"
}

fn extract_xai_failure_message(value: &Value) -> Option<String> {
    value
        .get("error")
        .and_then(|error| {
            error
                .as_object()
                .and_then(|object| object.get("message").and_then(Value::as_str))
                .or_else(|| error.as_str())
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

async fn read_json_response(resp: reqwest::Response, label: &str) -> Result<Value, ToolError> {
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read {label} response: {e}")))?;
    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "{label} error ({status}): {text}"
        )));
    }
    serde_json::from_str(&text)
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse {label} response: {e}")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoArtifact {
    url: String,
    file_size: Option<u64>,
    content_type: Option<String>,
}

fn extract_video(value: &Value) -> Option<VideoArtifact> {
    if let Some(data) = value.get("data").filter(|data| data.is_object()) {
        if let Some(video) = extract_video(data) {
            return Some(video);
        }
    }
    let video = value.get("video")?;
    if let Some(url) = video.as_str().filter(|url| !url.trim().is_empty()) {
        return Some(VideoArtifact {
            url: url.to_string(),
            file_size: None,
            content_type: None,
        });
    }
    let obj = video.as_object()?;
    let url = obj.get("url")?.as_str()?.trim();
    if url.is_empty() {
        return None;
    }
    Some(VideoArtifact {
        url: url.to_string(),
        file_size: obj.get("file_size").and_then(Value::as_u64),
        content_type: obj
            .get("content_type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

include!("video_gen/tests.rs");
