//! Real [`CronjobBackend`] implementation backed by [`CronScheduler`].
//!
//! This backend bridges the `cronjob` agent tool (from `hermes-tools`) to the
//! live scheduler in `hermes-cron`. When wired in the binary layer, the
//! agent's tool call immediately creates / lists / pauses / resumes / removes
//! / runs real cron jobs instead of returning a "pending" envelope.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

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

    async fn validate_context_refs_exist(&self, refs: &[String]) -> Result<(), ToolError> {
        for ref_id in refs {
            if self.scheduler.get_job(ref_id).await.is_none() {
                return Err(ToolError::InvalidParams(format!(
                    "context_from job '{}' not found. Use cronjob(action='list') to inspect available jobs.",
                    ref_id
                )));
            }
        }
        Ok(())
    }
}

fn normalize_context_from_refs(raw: &Value) -> Result<Option<Vec<String>>, ToolError> {
    match raw {
        Value::Null => Ok(None),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![trimmed.to_string()]))
            }
        }
        Value::Array(arr) => {
            let mut refs = Vec::new();
            for item in arr {
                let s = item.as_str().ok_or_else(|| {
                    ToolError::InvalidParams(
                        "context_from array values must be strings".to_string(),
                    )
                })?;
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    refs.push(trimmed.to_string());
                }
            }
            if refs.is_empty() {
                Ok(None)
            } else {
                Ok(Some(refs))
            }
        }
        _ => Err(ToolError::InvalidParams(
            "context_from must be a string, array of strings, or null".to_string(),
        )),
    }
}

fn parse_context_from_create(raw: Option<&Value>) -> Result<Option<Vec<String>>, ToolError> {
    match raw {
        Some(v) => normalize_context_from_refs(v),
        None => Ok(None),
    }
}

fn parse_context_from_update(
    raw: Option<&Value>,
) -> Result<Option<Option<Vec<String>>>, ToolError> {
    match raw {
        Some(v) => Ok(Some(normalize_context_from_refs(v)?)),
        None => Ok(None),
    }
}

fn normalize_workdir_value(raw: &Value) -> Result<Option<String>, ToolError> {
    match raw {
        Value::Null => Ok(None),
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        _ => Err(ToolError::InvalidParams(
            "workdir must be an absolute path string or null".to_string(),
        )),
    }
}

fn windows_absolute_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn validate_tool_script_reference(script: &str) -> Result<(), ToolError> {
    let trimmed = script.trim();
    if trimmed.is_empty() || trimmed.chars().any(char::is_whitespace) {
        return Ok(());
    }
    if trimmed.starts_with('~')
        || windows_absolute_path(trimmed)
        || Path::new(trimmed).is_absolute()
    {
        return Err(ToolError::InvalidParams(
            "cron script must be an inline script or a relative path under Hermes scripts/"
                .to_string(),
        ));
    }
    if trimmed.contains("../") || trimmed.contains("..\\") {
        return Err(ToolError::InvalidParams(
            "cron script path traversal escapes Hermes scripts/".to_string(),
        ));
    }
    Ok(())
}

fn parse_workdir_create(raw: Option<&Value>) -> Result<Option<String>, ToolError> {
    match raw {
        Some(v) => normalize_workdir_value(v),
        None => Ok(None),
    }
}

