//! Real vision backend: call OpenAI-compatible multimodal API.

use std::net::IpAddr;
use std::path::Path;

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Value};

use crate::tools::vision::VisionBackend;
use hermes_core::ToolError;

/// Vision backend that calls an OpenAI-compatible vision endpoint.
pub struct OpenAiVisionBackend {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VisionEndpointConfig {
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
}

impl OpenAiVisionBackend {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url,
            model,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let cfg = VisionEndpointConfig::from_env()?;
        Ok(Self::new(cfg.api_key, cfg.base_url, cfg.model))
    }

    async fn encode_image_if_local(&self, image_url: &str) -> Result<Value, ToolError> {
        if image_url.starts_with("http://") || image_url.starts_with("https://") {
            if !validate_image_url_for_vision(image_url) {
                return Err(ToolError::InvalidParams(format!(
                    "Image URL is blocked by vision URL safety policy: {image_url}"
                )));
            }
            Ok(json!({"type": "image_url", "image_url": {"url": image_url}}))
        } else {
            // Local file - read and base64 encode
            let data = tokio::fs::read(image_url).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to read image '{}': {}", image_url, e))
            })?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = determine_image_mime_type(Path::new(image_url));
            Ok(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{}", mime, encoded)}
            }))
        }
    }
}

impl VisionEndpointConfig {
    fn from_env() -> Result<Self, ToolError> {
        let explicit_base_url = env_nonempty(&["AUXILIARY_VISION_BASE_URL", "VISION_BASE_URL"]);
        let provider = env_nonempty(&["AUXILIARY_VISION_PROVIDER", "VISION_PROVIDER"])
            .unwrap_or_else(|| {
                if explicit_base_url.is_some() {
                    "custom".to_string()
                } else {
                    "openai".to_string()
                }
            });
        let provider = canonical_vision_provider(&provider);
        let base_url = explicit_base_url
            .or_else(|| {
                if provider == "openai" {
                    env_nonempty(&["OPENAI_BASE_URL"])
                } else {
                    None
                }
            })
            .or_else(|| default_vision_base_url(&provider).map(str::to_string))
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "Unsupported vision provider `{provider}`; set AUXILIARY_VISION_BASE_URL for OpenAI-compatible custom endpoints"
                ))
            })?;
        let api_key = env_nonempty(&["AUXILIARY_VISION_API_KEY", "VISION_API_KEY"])
            .or_else(|| provider_vision_api_key(&provider))
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "No API key configured for vision provider `{provider}`; set AUXILIARY_VISION_API_KEY or the provider-specific key"
                ))
            })?;
        let model = env_nonempty(&["AUXILIARY_VISION_MODEL", "VISION_MODEL"])
            .or_else(|| default_vision_model(&provider).map(str::to_string))
            .ok_or_else(|| {
                ToolError::ExecutionFailed(format!(
                    "No vision model configured for provider `{provider}`; set AUXILIARY_VISION_MODEL"
                ))
            })?;
        Ok(Self {
            provider,
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
        })
    }
}

fn env_nonempty(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    })
}

fn canonical_vision_provider(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "openrouter" | "open-router" => "openrouter".to_string(),
        "nous" | "nousresearch" | "nous-research" => "nous".to_string(),
        "google" | "google-ai-studio" | "google-gemini" => "gemini".to_string(),
        "custom" | "openai-compatible" | "openai-compatible-endpoint" => "custom".to_string(),
        "x-ai" | "grok" => "xai".to_string(),
        "z-ai" | "glm" => "zai".to_string(),
        other => other.to_string(),
    }
}

fn default_vision_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("https://api.openai.com/v1"),
        "openrouter" => Some("https://openrouter.ai/api/v1"),
        "nous" => Some("https://inference-api.nousresearch.com/v1"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
        "xai" => Some("https://api.x.ai/v1"),
        "zai" => Some("https://api.z.ai/api/paas/v4"),
        "gmi" => Some("https://api.gmi-serving.com/v1"),
        "huggingface" => Some("https://router.huggingface.co/v1"),
        "custom" => None,
        _ => None,
    }
}

