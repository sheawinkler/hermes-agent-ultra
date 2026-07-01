//! Agent-facing guidance for Flowy media tools (gateway system hints).

/// System hint injected when Flowy image/video tools are available.
pub fn gateway_media_system_hint(has_workflow_tools: bool) -> String {
    let mut lines = vec![
        "[SYSTEM] You have Flowy cloud media tools.".to_string(),
        "Prompt quality rules:".to_string(),
        "- Images: rich scene detail — subject, materials/textures, lighting, composition, mood; avoid vague one-liners.".to_string(),
        "- Videos: separate SCENE (detailed visuals) from MOTION (camera move + subject movement); never describe only a static still.".to_string(),
        "- Image-to-video: describe motion and changes only; do not repeat the reference image appearance.".to_string(),
        "- Mobile/chat (e.g. WeCom): prefer 9:16 for short video; desktop/demos prefer 16:9.".to_string(),
        "- Use negative_prompt for video when artifacts appear (blur, watermark, jitter).".to_string(),
        "Tool choice:".to_string(),
        "- Quick single image → `image_generate`.".to_string(),
        "- Video or higher quality → `media_workflow_plan` then `media_workflow_run` (includes prompt refinement).".to_string(),
        "- `media_workflow_plan` with `preview: true` shows refined prompts + credit estimate before generation.".to_string(),
        "- `media_workflow_run` is async by default — poll `media_workflow_status` until succeeded/failed; use `media_workflow_cancel` to abort.".to_string(),
        "- User already sent an image URL for video → workflow with `image_url` (img2video_direct); for edits → img2img.".to_string(),
        "- Multi-scene / storyboard / 分镜 / 很多场景 → `media_workflow_plan` with `storyboard_multi` (NOT `video_generate`). Never invent a storyboard table while generating a single 5s clip.".to_string(),
        "- Long video (>10s, e.g. 20s) → `media_workflow_plan` auto-selects `long_txt2video` / `long_img2video*` — splits into ~10s Seedance clips, chains last-frame, concat locally (Hermes auto-installs ffmpeg to ~/.hermes/bin when needed). Pass `duration` or write \"20秒\" in the objective.".to_string(),
        "Deliver files via MEDIA:/local_path from tool results. Do NOT redirect users to Kling, Sora, Pika, 海螺, etc.".to_string(),
        "Post-actions: image_variation, image_upscale, video_extend workflows for iterate/upscale/extend.".to_string(),
        "Always include `user_prompt_block` from tool results in your reply so the user sees the final prompt sent to the image/video API (WeCom, CLI, Telegram, etc.).".to_string(),
    ];
    if has_workflow_tools {
        lines.insert(
            7,
            "- Workflows refine prompts automatically; still pass a clear user objective."
                .to_string(),
        );
    }
    lines.join("\n")
}

/// Short prompt field description for tool JSON schemas.
pub const IMAGE_PROMPT_SCHEMA_DESC: &str = "Detailed image description: subject, style/medium, composition, lighting, textures/materials, mood. Include concrete visual specifics — not a one-line summary.";

/// Short prompt field description for video tool schema.
pub const VIDEO_PROMPT_SCHEMA_DESC: &str = "Video prompt: (1) rich scene/visual detail (2) camera motion e.g. dolly in, pan, orbit (3) subject movement. For image-to-video, focus on motion/changes only.";

pub const VIDEO_NEGATIVE_SCHEMA_DESC: &str =
    "Optional negative prompt (e.g. blurry, watermark, jitter, distorted, subtitles).";

pub const VIDEO_SEED_SCHEMA_DESC: &str =
    "Optional seed for reproducibility when the model supports it.";
