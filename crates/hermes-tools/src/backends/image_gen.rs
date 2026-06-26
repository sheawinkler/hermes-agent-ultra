//! Real image generation backends.
//!
//! Supports:
//!
//! 1. **fal direct**: `FAL_KEY` env var -> calls `https://fal.run/fal-ai/...`
//!    with the user's `Authorization: Key ...` header.
//! 2. **fal managed**: when `FAL_KEY` is missing AND
//!    `HERMES_ENABLE_NOUS_MANAGED_TOOLS` is on with a Nous OAuth token,
//!    routes via the `fal-queue` vendor gateway with `Bearer` auth.
//! 3. **OpenAI Codex OAuth**: `image_gen.provider: codex` or
//!    `HERMES_IMAGE_GEN_PROVIDER=openai-codex` routes through the ChatGPT
//!    Codex Responses `image_generation` tool and saves the PNG under
//!    `$HERMES_HOME/cache/images/`.
//! 4. **OpenRouter-compatible image output**: `image_gen.provider: openrouter`
//!    or `nous` uses OpenAI-style `/chat/completions` image output with
//!    reference-image grounding and stores returned images under
//!    `$HERMES_HOME/cache/images/`.
//!
//! The active transport is reflected in the response JSON (`transport`
//! field) for observability.

use async_trait::async_trait;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use reqwest::Client;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tools::image_gen::{ImageGenBackend, ImageGenCapabilities, ImageGenerateRequest};
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

/// Default fal.ai model when running through direct mode. Same default as the
/// current upstream `image_generation_tool.py`.
const DEFAULT_FAL_MODEL_PATH: &str = "fal-ai/flux-2/klein/9b";
const DEFAULT_FAL_ASPECT_RATIO: &str = "landscape";
const DEFAULT_CODEX_IMAGE_MODEL: &str = "gpt-image-2-medium";
const CODEX_IMAGE_API_MODEL: &str = "gpt-image-2";
const DEFAULT_CODEX_IMAGE_CHAT_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_IMAGE_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_IMAGE_INSTRUCTIONS: &str = "You are an assistant that must fulfill image generation requests by using the image_generation tool when provided.";
const CODEX_CLOUDFLARE_ORIGINATOR: &str = "codex_cli_rs";
const DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL: &str = "google/gemini-2.5-flash-image";
const DEFAULT_OPENROUTER_IMAGE_BASE_URL: &str = "https://openrouter.ai/api/v1";
const DEFAULT_NOUS_IMAGE_BASE_URL: &str = "https://inference.nousresearch.com/v1";
const OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES: usize = 3;
const OPENROUTER_COMPAT_TIMEOUT_SECS: u64 = 180;
const OPENROUTER_COMPAT_HTTP_REFERER: &str = "https://github.com/NousResearch/hermes-agent";
const OPENROUTER_COMPAT_X_TITLE: &str = "Hermes Agent";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FalSizeStyle {
    ImageSizePreset,
    AspectRatio,
    GptLiteral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FalModelSpec {
    id: &'static str,
    display: &'static str,
    size_style: FalSizeStyle,
    landscape: &'static str,
    square: &'static str,
    portrait: &'static str,
    supports: &'static [&'static str],
    edit_endpoint: Option<&'static str>,
    edit_supports: &'static [&'static str],
    max_reference_images: usize,
}

#[derive(Debug, Clone, PartialEq)]
struct FalPreparedRequest {
    endpoint: String,
    body: Value,
    modality: &'static str,
    source_image_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FalTransport {
    Direct {
        api_key: String,
    },
    Managed {
        gateway_origin: String,
        nous_token: String,
    },
}

impl FalTransport {
    fn label(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Managed { .. } => "managed",
        }
    }

    /// Returns the full submit URL for the given fal model path
    /// (e.g. `fal-ai/flux/dev`).
    fn submit_url(&self, model_path: &str) -> String {
        match self {
            Self::Direct { .. } => format!("https://fal.run/{model_path}"),
            // Managed gateways expose a uniform `/run/{model}` endpoint.
            Self::Managed { gateway_origin, .. } => {
                let root = gateway_origin.trim_end_matches('/');
                format!("{root}/run/{model_path}")
            }
        }
    }

    fn auth_header(&self) -> (String, String) {
        match self {
            Self::Direct { api_key } => ("Authorization".into(), format!("Key {api_key}")),
            Self::Managed { nous_token, .. } => {
                ("Authorization".into(), format!("Bearer {nous_token}"))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexImageTier {
    id: &'static str,
    quality: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAICodexImageGenConfig {
    access_token: Option<String>,
    base_url: String,
    chat_model: String,
    tier_id: String,
    quality: String,
    output_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRouterCompatImageProviderKind {
    OpenRouter,
    Nous,
}

impl OpenRouterCompatImageProviderKind {
    fn provider_id(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::Nous => "nous",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::OpenRouter => "OpenRouter",
            Self::Nous => "Nous Portal",
        }
    }

    fn config_key(self) -> &'static str {
        self.provider_id()
    }

    fn model_env_var(self) -> &'static str {
        match self {
            Self::OpenRouter => "OPENROUTER_IMAGE_MODEL",
            Self::Nous => "NOUS_IMAGE_MODEL",
        }
    }

    fn api_key_env_vars(self) -> &'static [&'static str] {
        match self {
            Self::OpenRouter => &["OPENROUTER_API_KEY"],
            Self::Nous => &["NOUS_API_KEY"],
        }
    }

    fn base_url_env_vars(self) -> &'static [&'static str] {
        match self {
            Self::OpenRouter => &["OPENROUTER_IMAGE_BASE_URL", "OPENROUTER_BASE_URL"],
            Self::Nous => &["NOUS_IMAGE_BASE_URL", "NOUS_BASE_URL"],
        }
    }

    fn default_base_url(self) -> &'static str {
        match self {
            Self::OpenRouter => DEFAULT_OPENROUTER_IMAGE_BASE_URL,
            Self::Nous => DEFAULT_NOUS_IMAGE_BASE_URL,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRouterCompatImageGenConfig {
    provider: OpenRouterCompatImageProviderKind,
    api_key: Option<String>,
    base_url: String,
    model: String,
    output_dir: PathBuf,
}

/// Image generation backend using ChatGPT/Codex OAuth and the Responses
/// `image_generation` tool.
#[derive(Debug)]
pub struct OpenAICodexImageGenBackend {
    client: Client,
    config: OpenAICodexImageGenConfig,
}

/// OpenRouter-compatible image generation over `/chat/completions`.
///
/// This serves both OpenRouter and Nous Portal: both accept `modalities:
/// ["image", "text"]`, reference images as `image_url` parts, and return
/// generated image URLs under `choices[].message.images`.
#[derive(Debug)]
pub struct OpenRouterCompatImageGenBackend {
    client: Client,
    config: OpenRouterCompatImageGenConfig,
}

/// Image generation backend using fal.ai (direct or via Nous-managed
/// gateway).
#[derive(Debug)]
pub struct FalImageGenBackend {
    client: Client,
    transport: FalTransport,
    model_path: String,
}

impl FalImageGenBackend {
    /// Construct a direct backend from an explicit API key. Uses
    /// `fal-ai/flux/dev` as the default model.
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            transport: FalTransport::Direct { api_key },
            model_path: DEFAULT_FAL_MODEL_PATH.into(),
        }
    }

    /// Override the fal model path (e.g. `fal-ai/flux-pro`).
    pub fn with_model_path(mut self, model_path: impl Into<String>) -> Self {
        self.model_path = model_path.into();
        self
    }

    /// Construct a managed-mode backend from a resolved gateway config.
    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            client: Client::new(),
            transport: FalTransport::Managed {
                gateway_origin: cfg.gateway_origin.clone(),
                nous_token: cfg.nous_user_token.clone(),
            },
            model_path: DEFAULT_FAL_MODEL_PATH.into(),
        }
    }

    /// Resolve the best-available transport.
    ///
    /// Priority: direct `FAL_KEY` → Nous-managed `fal-queue` vendor →
    /// `Err` with a hint covering both paths.
    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        let model_path = resolve_fal_model_path();
        if let Ok(key) = std::env::var("FAL_KEY") {
            let trimmed = key.trim();
            if !trimmed.is_empty() {
                return Ok(Self::new(trimmed.to_string()).with_model_path(model_path));
            }
        }
        if let Some(cfg) = resolve_managed_tool_gateway("fal-queue", ResolveOptions::default()) {
            return Ok(Self::from_managed(&cfg).with_model_path(model_path));
        }
        Err(ToolError::ExecutionFailed(
            "FAL_KEY not set and Nous-managed fal-queue gateway is not configured.".into(),
        ))
    }

    /// Backwards-compatible alias.
    pub fn from_env() -> Result<Self, ToolError> {
        Self::from_env_or_managed()
    }

    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }

    pub fn model_path(&self) -> &str {
        &self.model_path
    }

    fn prepare_request(
        &self,
        request: &ImageGenerateRequest,
    ) -> Result<FalPreparedRequest, ToolError> {
        if request.prompt.trim().is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }

        let source_images = request.source_image_urls();
        if source_images.is_empty() {
            return Ok(FalPreparedRequest {
                endpoint: self.model_path.clone(),
                body: build_fal_text_payload(&self.model_path, request),
                modality: "text",
                source_image_count: 0,
            });
        }

        let spec = fal_model_spec(&self.model_path).ok_or_else(|| {
            ToolError::InvalidParams(format!(
                "FAL model '{}' is not declared as image-to-image capable. Omit image_url/reference_image_urls, or switch to an edit-capable model.",
                self.model_path
            ))
        })?;
        let edit_endpoint = spec.edit_endpoint.ok_or_else(|| {
            ToolError::InvalidParams(format!(
                "Model '{}' ({}) is not capable of image-to-image / editing. Omit image_url/reference_image_urls, or switch to an edit-capable FAL model.",
                spec.display, spec.id
            ))
        })?;
        let max_refs = spec.max_reference_images;
        let clamped_sources: Vec<String> = if max_refs > 0 {
            source_images.into_iter().take(max_refs).collect()
        } else {
            source_images
        };
        Ok(FalPreparedRequest {
            endpoint: edit_endpoint.to_string(),
            body: build_fal_edit_payload(spec, request, &clamped_sources),
            modality: "image",
            source_image_count: clamped_sources.len(),
        })
    }
}

