//! Video analysis tool.
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

/// Tool for analyzing videos using frame-sampled vision passes.
pub struct VideoAnalyzeHandler {
    backend: Arc<dyn VideoBackend>,
}

impl VideoAnalyzeHandler {
    pub fn new(backend: Arc<dyn VideoBackend>) -> Self {
        Self { backend }
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
}
