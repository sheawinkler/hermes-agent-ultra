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
//! 5. **Krea direct or managed**: `image_gen.provider: krea` routes to Krea 2
//!    direct with `KREA_API_KEY` or to the Nous-managed Krea gateway.
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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
const DEFAULT_KREA_IMAGE_BASE_URL: &str = "https://api.krea.ai";
const DEFAULT_KREA_IMAGE_MODEL: &str = "krea-2-medium";
const DEFAULT_KREA_RESOLUTION: &str = "1K";
const DEFAULT_KREA_CREATIVITY: &str = "medium";
const DEFAULT_KREA_STYLE_REFERENCE_STRENGTH: f64 = 0.6;
const OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES: usize = 3;
const OPENROUTER_COMPAT_TIMEOUT_SECS: u64 = 300;
const OPENROUTER_COMPAT_HTTP_REFERER: &str = "https://github.com/NousResearch/hermes-agent";
const OPENROUTER_COMPAT_X_TITLE: &str = "Hermes Agent";
const KREA_MAX_REFERENCE_IMAGES: usize = 10;
const KREA_SUBMIT_TIMEOUT_SECS: u64 = 30;
const KREA_POLL_INITIAL_INTERVAL: Duration = Duration::from_secs(2);
const KREA_POLL_MAX_INTERVAL: Duration = Duration::from_secs(5);
const KREA_POLL_TIMEOUT: Duration = Duration::from_secs(180);
const KREA_POLL_BACKOFF: f64 = 1.3;
const KREA_RETRYABLE_POLL_STATUSES: &[u16] = &[408, 409, 425, 429, 500, 502, 503, 504];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KreaModelSpec {
    id: &'static str,
    display: &'static str,
    path: &'static str,
}

const KREA_MODELS: &[KreaModelSpec] = &[
    KreaModelSpec {
        id: "krea-2-medium",
        display: "Krea 2 Medium",
        path: "medium",
    },
    KreaModelSpec {
        id: "krea-2-large",
        display: "Krea 2 Large",
        path: "large",
    },
    KreaModelSpec {
        id: "krea-2-medium-turbo",
        display: "Krea 2 Medium Turbo",
        path: "medium-turbo",
    },
];

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum KreaTransport {
    Direct {
        api_key: String,
        base_url: String,
    },
    Managed {
        gateway_origin: String,
        nous_token: String,
    },
}