impl OpenAICodexImageGenConfig {
    pub fn new(access_token: Option<String>) -> Self {
        let tier = resolve_codex_image_tier();
        Self {
            access_token,
            base_url: env_optional_nonempty("HERMES_OPENAI_CODEX_BASE_URL")
                .or_else(|| env_optional_nonempty("OPENAI_CODEX_BASE_URL"))
                .unwrap_or_else(|| DEFAULT_CODEX_IMAGE_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            chat_model: env_optional_nonempty("HERMES_CODEX_IMAGE_CHAT_MODEL")
                .or_else(|| env_optional_nonempty("OPENAI_CODEX_IMAGE_CHAT_MODEL"))
                .unwrap_or_else(|| DEFAULT_CODEX_IMAGE_CHAT_MODEL.to_string()),
            tier_id: tier.id.to_string(),
            quality: tier.quality.to_string(),
            output_dir: hermes_config::hermes_home().join("cache").join("images"),
        }
    }

    pub fn from_env_or_auth_store() -> Result<Self, ToolError> {
        let auth = codex_image_auth_from_env_or_store();
        let mut cfg = Self::new(auth.access_token);
        if let Some(base_url) = auth.base_url {
            cfg.base_url = base_url.trim_end_matches('/').to_string();
        }
        if cfg.access_token.as_deref().is_none_or(str::is_empty) {
            return Err(ToolError::ExecutionFailed(
                "OpenAI Codex image generation requires Codex OAuth credentials. Run `hermes auth codex` or set HERMES_OPENAI_CODEX_API_KEY.".into(),
            ));
        }
        Ok(cfg)
    }

    pub fn unconfigured() -> Self {
        Self::new(None)
    }

    pub fn tier_id(&self) -> &str {
        &self.tier_id
    }

    pub fn quality(&self) -> &str {
        &self.quality
    }
}

impl OpenAICodexImageGenBackend {
    pub fn new(access_token: String) -> Self {
        Self::from_config(OpenAICodexImageGenConfig::new(Some(access_token)))
    }

    pub fn from_config(config: OpenAICodexImageGenConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .unwrap_or_else(|err| {
                    tracing::warn!("failed to build Codex image HTTP client: {}", err);
                    Client::new()
                }),
            config,
        }
    }

    pub fn from_env_or_auth_store() -> Result<Self, ToolError> {
        Ok(Self::from_config(
            OpenAICodexImageGenConfig::from_env_or_auth_store()?,
        ))
    }

    pub fn unconfigured() -> Self {
        Self::from_config(OpenAICodexImageGenConfig::unconfigured())
    }

    pub fn config(&self) -> &OpenAICodexImageGenConfig {
        &self.config
    }

    fn responses_url(&self) -> String {
        format!("{}/responses", self.config.base_url.trim_end_matches('/'))
    }
}

impl OpenRouterCompatImageGenConfig {
    pub fn new(provider: OpenRouterCompatImageProviderKind, api_key: Option<String>) -> Self {
        Self {
            provider,
            api_key,
            base_url: resolve_openrouter_compat_base_url(provider),
            model: resolve_openrouter_compat_model(provider),
            output_dir: hermes_config::hermes_home().join("cache").join("images"),
        }
    }

    pub fn from_env_or_config(
        provider: OpenRouterCompatImageProviderKind,
    ) -> Result<Self, ToolError> {
        let api_key = resolve_openrouter_compat_api_key(provider);
        let cfg = Self::new(provider, api_key);
        if cfg.api_key.as_deref().is_none_or(str::is_empty) {
            return Err(ToolError::ExecutionFailed(format!(
                "{} image generation requires credentials. Set {} or configure image_gen.{}.api_key.",
                provider.display_name(),
                provider.api_key_env_vars().join("/"),
                provider.config_key()
            )));
        }
        Ok(cfg)
    }

    pub fn unconfigured(provider: OpenRouterCompatImageProviderKind) -> Self {
        Self::new(provider, None)
    }

    pub fn provider(&self) -> OpenRouterCompatImageProviderKind {
        self.provider
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl OpenRouterCompatImageGenBackend {
    pub fn from_config(config: OpenRouterCompatImageGenConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(OPENROUTER_COMPAT_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|err| {
                    tracing::warn!(
                        "failed to build OpenRouter-compatible image HTTP client: {}",
                        err
                    );
                    Client::new()
                }),
            config,
        }
    }

    pub fn from_env_or_config(
        provider: OpenRouterCompatImageProviderKind,
    ) -> Result<Self, ToolError> {
        Ok(Self::from_config(
            OpenRouterCompatImageGenConfig::from_env_or_config(provider)?,
        ))
    }

    pub fn unconfigured(provider: OpenRouterCompatImageProviderKind) -> Self {
        Self::from_config(OpenRouterCompatImageGenConfig::unconfigured(provider))
    }

    pub fn config(&self) -> &OpenRouterCompatImageGenConfig {
        &self.config
    }

    fn chat_completions_url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

/// Configured built-in image generation backend.
#[derive(Debug)]
pub enum ImageGenRuntimeBackend {
    Fal(FalImageGenBackend),
    OpenAICodex(OpenAICodexImageGenBackend),
    OpenRouterCompat(OpenRouterCompatImageGenBackend),
}

impl ImageGenRuntimeBackend {
    pub fn from_env_or_managed() -> Self {
        match selected_image_provider() {
            Some("openai-codex") => OpenAICodexImageGenBackend::from_env_or_auth_store()
                .unwrap_or_else(|_| OpenAICodexImageGenBackend::unconfigured())
                .into(),
            Some("openrouter") => OpenRouterCompatImageGenBackend::from_env_or_config(
                OpenRouterCompatImageProviderKind::OpenRouter,
            )
            .unwrap_or_else(|_| {
                OpenRouterCompatImageGenBackend::unconfigured(
                    OpenRouterCompatImageProviderKind::OpenRouter,
                )
            })
            .into(),
            Some("nous") => OpenRouterCompatImageGenBackend::from_env_or_config(
                OpenRouterCompatImageProviderKind::Nous,
            )
            .unwrap_or_else(|_| {
                OpenRouterCompatImageGenBackend::unconfigured(
                    OpenRouterCompatImageProviderKind::Nous,
                )
            })
            .into(),
            _ => FalImageGenBackend::from_env_or_managed()
                .unwrap_or_else(|_| FalImageGenBackend::new(String::new()))
                .into(),
        }
    }

    pub fn provider_label(&self) -> &'static str {
        match self {
            Self::Fal(_) => "fal",
            Self::OpenAICodex(_) => "openai-codex",
            Self::OpenRouterCompat(backend) => backend.config.provider.provider_id(),
        }
    }

    pub fn required_env_vars(&self) -> Vec<String> {
        match self {
            Self::Fal(_) => vec!["FAL_KEY".into()],
            Self::OpenAICodex(_) => vec!["HERMES_OPENAI_CODEX_API_KEY".into()],
            Self::OpenRouterCompat(backend) => backend
                .config
                .provider
                .api_key_env_vars()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        }
    }
}

impl From<FalImageGenBackend> for ImageGenRuntimeBackend {
    fn from(value: FalImageGenBackend) -> Self {
        Self::Fal(value)
    }
}

impl From<OpenAICodexImageGenBackend> for ImageGenRuntimeBackend {
    fn from(value: OpenAICodexImageGenBackend) -> Self {
        Self::OpenAICodex(value)
    }
}

impl From<OpenRouterCompatImageGenBackend> for ImageGenRuntimeBackend {
    fn from(value: OpenRouterCompatImageGenBackend) -> Self {
        Self::OpenRouterCompat(value)
    }
}

#[async_trait]
impl ImageGenBackend for FalImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        let prepared = self.prepare_request(&request)?;
        let url = self.transport.submit_url(&prepared.endpoint);
        let (auth_name, auth_value) = self.transport.auth_header();

        let resp = self
            .client
            .post(url)
            .header(auth_name, auth_value)
            .header("Content-Type", "application/json")
            .json(&prepared.body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("fal.ai API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read fal.ai response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "fal.ai API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse fal.ai response: {}", e))
        })?;

        let images: Vec<Value> = data
            .get("images")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|img| {
                        json!({
                            "url": img.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                            "width": img.get("width").and_then(|w| w.as_u64()).unwrap_or(0),
                            "height": img.get("height").and_then(|h| h.as_u64()).unwrap_or(0),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let image = images
            .first()
            .and_then(|img| img.get("url"))
            .and_then(Value::as_str)
            .map(Value::from)
            .unwrap_or(Value::Null);

        Ok(json!({
            "success": true,
            "image": image,
            "images": images,
            "modality": prepared.modality,
            "transport": self.transport.label(),
            "model": self.model_path,
            "endpoint": prepared.endpoint,
            "source_images": prepared.source_image_count,
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        let spec = fal_model_spec(&self.model_path);
        ImageGenCapabilities {
            provider: Some("FAL.ai".to_string()),
            model: Some(
                spec.map(|spec| spec.display)
                    .unwrap_or_else(|| self.model_path.as_str())
                    .to_string(),
            ),
            modalities: if spec.and_then(|spec| spec.edit_endpoint).is_some() {
                vec!["text".to_string(), "image".to_string()]
            } else {
                vec!["text".to_string()]
            },
            max_reference_images: spec
                .filter(|spec| spec.edit_endpoint.is_some())
                .map(|spec| spec.max_reference_images)
                .unwrap_or(0),
        }
    }
}

#[async_trait]
impl ImageGenBackend for OpenAICodexImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        if request.has_image_inputs() {
            return Err(ToolError::InvalidParams(
                "OpenAI Codex image generation is text-to-image only in this Rust backend; omit image_url/reference_image_urls or switch to an edit-capable FAL model.".into(),
            ));
        }
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }
        let token = self
            .config
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "OpenAI Codex image generation requires Codex OAuth credentials. Run `hermes auth codex` or set HERMES_OPENAI_CODEX_API_KEY.".into(),
                )
            })?;
        let image_size = codex_image_size_from_tool_size(request.size.as_deref());
        let body = codex_image_responses_payload(
            prompt,
            image_size,
            self.config.quality.as_str(),
            self.config.chat_model.as_str(),
        );
        let mut req = self
            .client
            .post(self.responses_url())
            .header("Accept", "text/event-stream")
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .json(&body);
        for (name, value) in codex_cloudflare_headers(Some(token)) {
            req = req.header(name, value);
        }

