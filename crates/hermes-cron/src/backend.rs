//! Real [`CronjobBackend`] implementation backed by [`CronScheduler`].
//!
//! This backend bridges the `cronjob` agent tool (from `hermes-tools`) to the
//! live scheduler in `hermes-cron`. When wired in the binary layer, the
//! agent's tool call immediately creates / lists / pauses / resumes / removes
//! / runs real cron jobs instead of returning a "pending" envelope.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use hermes_core::ToolError;
use hermes_tools::tools::cronjob::CronjobBackend;
use hermes_tools::tools::messaging::MessagingSessionContext;

use crate::job::{CronJob, JobStatus, ModelConfig};
use crate::python_job::{parse_deliver_string, JobOrigin};
use crate::runner::detect_cron_prompt_injection;
use crate::schedule::{normalize_schedule_input, ScheduleSpec};
use crate::scheduler::{CronError, CronScheduler};

/// Reject absolute paths and directory traversal in cron script fields.
///
/// Mirrors Python `cronjob_tools._validate_cron_script_path()`.
fn validate_script_path(script: &str) -> Result<(), ToolError> {
    let raw = script.trim();
    if raw.is_empty() {
        return Ok(());
    }
    let is_absolute = raw.starts_with('/')
        || raw.starts_with('~')
        || raw.starts_with('\\')
        || (raw.len() >= 2 && raw.as_bytes()[1] == b':');
    if is_absolute {
        return Err(ToolError::InvalidParams(format!(
            "cron script must be a name relative to ~/.hermes/scripts/, not an absolute path: {raw:?}"
        )));
    }
    for component in std::path::Path::new(raw).components() {
        if component == std::path::Component::ParentDir {
            return Err(ToolError::InvalidParams(format!(
                "cron script path must not contain '..': {raw:?}"
            )));
        }
    }
    Ok(())
}

/// Scan `prompt` and `script` for injection / exfil patterns at write time.
///
/// Calling this in `create()` / `update()` surfaces errors immediately, before
/// the job is persisted — rather than waiting until execution.
fn scan_job_content(task: &str, script: Option<&str>) -> Result<(), ToolError> {
    if let Some(rule) = detect_cron_prompt_injection(task) {
        return Err(ToolError::InvalidParams(format!(
            "prompt blocked by security scanner ({rule})"
        )));
    }
    if let Some(s) = script {
        let trimmed = s.trim();
        if let Some(rule) = detect_cron_prompt_injection(trimmed) {
            return Err(ToolError::InvalidParams(format!(
                "script blocked by security scanner ({rule})"
            )));
        }
        validate_script_path(trimmed)?;
    }
    Ok(())
}

/// A [`CronjobBackend`] that delegates to a running [`CronScheduler`].
pub struct ScheduledCronjobBackend {
    scheduler: Arc<CronScheduler>,
    /// Optional session context for auto-capturing origin (platform + chat_id).
    session_context: Option<Arc<MessagingSessionContext>>,
}

impl ScheduledCronjobBackend {
    pub fn new(scheduler: Arc<CronScheduler>) -> Self {
        Self {
            scheduler,
            session_context: None,
        }
    }

    /// Create a backend that auto-captures origin from the messaging session.
    pub fn with_session_context(
        scheduler: Arc<CronScheduler>,
        session_context: Arc<MessagingSessionContext>,
    ) -> Self {
        Self {
            scheduler,
            session_context: Some(session_context),
        }
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

fn parse_string_list(raw: Option<&Value>, field: &str) -> Result<Option<Vec<String>>, ToolError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    if let Some(arr) = raw.as_array() {
        let mut out = Vec::new();
        for item in arr {
            let s = item.as_str().ok_or_else(|| {
                ToolError::InvalidParams(format!("{field} array values must be strings"))
            })?;
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                out.push(trimmed.to_string());
            }
        }
        return Ok(Some(out));
    }
    Err(ToolError::InvalidParams(format!(
        "{field} must be an array of strings or null"
    )))
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

fn apply_skills_update(job: &mut CronJob, raw: Option<&Value>) -> Result<(), ToolError> {
    let Some(raw) = raw else {
        return Ok(());
    };
    if raw.is_null() {
        job.skills = None;
        return Ok(());
    }
    if let Some(s) = raw.as_str() {
        let trimmed = s.trim();
        job.skills = if trimmed.is_empty() {
            None
        } else {
            Some(vec![trimmed.to_string()])
        };
        return Ok(());
    }
    if let Some(arr) = raw.as_array() {
        let mut skills = Vec::new();
        for item in arr {
            let s = item.as_str().ok_or_else(|| {
                ToolError::InvalidParams("skills array values must be strings".into())
            })?;
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                skills.push(trimmed.to_string());
            }
        }
        job.skills = if skills.is_empty() { None } else { Some(skills) };
        return Ok(());
    }
    Err(ToolError::InvalidParams(
        "skills must be a string, array of strings, or null".into(),
    ))
}

