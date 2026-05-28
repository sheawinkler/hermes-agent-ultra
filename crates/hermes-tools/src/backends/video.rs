//! Real video backend: sample video frames with ffmpeg and analyze with vision.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tokio::process::Command;

use crate::tools::video::VideoBackend;
use crate::tools::vision::VisionBackend;
use hermes_core::ToolError;

/// Video backend that samples frames via ffmpeg and calls a vision backend.
pub struct VisionFrameSamplingVideoBackend {
    vision_backend: Arc<dyn VisionBackend>,
}

impl VisionFrameSamplingVideoBackend {
    pub fn new(vision_backend: Arc<dyn VisionBackend>) -> Self {
        Self { vision_backend }
    }

    async fn materialize_source(
        &self,
        video_url: &str,
        work_dir: &Path,
    ) -> Result<PathBuf, ToolError> {
        if video_url.starts_with("http://") || video_url.starts_with("https://") {
            let resp = reqwest::get(video_url)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("video download failed: {e}")))?;
            if !resp.status().is_success() {
                return Err(ToolError::ExecutionFailed(format!(
                    "video download failed with status {}",
                    resp.status()
                )));
            }
            let bytes = resp.bytes().await.map_err(|e| {
                ToolError::ExecutionFailed(format!("video download read failed: {e}"))
            })?;
            let path = work_dir.join("input_video.bin");
            tokio::fs::write(&path, &bytes).await.map_err(|e| {
                ToolError::ExecutionFailed(format!("failed to persist downloaded video: {e}"))
            })?;
            Ok(path)
        } else {
            let path = PathBuf::from(video_url);
            if !path.exists() {
                return Err(ToolError::ExecutionFailed(format!(
                    "video path does not exist: {}",
                    path.display()
                )));
            }
            Ok(path)
        }
    }

    async fn sample_frames(
        &self,
        input: &Path,
        frames_dir: &Path,
        max_frames: usize,
    ) -> Result<Vec<PathBuf>, ToolError> {
        // Pre-check ffmpeg availability for a user-friendly error.
        if !hermes_config::dep_check::is_available(hermes_config::RuntimeDep::Ffmpeg) {
            return Err(ToolError::ExecutionFailed(
                if cfg!(windows) {
                    "ffmpeg is not installed or not on PATH. Install with: scoop install ffmpeg"
                        .to_string()
                } else {
                    "ffmpeg is not installed or not on PATH. Install ffmpeg and ensure it is available."
                        .to_string()
                },
            ));
        }

        let pattern = frames_dir.join("frame-%03d.jpg");
        let status = Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(input)
            .arg("-vf")
            .arg("fps=1")
            .arg("-frames:v")
            .arg(max_frames.to_string())
            .arg(pattern.to_string_lossy().to_string())
            .status()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "failed to execute ffmpeg (is it installed?): {e}"
                ))
            })?;

        if !status.success() {
            return Err(ToolError::ExecutionFailed(
                "ffmpeg failed while sampling video frames".to_string(),
            ));
        }

        let mut frames: Vec<PathBuf> = std::fs::read_dir(frames_dir)
            .map_err(|e| ToolError::ExecutionFailed(format!("read frame dir failed: {e}")))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|x| x.to_str()).is_some())
            .collect();
        frames.sort();
        if frames.is_empty() {
            return Err(ToolError::ExecutionFailed(
                "no frames were extracted from video".to_string(),
            ));
        }
        Ok(frames)
    }
}

#[async_trait]
impl VideoBackend for VisionFrameSamplingVideoBackend {
    async fn analyze_video(
        &self,
        video_url: &str,
        question: &str,
        max_frames: usize,
    ) -> Result<String, ToolError> {
        let work_dir = std::env::temp_dir().join(format!(
            "hermes-video-analyze-{}",
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&work_dir).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to create temp work dir: {e}"))
        })?;
        let frames_dir = work_dir.join("frames");
        tokio::fs::create_dir_all(&frames_dir).await.map_err(|e| {
            ToolError::ExecutionFailed(format!("failed to create temp frame dir: {e}"))
        })?;

        let result = async {
            let input = self.materialize_source(video_url, &work_dir).await?;
            let frames = self.sample_frames(&input, &frames_dir, max_frames).await?;

            let mut frame_analyses = Vec::new();
            for (idx, frame) in frames.iter().enumerate() {
                let frame_q = format!(
                    "{}\n\nFrame {}/{}: describe key visual events and entities.",
                    question,
                    idx + 1,
                    frames.len()
                );
                let analysis = self
                    .vision_backend
                    .analyze(&frame.to_string_lossy(), &frame_q)
                    .await?;
                frame_analyses.push(serde_json::json!({
                    "frame_index": idx + 1,
                    "frame_path": frame,
                    "analysis": analysis
                }));
            }

            Ok::<String, ToolError>(
                serde_json::json!({
                    "video_url": video_url,
                    "question": question,
                    "frames_analyzed": frame_analyses.len(),
                    "frame_analyses": frame_analyses,
                })
                .to_string(),
            )
        }
        .await;

        // Best-effort cleanup.
        let _ = tokio::fs::remove_dir_all(&work_dir).await;
        result
    }
}
