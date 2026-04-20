//! Real [`CronjobBackend`] implementation backed by [`CronScheduler`].
//!
//! This backend bridges the `cronjob` agent tool (from `hermes-tools`) to the
//! live scheduler in `hermes-cron`. When wired in the binary layer, the
//! agent's tool call immediately creates / lists / pauses / resumes / removes
//! / runs real cron jobs instead of returning a "pending" envelope.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use hermes_core::ToolError;
use hermes_tools::tools::cronjob::CronjobBackend;

use crate::job::{CronJob, JobStatus};
use crate::scheduler::{CronError, CronScheduler};

/// A [`CronjobBackend`] that delegates to a running [`CronScheduler`].
pub struct ScheduledCronjobBackend {
    scheduler: Arc<CronScheduler>,
}

impl ScheduledCronjobBackend {
    pub fn new(scheduler: Arc<CronScheduler>) -> Self {
        Self { scheduler }
    }

    pub fn scheduler(&self) -> &Arc<CronScheduler> {
        &self.scheduler
    }
}

fn cron_err_to_tool(e: CronError) -> ToolError {
    match e {
        CronError::JobNotFound(id) => {
            ToolError::ExecutionFailed(format!("cron job not found: {}", id))
        }
        CronError::InvalidJob(msg) => {
            ToolError::InvalidParams(format!("invalid cron job: {}", msg))
        }
        CronError::JobAlreadyExists(id) => {
            ToolError::ExecutionFailed(format!("cron job already exists: {}", id))
        }
        other => ToolError::ExecutionFailed(other.to_string()),
    }
}

#[async_trait]
impl CronjobBackend for ScheduledCronjobBackend {
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        toolset: Option<&str>,
    ) -> Result<String, ToolError> {
        let mut job = CronJob::new(schedule, task);
        if !name.trim().is_empty() {
            job.name = Some(name.to_string());
        }
        if let Some(ts) = toolset.filter(|s| !s.trim().is_empty()) {
            // Toolset arrives as a simple name; store it as a single-skill hint.
            job.skills = Some(vec![ts.to_string()]);
        }

        let id = self
            .scheduler
            .create_job(job)
            .await
            .map_err(cron_err_to_tool)?;

        Ok(json!({
            "action": "created",
            "id": id,
            "name": name,
            "schedule": schedule,
            "task": task,
            "toolset": toolset,
        })
        .to_string())
    }

    async fn list(&self) -> Result<String, ToolError> {
        let jobs = self.scheduler.list_jobs().await;
        let rendered: Vec<_> = jobs
            .iter()
            .map(|j| {
                json!({
                    "id": j.id,
                    "name": j.name,
                    "schedule": j.schedule,
                    "prompt": j.prompt,
                    "status": j.status.to_string(),
                    "next_run": j.next_run,
                    "last_run": j.last_run,
                    "run_count": j.run_count,
                })
            })
            .collect();
        Ok(json!({
            "action": "list",
            "count": rendered.len(),
            "jobs": rendered,
        })
        .to_string())
    }

    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
    ) -> Result<String, ToolError> {
        let mut job = self
            .scheduler
            .get_job(id)
            .await
            .ok_or_else(|| ToolError::ExecutionFailed(format!("cron job not found: {}", id)))?;

        if let Some(s) = schedule {
            job.schedule = s.to_string();
        }
        if let Some(t) = task {
            job.prompt = t.to_string();
        }
        if let Some(en) = enabled {
            job.status = if en {
                JobStatus::Active
            } else {
                JobStatus::Paused
            };
            if matches!(job.status, JobStatus::Active) {
                job.next_run = job.compute_next_run(chrono::Utc::now());
            }
        }

        self.scheduler
            .update_job(id, job)
            .await
            .map_err(cron_err_to_tool)?;

        Ok(json!({"action": "updated", "id": id}).to_string())
    }

    async fn pause(&self, id: &str) -> Result<String, ToolError> {
        self.scheduler
            .pause_job(id)
            .await
            .map_err(cron_err_to_tool)?;
        Ok(json!({"action": "paused", "id": id}).to_string())
    }

    async fn resume(&self, id: &str) -> Result<String, ToolError> {
        self.scheduler
            .resume_job(id)
            .await
            .map_err(cron_err_to_tool)?;
        Ok(json!({"action": "resumed", "id": id}).to_string())
    }

    async fn remove(&self, id: &str) -> Result<String, ToolError> {
        self.scheduler
            .remove_job(id)
            .await
            .map_err(cron_err_to_tool)?;
        Ok(json!({"action": "removed", "id": id}).to_string())
    }

    async fn run(&self, id: &str) -> Result<String, ToolError> {
        let res = self.scheduler.run_job(id).await.map_err(cron_err_to_tool)?;
        let last_text = res
            .messages
            .iter()
            .rev()
            .find_map(|m| {
                matches!(m.role, hermes_core::MessageRole::Assistant)
                    .then(|| m.content.clone())
                    .flatten()
            })
            .unwrap_or_default();
        Ok(json!({
            "action": "ran",
            "id": id,
            "finished_naturally": res.finished_naturally,
            "turns": res.total_turns,
            "output": last_text,
        })
        .to_string())
    }
}
