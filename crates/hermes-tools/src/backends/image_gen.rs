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
//!
//! The active transport is reflected in the response JSON (`transport`
//! field) for observability.

use async_trait::async_trait;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tools::image_gen::ImageGenBackend;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

/// Default fal.ai model when running through direct mode. Same default as
/// the Python `image_generation_tool.py`.
const DEFAULT_FAL_MODEL_PATH: &str = "fal-ai/flux/dev";
const DEFAULT_CODEX_IMAGE_MODEL: &str = "gpt-image-2-medium";
const CODEX_IMAGE_API_MODEL: &str = "gpt-image-2";
const DEFAULT_CODEX_IMAGE_CHAT_MODEL: &str = "gpt-5.5";
const DEFAULT_CODEX_IMAGE_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CODEX_IMAGE_INSTRUCTIONS: &str = "You are an assistant that must fulfill image generation requests by using the image_generation tool when provided.";
const CODEX_CLOUDFLARE_ORIGINATOR: &str = "codex_cli_rs";

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

/// Image generation backend using ChatGPT/Codex OAuth and the Responses
/// `image_generation` tool.
#[derive(Debug)]
pub struct OpenAICodexImageGenBackend {
    client: Client,
    config: OpenAICodexImageGenConfig,
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

/// Configured built-in image generation backend.
#[derive(Debug)]
pub enum ImageGenRuntimeBackend {
    Fal(FalImageGenBackend),
    OpenAICodex(OpenAICodexImageGenBackend),
}

impl ImageGenRuntimeBackend {
    pub fn from_env_or_managed() -> Self {
        match selected_image_provider() {
            Some("openai-codex") => OpenAICodexImageGenBackend::from_env_or_auth_store()
                .unwrap_or_else(|_| OpenAICodexImageGenBackend::unconfigured())
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
        }
    }

    pub fn required_env_vars(&self) -> Vec<String> {
        match self {
            Self::Fal(_) => vec!["FAL_KEY".into()],
            Self::OpenAICodex(_) => vec!["HERMES_OPENAI_CODEX_API_KEY".into()],
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

#[async_trait]
impl ImageGenBackend for FalImageGenBackend {
    async fn generate(
        &self,
        prompt: &str,
        size: Option<&str>,
        _style: Option<&str>,
        n: Option<u32>,
    ) -> Result<String, ToolError> {
        let (width, height) = match size {
            Some("256x256") => (256, 256),
            Some("512x512") => (512, 512),
            _ => (1024, 1024),
        };

        let body = json!({
            "prompt": prompt,
            "image_size": {
                "width": width,
                "height": height,
            },
            "num_images": n.unwrap_or(1),
        });

        let url = self.transport.submit_url(&self.model_path);
        let (auth_name, auth_value) = self.transport.auth_header();

        let resp = self
            .client
            .post(url)
            .header(auth_name, auth_value)
            .header("Content-Type", "application/json")
            .json(&body)
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

        Ok(json!({
            "images": images,
            "transport": self.transport.label(),
            "model": self.model_path,
        })
        .to_string())
    }
}

#[async_trait]
impl ImageGenBackend for OpenAICodexImageGenBackend {
    async fn generate(
        &self,
        prompt: &str,
        size: Option<&str>,
        _style: Option<&str>,
        _n: Option<u32>,
    ) -> Result<String, ToolError> {
        let prompt = prompt.trim();
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
        let image_size = codex_image_size_from_tool_size(size);
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
        })
        .to_string())
    }
}

#[async_trait]
impl ImageGenBackend for ImageGenRuntimeBackend {
    async fn generate(
        &self,
        prompt: &str,
        size: Option<&str>,
        style: Option<&str>,
        n: Option<u32>,
    ) -> Result<String, ToolError> {
        match self {
            Self::Fal(backend) => backend.generate(prompt, size, style, n).await,
            Self::OpenAICodex(backend) => backend.generate(prompt, size, style, n).await,
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
    let raw = std::fs::read_to_string(hermes_config::paths::config_path()).ok()?;
    let root: serde_yaml::Value = serde_yaml::from_str(&raw).ok()?;
    yaml_get(&root, "image_gen").cloned()
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
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "FAL_KEY",
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
        let _g = EnvScope::new();
        std::fs::write(
            hermes_config::paths::auth_json_path(),
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
            .generate("paint a launch", Some("landscape"), None, None)
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
}
