//! DAG workflow executor.

use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use hermes_core::ToolError;

use super::control::WorkflowRunControl;
use super::definition::{WorkflowDefinition, WorkflowPlan, WorkflowStep};
use super::store::{WorkflowRunRecord, WorkflowRunStatus, WorkflowRunStore};
use crate::backends::FlowyMediaServices;
use crate::backends::flowy_video::FlowyVideoGenBackend;
use crate::backends::traits::{FlowyMediaBackend, MediaGenerationBackend, MediaImageRequest};
use crate::llm_refine::{plan_storyboard, refine_with_llm_or_template};
use crate::progress::{
    WorkflowStepProgress, long_video_concat, long_video_planning, long_video_segment,
    prompt_refine_working, report_intermediate_artifact, report_media_progress,
    report_workflow_step_event, storyboard_planning, storyboard_shot_image, storyboard_shot_video,
    workflow_started, workflow_step_progress,
};
use crate::prompt_refine::RefineInput;
use crate::qa::{qa_check_image, qa_check_video};
use crate::video_segment::{
    concat_videos, extract_last_frame_png, persist_concatenated_video, plan_segment_durations,
    png_file_to_data_url, segment_video_prompt,
};

pub struct WorkflowExecutor {
    pub(crate) services: FlowyMediaServices,
    media_backend: Arc<dyn MediaGenerationBackend>,
    flowy_video: Arc<FlowyVideoGenBackend>,
    store: Arc<WorkflowRunStore>,
    control: WorkflowRunControl,
    max_retries: u32,
}

impl WorkflowExecutor {
    pub fn new(
        services: FlowyMediaServices,
        store: Arc<WorkflowRunStore>,
        control: WorkflowRunControl,
        max_retries: u32,
    ) -> Self {
        let image_backend = Arc::new(crate::backends::flowy_image::FlowyImageGenBackend::new(
            services.clone(),
        ));
        let flowy_video = Arc::new(FlowyVideoGenBackend::new(services.clone()));
        let media_backend = Arc::new(FlowyMediaBackend::new(
            image_backend,
            video_backend_trait(Arc::clone(&flowy_video)),
        ));
        Self {
            services,
            media_backend,
            flowy_video,
            store,
            control,
            max_retries: max_retries.clamp(1, 5),
        }
    }

    pub fn control(&self) -> &WorkflowRunControl {
        &self.control
    }

    pub async fn run_plan(&self, plan: &WorkflowPlan) -> Result<WorkflowRunRecord, ToolError> {
        let def = WorkflowDefinition {
            id: plan.workflow_id.clone(),
            version: plan.template_version,
            description: String::new(),
            inputs: plan.inputs.clone(),
            steps: plan.steps.clone(),
        };
        self.run_definition(&def).await
    }

    pub async fn run_definition(
        &self,
        def: &WorkflowDefinition,
    ) -> Result<WorkflowRunRecord, ToolError> {
        let record = self.store.create_run(&def.id, def.inputs.clone());
        self.run_definition_existing(&record.run_id, def).await
    }

