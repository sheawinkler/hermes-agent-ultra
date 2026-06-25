//! Image generation tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// ImageGenBackend trait
// ---------------------------------------------------------------------------

/// Unified image-generation request. Source-image fields are optional; when any
/// source image is present, edit-capable backends route to image-to-image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerateRequest {
    pub prompt: String,
    pub size: Option<String>,
    pub style: Option<String>,
    pub n: Option<u32>,
    pub image_url: Option<String>,
    pub reference_image_urls: Vec<String>,
}

impl ImageGenerateRequest {
    pub fn has_image_inputs(&self) -> bool {
        self.image_url.is_some() || !self.reference_image_urls.is_empty()
    }

    pub fn source_image_urls(&self) -> Vec<String> {
        let mut urls = Vec::new();
        if let Some(url) = self
            .image_url
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            urls.push(url.to_string());
        }
        urls.extend(
            self.reference_image_urls
                .iter()
                .map(String::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToOwned::to_owned),
        );
        urls
    }
}

/// Backend-reported capability metadata used to keep the tool schema honest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenCapabilities {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub modalities: Vec<String>,
    pub max_reference_images: usize,
}

impl ImageGenCapabilities {
    pub fn text_only() -> Self {
        Self {
            provider: None,
            model: None,
            modalities: vec!["text".to_string()],
            max_reference_images: 0,
        }
    }

    pub fn supports_image_input(&self) -> bool {
        self.modalities.iter().any(|m| m == "image")
    }
}

/// Backend for image generation operations.
#[async_trait]
pub trait ImageGenBackend: Send + Sync {
    /// Generate an image from text or edit source images.
    async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError>;

    fn capabilities(&self) -> ImageGenCapabilities {
        ImageGenCapabilities::text_only()
    }
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
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt' parameter".into()))?;

        let n = match params.get("n").and_then(|v| v.as_u64()) {
            Some(value) if value <= u32::MAX as u64 => Some(value as u32),
            Some(_) => {
                return Err(ToolError::InvalidParams(
                    "'n' must fit within an unsigned 32-bit integer".into(),
                ));
            }
            None => None,
        };

        let request = ImageGenerateRequest {
            prompt: prompt.to_string(),
            size: optional_string_param(&params, "size"),
            style: optional_string_param(&params, "style"),
            n,
            image_url: optional_string_param(&params, "image_url"),
            reference_image_urls: parse_reference_image_urls(&params),
        };

        self.backend.generate(request).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "prompt".into(),
            json!({
                "type": "string",
                "description": "Text description of the image to generate"
            }),
        );
        props.insert("size".into(), json!({
            "type": "string",
            "description": "Image size or aspect: 'landscape', 'square', 'portrait', '256x256', '512x512', '1024x1024', '1536x1024', or '1024x1536'.",
            "enum": ["landscape", "square", "portrait", "256x256", "512x512", "1024x1024", "1536x1024", "1024x1536"]
        }));
        props.insert(
            "style".into(),
            json!({
                "type": "string",
                "description": "Image style: 'natural' or 'vivid'"
            }),
        );
        props.insert(
            "n".into(),
            json!({
                "type": "integer",
                "description": "Number of images to generate (default: 1)",
                "default": 1
            }),
        );
        props.insert(
            "image_url".into(),
            json!({
                "type": "string",
                "description": "Optional source image URL/path to edit or transform. Omit for text-to-image."
            }),
        );
        props.insert(
            "reference_image_urls".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional additional reference image URLs/paths for image-to-image editing."
            }),
        );

        tool_schema(
            "image_generate",
            image_schema_description(&self.backend.capabilities()),
            JsonSchema::object(props, vec!["prompt".into()]),
        )
    }
}

fn optional_string_param(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_reference_image_urls(params: &Value) -> Vec<String> {
    let mut refs = Vec::new();
    for key in ["reference_image_urls", "reference_images"] {
        collect_reference_image_urls(params.get(key), &mut refs);
    }
    refs
}

fn collect_reference_image_urls(value: Option<&Value>, out: &mut Vec<String>) {
    match value {
        Some(Value::String(value)) => {
            let value = value.trim();
            if !value.is_empty() {
                out.push(value.to_string());
            }
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(value) = item.as_str().map(str::trim).filter(|v| !v.is_empty()) {
                    out.push(value.to_string());
                }
            }
        }
        _ => {}
    }
}

