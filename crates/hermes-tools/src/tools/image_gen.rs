//! Image generation tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// ImageGenBackend trait
// ---------------------------------------------------------------------------

/// Backend for image generation operations.
#[async_trait]
pub trait ImageGenBackend: Send + Sync {
    /// Generate an image from a prompt.
    async fn generate(
        &self,
        prompt: &str,
        size: Option<&str>,
        style: Option<&str>,
        n: Option<u32>,
    ) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// ImageGenerateHandler
// ---------------------------------------------------------------------------

/// Tool for generating images from text prompts.
pub struct ImageGenerateHandler {
    backend: Arc<dyn ImageGenBackend>,
}

impl ImageGenerateHandler {
    pub fn new(backend: Arc<dyn ImageGenBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for ImageGenerateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let prompt = params.get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt' parameter".into()))?;

        let size = params.get("size").and_then(|v| v.as_str());
        let style = params.get("style").and_then(|v| v.as_str());
        let n = params.get("n").and_then(|v| v.as_u64()).map(|n| n as u32);

        self.backend.generate(prompt, size, style, n).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("prompt".into(), json!({
            "type": "string",
            "description": "Text description of the image to generate"
        }));
        props.insert("size".into(), json!({
            "type": "string",
            "description": "Image size: '256x256', '512x512', '1024x1024' (default: '1024x1024')",
            "enum": ["256x256", "512x512", "1024x1024"]
        }));
        props.insert("style".into(), json!({
            "type": "string",
            "description": "Image style: 'natural' or 'vivid'"
        }));
        props.insert("n".into(), json!({
            "type": "integer",
            "description": "Number of images to generate (default: 1)",
            "default": 1
        }));

        tool_schema(
            "image_generate",
            "Generate images from text descriptions using AI image generation models.",
            JsonSchema::object(props, vec!["prompt".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockImageGenBackend;
    #[async_trait]
    impl ImageGenBackend for MockImageGenBackend {
        async fn generate(&self, prompt: &str, _size: Option<&str>, _style: Option<&str>, _n: Option<u32>) -> Result<String, ToolError> {
            Ok(format!("Generated image for: {}", prompt))
        }
    }

    #[tokio::test]
    async fn test_image_generate_schema() {
        let handler = ImageGenerateHandler::new(Arc::new(MockImageGenBackend));
        assert_eq!(handler.schema().name, "image_generate");
    }

    #[tokio::test]
    async fn test_image_generate_execute() {
        let handler = ImageGenerateHandler::new(Arc::new(MockImageGenBackend));
        let result = handler.execute(json!({"prompt": "a sunset"})).await.unwrap();
        assert!(result.contains("sunset"));
    }
}