    pub async fn run_definition_existing(
        &self,
        run_id: &str,
        def: &WorkflowDefinition,
    ) -> Result<WorkflowRunRecord, ToolError> {
        let Some(mut record) = self.store.get(run_id) else {
            return Err(ToolError::ExecutionFailed(format!(
                "workflow run not found: {run_id}"
            )));
        };
        record.status = WorkflowRunStatus::Running;
        self.store.save(&record);

        record.status = WorkflowRunStatus::Running;
        self.store.save(&record);

        let order = topo_sort(&def.steps)?;
        let step_total = order.len();
        report_media_progress(workflow_started(&def.id, step_total));

        let mut ctx: HashMap<String, Value> = HashMap::new();
        ctx.insert("inputs".into(), def.inputs.clone());

        let mut step_idx = 0usize;
        let mut workflow_retries = 0u32;
        const MAX_WORKFLOW_RESTARTS: u32 = 2;

        while step_idx < order.len() {
            if self.control.is_cancelled(run_id) {
                record.status = WorkflowRunStatus::Cancelled;
                record.error = Some("workflow cancelled by user".into());
                record.current_step = None;
                self.store.save(&record);
                return Err(ToolError::ExecutionFailed("workflow cancelled".into()));
            }

            let step_id = order[step_idx].clone();
            let step = def
                .steps
                .iter()
                .find(|s| s.id == step_id)
                .ok_or_else(|| ToolError::ExecutionFailed(format!("missing step {step_id}")))?;

            record.current_step = Some(step_id.clone());
            self.store.save(&record);

            let resolved_input = resolve_value(&step.input, &ctx);
            let medium = resolved_input.get("medium").and_then(|v| v.as_str());
            let pct = Some(((step_idx + 1) * 100 / step_total.max(1)).min(99) as u8);
            report_workflow_step_event(WorkflowStepProgress {
                run_id,
                workflow_id: &def.id,
                step_no: step_idx + 1,
                step_total,
                phase: &step.kind,
                message: workflow_step_progress(
                    &def.id,
                    step_idx + 1,
                    step_total,
                    &step.kind,
                    &step_id,
                    medium,
                ),
                pct,
                artifact_preview: None,
            });

            let output = match self
                .run_step_with_retry(
                    run_id,
                    &def.id,
                    step_idx + 1,
                    step_total,
                    step,
                    &resolved_input,
                )
                .await
            {
                Ok(output) => output,
                Err(err) => {
                    if let Some(retry_from) =
                        step.on_fail.as_ref().and_then(|a| a.retry_from.clone())
                        && let Some(restart_idx) = order.iter().position(|s| s == &retry_from)
                        && workflow_retries < MAX_WORKFLOW_RESTARTS
                    {
                        workflow_retries += 1;
                        tracing::warn!(
                            step = %step_id,
                            retry_from = %retry_from,
                            attempt = workflow_retries,
                            "workflow restarting from earlier step after failure"
                        );
                        clear_outputs_from(&mut ctx, &mut record, &order, restart_idx);
                        step_idx = restart_idx;
                        continue;
                    }
                    record.status = WorkflowRunStatus::Failed;
                    record.error = Some(err.to_string());
                    record.current_step = None;
                    self.store.save(&record);
                    return Err(err);
                }
            };

            if step.kind == "image_generate"
                && let Some(path) = local_path_from_step_output(&output)
            {
                let kind = if step_id.contains("keyframe") {
                    "keyframe"
                } else {
                    "image"
                };
                report_intermediate_artifact(
                    run_id,
                    &def.id,
                    step_idx + 1,
                    step_total,
                    kind,
                    &path,
                );
            }

            ctx.insert(format!("steps.{step_id}"), output.clone());
            record.step_outputs.insert(step_id.clone(), output);
            self.store.save(&record);
            step_idx += 1;
        }

        record.status = WorkflowRunStatus::Succeeded;
        record.current_step = None;
        record.artifacts = collect_artifacts(&record.step_outputs);
        self.store.save(&record);
        Ok(record)
    }

