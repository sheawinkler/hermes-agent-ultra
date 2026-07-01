use async_trait::async_trait;
use serde_json::{Value, json};

use hermes_config::MediaGenConfig;
use hermes_core::{ToolError, ToolHandler};

use crate::backends::FlowyMediaServices;
use crate::credits::estimate_workflow_credits;
use crate::platform::{default_aspect_for_platform, routing_rationale};
use crate::preview::build_prompt_preview;
use crate::video_segment::{
    parse_duration_secs_from_text, plan_segment_durations, route_long_video_template,
};
use crate::workflows::WorkflowPlan;
use crate::workflows::templates::{
    builtin_template, default_template_inputs, list_builtin_templates, suggest_template_id,
};

pub struct MediaWorkflowPlanHandler {
    media_config: MediaGenConfig,
    services: Option<FlowyMediaServices>,
}

impl MediaWorkflowPlanHandler {
    pub fn new(media_config: MediaGenConfig, services: Option<FlowyMediaServices>) -> Self {
        Self {
            media_config,
            services,
        }
    }
}

#[async_trait]
impl ToolHandler for MediaWorkflowPlanHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let objective = params
            .get("objective")
            .or_else(|| params.get("prompt"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::InvalidParams("missing 'objective' or 'prompt'".into()))?;

        let has_image = params
            .get("image_url")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.trim().is_empty());

        let platform = params
            .get("platform")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let mut template_id = params
            .get("workflow_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                suggest_template_id(
                    objective,
                    has_image,
                    &self.media_config.workflows.default_templates,
                )
            });

        let model_for_routing = params
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.media_config.video.model.clone());

        let target_duration = params
            .get("duration")
            .and_then(|v| v.as_u64())
            .map(|d| d as u32)
            .or_else(|| parse_duration_secs_from_text(objective))
            .unwrap_or(self.media_config.video.default_duration);

        template_id = route_long_video_template(&template_id, target_duration, &model_for_routing);

        let def = builtin_template(&template_id).ok_or_else(|| {
            ToolError::InvalidParams(format!(
                "unknown workflow_id '{template_id}' — available: {}",
                list_builtin_templates().join(", ")
            ))
        })?;

        let mut inputs = default_template_inputs(&template_id, objective, platform);
        if let Some(model) = params.get("model") {
            inputs["model"] = model.clone();
        }
        if let Some(url) = params.get("image_url") {
            inputs["image_url"] = url.clone();
        }
        if let Some(url) = params.get("last_frame_url") {
            inputs["last_frame_url"] = url.clone();
        }
        if let Some(duration) = params.get("duration") {
            inputs["duration"] = duration.clone();
        } else {
            inputs["duration"] = json!(target_duration);
        }
        if let Some(ratio) = params.get("aspect_ratio") {
            inputs["aspect_ratio"] = ratio.clone();
        } else if platform.is_some() {
            inputs["aspect_ratio"] = json!(default_aspect_for_platform(platform));
        }
        if let Some(resolution) = params.get("resolution") {
            inputs["resolution"] = resolution.clone();
        }
        if let Some(shots) = params.get("shots_to_render") {
            inputs["shots_to_render"] = shots.clone();
        }
        if let Some(preview_only) = params.get("preview_only") {
            inputs["preview_only"] = preview_only.clone();
        }
        if let Some(extra) = params.get("inputs").and_then(Value::as_object) {
            for (k, v) in extra {
                inputs[k] = v.clone();
            }
        }

        if (template_id == "img2video_direct" || template_id == "long_img2video_direct")
            && inputs
                .get("image_url")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(ToolError::InvalidParams(
                "img2video_direct / long_img2video_direct requires image_url — pass the user's reference image URL".into(),
            ));
        }

        if (template_id == "img2img" || template_id == "image_upscale")
            && inputs
                .get("image_url")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(ToolError::InvalidParams(
                "img2img / image_upscale requires image_url — pass the user's reference image URL"
                    .into(),
            ));
        }

        if template_id == "video_extend"
            && inputs
                .get("last_frame_url")
                .and_then(|v| v.as_str())
                .is_none_or(|s| s.trim().is_empty())
        {
            return Err(ToolError::InvalidParams(
                "video_extend requires last_frame_url from a prior clip".into(),
            ));
        }

        let credit_estimate = estimate_workflow_credits(&template_id, &inputs, &self.media_config);
        let balance = if let Some(svc) = &self.services {
            svc.credit_balance().await.ok()
        } else {
            None
        };

        let preview_requested = params
            .get("preview")
            .and_then(|v| v.as_bool())
            .unwrap_or(self.media_config.workflows.confirm_before_run);

        let prompt_preview = if preview_requested {
            Some(
                build_prompt_preview(
                    &template_id,
                    objective,
                    &inputs,
                    self.services.as_ref(),
                    &self.media_config,
                )
                .await,
            )
        } else {
            None
        };

        let plan = WorkflowPlan::from_definition(&def, inputs);
        let rationale = routing_rationale(&template_id, objective, has_image);
        let max_clip = crate::video_segment::max_clip_duration_for_model(&model_for_routing);
        let segment_plan = plan_segment_durations(target_duration, max_clip);
        if segment_plan.segment_count() > 1 {
            hermes_config::spawn_background_install(vec![hermes_config::RuntimeDep::Ffmpeg]);
        }

        let next_tool = if preview_requested {
            "media_workflow_run after user confirms the preview"
        } else {
            "media_workflow_run"
        };

        Ok(json!({
            "plan": plan,
            "workflow_id": template_id,
            "routing_rationale": rationale,
            "segment_plan": {
                "target_duration_secs": segment_plan.target_duration_secs,
                "max_clip_secs": segment_plan.max_clip_secs,
                "segment_durations": segment_plan.segment_durations,
                "segment_count": segment_plan.segment_count(),
                "requires_ffmpeg": segment_plan.segment_count() > 1,
                "ffmpeg_auto_install": segment_plan.segment_count() > 1,
            },
            "available_templates": list_builtin_templates(),
            "credits": credit_estimate.to_json(balance),
            "prompt_preview": prompt_preview,
            "next_tool": next_tool,
            "hint": if preview_requested {
                "Show prompt_preview.user_prompt_block and credits to the user; run media_workflow_run only after they confirm."
            } else {
                "Call media_workflow_run with { \"plan\": <plan above> } to execute. Prompts will be refined for rich visual detail and motion."
            },
            "suggested_next_actions": post_action_hints(&template_id),
        })
        .to_string())
    }

    fn schema(&self) -> hermes_core::ToolSchema {
        crate::tool_schemas::media_workflow_plan_schema()
    }
}

fn post_action_hints(template_id: &str) -> Vec<&'static str> {
    match template_id {
        "txt2img" | "img2img" | "image_variation" => vec![
            "image_variation — more alternate takes",
            "image_upscale — enhance resolution",
            "img2video_direct — animate the result",
        ],
        "prompt_refine_txt2video"
        | "img2video_direct"
        | "img2video"
        | "long_txt2video"
        | "long_img2video_direct"
        | "long_img2video" => vec![
            "video_extend — continue from last frame",
            "image_variation — new keyframe take",
        ],
        "storyboard_multi" => vec![
            "storyboard_multi with preview_only — show shot plan first",
            "storyboard_multi with shots_to_render — re-render selected shots",
        ],
        _ => vec!["image_variation", "video_extend", "image_upscale"],
    }
}