fn parse_workdir_update(raw: Option<&Value>) -> Result<Option<Option<String>>, ToolError> {
    match raw {
        Some(v) => Ok(Some(normalize_workdir_value(v)?)),
        None => Ok(None),
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
        context_from: Option<&Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        workdir: Option<&Value>,
    ) -> Result<String, ToolError> {
        let mut job = CronJob::new(schedule, task);
        if !name.trim().is_empty() {
            job.name = Some(name.to_string());
        }
        if let Some(ts) = toolset.filter(|s| !s.trim().is_empty()) {
            // Toolset arrives as a simple name; store it as a single-skill hint.
            job.skills = Some(vec![ts.to_string()]);
        }
        if let Some(script) = script {
            let trimmed = script.trim();
            if !trimmed.is_empty() {
                validate_tool_script_reference(trimmed)?;
                job.script = Some(trimmed.to_string());
            }
        }
        if let Some(no_agent) = no_agent {
            job.no_agent = no_agent;
        }
        job.workdir = parse_workdir_create(workdir)?;
        let context_from = parse_context_from_create(context_from)?;
        if let Some(ref refs) = context_from {
            self.validate_context_refs_exist(refs).await?;
        }
        job.context_from = context_from.clone();

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
            "script": script,
            "no_agent": no_agent.unwrap_or(false),
            "workdir": self.scheduler.get_job(&id).await.and_then(|j| j.workdir),
            "context_from": context_from,
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
                    "workdir": j.workdir,
                    "context_from": j.context_from,
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
        context_from: Option<&Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        workdir: Option<&Value>,
    ) -> Result<String, ToolError> {
        let mut job = self
            .scheduler
            .get_job(id)
            .await
            .ok_or_else(|| ToolError::ExecutionFailed(format!("cron job not found: {}", id)))?;

        if let Some(s) = schedule {
            job.schedule = s.to_string();
            job.next_run = None;
        }
        if let Some(t) = task {
            job.prompt = t.to_string();
        }
        if let Some(script) = script {
            let trimmed = script.trim();
            if !trimmed.is_empty() {
                validate_tool_script_reference(trimmed)?;
            }
            job.script = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Some(no_agent) = no_agent {
            job.no_agent = no_agent;
        }
        if let Some(workdir_update) = parse_workdir_update(workdir)? {
            job.workdir = workdir_update;
        }
        if let Some(context_update) = parse_context_from_update(context_from)? {
            if let Some(ref refs) = context_update {
                self.validate_context_refs_exist(refs).await?;
            }
            job.context_from = context_update;
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

        Ok(json!({
            "action": "updated",
            "id": id,
            "workdir": self.scheduler.get_job(id).await.and_then(|j| j.workdir),
            "context_from": self.scheduler.get_job(id).await.and_then(|j| j.context_from),
        })
        .to_string())
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

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use tempfile::tempdir;

    use crate::cli_support::cron_scheduler_for_data_dir;

    fn make_backend() -> ScheduledCronjobBackend {
        let root = tempdir().expect("tempdir").keep();
        let run_runtime = Arc::new(
            crate::RunRuntime::open_for_ephemeral_testing(&root).expect("ephemeral run runtime"),
        );
        let execution_mode = hermes_core::ResolvedExecutionMode::default();
        let execution_containment = Arc::new(
            hermes_environments::ExecutionContainmentContext::manual(execution_mode)
                .expect("default mode must admit manual containment"),
        );
        let tool_registry = Arc::new(hermes_agent::agent_loop::ToolRegistry::new());
        tool_registry.set_execution_containment_context(Arc::clone(&execution_containment));
        let scheduler = Arc::new(
            cron_scheduler_for_data_dir(
                root.join("cron"),
                tool_registry,
                run_runtime,
                execution_mode,
                execution_containment,
            )
            .expect("test cron scheduler"),
        );
        ScheduledCronjobBackend::new(scheduler)
    }

    #[test]
    fn normalize_context_from_string_and_array() {
        let single = normalize_context_from_refs(&json!("abc123")).expect("single");
        assert_eq!(single, Some(vec!["abc123".to_string()]));

        let many = normalize_context_from_refs(&json!(["a", " b ", ""])).expect("array");
        assert_eq!(many, Some(vec!["a".to_string(), "b".to_string()]));

        let empty = normalize_context_from_refs(&json!(["", " "])).expect("empty");
        assert_eq!(empty, None);
    }

    #[test]
    fn parse_context_from_update_preserves_tristate() {
        assert!(parse_context_from_update(None).expect("none").is_none());
        assert_eq!(
            parse_context_from_update(Some(&json!(""))).expect("clear"),
            Some(None)
        );
        assert_eq!(
            parse_context_from_update(Some(&json!(["a"]))).expect("set"),
            Some(Some(vec!["a".to_string()]))
        );
    }

    #[tokio::test]
    async fn create_and_update_context_from_roundtrip() {
        let backend = make_backend();
        let scheduler = backend.scheduler().clone();

        let source_id = scheduler
            .create_job(CronJob::new("0 * * * *", "collect source"))
            .await
            .expect("create source");

        let created = backend
            .create(
                "consumer",
                "0 * * * *",
                "consume context",
                None,
                Some(&json!(source_id.clone())),
                None,
                None,
                None,
            )
            .await
            .expect("create");
        let created_v: serde_json::Value = serde_json::from_str(&created).expect("json");
        let consumer_id = created_v.get("id").and_then(|v| v.as_str()).expect("id");

        let loaded = scheduler.get_job(consumer_id).await.expect("consumer job");
        assert_eq!(loaded.context_from, Some(vec![source_id.clone()]));

        backend
            .update(
                consumer_id,
                None,
                None,
                None,
                Some(&json!([source_id.clone()])),
                None,
                None,
                None,
            )
            .await
            .expect("update set");
        let loaded = scheduler
            .get_job(consumer_id)
            .await
            .expect("consumer after set");
        assert_eq!(loaded.context_from, Some(vec![source_id.clone()]));

        backend
            .update(
                consumer_id,
                None,
                None,
                None,
                Some(&json!([])),
                None,
                None,
                None,
            )
            .await
            .expect("update clear");
        let loaded = scheduler
            .get_job(consumer_id)
            .await
            .expect("consumer after clear");
        assert_eq!(loaded.context_from, None);
    }

    #[tokio::test]
    async fn create_and_update_workdir_roundtrip() {
        let backend = make_backend();
        let scheduler = backend.scheduler().clone();
        let workdir = tempdir().expect("workdir");

        let created = backend
            .create(
                "consumer",
                "0 * * * *",
                "consume context",
                None,
                None,
                None,
                None,
                Some(&json!(workdir.path().to_string_lossy().to_string())),
            )
            .await
            .expect("create");
        let created_v: serde_json::Value = serde_json::from_str(&created).expect("json");
        let job_id = created_v.get("id").and_then(|v| v.as_str()).expect("id");

        let loaded = scheduler.get_job(job_id).await.expect("job");
        assert_eq!(
            loaded.workdir,
            Some(
                std::fs::canonicalize(workdir.path())
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            )
        );

        backend
            .update(job_id, None, None, None, None, None, None, Some(&json!("")))
            .await
            .expect("clear");
        let loaded = scheduler.get_job(job_id).await.expect("job");
        assert_eq!(loaded.workdir, None);
    }

    #[tokio::test]
    async fn create_rejects_absolute_tool_script() {
        let backend = make_backend();
        let err = backend
            .create(
                "bad",
                "0 * * * *",
                "task",
                None,
                None,
                Some("/tmp/evil.py"),
                None,
                None,
            )
            .await
            .expect_err("reject absolute");
        assert!(err.to_string().contains("relative path"));
    }
}
