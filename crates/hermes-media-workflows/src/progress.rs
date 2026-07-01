//! User-facing progress messages for Flowy media tools.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use hermes_core::tool_progress::report_tool_progress;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

/// Structured progress payload for gateway/UI consumers (also rendered as human text).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaProgressEvent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    pub step_no: usize,
    pub step_total: usize,
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pct: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_preview: Option<String>,
}

impl MediaProgressEvent {
    pub fn display_line(&self) -> String {
        let bar = self
            .pct
            .map(|p| format!(" [{}]", progress_bar(p)))
            .unwrap_or_default();
        if let Some(path) = &self.artifact_preview {
            format!("{}{} · 预览 MEDIA:{path}", self.message, bar)
        } else {
            format!("{}{}", self.message, bar)
        }
    }
}

fn progress_bar(pct: u8) -> String {
    let filled = (pct as usize).min(100) / 10;
    format!(
        "{}{} {pct}%",
        "█".repeat(filled),
        "░".repeat(10usize.saturating_sub(filled))
    )
}

pub fn report_media_progress(message: impl Into<String>) {
    report_tool_progress(message);
}

pub fn report_structured_media_progress(event: &MediaProgressEvent) {
    report_tool_progress(event.display_line());
}

/// Inputs for structured workflow step progress reporting.
pub struct WorkflowStepProgress<'a> {
    pub run_id: &'a str,
    pub workflow_id: &'a str,
    pub step_no: usize,
    pub step_total: usize,
    pub phase: &'a str,
    pub message: String,
    pub pct: Option<u8>,
    pub artifact_preview: Option<String>,
}

pub fn report_workflow_step_event(event: WorkflowStepProgress<'_>) {
    let payload = MediaProgressEvent {
        run_id: Some(event.run_id.to_string()),
        workflow_id: Some(event.workflow_id.to_string()),
        step_no: event.step_no,
        step_total: event.step_total,
        phase: event.phase.to_string(),
        message: event.message,
        pct: event.pct,
        artifact_preview: event.artifact_preview,
    };
    report_structured_media_progress(&payload);
}

pub fn report_intermediate_artifact(
    run_id: &str,
    workflow_id: &str,
    step_no: usize,
    step_total: usize,
    kind: &str,
    local_path: &str,
) {
    let label = match kind {
        "keyframe" | "image" => "关键帧已生成",
        "video" => "视频片段已生成",
        _ => "中间产物已就绪",
    };
    let pct = ((step_no * 100) / step_total.max(1)).min(99) as u8;
    report_workflow_step_event(WorkflowStepProgress {
        run_id,
        workflow_id,
        step_no,
        step_total,
        phase: "artifact",
        message: label.to_string(),
        pct: Some(pct),
        artifact_preview: Some(local_path.to_string()),
    });
}