        let resp = req.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Codex image generation request failed: {e}"))
        })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Codex image response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Codex Responses API returned HTTP {status}: {}",
                text.chars().take(500).collect::<String>()
            )));
        }
        let image_b64 = collect_codex_image_b64_from_sse(&text)?.ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Codex response contained no image_generation_call result".into(),
            )
        })?;
        let image_path =
            save_codex_image_b64(&image_b64, &self.config.output_dir, &self.config.tier_id)?;
        let image = image_path.to_string_lossy().to_string();
        Ok(json!({
            "success": true,
            "image": image,
            "images": [{
                "url": image,
                "path": image_path.to_string_lossy(),
                "width": 0,
                "height": 0,
            }],
            "provider": "openai-codex",
            "transport": "codex",
            "model": self.config.tier_id,
            "prompt": prompt,
            "size": image_size,
            "quality": self.config.quality,
            "modality": "text",
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        ImageGenCapabilities {
            provider: Some("openai-codex".to_string()),
            model: Some(self.config.tier_id.clone()),
            modalities: vec!["text".to_string()],
            max_reference_images: 0,
        }
    }
}

#[async_trait]
impl ImageGenBackend for OpenRouterCompatImageGenBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        let prompt = request.prompt.trim();
        if prompt.is_empty() {
            return Err(ToolError::InvalidParams(
                "Prompt is required and must be a non-empty string.".into(),
            ));
        }
        let token = self
            .config
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "{} image generation requires credentials. Set {} or configure image_gen.{}.api_key.",
                    self.config.provider.display_name(),
                    self.config.provider.api_key_env_vars().join("/"),
                    self.config.provider.config_key()
                ))
            })?;
        let aspect = openrouter_compat_aspect_from_tool_size(request.size.as_deref());
        let references = openrouter_compat_reference_image_parts(&request)?
            .into_iter()
            .take(OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES)
            .collect::<Vec<_>>();
        let body = openrouter_compat_chat_payload(
            self.config.model.as_str(),
            prompt,
            aspect,
            references.as_slice(),
        );
        let resp = self
            .client
            .post(self.chat_completions_url())
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .header("HTTP-Referer", OPENROUTER_COMPAT_HTTP_REFERER)
            .header("X-Title", OPENROUTER_COMPAT_X_TITLE)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation timed out ({}s)",
                        self.config.provider.display_name(),
                        OPENROUTER_COMPAT_TIMEOUT_SECS
                    ))
                } else if e.is_connect() {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation connection error: {e}",
                        self.config.provider.display_name()
                    ))
                } else {
                    ToolError::ExecutionFailed(format!(
                        "{} image generation request failed: {e}",
                        self.config.provider.display_name()
                    ))
                }
            })?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "Failed to read {} image response: {e}",
                self.config.provider.display_name()
            ))
        })?;
        if !status.is_success() {
            let message = openrouter_compat_error_message(&text);
            return Err(ToolError::ExecutionFailed(format!(
                "{} image generation failed ({status}): {message}",
                self.config.provider.display_name()
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!(
                "{} returned invalid JSON: {e}",
                self.config.provider.display_name()
            ))
        })?;
        let images = extract_openrouter_compat_images(&data);
        let first = images.first().ok_or_else(|| {
            ToolError::ExecutionFailed(format!(
                "{} returned no image. Ensure the model '{}' supports image output.",
                self.config.provider.display_name(),
                self.config.model
            ))
        })?;
        let image_path = save_openrouter_compat_generated_image(
            &self.client,
            first,
            &self.config.output_dir,
            self.config.provider.provider_id(),
        )
        .await?;
        let image = image_path.to_string_lossy().to_string();
        let modality = if request.has_image_inputs() {
            "image"
        } else {
            "text"
        };
        Ok(json!({
            "success": true,
            "image": image,
            "images": [{
                "url": image,
                "source_url": first,
                "path": image_path.to_string_lossy(),
                "width": 0,
                "height": 0,
            }],
            "provider": self.config.provider.provider_id(),
            "transport": "openrouter-compatible",
            "model": self.config.model,
            "prompt": prompt,
            "aspect_ratio": aspect,
            "modality": modality,
            "source_images": references.len(),
        })
        .to_string())
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        ImageGenCapabilities {
            provider: Some(self.config.provider.provider_id().to_string()),
            model: Some(self.config.model.clone()),
            modalities: vec!["text".to_string(), "image".to_string()],
            max_reference_images: OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES,
        }
    }
}

#[async_trait]
impl ImageGenBackend for ImageGenRuntimeBackend {
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
        match self {
            Self::Fal(backend) => backend.generate(request).await,
            Self::OpenAICodex(backend) => backend.generate(request).await,
            Self::OpenRouterCompat(backend) => backend.generate(request).await,
        }
    }

    fn capabilities(&self) -> ImageGenCapabilities {
        match self {
            Self::Fal(backend) => backend.capabilities(),
            Self::OpenAICodex(backend) => backend.capabilities(),
            Self::OpenRouterCompat(backend) => backend.capabilities(),
        }
    }
}

#[derive(Debug, Default)]
struct CodexImageAuth {
    access_token: Option<String>,
    base_url: Option<String>,
}

fn env_optional_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn resolve_fal_model_path() -> String {
    for key in ["FAL_IMAGE_MODEL", "HERMES_FAL_IMAGE_MODEL"] {
        if let Some(value) =
            env_optional_nonempty(key).and_then(|value| normalize_fal_model_path(&value))
        {
            return value;
        }
    }
    if let Some(cfg) = load_image_gen_config() {
        if let Some(fal_cfg) = yaml_get(&cfg, "fal") {
            if let Some(value) = yaml_get_str(fal_cfg, "model").and_then(normalize_fal_model_path) {
                return value;
            }
        }
        if let Some(value) = yaml_get_str(&cfg, "model").and_then(normalize_fal_model_path) {
            return value;
        }
    }
    DEFAULT_FAL_MODEL_PATH.to_string()
}

fn normalize_fal_model_path(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else if fal_model_spec(value).is_some() || value.contains('/') {
        Some(value.to_string())
    } else {
        None
    }
}

fn build_fal_text_payload(model_path: &str, request: &ImageGenerateRequest) -> Value {
    let Some(spec) = fal_model_spec(model_path) else {
        return build_legacy_fal_text_payload(request);
    };
    let mut payload = fal_model_defaults(spec.id);
    payload.insert("prompt".to_string(), json!(request.prompt.trim()));
    insert_fal_size(&mut payload, spec, spec.supports, request.size.as_deref());
    insert_common_fal_overrides(&mut payload, spec.supports, request);
    retain_supported_keys(&mut payload, spec.supports);
    Value::Object(payload)
}

fn build_fal_edit_payload(
    spec: FalModelSpec,
    request: &ImageGenerateRequest,
    source_images: &[String],
) -> Value {
    let mut payload = fal_model_defaults(spec.id);
    payload.insert("prompt".to_string(), json!(request.prompt.trim()));
    payload.insert("image_urls".to_string(), json!(source_images));
    insert_fal_size(
        &mut payload,
        spec,
        spec.edit_supports,
        request.size.as_deref(),
    );
    insert_common_fal_overrides(&mut payload, spec.edit_supports, request);
    retain_supported_keys(&mut payload, spec.edit_supports);
    Value::Object(payload)
}

fn build_legacy_fal_text_payload(request: &ImageGenerateRequest) -> Value {
    let (width, height) = match request.size.as_deref().map(str::trim) {
        Some("256x256") => (256, 256),
        Some("512x512") => (512, 512),
        _ => (1024, 1024),
    };
    json!({
        "prompt": request.prompt.trim(),
        "image_size": {
            "width": width,
            "height": height,
        },
        "num_images": request.n.unwrap_or(1),
    })
}

fn insert_fal_size(
    payload: &mut Map<String, Value>,
    spec: FalModelSpec,
    supports: &[&str],
    size: Option<&str>,
) {
    let aspect = fal_aspect_from_tool_size(size);
    let value = match aspect {
        "square" => spec.square,
        "portrait" => spec.portrait,
        _ => spec.landscape,
    };
    match spec.size_style {
        FalSizeStyle::ImageSizePreset | FalSizeStyle::GptLiteral => {
            if supports_key(supports, "image_size") {
                payload.insert("image_size".to_string(), json!(value));
            }
        }
        FalSizeStyle::AspectRatio => {
            if supports_key(supports, "aspect_ratio") {
                payload.insert("aspect_ratio".to_string(), json!(value));
            }
        }
    }
}

fn insert_common_fal_overrides(
    payload: &mut Map<String, Value>,
    supports: &[&str],
    request: &ImageGenerateRequest,
) {
    if let Some(n) = request.n {
        if supports_key(supports, "num_images") {
            payload.insert("num_images".to_string(), json!(n));
        }
    }
    if let Some(style) = request
        .style
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        if supports_key(supports, "style") {
            payload.insert("style".to_string(), json!(style));
        }
    }
}

