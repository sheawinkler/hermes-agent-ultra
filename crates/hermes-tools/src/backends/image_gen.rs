//! Real image generation backend: fal.ai API.

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use hermes_core::ToolError;
use crate::tools::image_gen::ImageGenBackend;

/// Image generation backend using fal.ai API.
pub struct FalImageGenBackend {
    client: Client,
    api_key: String,
}

impl FalImageGenBackend {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = std::env::var("FAL_KEY")
            .map_err(|_| ToolError::ExecutionFailed("FAL_KEY environment variable not set".into()))?;
        Ok(Self::new(api_key))
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

        let resp = self.client
            .post("https://fal.run/fal-ai/flux/dev")
            .header("Authorization", format!("Key {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("fal.ai API request failed: {}", e)))?;

        let status = resp.status();
        let text = resp.text().await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read fal.ai response: {}", e)))?;

        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!("fal.ai API error ({}): {}", status, text)));
        }

        let data: Value = serde_json::from_str(&text)
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to parse fal.ai response: {}", e)))?;

        let images: Vec<Value> = data.get("images")
            .and_then(|i| i.as_array())
            .map(|arr| {
                arr.iter().map(|img| {
                    json!({
                        "url": img.get("url").and_then(|u| u.as_str()).unwrap_or(""),
                        "width": img.get("width").and_then(|w| w.as_u64()).unwrap_or(0),
                        "height": img.get("height").and_then(|h| h.as_u64()).unwrap_or(0),
                    })
                }).collect()
            })
            .unwrap_or_default();

        Ok(json!({"images": images}).to_string())
    }
}