fn apply_repeat_for_oneshot(job: &mut CronJob) {
    if job.repeat.is_some() {
        return;
    }
    if matches!(
        job.schedule_spec.as_ref(),
        Some(ScheduleSpec::Once { .. })
    ) {
        job.repeat = Some(1);
    }
}

/// RFC3339 UTC + Hermes wall-clock label for agent-facing confirmation text.
fn next_run_response_fields(next_run: Option<DateTime<Utc>>) -> (Value, Value) {
    match next_run {
        Some(dt) => (
            json!(dt.to_rfc3339()),
            json!(hermes_core::format_wall_datetime(dt)),
        ),
        None => (Value::Null, Value::Null),
    }
}

#[async_trait]
impl CronjobBackend for ScheduledCronjobBackend {
    async fn create(
        &self,
        name: Option<&str>,
        schedule: &str,
        task: &str,
        skills: Option<&[String]>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        context_from: Option<&Value>,
        enabled_toolsets: Option<&[String]>,
        workdir: Option<&str>,
        profile: Option<&str>,
        script: Option<&str>,
        no_agent: Option<bool>,
        deliver: Option<&str>,
        repeat: Option<u32>,
    ) -> Result<String, ToolError> {
        let mut job = CronJob::new(schedule, task);
        if let Some(name) = name.filter(|s| !s.trim().is_empty()) {
            job.name = Some(name.to_string());
        } else if !task.trim().is_empty() {
            job.name = Some(task.chars().take(50).collect());
        } else if let Some(first) = skills.and_then(|s| s.first()) {
            job.name = Some(first.chars().take(50).collect());
        }
        if let Some(skills) = skills.filter(|s| !s.is_empty()) {
            job.skills = Some(skills.to_vec());
        }
        if model.is_some() || provider.is_some() || base_url.is_some() {
            job.model = Some(ModelConfig {
                model: model.map(|s| s.to_string()),
                provider: provider.map(|s| s.to_string()),
                base_url: base_url.map(|s| s.to_string()),
            });
        }
        if let Some(script) = script {
            let trimmed = script.trim();
            if !trimmed.is_empty() {
                job.script = Some(trimmed.to_string());
            }
        }
        if let Some(toolsets) = enabled_toolsets.filter(|s| !s.is_empty()) {
            job.enabled_toolsets = Some(toolsets.to_vec());
        }
        if let Some(workdir) = workdir.map(str::trim).filter(|s| !s.is_empty()) {
            job.workdir = Some(workdir.to_string());
        }
        if let Some(profile) = profile.map(str::trim).filter(|s| !s.is_empty()) {
            job.profile = Some(profile.to_string());
        }
        if let Some(no_agent) = no_agent {
            job.no_agent = no_agent;
        }
        if let Some(deliver) = deliver.filter(|s| !s.trim().is_empty()) {
            job.deliver = parse_deliver_string(deliver);
        }
        if let Some(repeat) = repeat {
            job.repeat = Some(repeat);
        }
        apply_repeat_for_oneshot(&mut job);

        scan_job_content(task, script)?;

        let context_from = parse_context_from_create(context_from)?;
        if let Some(ref refs) = context_from {
            self.validate_context_refs_exist(refs).await?;
        }
        job.context_from = context_from.clone();

        // Auto-capture origin from messaging session (platform + chat_id).
        if let Some(ref session) = self.session_context {
            if let Some((platform, chat_id)) = session.get() {
                job.origin = Some(JobOrigin {
                    platform,
                    chat_id: Some(chat_id),
                    thread_id: None,
                });
            }
        }

        let response_name = job.name.clone();
        let response_skills = job.skills.clone();
        let response_repeat = job.repeat;

        let id = self
            .scheduler
            .create_job(job)
            .await
            .map_err(cron_err_to_tool)?;

        let (next_run, next_run_display) = self
            .scheduler
            .get_job(&id)
            .await
            .and_then(|j| j.next_run)
            .map(|dt| next_run_response_fields(Some(dt)))
            .unwrap_or_else(|| next_run_response_fields(None));

        Ok(json!({
            "action": "created",
            "id": id,
            "name": response_name,
            "schedule": schedule,
            "task": task,
            "skills": response_skills,
            "model": model,
            "provider": provider,
            "base_url": base_url,
            "script": script,
            "no_agent": no_agent.unwrap_or(false),
            "deliver": deliver,
            "repeat": response_repeat,
            "context_from": context_from,
            "enabled_toolsets": enabled_toolsets,
            "workdir": workdir,
            "profile": profile,
            "next_run": next_run,
            "next_run_display": next_run_display,
        })
        .to_string())
    }