fn fal_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("square") | Some("1:1") | Some("1024x1024") | Some("512x512") | Some("256x256") => {
            "square"
        }
        Some("portrait") | Some("9:16") | Some("1024x1536") => "portrait",
        Some("landscape") | Some("16:9") | Some("1536x1024") => "landscape",
        _ => DEFAULT_FAL_ASPECT_RATIO,
    }
}

fn retain_supported_keys(payload: &mut Map<String, Value>, supports: &[&str]) {
    payload.retain(|key, _| supports_key(supports, key));
}

fn supports_key(supports: &[&str], key: &str) -> bool {
    supports.contains(&key)
}

fn fal_model_defaults(model_path: &str) -> Map<String, Value> {
    let mut payload = Map::new();
    match model_path {
        "fal-ai/flux-2/klein/9b" => {
            payload.insert("num_inference_steps".to_string(), json!(4));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
        }
        "fal-ai/flux-2-pro" => {
            payload.insert("num_inference_steps".to_string(), json!(50));
            payload.insert("guidance_scale".to_string(), json!(4.5));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
            payload.insert("safety_tolerance".to_string(), json!("5"));
            payload.insert("sync_mode".to_string(), json!(true));
        }
        "fal-ai/z-image/turbo" => {
            payload.insert("num_inference_steps".to_string(), json!(8));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("enable_safety_checker".to_string(), json!(false));
            payload.insert("enable_prompt_expansion".to_string(), json!(false));
        }
        "fal-ai/nano-banana-pro" => {
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("safety_tolerance".to_string(), json!("5"));
            payload.insert("resolution".to_string(), json!("1K"));
        }
        "fal-ai/gpt-image-1.5" | "fal-ai/gpt-image-2" => {
            payload.insert("quality".to_string(), json!("medium"));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
        }
        "fal-ai/ideogram/v3" => {
            payload.insert("rendering_speed".to_string(), json!("BALANCED"));
            payload.insert("expand_prompt".to_string(), json!(true));
            payload.insert("style".to_string(), json!("AUTO"));
        }
        "fal-ai/recraft/v4/pro/text-to-image" => {
            payload.insert("enable_safety_checker".to_string(), json!(false));
        }
        "fal-ai/qwen-image" => {
            payload.insert("num_inference_steps".to_string(), json!(30));
            payload.insert("guidance_scale".to_string(), json!(2.5));
            payload.insert("num_images".to_string(), json!(1));
            payload.insert("output_format".to_string(), json!("png"));
            payload.insert("acceleration".to_string(), json!("regular"));
        }
        "fal-ai/krea/v2/medium/text-to-image" | "fal-ai/krea/v2/large/text-to-image" => {
            payload.insert("creativity".to_string(), json!("medium"));
        }
        _ => {}
    }
    payload
}

fn fal_model_spec(model_path: &str) -> Option<FalModelSpec> {
    match model_path.trim() {
        "fal-ai/flux-2/klein/9b" => Some(FalModelSpec {
            id: "fal-ai/flux-2/klein/9b",
            display: "FLUX 2 Klein 9B",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "seed",
                "output_format",
                "enable_safety_checker",
            ],
            edit_endpoint: Some("fal-ai/flux-2/klein/9b/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "seed",
                "output_format",
                "enable_safety_checker",
            ],
            max_reference_images: 9,
        }),
        "fal-ai/flux-2-pro" => Some(FalModelSpec {
            id: "fal-ai/flux-2-pro",
            display: "FLUX 2 Pro",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "enable_safety_checker",
                "safety_tolerance",
                "sync_mode",
                "seed",
            ],
            edit_endpoint: Some("fal-ai/flux-2-pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "enable_safety_checker",
                "safety_tolerance",
                "sync_mode",
                "seed",
            ],
            max_reference_images: 9,
        }),
        "fal-ai/z-image/turbo" => Some(FalModelSpec {
            id: "fal-ai/z-image/turbo",
            display: "Z-Image Turbo",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "num_images",
                "seed",
                "output_format",
                "enable_safety_checker",
                "enable_prompt_expansion",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/nano-banana-pro" => Some(FalModelSpec {
            id: "fal-ai/nano-banana-pro",
            display: "Nano Banana Pro (Gemini 3 Pro Image)",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "num_images",
                "output_format",
                "safety_tolerance",
                "seed",
                "sync_mode",
                "resolution",
                "enable_web_search",
                "limit_generations",
            ],
            edit_endpoint: Some("fal-ai/nano-banana-pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "aspect_ratio",
                "num_images",
                "output_format",
                "safety_tolerance",
                "seed",
                "sync_mode",
                "resolution",
                "enable_web_search",
                "limit_generations",
            ],
            max_reference_images: 2,
        }),
        "fal-ai/gpt-image-1.5" => Some(FalModelSpec {
            id: "fal-ai/gpt-image-1.5",
            display: "GPT Image 1.5",
            size_style: FalSizeStyle::GptLiteral,
            landscape: "1536x1024",
            square: "1024x1024",
            portrait: "1024x1536",
            supports: &[
                "prompt",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "background",
                "sync_mode",
            ],
            edit_endpoint: Some("fal-ai/gpt-image-1.5/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
            ],
            max_reference_images: 16,
        }),
        "fal-ai/gpt-image-2" => Some(FalModelSpec {
            id: "fal-ai/gpt-image-2",
            display: "GPT Image 2",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_4_3",
            square: "square_hd",
            portrait: "portrait_4_3",
            supports: &[
                "prompt",
                "image_size",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
            ],
            edit_endpoint: Some("openai/gpt-image-2/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "quality",
                "num_images",
                "output_format",
                "sync_mode",
                "mask_image_url",
            ],
            max_reference_images: 16,
        }),
        "fal-ai/ideogram/v3" => Some(FalModelSpec {
            id: "fal-ai/ideogram/v3",
            display: "Ideogram V3",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "rendering_speed",
                "expand_prompt",
                "style",
                "seed",
            ],
            edit_endpoint: Some("fal-ai/ideogram/v3/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "rendering_speed",
                "expand_prompt",
                "style",
                "seed",
            ],
            max_reference_images: 1,
        }),
        "fal-ai/recraft/v4/pro/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/recraft/v4/pro/text-to-image",
            display: "Recraft V4 Pro",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "enable_safety_checker",
                "colors",
                "background_color",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/qwen-image" => Some(FalModelSpec {
            id: "fal-ai/qwen-image",
            display: "Qwen Image",
            size_style: FalSizeStyle::ImageSizePreset,
            landscape: "landscape_16_9",
            square: "square_hd",
            portrait: "portrait_16_9",
            supports: &[
                "prompt",
                "image_size",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "acceleration",
                "seed",
                "sync_mode",
            ],
            edit_endpoint: Some("fal-ai/qwen-image-2/pro/edit"),
            edit_supports: &[
                "prompt",
                "image_urls",
                "num_inference_steps",
                "guidance_scale",
                "num_images",
                "output_format",
                "acceleration",
                "seed",
                "sync_mode",
            ],
            max_reference_images: 3,
        }),
        "fal-ai/krea/v2/medium/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/krea/v2/medium/text-to-image",
            display: "Krea 2 Medium",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "creativity",
                "seed",
                "image_style_references",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        "fal-ai/krea/v2/large/text-to-image" => Some(FalModelSpec {
            id: "fal-ai/krea/v2/large/text-to-image",
            display: "Krea 2 Large",
            size_style: FalSizeStyle::AspectRatio,
            landscape: "16:9",
            square: "1:1",
            portrait: "9:16",
            supports: &[
                "prompt",
                "aspect_ratio",
                "creativity",
                "seed",
                "image_style_references",
            ],
            edit_endpoint: None,
            edit_supports: &[],
            max_reference_images: 0,
        }),
        _ => None,
    }
}

fn selected_image_provider() -> Option<&'static str> {
    for key in [
        "HERMES_IMAGE_GEN_PROVIDER",
        "HERMES_IMAGE_GEN_BACKEND",
        "IMAGE_GEN_PROVIDER",
        "IMAGE_GEN_BACKEND",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if let Some(provider) = normalize_image_provider(&value) {
                return Some(provider);
            }
        }
    }
    configured_image_provider().and_then(|value| normalize_image_provider(&value))
}

fn normalize_image_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "codex" | "openai-codex" | "openai_codex" | "chatgpt" | "chatgpt-codex" => {
            Some("openai-codex")
        }
        "fal" | "fal-ai" | "fal_ai" => Some("fal"),
        "openrouter" | "open-router" | "or" => Some("openrouter"),
        "nous" | "nous-portal" | "nous_api" | "nous-api" | "nousapi" => Some("nous"),
        _ => None,
    }
}

fn configured_image_provider() -> Option<String> {
    let cfg = load_image_gen_config()?;
    for key in ["provider", "backend"] {
        if let Some(value) = yaml_get_str(&cfg, key) {
            return Some(value.to_string());
        }
    }
    None
}

fn load_image_gen_config() -> Option<serde_yaml::Value> {
    let root = load_config_yaml_root()?;
    yaml_get(&root, "image_gen").cloned()
}

fn load_config_yaml_root() -> Option<serde_yaml::Value> {
    let raw = std::fs::read_to_string(hermes_config::paths::config_path()).ok()?;
    serde_yaml::from_str(&raw).ok()
}

fn yaml_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_string()))
}

fn yaml_get_str<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a str> {
    yaml_get(value, key)?
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn yaml_get_any_str<'a>(value: &'a serde_yaml::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| yaml_get_str(value, key))
}

fn yaml_provider_section(
    root: &serde_yaml::Value,
    provider: OpenRouterCompatImageProviderKind,
) -> Option<&serde_yaml::Value> {
    let aliases: &[&str] = match provider {
        OpenRouterCompatImageProviderKind::OpenRouter => &["openrouter"],
        OpenRouterCompatImageProviderKind::Nous => &["nous", "nous-api", "nous_api", "nousapi"],
    };
    for parent in ["llm_providers", "providers"] {
        if let Some(table) = yaml_get(root, parent) {
            for alias in aliases {
                if let Some(section) = yaml_get(table, alias) {
                    return Some(section);
                }
            }
        }
    }
    None
}

