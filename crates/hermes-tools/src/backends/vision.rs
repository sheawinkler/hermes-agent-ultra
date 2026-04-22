//! Real vision backend: call OpenAI-compatible multimodal API.

use async_trait::async_trait;
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
        let model = std::env::var("VISION_MODEL").unwrap_or_else(|_| "gpt-4o".to_string());
        Ok(Self::new(api_key, base_url, model))
    }

    async fn encode_image_if_local(&self, image_url: &str) -> Result<Value, ToolError> {
        if image_url.starts_with("http://") || image_url.starts_with("https://") {
            Ok(json!({"type": "image_url", "image_url": {"url": image_url}}))
        } else {
            // Local file - read and base64 encode
            let data = tokio::fs::read(image_url).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("Failed to read image '{}': {}", image_url, e))
            })?;
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            let mime = if image_url.ends_with(".png") {
                "image/png"
            } else if image_url.ends_with(".gif") {
                "image/gif"
            } else if image_url.ends_with(".webp") {
                "image/webp"
            } else {
                "image/jpeg"
            };
            Ok(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{};base64,{}", mime, encoded)}
            }))
        }
    }
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

        let content = data["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("No analysis available");

        Ok(content.to_string())
    }
}