fn default_vision_model(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" | "custom" => Some("gpt-4o"),
        "openrouter" => Some("openai/gpt-4o"),
        "nous" => Some("openai/gpt-4o"),
        "gemini" => Some("gemini-2.5-flash"),
        "xai" => Some("grok-2-vision-1212"),
        "zai" => Some("glm-4.5v"),
        "gmi" => Some("google/gemini-2.5-flash"),
        "huggingface" => None,
        _ => None,
    }
}

fn provider_vision_api_key(provider: &str) -> Option<String> {
    let keys: &[&str] = match provider {
        "openai" | "custom" => &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "nous" => &["NOUS_API_KEY", "HERMES_MOA_API_KEY"],
        "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "zai" => &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
        "gmi" => &["GMI_API_KEY"],
        "huggingface" => &["HF_TOKEN", "HUGGINGFACE_API_KEY"],
        _ => &[],
    };
    env_nonempty(keys)
}

pub(crate) fn determine_image_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "image/jpeg",
    }
}

fn is_blocked_image_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty()
        || host == "localhost"
        || host == "metadata.google.internal"
        || host == "metadata.goog"
        || host == "metadata.internal"
    {
        return true;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => {
                let o = v4.octets();
                v4.is_loopback()
                    || v4.is_private()
                    || v4.is_link_local()
                    || v4.is_unspecified()
                    || v4.is_multicast()
                    || (o[0] == 100 && (64..=127).contains(&o[1]))
                    || (o[0] == 198 && (18..=19).contains(&o[1]))
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (v6.segments()[0] & 0xffc0) == 0xfe80
                    || (v6.segments()[0] & 0xfe00) == 0xfc00
                    || v6.to_ipv4_mapped().is_some_and(|v4| {
                        let o = v4.octets();
                        v4.is_loopback()
                            || v4.is_private()
                            || v4.is_link_local()
                            || (o[0] == 100 && (64..=127).contains(&o[1]))
                    })
            }
        };
    }
    false
}

pub(crate) fn validate_image_url_for_vision(image_url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(image_url.trim()) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    !is_blocked_image_host(host)
}

pub(crate) fn vision_response_content(data: &Value) -> String {
    data["choices"][0]["message"]["content"]
        .as_str()
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .unwrap_or("No analysis available")
        .to_string()
}