    async fn run_step_with_retry(
        &self,
        run_id: &str,
        workflow_id: &str,
        step_no: usize,
        step_total: usize,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let mut last_err = None;
        for attempt in 0..self.max_retries {
            if self.control.is_cancelled(run_id) {
                return Err(ToolError::ExecutionFailed("workflow cancelled".into()));
            }
            match self
                .run_step(run_id, workflow_id, step_no, step_total, step, input)
                .await
            {
                Ok(v) => return Ok(v),
                Err(err) => {
                    let retryable = is_retryable_error(&err);
                    tracing::warn!(
                        step = %step.id,
                        attempt = attempt + 1,
                        retryable,
                        error = %err,
                        "workflow step failed"
                    );
                    last_err = Some(err);
                    if !retryable || attempt + 1 >= self.max_retries {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2_u64.pow(attempt))).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            ToolError::ExecutionFailed("workflow step failed without error".into())
        }))
    }

    async fn run_step(
        &self,
        run_id: &str,
        workflow_id: &str,
        step_no: usize,
        step_total: usize,
        step: &WorkflowStep,
        input: &Value,
    ) -> Result<Value, ToolError> {
        match step.kind.as_str() {
            "image_generate" => self.run_image_step(input).await,
            "video_generate" => self.run_video_step(run_id, input).await,
            "video_long_generate" => {
                self.run_long_video_step(run_id, workflow_id, step_no, step_total, input)
                    .await
            }
            "prompt_refine" => self.run_prompt_refine(input).await,
            "storyboard_multi" => {
                self.run_storyboard_multi(run_id, workflow_id, step_no, step_total, input)
                    .await
            }
            "qa_check" => self.run_qa_check(input).await,
            other => Err(ToolError::ExecutionFailed(format!(
                "unsupported workflow step kind: {other}"
            ))),
        }
    }

    async fn run_image_step(&self, input: &Value) -> Result<Value, ToolError> {
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("image step missing prompt".into()))?;
        let raw = self
            .media_backend
            .generate_image(MediaImageRequest {
                prompt: prompt.to_string(),
                model: input
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                image_url: input
                    .get("image_url")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            })
            .await?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| ToolError::ExecutionFailed(format!("image step JSON: {e}")))?;
        let best_url = parsed
            .get("assets")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|a| a.get("url").or_else(|| a.get("local_path")))
            .or_else(|| {
                parsed
                    .get("images")
                    .and_then(|v| v.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|img| img.get("url").or_else(|| img.get("local_path")))
            })
            .cloned()
            .unwrap_or(Value::Null);
        Ok(json!({
            "raw": parsed,
            "api_prompt": prompt,
            "negative_prompt": input.get("negative_prompt").and_then(|v| v.as_str()),
            "best_url": best_url,
            "output": parsed.get("assets").or_else(|| parsed.get("images")).cloned().unwrap_or(Value::Null),
        }))
    }

    async fn run_video_step(&self, run_id: &str, input: &Value) -> Result<Value, ToolError> {
        use hermes_tools::tools::video::VideoGenerateRequest;

        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("video step missing prompt".into()))?;
        let reference_image_urls: Vec<String> = input
            .get("reference_image_urls")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let request = VideoGenerateRequest {
            prompt: prompt.to_string(),
            model: input
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            model_explicit: input.get("model").is_some(),
            image_url: input
                .get("image_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reference_image_urls,
            duration: input
                .get("duration")
                .and_then(|v| v.as_u64())
                .map(|d| d as u32),
            aspect_ratio: input
                .get("aspect_ratio")
                .and_then(|v| v.as_str())
                .unwrap_or("16:9")
                .to_string(),
            resolution: input
                .get("resolution")
                .and_then(|v| v.as_str())
                .unwrap_or("720p")
                .to_string(),
            negative_prompt: input
                .get("negative_prompt")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            seed: input.get("seed").and_then(|v| v.as_i64()),
            last_frame_url: input
                .get("last_frame_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reference_video_url: input
                .get("reference_video_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            reference_audio_url: input
                .get("reference_audio_url")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            generate_audio: input.get("generate_audio").and_then(|v| v.as_bool()),
            audio: None,
        };

        let raw = self
            .flowy_video
            .generate_for_workflow(request, Some(run_id), Some(&self.control))
            .await?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|e| ToolError::ExecutionFailed(format!("video step JSON: {e}")))?;
        Ok(json!({
            "raw": parsed,
            "api_prompt": prompt,
            "negative_prompt": input.get("negative_prompt").and_then(|v| v.as_str()),
            "motion_prompt": input.get("motion_prompt").and_then(|v| v.as_str()),
            "video_url": parsed.get("video"),
            "local_path": parsed.pointer("/assets/0/local_path").or_else(|| parsed.get("local_path")),
            "output": parsed,
        }))
    }

    async fn run_long_video_step(
        &self,
        run_id: &str,
        workflow_id: &str,
        step_no: usize,
        step_total: usize,
        input: &Value,
    ) -> Result<Value, ToolError> {
        use std::path::PathBuf;

        use crate::delivery::{MediaProvenance, VideoTaskMeta, video_generation_response};

        let base_prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("long video step missing prompt".into()))?;
        let target_duration = input
            .get("duration")
            .and_then(|v| v.as_u64())
            .map(|d| d as u32)
            .filter(|d| *d > 0)
            .unwrap_or(self.services.media.video.default_duration);

        let model = self
            .services
            .resolve_video_model(input.get("model").and_then(|v| v.as_str()))
            .await?;
        let max_clip = crate::video_segment::max_clip_duration_for_model(&model);
        let plan = plan_segment_durations(target_duration, max_clip);
        let segment_total = plan.segment_count();

        report_media_progress(long_video_planning(
            plan.target_duration_secs,
            segment_total,
            max_clip,
        ));

        if segment_total > 1 {
            crate::video_segment::ensure_ffmpeg_ready().await?;
        }

        let work_dir = hermes_config::hermes_home()
            .join("media")
            .join("segments")
            .join(run_id);
        tokio::fs::create_dir_all(&work_dir)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("create segment work dir: {e}")))?;

        let mut segment_paths: Vec<PathBuf> = Vec::with_capacity(segment_total);
        let mut segment_artifacts = Vec::with_capacity(segment_total);
        let mut chain_image_url = input
            .get("image_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);

        for (idx, &clip_secs) in plan.segment_durations.iter().enumerate() {
            if self.control.is_cancelled(run_id) {
                return Err(ToolError::ExecutionFailed("workflow cancelled".into()));
            }

            report_workflow_step_event(WorkflowStepProgress {
                run_id,
                workflow_id,
                step_no,
                step_total,
                phase: "video_long_generate",
                message: long_video_segment(idx + 1, segment_total, clip_secs),
                pct: Some(((idx * 100) / segment_total.max(1)).min(95) as u8),
                artifact_preview: None,
            });

            let seg_prompt = segment_video_prompt(base_prompt, idx, segment_total);
            let mut seg_input = input.clone();
            if let Some(obj) = seg_input.as_object_mut() {
                obj.insert("prompt".into(), json!(seg_prompt));
                obj.insert("duration".into(), json!(clip_secs));
                if let Some(url) = chain_image_url.clone() {
                    obj.insert("image_url".into(), json!(url));
                } else {
                    obj.remove("image_url");
                }
                obj.insert("model".into(), json!(model));
            }

            let seg_out = self.run_video_step(run_id, &seg_input).await?;
            let seg_path = ensure_local_video_path(&seg_out, &model).await?;
            segment_paths.push(seg_path.clone());

            if let Some(path_str) = local_path_from_step_output(&seg_out) {
                report_intermediate_artifact(
                    run_id,
                    workflow_id,
                    step_no,
                    step_total,
                    "video",
                    &path_str,
                );
                segment_artifacts.push(json!({
                    "segment": idx + 1,
                    "duration_secs": clip_secs,
                    "local_path": path_str,
                }));
            }

            if idx + 1 < segment_total {
                let frame_path = work_dir.join(format!("seg_{idx}_last.png"));
                extract_last_frame_png(&seg_path, &frame_path).await?;
                chain_image_url = Some(png_file_to_data_url(&frame_path)?);
            }
        }

        let output_path = work_dir.join("concat_output.mp4");
        if segment_total > 1 {
            report_media_progress(long_video_concat(segment_total));
            concat_videos(&segment_paths, &output_path).await?;
        } else {
            tokio::fs::copy(&segment_paths[0], &output_path)
                .await
                .map_err(|e| ToolError::ExecutionFailed(format!("copy segment video: {e}")))?;
        }

        let artifact =
            persist_concatenated_video(&output_path, "flowy", &model, plan.target_duration_secs)
                .await?;

        let task = VideoTaskMeta {
            local_id: format!("long-{run_id}"),
            task_id: String::new(),
            status: 1,
        };
        let provenance = MediaProvenance::for_api_call(
            base_prompt,
            input
                .get("negative_prompt")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            None,
            None,
        );
        let remote_url = artifact.remote_url.clone().unwrap_or_default();
        let file_fallback = format!("file://{}", artifact.local_path.display());
        let video_ref = if remote_url.is_empty() {
            file_fallback.as_str()
        } else {
            remote_url.as_str()
        };
        let response_str =
            video_generation_response(&model, video_ref, Some(&artifact), &task, provenance, None);
        let parsed: Value = serde_json::from_str(&response_str)
            .map_err(|e| ToolError::ExecutionFailed(format!("long video response JSON: {e}")))?;

        Ok(json!({
            "raw": parsed,
            "api_prompt": base_prompt,
            "negative_prompt": input.get("negative_prompt").and_then(|v| v.as_str()),
            "video_url": parsed.get("video"),
            "local_path": artifact.local_path.to_string_lossy(),
            "segment_plan": {
                "target_duration_secs": plan.target_duration_secs,
                "max_clip_secs": plan.max_clip_secs,
                "segment_durations": plan.segment_durations,
                "segment_count": segment_total,
            },
            "segment_artifacts": segment_artifacts,
            "output": parsed,
        }))
    }

    async fn run_prompt_refine(&self, input: &Value) -> Result<Value, ToolError> {
        let prompt = input.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
        let medium = input
            .get("medium")
            .and_then(|v| v.as_str())
            .unwrap_or("image");
        let aspect_ratio = input.get("aspect_ratio").and_then(|v| v.as_str());
        let has_reference_image = input
            .get("has_reference_image")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                input
                    .get("has_reference_image")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s == "true")
            });

        let refined = {
            report_media_progress(prompt_refine_working(medium));
            refine_with_llm_or_template(
                &self.services,
                &RefineInput {
                    prompt,
                    medium,
                    aspect_ratio,
                    has_reference_image,
                },
            )
            .await
        };

        Ok(json!({
            "output": refined.output,
            "image_prompt": refined.image_prompt,
            "video_prompt": refined.video_prompt,
            "motion_prompt": refined.motion_prompt,
            "negative_prompt": refined.negative_prompt,
        }))
    }

    async fn run_storyboard_multi(
        &self,
        run_id: &str,
        workflow_id: &str,
        step_no: usize,
        step_total: usize,
        input: &Value,
    ) -> Result<Value, ToolError> {
        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("storyboard missing prompt".into()))?;
        let aspect_ratio = input
            .get("aspect_ratio")
            .and_then(|v| v.as_str())
            .unwrap_or("16:9");
        let resolution = input
            .get("resolution")
            .and_then(|v| v.as_str())
            .unwrap_or("720p");
        let max_shots = input
            .get("max_shots")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32)
            .unwrap_or(self.services.media.workflows.storyboard_max_shots)
            .clamp(1, 5);
        let preview_only = input
            .get("preview_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let shots_to_render: Vec<usize> = input
            .get("shots_to_render")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64().map(|n| n as usize))
                    .collect()
            })
            .unwrap_or_default();

        let plan = {
            report_media_progress(storyboard_planning());
            plan_storyboard(&self.services, prompt, max_shots).await
        };

        let storyboard_preview: Vec<Value> = plan
            .shots
            .iter()
            .enumerate()
            .map(|(idx, shot)| {
                json!({
                    "shot": idx + 1,
                    "scene_prompt": shot.scene_prompt,
                    "motion_prompt": shot.motion_prompt,
                    "duration_secs": shot.duration_secs,
                })
            })
            .collect();

        report_workflow_step_event(WorkflowStepProgress {
            run_id,
            workflow_id,
            step_no,
            step_total,
            phase: "storyboard_preview",
            message: format!("分镜规划完成，共 {} 个镜头", plan.shots.len()),
            pct: Some(10),
            artifact_preview: None,
        });

        if preview_only {
            return Ok(json!({
                "preview_only": true,
                "storyboard_preview": storyboard_preview,
                "negative_prompt": plan.negative_prompt,
                "hint": "Show storyboard_preview to the user; call media_workflow_run with shots_to_render when confirmed."
            }));
        }

        let shot_total = plan.shots.len();
        let mut shot_outputs = Vec::new();
        let mut artifacts = Vec::new();

        for (idx, shot) in plan.shots.iter().enumerate() {
            let shot_no = idx + 1;
            if !shots_to_render.is_empty() && !shots_to_render.contains(&shot_no) {
                continue;
            }

            let mut shot_err = None;
            for attempt in 0..self.max_retries {
                if self.control.is_cancelled(run_id) {
                    return Err(ToolError::ExecutionFailed("workflow cancelled".into()));
                }
                report_media_progress(storyboard_shot_image(shot_no, shot_total));
                match self
                    .run_image_step(&json!({
                        "prompt": shot.scene_prompt,
                    }))
                    .await
                {
                    Ok(image_out) => {
                        if let Some(path) = local_path_from_step_output(&image_out) {
                            report_intermediate_artifact(
                                run_id,
                                workflow_id,
                                step_no,
                                step_total,
                                "keyframe",
                                &path,
                            );
                        }
                        report_media_progress(storyboard_shot_video(
                            shot_no,
                            shot_total,
                            shot.duration_secs,
                        ));
                        match self
                            .run_video_step(
                                run_id,
                                &json!({
                                    "prompt": shot.motion_prompt,
                                    "image_url": image_out.get("best_url"),
                                    "duration": shot.duration_secs,
                                    "aspect_ratio": aspect_ratio,
                                    "resolution": resolution,
                                    "negative_prompt": plan.negative_prompt,
                                }),
                            )
                            .await
                        {
                            Ok(video_out) => {
                                if let Some(path) = local_path_from_step_output(&video_out) {
                                    artifacts.push(json!({
                                        "shot": shot_no,
                                        "local_path": path,
                                        "kind": "video",
                                    }));
                                }
                                shot_outputs.push(json!({
                                    "shot": shot_no,
                                    "image": image_out,
                                    "video": video_out,
                                }));
                                shot_err = None;
                                break;
                            }
                            Err(err) => {
                                shot_err = Some(err);
                                if attempt + 1 < self.max_retries {
                                    tokio::time::sleep(tokio::time::Duration::from_secs(
                                        2_u64.pow(attempt),
                                    ))
                                    .await;
                                }
                            }
                        }
                        if shot_err.is_none() {
                            break;
                        }
                    }
                    Err(err) => {
                        shot_err = Some(err);
                        if attempt + 1 < self.max_retries {
                            tokio::time::sleep(tokio::time::Duration::from_secs(
                                2_u64.pow(attempt),
                            ))
                            .await;
                        }
                    }
                }
            }
            if let Some(err) = shot_err {
                return Err(err);
            }
        }

        Ok(json!({
            "output": shot_outputs,
            "shots": shot_outputs,
            "storyboard_preview": storyboard_preview,
            "artifacts": artifacts,
            "negative_prompt": plan.negative_prompt,
            "suggested_next_actions": [
                "image_variation — alternate keyframe takes",
                "video_extend — continue from last frame",
                "storyboard_multi with shots_to_render — re-render selected shots only"
            ],
        }))
    }

    async fn run_qa_check(&self, input: &Value) -> Result<Value, ToolError> {
        let kind = input
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("image");
        let target = input
            .get("target_step")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("qa_check missing target_step".into()))?;
        let step_output = input
            .get("step_output")
            .cloned()
            .ok_or_else(|| ToolError::InvalidParams("qa_check missing step_output".into()))?;

        let local_path = step_output
            .pointer("/raw/assets/0/local_path")
            .or_else(|| step_output.pointer("/assets/0/local_path"))
            .or_else(|| step_output.get("local_path"))
            .and_then(|v| v.as_str());

        let Some(path_str) = local_path else {
            return Ok(json!({
                "passed": true,
                "skipped": true,
                "reason": "no local_path to QA"
            }));
        };

        let path = std::path::PathBuf::from(path_str);
        let report = if kind == "video" {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            qa_check_video(&path, size)
        } else {
            let bytes = std::fs::read(&path).unwrap_or_default();
            qa_check_image(&path, &bytes)
        };

        if !report.passed {
            let issues = report.issues.clone();
            report.into_result(&format!("{target} {kind}"))?;
            return Ok(json!({
                "passed": false,
                "target_step": target,
                "issues": issues,
            }));
        }
        Ok(json!({
            "passed": true,
            "target_step": target,
            "issues": report.issues,
        }))
    }
}