/// Periodic progress while a long-running media operation blocks.
pub struct MediaProgressHeartbeat {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl MediaProgressHeartbeat {
    /// Emit `message(elapsed_secs)` every `interval_secs` until stopped.
    pub fn start(interval_secs: u64, message: impl Fn(u64) -> String + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = Arc::clone(&stop);
        let interval = Duration::from_secs(interval_secs.max(3));
        let handle = tokio::spawn(async move {
            let mut elapsed = 0u64;
            loop {
                tokio::time::sleep(interval).await;
                if stop_worker.load(Ordering::Acquire) {
                    break;
                }
                elapsed = elapsed.saturating_add(interval.as_secs());
                report_media_progress(message(elapsed));
            }
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Workflow step label with template id and position.
pub fn workflow_step_progress(
    workflow_id: &str,
    step_no: usize,
    step_total: usize,
    kind: &str,
    step_id: &str,
    medium: Option<&str>,
) -> String {
    let prefix = format!("[{workflow_id}] 步骤 {step_no}/{step_total}");
    match kind {
        "prompt_refine" => {
            let what = match medium {
                Some("video") => "视频场景与运动描述",
                Some("motion") => "图生视频运动描述",
                Some("edit") => "图片编辑描述",
                _ => "图片描述",
            };
            format!("{prefix}：正在用 AI 优化{what}（{step_id}）")
        }
        "image_generate" => {
            let label = if medium == Some("motion") || step_id.contains("keyframe") {
                "关键帧图片"
            } else {
                "图片"
            };
            format!("{prefix}：正在生成{label}（{step_id}）")
        }
        "video_generate" => format!("{prefix}：正在生成视频片段（{step_id}）"),
        "video_long_generate" => {
            format!("{prefix}：正在分段生成长视频并拼接（{step_id}）")
        }
        "storyboard_multi" => {
            format!("{prefix}：正在规划分镜并依次生成各镜头（{step_id}）")
        }
        "qa_check" => format!("{prefix}：正在检查生成质量（{step_id}）"),
        other => format!("{prefix}：正在执行 {step_id}（{other}）"),
    }
}

pub fn workflow_started(workflow_id: &str, step_total: usize) -> String {
    format!("[{workflow_id}] 工作流已开始，共 {step_total} 个步骤")
}

pub fn prompt_refine_working(medium: &str) -> &'static str {
    match medium {
        "video" => "正在用 AI 细化视频画面与镜头运动…",
        "motion" => "正在用 AI 细化图生视频的运动描述…",
        "edit" => "正在用 AI 细化图片编辑与修改描述…",
        _ => "正在用 AI 细化图片描述与画面细节…",
    }
}

pub fn storyboard_planning() -> &'static str {
    "正在用 AI 规划分镜脚本（场景 + 运动）…"
}

pub fn storyboard_shot_image(shot: usize, total: usize) -> String {
    format!("分镜 {shot}/{total}：正在生成该镜头关键帧图片…")
}

pub fn storyboard_shot_video(shot: usize, total: usize, duration_secs: u32) -> String {
    format!("分镜 {shot}/{total}：正在将该镜头转为约 {duration_secs} 秒视频…")
}

pub fn long_video_planning(target_secs: u32, segment_count: usize, max_clip_secs: u32) -> String {
    format!(
        "目标时长约 {target_secs} 秒 — Seedance 单次最多 {max_clip_secs} 秒，将拆分为 {segment_count} 段并首尾帧衔接；Hermes 会自动安装 ffmpeg 并完成拼接"
    )
}

pub fn long_video_segment(segment: usize, total: usize, clip_secs: u32) -> String {
    format!("长视频第 {segment}/{total} 段：正在生成约 {clip_secs} 秒片段…")
}

pub fn long_video_concat(segment_count: usize) -> String {
    format!("{segment_count} 段视频已生成，正在用 ffmpeg 拼接为完整成片…")
}

pub fn image_credits_check() -> &'static str {
    "正在检查图片生成积分余额…"
}

pub fn image_resolving_model() -> &'static str {
    "正在选择图片模型…"
}

pub fn image_submitting() -> &'static str {
    "正在向云端提交图片生成请求…"
}

pub fn image_waiting_upstream(elapsed_secs: u64) -> String {
    format!("正在等待云端绘图（已等待 {elapsed_secs} 秒）…")
}

pub fn image_persisting() -> &'static str {
    "图片已生成，正在下载并保存到本地…"
}

/// Opening message when `video_generate` starts.
pub fn video_generate_started(has_image: bool, duration_secs: u32) -> String {
    if has_image {
        format!("已提交图生视频任务（约 {duration_secs} 秒成片），正在连接云端…")
    } else {
        format!("已提交文生视频任务（约 {duration_secs} 秒成片），正在连接云端…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_step_labels_differ_by_medium() {
        let img = workflow_step_progress("txt2img", 1, 3, "prompt_refine", "refine", Some("image"));
        assert!(img.contains("图片描述"));
        let vid =
            workflow_step_progress("txt2video", 1, 2, "prompt_refine", "refine", Some("video"));
        assert!(vid.contains("视频场景"));
    }
}