impl KreaTransport {
    fn label(&self) -> &'static str {
        match self {
            Self::Direct { .. } => "direct",
            Self::Managed { .. } => "managed",
        }
    }

    fn origin(&self) -> &str {
        match self {
            Self::Direct { base_url, .. } => base_url,
            Self::Managed { gateway_origin, .. } => gateway_origin,
        }
    }

    fn submit_url(&self, model: KreaModelSpec) -> String {
        format!(
            "{}/generate/image/krea/krea-2/{}",
            self.origin().trim_end_matches('/'),
            model.path
        )
    }

    fn job_url(&self, job_id: &str) -> String {
        format!("{}/jobs/{job_id}", self.origin().trim_end_matches('/'))
    }

    fn auth_token(&self) -> Option<&str> {
        match self {
            Self::Direct { api_key, .. } => Some(api_key.as_str()),
            Self::Managed { nous_token, .. } => Some(nous_token.as_str()),
        }
        .map(str::trim)
        .filter(|value| !value.is_empty())
    }

    fn is_managed(&self) -> bool {
        matches!(self, Self::Managed { .. })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct KreaImageGenConfig {
    transport: KreaTransport,
    model: String,
    creativity: String,
    output_dir: PathBuf,
    poll_initial_interval: Duration,
    poll_max_interval: Duration,
    poll_timeout: Duration,
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

/// Krea 2 image generation backend. Supports direct `KREA_API_KEY` and the
/// Nous-managed Krea gateway, then hides Krea's submit/poll job lifecycle
/// behind the synchronous `ImageGenBackend::generate` contract.
#[derive(Debug)]
pub struct KreaImageGenBackend {
    client: Client,
    config: KreaImageGenConfig,
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

impl KreaImageGenConfig {
    pub fn direct(api_key: String) -> Self {
        Self {
            transport: KreaTransport::Direct {
                api_key,
                base_url: resolve_krea_base_url(),
            },
            model: resolve_krea_model(None).id.to_string(),
            creativity: resolve_krea_creativity(None),
            output_dir: hermes_config::hermes_home().join("cache").join("images"),
            poll_initial_interval: KREA_POLL_INITIAL_INTERVAL,
            poll_max_interval: KREA_POLL_MAX_INTERVAL,
            poll_timeout: KREA_POLL_TIMEOUT,
        }
    }

    pub fn managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            transport: KreaTransport::Managed {
                gateway_origin: cfg.gateway_origin.trim_end_matches('/').to_string(),
                nous_token: cfg.nous_user_token.clone(),
            },
            model: resolve_krea_model(None).id.to_string(),
            creativity: resolve_krea_creativity(None),
            output_dir: hermes_config::hermes_home().join("cache").join("images"),
            poll_initial_interval: KREA_POLL_INITIAL_INTERVAL,
            poll_max_interval: KREA_POLL_MAX_INTERVAL,
            poll_timeout: KREA_POLL_TIMEOUT,
        }
    }

    pub fn unconfigured() -> Self {
        Self::direct(String::new())
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let candidate = model.into();
        if let Some(spec) = krea_model_spec(&candidate) {
            self.model = spec.id.to_string();
        }
        self
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        if let KreaTransport::Direct {
            base_url: current, ..
        } = &mut self.transport
        {
            let value = base_url.into();
            *current = value.trim_end_matches('/').to_string();
        }
        self
    }

    pub fn with_poll_timing(
        mut self,
        initial_interval: Duration,
        max_interval: Duration,
        timeout: Duration,
    ) -> Self {
        self.poll_initial_interval = initial_interval;
        self.poll_max_interval = max_interval;
        self.poll_timeout = timeout;
        self
    }

    pub fn transport_label(&self) -> &'static str {
        self.transport.label()
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn creativity(&self) -> &str {
        &self.creativity
    }
}

impl KreaImageGenBackend {
    pub fn new(api_key: String) -> Self {
        Self::from_config(KreaImageGenConfig::direct(api_key))
    }

    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self::from_config(KreaImageGenConfig::managed(cfg))
    }

    pub fn from_config(config: KreaImageGenConfig) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(KREA_SUBMIT_TIMEOUT_SECS))
                .build()
                .unwrap_or_else(|err| {
                    tracing::warn!("failed to build Krea image HTTP client: {}", err);
                    Client::new()
                }),
            config,
        }
    }

    pub fn from_env_or_managed() -> Result<Self, ToolError> {
        let direct_key = resolve_krea_api_key();
        if !krea_prefers_gateway() {
            if let Some(key) = direct_key.as_ref() {
                return Ok(Self::new(key.clone()));
            }
        }
        if let Some(cfg) = resolve_managed_tool_gateway("krea", ResolveOptions::default()) {
            return Ok(Self::from_managed(&cfg));
        }
        if let Some(key) = direct_key {
            return Ok(Self::new(key));
        }
        Err(ToolError::ExecutionFailed(
            "KREA_API_KEY not set and Nous-managed Krea gateway is not configured.".into(),
        ))
    }

    pub fn unconfigured() -> Self {
        Self::from_config(KreaImageGenConfig::unconfigured())
    }

    pub fn config(&self) -> &KreaImageGenConfig {
        &self.config
    }
}

/// Configured built-in image generation backend.
#[derive(Debug)]
pub enum ImageGenRuntimeBackend {
    Fal(FalImageGenBackend),
    OpenAICodex(OpenAICodexImageGenBackend),
    OpenRouterCompat(OpenRouterCompatImageGenBackend),
    Krea(KreaImageGenBackend),
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
            Some("krea") => KreaImageGenBackend::from_env_or_managed()
                .unwrap_or_else(|_| KreaImageGenBackend::unconfigured())
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
            Self::Krea(_) => "krea",
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
            Self::Krea(_) => vec!["KREA_API_KEY".into()],
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

impl From<KreaImageGenBackend> for ImageGenRuntimeBackend {
    fn from(value: KreaImageGenBackend) -> Self {
        Self::Krea(value)
    }
}

include!("image_gen/provider_generation.rs");

#[derive(Debug, Default)]
struct CodexImageAuth {
    access_token: Option<String>,
    base_url: Option<String>,
}

include!("image_gen/fal_payloads.rs");
include!("image_gen/provider_config_payloads.rs");
include!("image_gen/auth_and_save.rs");
#[cfg(test)]
mod fal_managed_tests;
