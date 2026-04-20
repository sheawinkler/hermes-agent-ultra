//! Real image generation backend: fal.ai API.
//!
//! Supports two transports:
//!
//! 1. **Direct**: `FAL_KEY` env var → calls `https://fal.run/fal-ai/...`
//!    with the user's `Authorization: Key ...` header.
//! 2. **Managed**: when `FAL_KEY` is missing AND
//!    `HERMES_ENABLE_NOUS_MANAGED_TOOLS` is on with a Nous OAuth token,
//!    routes via the `fal-queue` vendor gateway with `Bearer` auth.
//!
//! The active transport is reflected in the response JSON (`transport`
//! field) for observability.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::image_gen::ImageGenBackend;
use hermes_config::managed_gateway::{
    resolve_managed_tool_gateway, ManagedToolGatewayConfig, ResolveOptions,
};
use hermes_core::ToolError;

/// Default fal.ai model when running through direct mode. Same default as
/// the Python `image_generation_tool.py`.
const DEFAULT_FAL_MODEL_PATH: &str = "fal-ai/flux/dev";

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
}