    async fn list(&self, include_disabled: bool) -> Result<String, ToolError> {
        let jobs = self.scheduler.list_jobs().await;
        let rendered: Vec<_> = jobs
            .iter()
            .filter(|j| include_disabled || matches!(j.status, JobStatus::Active))
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
                    "context_from": j.context_from,
                    "enabled_toolsets": j.enabled_toolsets,
                    "workdir": j.workdir,
                    "profile": j.profile,
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
        enabled_toolsets: Option<&Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        skills: Option<&Value>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        workdir: Option<&str>,
        profile: Option<&str>,
        deliver: Option<&str>,
        repeat: Option<u32>,
    ) -> Result<String, ToolError> {
        let mut job = self
            .scheduler
            .get_job(id)
            .await
            .ok_or_else(|| ToolError::ExecutionFailed(format!("cron job not found: {}", id)))?;

        if let Some(s) = schedule {
            job.schedule = normalize_schedule_input(s);
            job.schedule_spec = None;
            job.schedule_display = None;
            job.next_run = None;
            job.normalize_schedule();
        }
        if let Some(t) = task {
            if let Some(rule) = detect_cron_prompt_injection(t) {
                return Err(ToolError::InvalidParams(format!(
                    "prompt blocked by security scanner ({rule})"
                )));
            }
            job.prompt = t.to_string();
        }
        if let Some(script) = script {
            let trimmed = script.trim();
            if !trimmed.is_empty() {
                validate_script_path(trimmed)?;
                if let Some(rule) = detect_cron_prompt_injection(trimmed) {
                    return Err(ToolError::InvalidParams(format!(
                        "script blocked by security scanner ({rule})"
                    )));
                }
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
        apply_skills_update(&mut job, skills)?;
        if model.is_some() || provider.is_some() || base_url.is_some() {
            job.model = Some(ModelConfig {
                model: model.map(|s| s.to_string()),
                provider: provider.map(|s| s.to_string()),
                base_url: base_url.map(|s| s.to_string()),
            });
        }
        if let Some(deliver) = deliver.filter(|s| !s.trim().is_empty()) {
            job.deliver = parse_deliver_string(deliver);
        }
        if let Some(repeat) = repeat {
            job.repeat = Some(repeat);
        }
        if let Some(toolsets) = parse_string_list(enabled_toolsets, "enabled_toolsets")? {
            job.enabled_toolsets = if toolsets.is_empty() { None } else { Some(toolsets) };
        }
        if let Some(workdir) = workdir {
            let trimmed = workdir.trim();
            job.workdir = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        if let Some(profile) = profile {
            let trimmed = profile.trim();
            job.profile = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
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
                job.next_run = job.compute_next_run(hermes_core::now_utc());
            }
        }

        self.scheduler
            .update_job(id, job)
            .await
            .map_err(cron_err_to_tool)?;

        Ok(json!({
            "action": "updated",
            "id": id,
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
        let dir = tempdir().expect("tempdir");
        let scheduler = Arc::new(cron_scheduler_for_data_dir(dir.path().to_path_buf()));
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
    async fn create_response_includes_next_run_display() {
        let backend = make_backend();
        let created = backend
            .create(
                Some("walk-reminder"),
                "3m",
                "Remind user to walk",
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("create");
        let created_v: serde_json::Value = serde_json::from_str(&created).expect("json");
        assert_eq!(created_v.get("action").and_then(|v| v.as_str()), Some("created"));
        let next_run = created_v
            .get("next_run")
            .and_then(|v| v.as_str())
            .expect("next_run");
        assert!(DateTime::parse_from_rfc3339(next_run).is_ok());
        let display = created_v
            .get("next_run_display")
            .and_then(|v| v.as_str())
            .expect("next_run_display");
        assert!(display.contains(" at "));
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
                Some("consumer"),
                "0 * * * *",
                "consume context",
                None,
                None,
                None,
                None,
                Some(&json!(source_id.clone())),
                None,
                None,
                None,
                None,
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
                None,
                None,
                None,
                None,
                None,
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
                Some(&json!([])),
                None,
                None,
                None,
                None,
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
}