fn scoped_image_provider_config(
    provider: OpenRouterCompatImageProviderKind,
) -> Option<serde_yaml::Value> {
    let cfg = load_image_gen_config()?;
    yaml_get(&cfg, provider.config_key()).cloned()
}

fn resolve_openrouter_compat_model(provider: OpenRouterCompatImageProviderKind) -> String {
    if let Some(value) = env_optional_nonempty(provider.model_env_var()) {
        return value;
    }
    scoped_image_provider_config(provider)
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL.to_string())
}

fn resolve_openrouter_compat_base_url(provider: OpenRouterCompatImageProviderKind) -> String {
    if let Some(value) = scoped_image_provider_config(provider)
        .as_ref()
        .and_then(|cfg| {
            yaml_get_any_str(cfg, &["base_url", "inference_base_url"]).map(ToOwned::to_owned)
        })
    {
        return value.trim_end_matches('/').to_string();
    }
    if let Some(root) = load_config_yaml_root() {
        if let Some(value) = yaml_provider_section(&root, provider)
            .and_then(|cfg| yaml_get_any_str(cfg, &["base_url", "inference_base_url"]))
        {
            return value.trim_end_matches('/').to_string();
        }
    }
    for key in provider.base_url_env_vars() {
        if let Some(value) = env_optional_nonempty(key) {
            return value.trim_end_matches('/').to_string();
        }
    }
    if provider == OpenRouterCompatImageProviderKind::Nous {
        if let Some(value) = read_provider_auth_string("nous", &["inference_base_url"]) {
            return value.trim_end_matches('/').to_string();
        }
    }
    provider.default_base_url().to_string()
}

fn resolve_openrouter_compat_api_key(
    provider: OpenRouterCompatImageProviderKind,
) -> Option<String> {
    if let Some(value) = scoped_image_provider_config(provider)
        .as_ref()
        .and_then(resolve_api_key_from_yaml_provider_section)
    {
        return Some(value);
    }
    if let Some(root) = load_config_yaml_root() {
        if let Some(value) = yaml_provider_section(&root, provider)
            .and_then(resolve_api_key_from_yaml_provider_section)
        {
            return Some(value);
        }
    }
    for key in provider.api_key_env_vars() {
        if let Some(value) = env_optional_nonempty(key) {
            return Some(value);
        }
    }
    if provider == OpenRouterCompatImageProviderKind::Nous {
        if let Some(value) = env_optional_nonempty("TOOL_GATEWAY_USER_TOKEN") {
            return Some(value);
        }
    }
    read_provider_auth_string(
        provider.provider_id(),
        &["agent_key", "api_key", "access_token"],
    )
    .or_else(|| read_provider_auth_tokens_string(provider.provider_id(), "access_token"))
}

fn resolve_api_key_from_yaml_provider_section(section: &serde_yaml::Value) -> Option<String> {
    if let Some(value) = yaml_get_str(section, "api_key").and_then(resolve_env_ref_or_literal) {
        return Some(value);
    }
    if let Some(env_name) = yaml_get_str(section, "api_key_env") {
        return env_optional_nonempty(env_name);
    }
    None
}

fn resolve_env_ref_or_literal(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_name) = trimmed
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        return env_optional_nonempty(env_name);
    }
    Some(trimmed.to_string())
}

fn openrouter_compat_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("landscape") | Some("16:9") | Some("1536x1024") => "16:9",
        Some("portrait") | Some("9:16") | Some("1024x1536") => "9:16",
        _ => "1:1",
    }
}

fn openrouter_compat_chat_payload(
    model: &str,
    prompt: &str,
    aspect_ratio: &str,
    reference_image_parts: &[String],
) -> Value {
    let mut content = vec![json!({"type": "text", "text": prompt})];
    content.extend(reference_image_parts.iter().map(|url| {
        json!({
            "type": "image_url",
            "image_url": {"url": url},
        })
    }));
    json!({
        "model": model,
        "modalities": ["image", "text"],
        "messages": [{
            "role": "user",
            "content": content,
        }],
        "image_config": {"aspect_ratio": aspect_ratio},
    })
}

fn openrouter_compat_reference_image_parts(
    request: &ImageGenerateRequest,
) -> Result<Vec<String>, ToolError> {
    request
        .source_image_urls()
        .into_iter()
        .take(OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES)
        .filter_map(|reference| openrouter_compat_image_url_part(reference.as_str()).transpose())
        .collect()
}

fn openrouter_compat_image_url_part(reference: &str) -> Result<Option<String>, ToolError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    if reference.starts_with("http://")
        || reference.starts_with("https://")
        || reference.starts_with("data:")
    {
        return Ok(Some(reference.to_string()));
    }
    let path = Path::new(reference);
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Could not read reference image {}: {err}",
                path.display()
            )));
        }
    };
    let mime = mime_type_for_path(path);
    Ok(Some(format!("data:{mime};base64,{}", STANDARD.encode(raw))))
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        _ => "image/png",
    }
}

fn extension_for_mime(mime: Option<&str>) -> &'static str {
    match mime
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

fn extract_openrouter_compat_images(payload: &Value) -> Vec<String> {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("message"))
        .filter_map(|message| message.get("images"))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|image| image.get("image_url"))
        .filter_map(|image_url| image_url.get("url"))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn openrouter_compat_error_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| raw.chars().take(500).collect())
}

async fn save_openrouter_compat_generated_image(
    client: &Client,
    image_url: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    if image_url.trim_start().starts_with("data:") {
        save_openrouter_compat_data_uri(image_url, output_dir, provider)
    } else {
        save_openrouter_compat_remote_image(client, image_url, output_dir, provider).await
    }
}

fn save_openrouter_compat_data_uri(
    data_uri: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    let (header, encoded) = data_uri.split_once(',').ok_or_else(|| {
        ToolError::ExecutionFailed("Generated image data URI did not contain base64 data".into())
    })?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|value| value.split_once(';').map(|(mime, _)| mime))
        .filter(|value| !value.trim().is_empty());
    let bytes = STANDARD.decode(encoded.trim()).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Generated image data URI was not valid base64: {e}"
        ))
    })?;
    write_openrouter_compat_image_bytes(
        output_dir,
        provider,
        extension_for_mime(mime),
        bytes.as_slice(),
    )
}

async fn save_openrouter_compat_remote_image(
    client: &Client,
    image_url: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    let resp = client.get(image_url).send().await.map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not download generated image {image_url}: {e}"
        ))
    })?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "Could not download generated image {image_url}: HTTP {status}"
        )));
    }
    let ext = url_extension(image_url)
        .unwrap_or_else(|| extension_for_mime(content_type.as_deref()).to_string());
    let bytes = resp.bytes().await.map_err(|e| {
        ToolError::ExecutionFailed(format!("Could not read generated image {image_url}: {e}"))
    })?;
    write_openrouter_compat_image_bytes(output_dir, provider, ext.as_str(), bytes.as_ref())
}

fn write_openrouter_compat_image_bytes(
    output_dir: &Path,
    provider: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf, ToolError> {
    std::fs::create_dir_all(output_dir).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not create image cache directory {}: {e}",
            output_dir.display()
        ))
    })?;
    let safe_provider = provider.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_");
    let safe_ext = ext
        .trim_start_matches('.')
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    let safe_ext = if safe_ext.is_empty() {
        "png"
    } else {
        safe_ext.as_str()
    };
    let path = output_dir.join(format!(
        "{}_gen_{}.{}",
        safe_provider,
        uuid::Uuid::new_v4().simple(),
        safe_ext
    ));
    std::fs::write(&path, bytes).map_err(|e| {
        ToolError::ExecutionFailed(format!("Could not save image {}: {e}", path.display()))
    })?;
    Ok(path)
}

fn url_extension(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    Path::new(parsed.path())
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| {
            matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
            )
        })
}