#[async_trait]
impl VisionBackend for OpenAiVisionBackend {
    async fn analyze(&self, image_url: &str, question: &str) -> Result<String, ToolError> {
        let image_content = self.encode_image_if_local(image_url).await?;

        let body = json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": question},
                    image_content,
                ]
            }],
            "max_tokens": 1024,
        });

        let resp = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Vision API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read vision response: {}", e))
        })?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Vision API error ({}): {}",
                status, text
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse vision response: {}", e))
        })?;

        Ok(vision_response_content(&data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear(keys: &[&'static str]) -> Self {
            let previous = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect::<Vec<_>>();
            for key in keys {
                std::env::remove_var(key);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn image_mime_type_matches_upstream_defaults() {
        assert_eq!(
            determine_image_mime_type(Path::new("photo.jpg")),
            "image/jpeg"
        );
        assert_eq!(
            determine_image_mime_type(Path::new("photo.jpeg")),
            "image/jpeg"
        );
        assert_eq!(
            determine_image_mime_type(Path::new("image.png")),
            "image/png"
        );
        assert_eq!(
            determine_image_mime_type(Path::new("anim.gif")),
            "image/gif"
        );
        assert_eq!(
            determine_image_mime_type(Path::new("modern.webp")),
            "image/webp"
        );
        assert_eq!(
            determine_image_mime_type(Path::new("unknown.bin")),
            "image/jpeg"
        );
    }

    #[test]
    fn image_url_validation_blocks_unsafe_schemes_and_hosts() {
        assert!(validate_image_url_for_vision(
            "https://example.com/image.jpg"
        ));
        assert!(validate_image_url_for_vision("http://cdn.example.org/pic"));
        assert!(!validate_image_url_for_vision(
            "ftp://example.com/image.jpg"
        ));
        assert!(!validate_image_url_for_vision("file:///etc/passwd"));
        assert!(!validate_image_url_for_vision(
            "http://localhost:8080/image.png"
        ));
        assert!(!validate_image_url_for_vision("http://127.0.0.1/admin"));
        assert!(!validate_image_url_for_vision(
            "http://169.254.169.254/latest"
        ));
        assert!(!validate_image_url_for_vision("http://[::1]/"));
    }

    #[test]
    fn vision_response_content_handles_null_and_empty_analysis() {
        assert_eq!(
            vision_response_content(&serde_json::json!({
                "choices": [{"message": {"content": null}}]
            })),
            "No analysis available"
        );
        assert_eq!(
            vision_response_content(&serde_json::json!({
                "choices": [{"message": {"content": "   "}}]
            })),
            "No analysis available"
        );
        assert_eq!(
            vision_response_content(&serde_json::json!({
                "choices": [{"message": {"content": "  The page shows a login form.  "}}]
            })),
            "The page shows a login form."
        );
    }

    #[test]
    fn vision_endpoint_defaults_to_legacy_openai_env() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let keys = [
            "AUXILIARY_VISION_PROVIDER",
            "AUXILIARY_VISION_BASE_URL",
            "AUXILIARY_VISION_API_KEY",
            "AUXILIARY_VISION_MODEL",
            "VISION_PROVIDER",
            "VISION_BASE_URL",
            "VISION_API_KEY",
            "VISION_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
        ];
        let _guard = EnvGuard::clear(&keys);
        std::env::set_var("HERMES_OPENAI_API_KEY", "sk-openai");
        std::env::set_var("OPENAI_BASE_URL", "https://openai.example/v1/");

        let cfg = VisionEndpointConfig::from_env().expect("vision config");

        assert_eq!(cfg.provider, "openai");
        assert_eq!(cfg.api_key, "sk-openai");
        assert_eq!(cfg.base_url, "https://openai.example/v1");
        assert_eq!(cfg.model, "gpt-4o");
    }

    #[test]
    fn vision_endpoint_accepts_any_openai_compatible_provider_model() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let keys = [
            "AUXILIARY_VISION_PROVIDER",
            "AUXILIARY_VISION_BASE_URL",
            "AUXILIARY_VISION_API_KEY",
            "AUXILIARY_VISION_MODEL",
            "OPENROUTER_API_KEY",
        ];
        let _guard = EnvGuard::clear(&keys);
        std::env::set_var("AUXILIARY_VISION_PROVIDER", "openrouter");
        std::env::set_var("AUXILIARY_VISION_MODEL", "anthropic/claude-opus-4.8");
        std::env::set_var("OPENROUTER_API_KEY", "sk-or");

        let cfg = VisionEndpointConfig::from_env().expect("vision config");

        assert_eq!(cfg.provider, "openrouter");
        assert_eq!(cfg.api_key, "sk-or");
        assert_eq!(cfg.base_url, "https://openrouter.ai/api/v1");
        assert_eq!(cfg.model, "anthropic/claude-opus-4.8");
    }

    #[test]
    fn vision_endpoint_base_url_promotes_custom_provider() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let keys = [
            "AUXILIARY_VISION_PROVIDER",
            "AUXILIARY_VISION_BASE_URL",
            "AUXILIARY_VISION_API_KEY",
            "AUXILIARY_VISION_MODEL",
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
        ];
        let _guard = EnvGuard::clear(&keys);
        std::env::set_var("AUXILIARY_VISION_BASE_URL", "https://vision.local/v1/");
        std::env::set_var("AUXILIARY_VISION_API_KEY", "sk-custom");
        std::env::set_var("AUXILIARY_VISION_MODEL", "local-vision-model");

        let cfg = VisionEndpointConfig::from_env().expect("vision config");

        assert_eq!(cfg.provider, "custom");
        assert_eq!(cfg.api_key, "sk-custom");
        assert_eq!(cfg.base_url, "https://vision.local/v1");
        assert_eq!(cfg.model, "local-vision-model");
    }
}