fn image_schema_description(caps: &ImageGenCapabilities) -> String {
    let mut parts = vec![
        "Generate high-quality images from text prompts, or edit/transform existing images when the active backend supports image inputs.".to_string(),
    ];

    let mut active = String::from("Active backend");
    if let Some(provider) = caps.provider.as_deref().filter(|v| !v.is_empty()) {
        active.push_str(": ");
        active.push_str(provider);
    }
    if let Some(model) = caps.model.as_deref().filter(|v| !v.is_empty()) {
        active.push_str(" - model: ");
        active.push_str(model);
    }
    parts.push(active);

    if caps.supports_image_input() {
        let ref_note = if caps.max_reference_images > 1 {
            format!(
                "; up to {} reference image(s) via reference_image_urls",
                caps.max_reference_images
            )
        } else {
            String::new()
        };
        parts.push(format!(
            "- supports text-to-image and image-to-image / editing; pass image_url{ref_note}"
        ));
    } else {
        parts.push(
            "- text-to-image only; do not pass image_url or reference_image_urls because they will be rejected"
                .to_string(),
        );
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct MockImageGenBackend {
        last_request: Mutex<Option<ImageGenerateRequest>>,
        capabilities: ImageGenCapabilities,
    }

    impl MockImageGenBackend {
        fn new(capabilities: ImageGenCapabilities) -> Self {
            Self {
                last_request: Mutex::new(None),
                capabilities,
            }
        }

        fn last_request(&self) -> ImageGenerateRequest {
            self.last_request.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait]
    impl ImageGenBackend for MockImageGenBackend {
        async fn generate(&self, request: ImageGenerateRequest) -> Result<String, ToolError> {
            *self.last_request.lock().unwrap() = Some(request.clone());
            Ok(format!("Generated image for: {}", request.prompt))
        }

        fn capabilities(&self) -> ImageGenCapabilities {
            self.capabilities.clone()
        }
    }

    #[tokio::test]
    async fn test_image_generate_schema() {
        let handler =
            ImageGenerateHandler::new(Arc::new(MockImageGenBackend::new(ImageGenCapabilities {
                provider: Some("FAL.ai".to_string()),
                model: Some("FLUX".to_string()),
                modalities: vec!["text".to_string(), "image".to_string()],
                max_reference_images: 9,
            })));
        assert_eq!(handler.schema().name, "image_generate");
        assert!(handler.schema().description.contains("image-to-image"));
        let schema = handler.schema();
        let props = schema.parameters.properties.as_ref().unwrap();
        assert!(props.contains_key("image_url"));
        assert!(props.contains_key("reference_image_urls"));
    }

    #[tokio::test]
    async fn test_image_generate_execute() {
        let backend = Arc::new(MockImageGenBackend::new(ImageGenCapabilities::text_only()));
        let handler = ImageGenerateHandler::new(backend.clone());
        let result = handler
            .execute(json!({
                "prompt": " a sunset ",
                "size": "landscape",
                "style": "natural",
                "n": 2,
                "image_url": " https://example.test/source.png ",
                "reference_image_urls": ["https://example.test/ref-a.png", "", 5],
                "reference_images": "https://example.test/ref-b.png"
            }))
            .await
            .unwrap();
        assert!(result.contains("sunset"));
        let request = backend.last_request();
        assert_eq!(request.prompt, "a sunset");
        assert_eq!(request.size.as_deref(), Some("landscape"));
        assert_eq!(request.style.as_deref(), Some("natural"));
        assert_eq!(request.n, Some(2));
        assert_eq!(
            request.source_image_urls(),
            vec![
                "https://example.test/source.png",
                "https://example.test/ref-a.png",
                "https://example.test/ref-b.png"
            ]
        );
    }
}
