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
        let api_key = std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| {
                ToolError::ExecutionFailed(
                    "HERMES_OPENAI_API_KEY (or OPENAI_API_KEY) not set".into(),
                )
            })?;
        let base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = std::env::var("AUXILIARY_VISION_MODEL")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("VISION_MODEL").ok())
            .unwrap_or_else(|| "gpt-4o".to_string());
        Ok(Self::new(api_key, base_url, model))
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
}
