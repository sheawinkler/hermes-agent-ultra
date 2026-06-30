//! Real video backend: sample video frames with ffmpeg and analyze with vision.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use base64::Engine;
use tokio::process::Command;

use crate::tools::video::VideoBackend;
use crate::tools::vision::VisionBackend;
use hermes_core::{subprocess::CommandNoWindowExt, ToolError};

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
            if detect_video_mime_type(&path).is_none() {
                return Err(ToolError::ExecutionFailed(format!(
                    "unsupported video format: {}",
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
            .suppress_windows_console()
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

pub(crate) fn detect_video_mime_type(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|v| v.to_str())
        .map(|v| v.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp4") => Some("video/mp4"),
        Some("webm") => Some("video/webm"),
        Some("mov") => Some("video/mov"),
        Some("mpeg") | Some("mpg") => Some("video/mpeg"),
        Some("avi") | Some("mkv") => Some("video/mp4"),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) fn video_to_base64_data_url(
    path: &Path,
    mime_type: Option<&str>,
) -> Result<String, ToolError> {
    let bytes = std::fs::read(path).map_err(|e| {
        ToolError::ExecutionFailed(format!("failed to read video {}: {e}", path.display()))
    })?;
    let mime = mime_type
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| detect_video_mime_type(path))
        .unwrap_or("video/mp4");
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_video_mime_type_matches_upstream_extension_table() {
        assert_eq!(
            detect_video_mime_type(Path::new("clip.mp4")),
            Some("video/mp4")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.MP4")),
            Some("video/mp4")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.webm")),
            Some("video/webm")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.mov")),
            Some("video/mov")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.mpeg")),
            Some("video/mpeg")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.mpg")),
            Some("video/mpeg")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.avi")),
            Some("video/mp4")
        );
        assert_eq!(
            detect_video_mime_type(Path::new("clip.mkv")),
            Some("video/mp4")
        );
        assert_eq!(detect_video_mime_type(Path::new("clip.flv")), None);
    }

    #[test]
    fn video_data_url_uses_detected_or_custom_mime_type() {
        let tmp = tempfile::tempdir().unwrap();
        let mp4 = tmp.path().join("demo.mp4");
        std::fs::write(&mp4, [0_u8, 1, 2, 3]).unwrap();
        assert!(video_to_base64_data_url(&mp4, None)
            .unwrap()
            .starts_with("data:video/mp4;base64,"));

        let webm = tmp.path().join("demo.webm");
        std::fs::write(&webm, [0_u8, 1, 2, 3]).unwrap();
        assert!(video_to_base64_data_url(&webm, Some("video/webm"))
            .unwrap()
            .starts_with("data:video/webm;base64,"));
    }
}