fn read_provider_auth_string(provider: &str, keys: &[&str]) -> Option<String> {
    provider_auth_values(provider)
        .into_iter()
        .find_map(|value| {
            keys.iter().find_map(|key| {
                value
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
}

fn read_provider_auth_tokens_string(provider: &str, key: &str) -> Option<String> {
    provider_auth_values(provider)
        .into_iter()
        .find_map(|value| {
            value
                .get("tokens")
                .and_then(|tokens| tokens.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn provider_auth_values(provider: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for path in provider_auth_candidate_paths(provider) {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(provider_value) = value
            .get("providers")
            .and_then(|providers| providers.get(provider))
            .cloned()
        {
            out.push(provider_value);
        } else if value.as_object().is_some() {
            out.push(value);
        }
    }
    out
}

fn provider_auth_candidate_paths(provider: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_optional_nonempty("HERMES_AUTH_FILE") {
        paths.push(PathBuf::from(path));
    }
    if provider == "nous" {
        if let Some(path) = env_optional_nonempty("HERMES_NOUS_OAUTH_FILE") {
            paths.push(PathBuf::from(path));
        }
        if let Some(home) = env_optional_nonempty("HOME") {
            paths.push(PathBuf::from(home).join(".hermes").join(".nous_oauth.json"));
        }
    }
    paths.push(hermes_config::paths::auth_json_path());
    dedup_paths(paths)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn resolve_codex_image_tier() -> CodexImageTier {
    if let Some(value) = env_optional_nonempty("OPENAI_IMAGE_MODEL") {
        if let Some(tier) = codex_image_tier(&value) {
            return tier;
        }
    }
    if let Some(cfg) = load_image_gen_config() {
        if let Some(openai_codex) = yaml_get(&cfg, "openai-codex") {
            if let Some(value) = yaml_get_str(openai_codex, "model") {
                if let Some(tier) = codex_image_tier(value) {
                    return tier;
                }
            }
        }
        if let Some(value) = yaml_get_str(&cfg, "model") {
            if let Some(tier) = codex_image_tier(value) {
                return tier;
            }
        }
    }
    codex_image_tier(DEFAULT_CODEX_IMAGE_MODEL).expect("default Codex image model tier")
}

fn codex_image_tier(model: &str) -> Option<CodexImageTier> {
    match model.trim() {
        "gpt-image-2-low" => Some(CodexImageTier {
            id: "gpt-image-2-low",
            quality: "low",
        }),
        "gpt-image-2-medium" => Some(CodexImageTier {
            id: "gpt-image-2-medium",
            quality: "medium",
        }),
        "gpt-image-2-high" => Some(CodexImageTier {
            id: "gpt-image-2-high",
            quality: "high",
        }),
        _ => None,
    }
}

fn codex_image_size_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim) {
        Some("1536x1024") | Some("landscape") => "1536x1024",
        Some("1024x1536") | Some("portrait") => "1024x1536",
        _ => "1024x1024",
    }
}

fn codex_image_responses_payload(
    prompt: &str,
    size: &str,
    quality: &str,
    chat_model: &str,
) -> Value {
    json!({
        "model": chat_model,
        "store": false,
        "instructions": CODEX_IMAGE_INSTRUCTIONS,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": prompt}],
        }],
        "tools": [{
            "type": "image_generation",
            "model": CODEX_IMAGE_API_MODEL,
            "size": size,
            "quality": quality,
            "output_format": "png",
            "background": "opaque",
            "partial_images": 1,
        }],
        "tool_choice": {
            "type": "allowed_tools",
            "mode": "required",
            "tools": [{"type": "image_generation"}],
        },
        "stream": true,
    })
}

fn collect_codex_image_b64_from_sse(raw: &str) -> Result<Option<String>, ToolError> {
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut latest: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            flush_codex_sse_event(&mut event_name, &mut data_lines, &mut latest)?;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    flush_codex_sse_event(&mut event_name, &mut data_lines, &mut latest)?;
    Ok(latest)
}

fn flush_codex_sse_event(
    event_name: &mut Option<String>,
    data_lines: &mut Vec<String>,
    latest: &mut Option<String>,
) -> Result<(), ToolError> {
    if data_lines.is_empty() {
        *event_name = None;
        return Ok(());
    }
    let raw = data_lines.join("\n").trim().to_string();
    let event = event_name.take();
    data_lines.clear();
    if raw.is_empty() || raw == "[DONE]" {
        return Ok(());
    }
    let mut payload: Value = serde_json::from_str(&raw).map_err(|e| {
        ToolError::ExecutionFailed(format!("Failed to parse Codex image SSE payload: {e}"))
    })?;
    if let (Some(event), Some(obj)) = (event, payload.as_object_mut()) {
        obj.entry("type".to_string())
            .or_insert(Value::String(event));
    }
    if let Some(found) = extract_codex_image_b64(&payload) {
        *latest = Some(found);
    }
    Ok(())
}

fn extract_codex_image_b64(value: &Value) -> Option<String> {
    match value {
        Value::Object(obj) => {
            let mut found = None;
            if obj.get("type").and_then(Value::as_str) == Some("image_generation_call") {
                if let Some(result) = obj
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    found = Some(result.to_string());
                }
            }
            if let Some(partial) = obj
                .get("partial_image_b64")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                found = Some(partial.to_string());
            }
            for child in obj.values() {
                if let Some(nested) = extract_codex_image_b64(child) {
                    found = Some(nested);
                }
            }
            found
        }
        Value::Array(items) => items.iter().filter_map(extract_codex_image_b64).next_back(),
        _ => None,
    }
}

fn save_codex_image_b64(
    image_b64: &str,
    output_dir: &Path,
    tier_id: &str,
) -> Result<PathBuf, ToolError> {
    std::fs::create_dir_all(output_dir).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not create Codex image cache directory {}: {e}",
            output_dir.display()
        ))
    })?;
    let encoded = image_b64
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(image_b64)
        .trim();
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        ToolError::ExecutionFailed(format!("Codex image response was not valid base64: {e}"))
    })?;
    let safe_tier = tier_id.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_");
    let path = output_dir.join(format!(
        "openai_codex_{}_{}.png",
        safe_tier,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&path, bytes).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not save Codex image {}: {e}",
            path.display()
        ))
    })?;
    Ok(path)
}

fn codex_image_auth_from_env_or_store() -> CodexImageAuth {
    let env_token = env_optional_nonempty("HERMES_OPENAI_CODEX_API_KEY")
        .or_else(|| env_optional_nonempty("OPENAI_CODEX_ACCESS_TOKEN"))
        .or_else(|| env_optional_nonempty("CODEX_ACCESS_TOKEN"));
    if let Some(access_token) = env_token {
        return CodexImageAuth {
            access_token: Some(access_token),
            base_url: env_optional_nonempty("HERMES_OPENAI_CODEX_BASE_URL")
                .or_else(|| env_optional_nonempty("OPENAI_CODEX_BASE_URL")),
        };
    }
    for path in codex_auth_store_candidate_paths() {
        if let Some(auth) = codex_image_auth_from_store_path(&path) {
            return auth;
        }
    }
    CodexImageAuth::default()
}

fn codex_auth_store_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_optional_nonempty("HERMES_AUTH_FILE") {
        paths.push(PathBuf::from(path));
    }
    paths.push(hermes_config::paths::auth_json_path());
    paths
}

fn codex_image_auth_from_store_path(path: &Path) -> Option<CodexImageAuth> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let state = parsed.get("providers")?.get("openai-codex")?;
    let token = state
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .or_else(|| state.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    let base_url = state
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Some(CodexImageAuth {
        access_token: Some(token),
        base_url,
    })
}

fn codex_cloudflare_headers(access_token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "originator".to_string(),
            CODEX_CLOUDFLARE_ORIGINATOR.to_string(),
        ),
        (
            "User-Agent".to_string(),
            format!(
                "{CODEX_CLOUDFLARE_ORIGINATOR}/{}",
                env!("CARGO_PKG_VERSION")
            ),
        ),
    ];
    if let Some(account_id) = access_token.and_then(codex_chatgpt_account_id) {
        headers.push(("ChatGPT-Account-ID".to_string(), account_id));
    }
    headers
}