fn video_backend_trait(
    v: Arc<FlowyVideoGenBackend>,
) -> Arc<dyn hermes_tools::VideoGenerateBackend> {
    v
}

fn is_retryable_error(err: &ToolError) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("timeout")
        || msg.contains("rate")
        || msg.contains("429")
        || msg.contains("502")
        || msg.contains("503")
        || msg.contains("504")
        || msg.contains("temporarily")
        || msg.contains("qa failed")
}

fn local_path_from_step_output(output: &Value) -> Option<String> {
    output
        .pointer("/raw/assets/0/local_path")
        .or_else(|| output.pointer("/assets/0/local_path"))
        .or_else(|| output.get("local_path"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

async fn ensure_local_video_path(
    output: &Value,
    model: &str,
) -> Result<std::path::PathBuf, ToolError> {
    use std::path::PathBuf;

    use crate::assets::persist_from_url;

    if let Some(path) = local_path_from_step_output(output) {
        let p = PathBuf::from(&path);
        if p.is_file() {
            return Ok(p);
        }
    }
    let url = output
        .pointer("/raw/video")
        .or_else(|| output.get("video_url"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            ToolError::ExecutionFailed(
                "video segment generated but no local_path or remote URL for ffmpeg".into(),
            )
        })?;
    if url.starts_with("file://") {
        return Ok(PathBuf::from(url.trim_start_matches("file://")));
    }
    let artifact = persist_from_url(url, "flowy", model).await?;
    Ok(artifact.local_path)
}

fn clear_outputs_from(
    ctx: &mut HashMap<String, Value>,
    record: &mut WorkflowRunRecord,
    order: &[String],
    from_idx: usize,
) {
    for step_id in order.iter().skip(from_idx) {
        ctx.remove(&format!("steps.{step_id}"));
        record.step_outputs.remove(step_id);
    }
}

fn topo_sort(steps: &[WorkflowStep]) -> Result<Vec<String>, ToolError> {
    let mut deps: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut ids = HashSet::new();
    for step in steps {
        ids.insert(step.id.as_str());
        deps.insert(
            step.id.as_str(),
            step.depends_on.iter().map(String::as_str).collect(),
        );
    }
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    let mut temp = HashSet::new();

    fn visit<'a>(
        id: &'a str,
        deps: &HashMap<&'a str, Vec<&'a str>>,
        visited: &mut HashSet<&'a str>,
        temp: &mut HashSet<&'a str>,
        order: &mut Vec<String>,
    ) -> Result<(), ToolError> {
        if visited.contains(id) {
            return Ok(());
        }
        if !temp.insert(id) {
            return Err(ToolError::ExecutionFailed(format!(
                "workflow cycle detected at step {id}"
            )));
        }
        if let Some(step_deps) = deps.get(id) {
            for dep in step_deps {
                visit(dep, deps, visited, temp, order)?;
            }
        }
        temp.remove(id);
        visited.insert(id);
        order.push(id.to_string());
        Ok(())
    }

    for id in ids {
        visit(id, &deps, &mut visited, &mut temp, &mut order)?;
    }
    Ok(order)
}

fn resolve_value(template: &Value, ctx: &HashMap<String, Value>) -> Value {
    match template {
        Value::String(s) if s.starts_with('$') => resolve_ref(s, ctx).unwrap_or(Value::Null),
        Value::Array(arr) => Value::Array(arr.iter().map(|v| resolve_value(v, ctx)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), resolve_value(v, ctx));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn resolve_ref(path: &str, ctx: &HashMap<String, Value>) -> Option<Value> {
    let path = path.trim_start_matches('$');
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let root = ctx.get(parts[0])?;
    let mut cur = root;
    for part in parts.iter().skip(1) {
        cur = cur.get(*part)?;
    }
    Some(cur.clone())
}

fn collect_artifacts(outputs: &HashMap<String, Value>) -> Vec<Value> {
    let mut artifacts = Vec::new();
    for (step, output) in outputs {
        if let Some(embedded) = output.get("artifacts").and_then(|v| v.as_array()) {
            for item in embedded {
                if let Some(path) = item.get("local_path").and_then(|p| p.as_str()) {
                    let kind = item.get("kind").and_then(|k| k.as_str()).unwrap_or("video");
                    artifacts.push(json!({ "step": step, "local_path": path, "kind": kind }));
                }
            }
        }
        if let Some(assets) = output.pointer("/raw/assets").and_then(|v| v.as_array()) {
            for asset in assets {
                if let Some(path) = asset.get("local_path").and_then(|p| p.as_str()) {
                    let kind = asset
                        .get("kind")
                        .and_then(|k| k.as_str())
                        .unwrap_or("media");
                    artifacts.push(json!({ "step": step, "local_path": path, "kind": kind }));
                }
            }
        }
        if let Some(local) = output
            .pointer("/raw/local_path")
            .or_else(|| output.get("local_path"))
            && local.is_string()
        {
            artifacts.push(json!({ "step": step, "local_path": local, "kind": "video" }));
        }
        if let Some(images) = output.pointer("/raw/images").and_then(|v| v.as_array()) {
            for img in images {
                if let Some(path) = img.get("local_path") {
                    artifacts.push(json!({ "step": step, "local_path": path, "kind": "image" }));
                }
            }
        }
    }
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topo_sort_respects_dependencies() {
        let steps = vec![
            WorkflowStep {
                id: "b".into(),
                kind: "image_generate".into(),
                depends_on: vec!["a".into()],
                input: json!({}),
                on_fail: None,
            },
            WorkflowStep {
                id: "a".into(),
                kind: "prompt_refine".into(),
                depends_on: vec![],
                input: json!({}),
                on_fail: None,
            },
        ];
        let order = topo_sort(&steps).expect("sort");
        assert_eq!(order, vec!["a", "b"]);
    }

    #[test]
    fn retryable_errors_detected() {
        assert!(is_retryable_error(&ToolError::ExecutionFailed(
            "HTTP 503 temporarily unavailable".into()
        )));
        assert!(!is_retryable_error(&ToolError::InvalidParams("bad".into())));
    }
}
