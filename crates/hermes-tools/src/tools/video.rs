//! Video analysis and generation tools.
//!
//! This tool samples representative frames from a video and runs vision
//! analysis across them, returning a structured synthesis.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

/// Backend for video analysis operations.
#[async_trait]
pub trait VideoBackend: Send + Sync {
    /// Analyze a video and return a structured result payload.
    async fn analyze_video(
        &self,
        video_url: &str,
        question: &str,
        max_frames: usize,
    ) -> Result<String, ToolError>;
}

/// Parameters for text-to-video or image-to-video generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoGenerateRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub model_explicit: bool,
    pub image_url: Option<String>,
    pub reference_image_urls: Vec<String>,
    pub duration: Option<u32>,
    pub aspect_ratio: String,
    pub resolution: String,
    pub negative_prompt: Option<String>,
    pub audio: Option<bool>,
    pub seed: Option<i64>,
}

/// Backend for video generation operations.
#[async_trait]
pub trait VideoGenerateBackend: Send + Sync {
    /// Generate a video and return a structured result payload.
    async fn generate_video(&self, request: VideoGenerateRequest) -> Result<String, ToolError>;
}

/// Tool for analyzing videos using frame-sampled vision passes.
pub struct VideoAnalyzeHandler {
    backend: Arc<dyn VideoBackend>,
}

impl VideoAnalyzeHandler {
    pub fn new(backend: Arc<dyn VideoBackend>) -> Self {
        Self { backend }
    }
}

/// Tool for generating videos from text prompts, optionally guided by a
/// starting image.
pub struct VideoGenerateHandler {
    backend: Arc<dyn VideoGenerateBackend>,
}

impl VideoGenerateHandler {
    pub fn new(backend: Arc<dyn VideoGenerateBackend>) -> Self {
        Self { backend }
    }
}

fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn optional_string_list(params: &Value, key: &str) -> Vec<String> {
    params
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn optional_u32(params: &Value, key: &str) -> Option<u32> {
    params.get(key).and_then(|v| {
        v.as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u32>().ok()))
    })
}

fn optional_i64(params: &Value, key: &str) -> Option<i64> {
    params.get(key).and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_u64().and_then(|n| i64::try_from(n).ok()))
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    })
}

#[async_trait]
impl ToolHandler for VideoGenerateHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let prompt = params
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'prompt' parameter".into()))?;

        let mut reference_image_urls = optional_string_list(&params, "reference_image_urls");
        if reference_image_urls.is_empty() {
            reference_image_urls = optional_string_list(&params, "reference_images");
        }

        let model = optional_string(&params, "model");
        let request = VideoGenerateRequest {
            prompt: prompt.to_string(),
            model_explicit: model.is_some(),
            model,
            image_url: optional_string(&params, "image_url"),
            reference_image_urls,
            duration: optional_u32(&params, "duration"),
            aspect_ratio: optional_string(&params, "aspect_ratio")
                .unwrap_or_else(|| "16:9".to_string()),
            resolution: optional_string(&params, "resolution")
                .unwrap_or_else(|| "720p".to_string()),
            negative_prompt: optional_string(&params, "negative_prompt"),
            audio: params.get("audio").and_then(|v| v.as_bool()),
            seed: optional_i64(&params, "seed"),
        };

        self.backend.generate_video(request).await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "prompt".into(),
            json!({
                "type": "string",
                "description": "Text prompt for text-to-video or image-to-video generation."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Provider model/family to use. FAL families are used by default; xAI models are honored when the video backend is configured for xAI.",
                "enum": ["ltx-2.3", "pixverse-v6", "veo3.1", "seedance-2.0", "kling-v3-4k", "happy-horse", "grok-imagine-video", "grok-imagine-video-1.5-preview", "grok-imagine-video-1.5-2026-05-30"],
                "default": "pixverse-v6"
            }),
        );
        props.insert(
            "image_url".into(),
            json!({
                "type": "string",
                "description": "Optional starting image URL for image-to-video generation."
            }),
        );
        props.insert(
            "reference_image_urls".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional reference image URLs or local paths for providers that support reference-guided generation."
            }),
        );
        props.insert(
            "duration".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 15,
                "description": "Requested duration in seconds. The backend clamps or snaps to the selected family."
            }),
        );
        props.insert(
            "aspect_ratio".into(),
            json!({
                "type": "string",
                "description": "Requested output aspect ratio when the selected family supports it.",
                "enum": ["21:9", "16:9", "4:3", "1:1", "3:4", "9:16"],
                "default": "16:9"
            }),
        );
        props.insert(
            "resolution".into(),
            json!({
                "type": "string",
                "description": "Requested output resolution when the selected family supports it.",
                "enum": ["360p", "480p", "540p", "720p", "1080p"],
                "default": "720p"
            }),
        );
        props.insert(
            "negative_prompt".into(),
            json!({
                "type": "string",
                "description": "Optional negative prompt for families that support it."
            }),
        );
        props.insert(
            "audio".into(),
            json!({
                "type": "boolean",
                "description": "Whether to request generated audio when the selected family supports it."
            }),
        );
        props.insert(
            "seed".into(),
            json!({
                "type": "integer",
                "description": "Optional deterministic seed."
            }),
        );

        tool_schema(
            "video_generate",
            "Generate a video from a prompt, optionally using an image-to-video starting image.",
            JsonSchema::object(props, vec!["prompt".into()]),
        )
    }
}