fn codex_chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.trim().split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .or_else(|_| URL_SAFE.decode(payload.as_bytes()))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod fal_managed_tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    /// Hermetic env scope: HERMES_HOME → tempdir + flag/token cleared.
    struct EnvScope {
        _tmp: tempfile::TempDir,
        home: PathBuf,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().to_path_buf();
            let keys = [
                "HERMES_HOME",
                "HOME",
                "FAL_KEY",
                "FAL_IMAGE_MODEL",
                "HERMES_FAL_IMAGE_MODEL",
                "OPENAI_IMAGE_MODEL",
                "HERMES_IMAGE_GEN_PROVIDER",
                "HERMES_IMAGE_GEN_BACKEND",
                "IMAGE_GEN_PROVIDER",
                "IMAGE_GEN_BACKEND",
                "HERMES_OPENAI_CODEX_API_KEY",
                "OPENAI_CODEX_ACCESS_TOKEN",
                "CODEX_ACCESS_TOKEN",
                "HERMES_OPENAI_CODEX_BASE_URL",
                "OPENAI_CODEX_BASE_URL",
                "HERMES_CODEX_IMAGE_CHAT_MODEL",
                "OPENAI_CODEX_IMAGE_CHAT_MODEL",
                "HERMES_AUTH_FILE",
                "OPENROUTER_API_KEY",
                "OPENROUTER_IMAGE_MODEL",
                "OPENROUTER_IMAGE_BASE_URL",
                "OPENROUTER_BASE_URL",
                "NOUS_API_KEY",
                "NOUS_IMAGE_MODEL",
                "NOUS_IMAGE_BASE_URL",
                "NOUS_BASE_URL",
                "HERMES_NOUS_OAUTH_FILE",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            std::env::set_var("HERMES_HOME", &home);
            std::env::set_var("HOME", &home);
            Self {
                _tmp: tmp,
                home,
                original,
                _g: g,
            }
        }

        fn auth_path(&self) -> PathBuf {
            self.home.join("auth.json")
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

    fn image_request(prompt: &str) -> ImageGenerateRequest {
        ImageGenerateRequest {
            prompt: prompt.to_string(),
            size: None,
            style: None,
            n: None,
            image_url: None,
            reference_image_urls: Vec::new(),
        }
    }

    #[test]
    fn from_env_or_managed_prefers_direct_key() {
        let _g = EnvScope::new();
        std::env::set_var("FAL_KEY", "direct-key");
        let b = FalImageGenBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "direct");
        assert_eq!(b.model_path(), DEFAULT_FAL_MODEL_PATH);
    }

    #[test]
    fn from_env_or_managed_falls_back_to_nous_gateway() {
        let _g = EnvScope::new();
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-tok");
        let b = FalImageGenBackend::from_env_or_managed().unwrap();
        assert_eq!(b.transport_label(), "managed");
    }

    #[test]
    fn from_env_or_managed_errors_when_neither_configured() {
        let _g = EnvScope::new();
        let err = FalImageGenBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("FAL_KEY"));
        assert!(err.to_string().contains("fal-queue"));
    }

    #[test]
    fn managed_submit_url_uses_run_path() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "fal-queue".into(),
            gateway_origin: "https://fal-queue.gw.example.com".into(),
            nous_user_token: "tok".into(),
            managed_mode: true,
        };
        let b = FalImageGenBackend::from_managed(&cfg);
        assert_eq!(
            b.transport.submit_url("fal-ai/flux/dev"),
            "https://fal-queue.gw.example.com/run/fal-ai/flux/dev"
        );
        let (name, value) = b.transport.auth_header();
        assert_eq!(name, "Authorization");
        assert_eq!(value, "Bearer tok");
    }

    #[test]
    fn direct_submit_url_uses_fal_run_root() {
        let b = FalImageGenBackend::new("k".into());
        assert_eq!(
            b.transport.submit_url("fal-ai/flux/dev"),
            "https://fal.run/fal-ai/flux/dev"
        );
        let (_, value) = b.transport.auth_header();
        assert_eq!(value, "Key k");
    }

    #[test]
    fn with_model_path_overrides_default() {
        let b = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/flux-pro");
        assert_eq!(b.model_path(), "fal-ai/flux-pro");
    }

    #[test]
    fn fal_model_path_reads_env_and_config() {
        let _g = EnvScope::new();
        std::env::set_var("FAL_KEY", "direct-key");
        std::env::set_var("FAL_IMAGE_MODEL", "fal-ai/gpt-image-2");
        let b = FalImageGenBackend::from_env_or_managed().unwrap();
        assert_eq!(b.model_path(), "fal-ai/gpt-image-2");

        std::env::remove_var("FAL_IMAGE_MODEL");
        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  provider: fal\n  fal:\n    model: fal-ai/nano-banana-pro\n",
        )
        .expect("write config");
        let b = FalImageGenBackend::from_env_or_managed().unwrap();
        assert_eq!(b.model_path(), "fal-ai/nano-banana-pro");

        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  provider: fal\n  model: gpt-image-2-high\n",
        )
        .expect("write config");
        let b = FalImageGenBackend::from_env_or_managed().unwrap();
        assert_eq!(b.model_path(), DEFAULT_FAL_MODEL_PATH);
    }

    #[test]
    fn fal_text_payload_uses_catalog_endpoint_and_supported_keys() {
        let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/gpt-image-2");
        let mut request = image_request("draw launch typography");
        request.size = Some("landscape".to_string());
        request.n = Some(2);

        let prepared = backend.prepare_request(&request).unwrap();
        assert_eq!(prepared.endpoint, "fal-ai/gpt-image-2");
        assert_eq!(prepared.modality, "text");
        assert_eq!(prepared.body["prompt"], "draw launch typography");
        assert_eq!(prepared.body["image_size"], "landscape_4_3");
        assert_eq!(prepared.body["quality"], "medium");
        assert_eq!(prepared.body["num_images"], 2);
        assert!(prepared.body.get("image_urls").is_none());
    }

    #[test]
    fn fal_edit_payload_uses_edit_endpoint_and_clamps_references() {
        let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/nano-banana-pro");
        let mut request = image_request("replace the sign text");
        request.size = Some("portrait".to_string());
        request.image_url = Some("https://example.test/source.png".to_string());
        request.reference_image_urls = vec![
            "https://example.test/ref-a.png".to_string(),
            "https://example.test/ref-b.png".to_string(),
            "https://example.test/ref-c.png".to_string(),
        ];

        let prepared = backend.prepare_request(&request).unwrap();
        assert_eq!(prepared.endpoint, "fal-ai/nano-banana-pro/edit");
        assert_eq!(prepared.modality, "image");
        assert_eq!(prepared.source_image_count, 2);
        assert_eq!(prepared.body["prompt"], "replace the sign text");
        assert_eq!(prepared.body["aspect_ratio"], "9:16");
        assert_eq!(
            prepared.body["image_urls"],
            json!([
                "https://example.test/source.png",
                "https://example.test/ref-a.png"
            ])
        );
    }

    #[test]
    fn fal_text_only_model_rejects_image_inputs() {
        let backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/z-image/turbo");
        let mut request = image_request("edit source");
        request.image_url = Some("https://example.test/source.png".to_string());
        let err = backend.prepare_request(&request).unwrap_err();
        assert!(err.to_string().contains("not capable of image-to-image"));
    }

    #[test]
    fn image_capabilities_reflect_fal_edit_support() {
        let edit_backend = FalImageGenBackend::new("k".into()).with_model_path("fal-ai/flux-2-pro");
        let caps = edit_backend.capabilities();
        assert_eq!(caps.provider.as_deref(), Some("FAL.ai"));
        assert!(caps.supports_image_input());
        assert_eq!(caps.max_reference_images, 9);

        let text_backend =
            FalImageGenBackend::new("k".into()).with_model_path("fal-ai/z-image/turbo");
        let caps = text_backend.capabilities();
        assert!(!caps.supports_image_input());
        assert_eq!(caps.max_reference_images, 0);
    }

    #[test]
    fn empty_direct_key_falls_through_to_error_when_no_managed() {
        let _g = EnvScope::new();
        std::env::set_var("FAL_KEY", "  ");
        let err = FalImageGenBackend::from_env_or_managed().unwrap_err();
        assert!(err.to_string().contains("FAL_KEY"));
    }

    #[test]
    fn selected_image_provider_reads_env_and_config() {
        let _g = EnvScope::new();
        std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "codex");
        assert_eq!(selected_image_provider(), Some("openai-codex"));

        std::env::remove_var("HERMES_IMAGE_GEN_PROVIDER");
        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  provider: openai-codex\n",
        )
        .expect("write config");
        assert_eq!(selected_image_provider(), Some("openai-codex"));

        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  backend: fal\n",
        )
        .expect("write config");
        assert_eq!(selected_image_provider(), Some("fal"));

        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  provider: openrouter\n",
        )
        .expect("write config");
        assert_eq!(selected_image_provider(), Some("openrouter"));

        std::env::set_var("HERMES_IMAGE_GEN_PROVIDER", "nous-portal");
        assert_eq!(selected_image_provider(), Some("nous"));
    }

    #[test]
    fn openrouter_config_resolves_env_and_scoped_config() {
        let _g = EnvScope::new();
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-env");
        std::env::set_var("OPENROUTER_IMAGE_MODEL", "black-forest-labs/flux.2-pro");
        let cfg = OpenRouterCompatImageGenConfig::from_env_or_config(
            OpenRouterCompatImageProviderKind::OpenRouter,
        )
        .unwrap();
        assert_eq!(
            cfg.provider(),
            OpenRouterCompatImageProviderKind::OpenRouter
        );
        assert_eq!(cfg.model(), "black-forest-labs/flux.2-pro");
        assert_eq!(cfg.base_url(), DEFAULT_OPENROUTER_IMAGE_BASE_URL);

        std::env::remove_var("OPENROUTER_IMAGE_MODEL");
        std::env::remove_var("OPENROUTER_API_KEY");
        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  provider: openrouter\n  openrouter:\n    model: google/gemini-3.1-flash-image-preview\n    api_key: config-openrouter-key\n    base_url: https://or.example/v1/\n",
        )
        .expect("write config");
        let cfg = OpenRouterCompatImageGenConfig::from_env_or_config(
            OpenRouterCompatImageProviderKind::OpenRouter,
        )
        .unwrap();
        assert_eq!(cfg.model(), "google/gemini-3.1-flash-image-preview");
        assert_eq!(cfg.base_url(), "https://or.example/v1");
    }

    #[test]
    fn nous_config_reads_auth_store_agent_key_and_inference_base_url() {
        let g = EnvScope::new();
        let auth_path = g.auth_path();
        std::env::set_var("HERMES_AUTH_FILE", &auth_path);
        std::fs::write(
            &auth_path,
            r#"{
              "version": 1,
              "providers": {
                "nous": {
                  "access_token": "portal-access",
                  "agent_key": "nous-agent-key",
                  "inference_base_url": "https://inference.nousresearch.com/v1/"
                }
              }
            }"#,
        )
        .expect("write auth");

        let cfg = OpenRouterCompatImageGenConfig::from_env_or_config(
            OpenRouterCompatImageProviderKind::Nous,
        )
        .unwrap();
        assert_eq!(cfg.provider(), OpenRouterCompatImageProviderKind::Nous);
        assert_eq!(cfg.base_url(), "https://inference.nousresearch.com/v1");
        assert_eq!(cfg.api_key.as_deref(), Some("nous-agent-key"));
    }

    #[test]
    fn openrouter_reference_images_inline_local_files_and_clamp() {
        let g = EnvScope::new();
        let ref_a = g.home.join("base.png");
        let ref_b = g.home.join("ref-b.png");
        let ref_c = g.home.join("ref-c.png");
        let ref_d = g.home.join("ref-d.png");
        std::fs::write(&ref_a, b"\x89PNG\r\n").expect("write ref a");
        std::fs::write(&ref_b, b"b").expect("write ref b");
        std::fs::write(&ref_c, b"c").expect("write ref c");
        std::fs::write(&ref_d, b"d").expect("write ref d");
        let mut request = image_request("same pet sprite");
        request.image_url = Some(ref_a.to_string_lossy().to_string());
        request.reference_image_urls = vec![
            ref_b.to_string_lossy().to_string(),
            ref_c.to_string_lossy().to_string(),
            ref_d.to_string_lossy().to_string(),
        ];

        let parts = openrouter_compat_reference_image_parts(&request).unwrap();
        assert_eq!(parts.len(), OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES);
        assert!(parts[0].starts_with("data:image/png;base64,"));
        assert_eq!(
            STANDARD
                .decode(parts[0].split_once(',').unwrap().1)
                .unwrap(),
            b"\x89PNG\r\n"
        );

        let payload = openrouter_compat_chat_payload(
            DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL,
            "same pet sprite",
            openrouter_compat_aspect_from_tool_size(Some("portrait")),
            parts.as_slice(),
        );
        assert_eq!(payload["modalities"], json!(["image", "text"]));
        assert_eq!(payload["image_config"]["aspect_ratio"], "9:16");
        assert_eq!(
            payload["messages"][0]["content"][0],
            json!({"type": "text", "text": "same pet sprite"})
        );
        assert_eq!(
            payload["messages"][0]["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["type"] == "image_url")
                .count(),
            OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES
        );
    }

    #[test]
    fn openrouter_capabilities_advertise_reference_grounding() {
        let backend = OpenRouterCompatImageGenBackend::unconfigured(
            OpenRouterCompatImageProviderKind::OpenRouter,
        );
        let caps = backend.capabilities();
        assert_eq!(caps.provider.as_deref(), Some("openrouter"));
        assert_eq!(
            caps.model.as_deref(),
            Some(DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL)
        );
        assert!(caps.supports_image_input());
        assert_eq!(
            caps.max_reference_images,
            OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES
        );

        let runtime: ImageGenRuntimeBackend = backend.into();
        assert_eq!(runtime.provider_label(), "openrouter");
        assert_eq!(runtime.required_env_vars(), vec!["OPENROUTER_API_KEY"]);
    }

    #[test]
    fn codex_image_model_precedence_matches_plugin_contract() {
        let _g = EnvScope::new();
        std::fs::write(
            hermes_config::paths::config_path(),
            "image_gen:\n  model: gpt-image-2-low\n  openai-codex:\n    model: gpt-image-2-high\n",
        )
        .expect("write config");
        let tier = resolve_codex_image_tier();
        assert_eq!(tier.id, "gpt-image-2-high");
        assert_eq!(tier.quality, "high");

        std::env::set_var("OPENAI_IMAGE_MODEL", "gpt-image-2-low");
        let tier = resolve_codex_image_tier();
        assert_eq!(tier.id, "gpt-image-2-low");
        assert_eq!(tier.quality, "low");

        std::env::set_var("OPENAI_IMAGE_MODEL", "bogus");
        std::fs::write(hermes_config::paths::config_path(), "image_gen: {}\n")
            .expect("write config");
        let tier = resolve_codex_image_tier();
        assert_eq!(tier.id, DEFAULT_CODEX_IMAGE_MODEL);
        assert_eq!(tier.quality, "medium");
    }

    #[test]
    fn codex_image_auth_reads_hermes_auth_store() {
        let g = EnvScope::new();
        let auth_path = g.auth_path();
        std::env::set_var("HERMES_AUTH_FILE", &auth_path);
        std::fs::write(
            &auth_path,
            r#"{
              "active_provider": "openai-codex",
              "providers": {
                "openai-codex": {
                  "tokens": {"access_token": "codex-access-token"},
                  "base_url": "https://chatgpt.example/backend-api/codex"
                }
              }
            }"#,
        )
        .expect("write auth");

        let auth = codex_image_auth_from_env_or_store();
        assert_eq!(auth.access_token.as_deref(), Some("codex-access-token"));
        assert_eq!(
            auth.base_url.as_deref(),
            Some("https://chatgpt.example/backend-api/codex")
        );
        let backend = OpenAICodexImageGenBackend::from_env_or_auth_store().unwrap();
        assert_eq!(backend.config().tier_id(), DEFAULT_CODEX_IMAGE_MODEL);
        assert_eq!(backend.config().quality(), "medium");
        assert_eq!(
            backend.config().base_url,
            "https://chatgpt.example/backend-api/codex"
        );
    }

    #[test]
    fn codex_cloudflare_headers_extract_chatgpt_account_id() {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "acct-image-123"
                }
            }))
            .unwrap(),
        );
        let token = format!("{header}.{payload}.sig");
        let headers = codex_cloudflare_headers(Some(token.as_str()));
        assert!(headers
            .iter()
            .any(|(name, value)| name == "originator" && value == "codex_cli_rs"));
        assert!(headers
            .iter()
            .any(|(name, value)| name == "ChatGPT-Account-ID" && value == "acct-image-123"));
    }

    #[test]
    fn codex_image_sse_parser_keeps_latest_partial_or_result() {
        let raw = concat!(
            "event: response.image_generation_call.partial_image\n",
            "data: {\"partial_image_b64\":\"first\"}\n\n",
            "data: {\"output\":[{\"type\":\"image_generation_call\",\"result\":\"final\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let image = collect_codex_image_b64_from_sse(raw).unwrap();
        assert_eq!(image.as_deref(), Some("final"));
    }

    #[tokio::test]
    async fn codex_image_generate_posts_responses_and_saves_png() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _g = EnvScope::new();
        let server = MockServer::start().await;
        std::env::set_var("HERMES_OPENAI_CODEX_API_KEY", "codex-token");
        std::env::set_var("HERMES_OPENAI_CODEX_BASE_URL", server.uri());
        std::env::set_var("OPENAI_IMAGE_MODEL", "gpt-image-2-high");

        let png_b64 = STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        let sse = format!(
            "event: response.image_generation_call.completed\n\
             data: {{\"type\":\"image_generation_call\",\"result\":\"{png_b64}\"}}\n\n\
             data: [DONE]\n\n"
        );
        Mock::given(method("POST"))
            .and(path("/responses"))
            .and(header("Authorization", "Bearer codex-token"))
            .and(header("Accept", "text/event-stream"))
            .and(header("originator", "codex_cli_rs"))
            .and(body_partial_json(json!({
                "model": DEFAULT_CODEX_IMAGE_CHAT_MODEL,
                "tools": [{
                    "type": "image_generation",
                    "model": CODEX_IMAGE_API_MODEL,
                    "size": "1536x1024",
                    "quality": "high",
                    "output_format": "png",
                    "background": "opaque",
                    "partial_images": 1
                }],
                "stream": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_string(sse))
            .mount(&server)
            .await;

        let backend = OpenAICodexImageGenBackend::from_env_or_auth_store().unwrap();
        let output = backend
            .generate(ImageGenerateRequest {
                prompt: "paint a launch".to_string(),
                size: Some("landscape".to_string()),
                style: None,
                n: None,
                image_url: None,
                reference_image_urls: Vec::new(),
            })
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["success"], true);
        assert_eq!(payload["provider"], "openai-codex");
        assert_eq!(payload["model"], "gpt-image-2-high");
        assert_eq!(payload["quality"], "high");
        let image = payload["image"].as_str().expect("image path");
        assert!(image.contains("cache/images/openai_codex_gpt_image_2_high_"));
        assert_eq!(std::fs::read(image).unwrap(), b"\x89PNG\r\n\x1a\n");
    }

    #[tokio::test]
    async fn openrouter_image_generate_posts_chat_completions_and_saves_data_uri() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _g = EnvScope::new();
        let server = MockServer::start().await;
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-test");
        std::env::set_var("OPENROUTER_IMAGE_BASE_URL", server.uri());
        std::env::set_var("OPENROUTER_IMAGE_MODEL", "google/gemini-2.5-flash-image");

        let image_b64 = STANDARD.encode(b"test-image-data");
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer sk-or-test"))
            .and(header("HTTP-Referer", OPENROUTER_COMPAT_HTTP_REFERER))
            .and(header("X-Title", OPENROUTER_COMPAT_X_TITLE))
            .and(body_partial_json(json!({
                "model": "google/gemini-2.5-flash-image",
                "modalities": ["image", "text"],
                "image_config": {"aspect_ratio": "1:1"},
                "messages": [{
                    "role": "user",
                    "content": [{"type": "text", "text": "a tiny rust crab pet"}]
                }]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "content": "",
                        "images": [{
                            "type": "image_url",
                            "image_url": {"url": format!("data:image/png;base64,{image_b64}")}
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
            OpenRouterCompatImageProviderKind::OpenRouter,
        )
        .unwrap();
        let output = backend
            .generate(ImageGenerateRequest {
                prompt: "a tiny rust crab pet".to_string(),
                size: Some("square".to_string()),
                style: None,
                n: None,
                image_url: None,
                reference_image_urls: Vec::new(),
            })
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["success"], true);
        assert_eq!(payload["provider"], "openrouter");
        assert_eq!(payload["transport"], "openrouter-compatible");
        assert_eq!(payload["model"], "google/gemini-2.5-flash-image");
        assert_eq!(payload["aspect_ratio"], "1:1");
        let image = payload["image"].as_str().expect("image path");
        assert!(image.contains("cache/images/openrouter_gen_"));
        assert_eq!(std::fs::read(image).unwrap(), b"test-image-data");
    }

    #[tokio::test]
    async fn nous_image_generate_posts_to_resolved_base_url_and_downloads_remote_image() {
        use wiremock::matchers::{body_partial_json, header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _g = EnvScope::new();
        let server = MockServer::start().await;
        std::env::set_var("NOUS_API_KEY", "nous-key");
        std::env::set_var("NOUS_IMAGE_BASE_URL", server.uri());

        Mock::given(method("GET"))
            .and(path("/generated.png"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/png")
                    .set_body_bytes(b"downloaded-image"),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(header("Authorization", "Bearer nous-key"))
            .and(body_partial_json(json!({
                "model": DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL,
                "modalities": ["image", "text"],
                "image_config": {"aspect_ratio": "16:9"},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "images": [{
                            "image_url": {"url": format!("{}/generated.png", server.uri())}
                        }]
                    }
                }]
            })))
            .mount(&server)
            .await;

        let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
            OpenRouterCompatImageProviderKind::Nous,
        )
        .unwrap();
        let mut request = image_request("a portal pet");
        request.size = Some("landscape".to_string());
        let output = backend.generate(request).await.unwrap();
        let payload: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(payload["success"], true);
        assert_eq!(payload["provider"], "nous");
        assert_eq!(payload["aspect_ratio"], "16:9");
        let image = payload["image"].as_str().expect("image path");
        assert!(image.contains("cache/images/nous_gen_"));
        assert_eq!(std::fs::read(image).unwrap(), b"downloaded-image");
    }

    #[tokio::test]
    async fn openrouter_image_generate_errors_when_response_has_no_images() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _g = EnvScope::new();
        let server = MockServer::start().await;
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-test");
        std::env::set_var("OPENROUTER_IMAGE_BASE_URL", server.uri());

        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "no image"}}]
            })))
            .mount(&server)
            .await;

        let backend = OpenRouterCompatImageGenBackend::from_env_or_config(
            OpenRouterCompatImageProviderKind::OpenRouter,
        )
        .unwrap();
        let err = backend
            .generate(image_request("missing output"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("returned no image"));
    }

    #[tokio::test]
    async fn codex_image_generate_rejects_image_inputs() {
        let mut request = image_request("edit source");
        request.image_url = Some("https://example.test/source.png".to_string());
        let err = OpenAICodexImageGenBackend::unconfigured()
            .generate(request)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("text-to-image only"));
    }
}