#[async_trait]
impl ToolHandler for VideoAnalyzeHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let video_url = params
            .get("video_url")
            .or_else(|| params.get("video_path"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::InvalidParams(
                    "Missing 'video_url' parameter (alias: 'video_path')".into(),
                )
            })?;

        // Upstream parity guard: tolerate malformed/non-string user_prompt.
        let question = params
            .get("question")
            .and_then(|v| v.as_str())
            .or_else(|| params.get("user_prompt").and_then(|v| v.as_str()))
            .unwrap_or("Summarize this video and list key visual events.");

        let max_frames = params
            .get("max_frames")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(6)
            .clamp(1, 24);

        self.backend
            .analyze_video(video_url, question, max_frames)
            .await
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "video_url".into(),
            json!({
                "type": "string",
                "description": "Video URL or local file path to analyze."
            }),
        );
        props.insert(
            "question".into(),
            json!({
                "type": "string",
                "description": "Question to ask about the video content."
            }),
        );
        props.insert(
            "user_prompt".into(),
            json!({
                "type": ["string", "object", "array", "null"],
                "description": "Backward-compatible prompt alias. Non-string values are ignored safely."
            }),
        );
        props.insert(
            "max_frames".into(),
            json!({
                "type": "integer",
                "minimum": 1,
                "maximum": 24,
                "description": "Maximum number of sampled frames to analyze (default: 6)."
            }),
        );

        tool_schema(
            "video_analyze",
            "Analyze a video by sampling frames and running vision reasoning over each frame.",
            JsonSchema::object(props, vec!["video_url".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockVideoBackend;

    #[async_trait]
    impl VideoBackend for MockVideoBackend {
        async fn analyze_video(
            &self,
            video_url: &str,
            question: &str,
            max_frames: usize,
        ) -> Result<String, ToolError> {
            Ok(format!("{video_url}|{question}|{max_frames}"))
        }
    }

    struct MockVideoGenerateBackend;

    #[async_trait]
    impl VideoGenerateBackend for MockVideoGenerateBackend {
        async fn generate_video(&self, request: VideoGenerateRequest) -> Result<String, ToolError> {
            Ok(format!(
                "{}|{}|{}|{}|{}|{}|{}",
                request.prompt,
                request.model.unwrap_or_default(),
                request.model_explicit,
                request.image_url.unwrap_or_default(),
                request.reference_image_urls.join(","),
                request.duration.unwrap_or_default(),
                request.aspect_ratio
            ))
        }
    }

    #[tokio::test]
    async fn execute_accepts_video_path_alias_and_user_prompt_string() {
        let handler = VideoAnalyzeHandler::new(Arc::new(MockVideoBackend));
        let out = handler
            .execute(json!({
                "video_path": "/tmp/demo.mp4",
                "user_prompt": "what happens?",
                "max_frames": 5
            }))
            .await
            .expect("execute");
        assert!(out.contains("/tmp/demo.mp4"));
        assert!(out.contains("what happens?"));
        assert!(out.ends_with("|5"));
    }

    #[tokio::test]
    async fn execute_ignores_non_string_user_prompt() {
        let handler = VideoAnalyzeHandler::new(Arc::new(MockVideoBackend));
        let out = handler
            .execute(json!({
                "video_url": "/tmp/demo.mp4",
                "user_prompt": {"not": "a string"}
            }))
            .await
            .expect("execute");
        assert!(out.contains("Summarize this video and list key visual events."));
    }

    #[tokio::test]
    async fn generate_execute_normalizes_optional_params() {
        let handler = VideoGenerateHandler::new(Arc::new(MockVideoGenerateBackend));
        let out = handler
            .execute(json!({
                "prompt": " cinematic city ",
                "model": "veo3.1",
                "image_url": " https://example.com/start.png ",
                "reference_image_urls": [" https://example.com/ref.png ", "", 123],
                "duration": "8",
                "aspect_ratio": "9:16"
            }))
            .await
            .expect("execute");
        assert_eq!(
            out,
            "cinematic city|veo3.1|true|https://example.com/start.png|https://example.com/ref.png|8|9:16"
        );
    }

    #[tokio::test]
    async fn generate_execute_accepts_reference_images_alias() {
        let handler = VideoGenerateHandler::new(Arc::new(MockVideoGenerateBackend));
        let out = handler
            .execute(json!({
                "prompt": "city",
                "reference_images": ["https://example.com/a.png", " https://example.com/b.png "]
            }))
            .await
            .expect("execute");
        assert_eq!(
            out,
            "city||false||https://example.com/a.png,https://example.com/b.png|0|16:9"
        );
    }

    #[tokio::test]
    async fn generate_requires_prompt() {
        let handler = VideoGenerateHandler::new(Arc::new(MockVideoGenerateBackend));
        let err = handler.execute(json!({"prompt":"   "})).await.unwrap_err();
        assert!(err.to_string().contains("prompt"));
    }

    #[tokio::test]
    async fn generate_schema_declares_video_generate() {
        let handler = VideoGenerateHandler::new(Arc::new(MockVideoGenerateBackend));
        let schema = handler.schema();
        assert_eq!(schema.name, "video_generate");
    }
}
