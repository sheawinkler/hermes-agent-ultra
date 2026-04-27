//! Cron scheduler engine.
//!
//! The `CronScheduler` manages the lifecycle of cron jobs: creation, listing,
//! updating, pausing, resuming, removal, and execution. It runs a background
//! loop that polls for due jobs (default every 60s, overridable via
//! `HERMES_CRON_TICK_SECS`) and dispatches them to the `CronRunner`.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::Utc;
use hermes_core::{AgentResult, MessageRole};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::time::{self, Duration};

use crate::completion::CronCompletionEvent;
use crate::job::{CronJob, JobStatus};
use crate::persistence::JobPersistence;
use crate::runner::CronRunner;

/// Background poll interval for due jobs. Default **60** seconds.
///
/// Override with **`HERMES_CRON_TICK_SECS`** (integer **1–300**) for integration tests
/// or local debugging; values outside the range are clamped.
fn cron_poll_interval() -> Duration {
    let secs = std::env::var("HERMES_CRON_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(secs)
}

const MAX_CONTEXT_CHARS: usize = 8_000;
const MAX_STORED_OUTPUT_CHARS: usize = 32_000;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

fn latest_assistant_output(result: &AgentResult) -> Option<String> {
    result.messages.iter().rev().find_map(|m| {
        if m.role == MessageRole::Assistant {
            m.content.clone().and_then(|c| {
                let trimmed = c.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            })
        } else {
            None
        }
    })
}

fn is_valid_context_job_ref(job_id: &str) -> bool {
    !job_id.is_empty()
        && job_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn build_context_prefix_for_job(job: &CronJob, jobs: &HashMap<String, CronJob>) -> Option<String> {
    let Some(context_refs) = job.context_from.as_ref() else {
        return None;
    };

    let mut sections = Vec::new();
    for source_job_id in context_refs {
        if !is_valid_context_job_ref(source_job_id) {
            tracing::warn!("context_from: skipping invalid job_id {:?}", source_job_id);
            continue;
        }
        let Some(source_job) = jobs.get(source_job_id) else {
            continue;
        };
        let Some(latest_output) = source_job
            .last_output
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        let mut output = latest_output.to_string();
        if output.chars().count() > MAX_CONTEXT_CHARS {
            output = format!(
                "{}\n\n[... output truncated ...]",
                truncate_chars(latest_output, MAX_CONTEXT_CHARS)
            );
        }

        sections.push(format!(
            "## Output from job '{}'\nThe following is the most recent output from a preceding cron job. Use it as context for your analysis.\n\n```\n{}\n```",
            source_job_id, output
        ));
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

// ---------------------------------------------------------------------------
// CronError
// ---------------------------------------------------------------------------

/// Errors that can occur during cron scheduler operations.
#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("Job not found: {0}")]
    JobNotFound(String),

    #[error("Invalid job: {0}")]
    InvalidJob(String),

    #[error("Job already exists: {0}")]
    JobAlreadyExists(String),

    #[error("Job is paused: {0}")]
    JobPaused(String),

    #[error("Job is completed: {0}")]
    JobCompleted(String),

    #[error("Agent error: {0}")]
    Agent(#[from] hermes_core::AgentError),

    #[error("Persistence error: {0}")]
    Persistence(String),

    #[error("Scheduler error: {0}")]
    Scheduler(String),
}

// ---------------------------------------------------------------------------
// CronScheduler
// ---------------------------------------------------------------------------

/// The cron scheduler manages cron jobs and runs a background loop that
/// dispatches due jobs to the runner.
pub struct CronScheduler {
    /// In-memory job store.
    jobs: Arc<Mutex<HashMap<String, CronJob>>>,
    /// Persistence backend.
    persistence: Arc<dyn JobPersistence>,
    /// Job runner.
    runner: Arc<CronRunner>,
    /// Optional broadcast of job completion (e.g. gateway HTTP webhooks).
    completion_tx: Option<broadcast::Sender<CronCompletionEvent>>,
    /// Notification handle to stop the scheduler loop.
    stop_notify: Arc<Notify>,
    /// Whether the scheduler loop is running.
    running: Arc<Mutex<bool>>,
}

impl CronScheduler {
    /// Create a new cron scheduler.
    pub fn new(persistence: Arc<dyn JobPersistence>, runner: Arc<CronRunner>) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            persistence,
            runner,
            completion_tx: None,
            stop_notify: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Receive [`CronCompletionEvent`] on every finished run (scheduled or manual).
    pub fn set_completion_broadcast(&mut self, tx: broadcast::Sender<CronCompletionEvent>) {
        self.completion_tx = Some(tx);
    }

    fn emit_completion(
        tx: &Option<broadcast::Sender<CronCompletionEvent>>,
        job: &CronJob,
        trigger: &'static str,
        outcome: Result<&AgentResult, String>,
    ) {
        let Some(sender) = tx else {
            return;
        };
        let ev = CronCompletionEvent::new(job, trigger, outcome);
        if let Err(e) = sender.send(ev) {
            tracing::debug!("cron completion broadcast dropped: {}", e);
        }
    }

    /// Load persisted jobs into the scheduler.
    ///
    /// Call this before `start()` to re-register jobs from the previous session.
    pub async fn load_persisted_jobs(&self) -> Result<(), CronError> {
        let jobs = self
            .persistence
            .load_jobs()
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        let mut guard = self.jobs.lock().await;
        for job in jobs {
            tracing::info!(
                "Loaded persisted job '{}' ({})",
                job.name.as_deref().unwrap_or(&job.id),
                job.id
            );
            guard.insert(job.id.clone(), job);
        }

        tracing::info!("Loaded {} persisted cron jobs", guard.len());
        Ok(())
    }

    /// Start the scheduler loop in the background.
    ///
    /// The loop sleeps (see [`cron_poll_interval`]) then dispatches due jobs.
    /// Returns immediately; the loop runs as a spawned tokio task.
    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        if *running {
            tracing::warn!("Scheduler is already running");
            return;
        }
        *running = true;
        drop(running);

        tracing::info!("Starting cron scheduler");

        let jobs = self.jobs.clone();
        let runner = self.runner.clone();
        let persistence = self.persistence.clone();
        let completion_tx = self.completion_tx.clone();
        let stop_notify = self.stop_notify.clone();
        let running_flag = self.running.clone();

        tokio::spawn(async move {
            loop {
                // Check if we should stop
                if !*running_flag.lock().await {
                    break;
                }

                // Wait for next poll tick or stop signal
                tokio::select! {
                    _ = time::sleep(cron_poll_interval()) => {
                        // Tick: check for due jobs
                    }
                    _ = stop_notify.notified() => {
                        tracing::info!("Scheduler received stop signal");
                        break;
                    }
                }

                let now = Utc::now();
                let mut guard = jobs.lock().await;
                let due_job_ids: Vec<String> = guard
                    .iter()
                    .filter(|(_, job)| job.is_due(now))
                    .map(|(id, _)| id.clone())
                    .collect();

                for job_id in due_job_ids {
                    let job = guard.get(&job_id).cloned();
                    if let Some(mut job) = job {
                        let mut runnable_job = job.clone();
                        if let Some(ctx_prefix) = build_context_prefix_for_job(&job, &guard) {
                            runnable_job.prompt =
                                format!("{}\n\n{}", ctx_prefix, runnable_job.prompt);
                        }
                        drop(guard);

                        // Run the job
                        tracing::info!(
                            "Executing cron job '{}' ({})",
                            job.name.as_deref().unwrap_or(&job.id),
                            job.id
                        );
                        match runner.run_job(&runnable_job).await {
                            Ok(result) => {
                                tracing::info!(
                                    "Cron job '{}' completed successfully (turns: {})",
                                    job.id,
                                    result.total_turns
                                );
                                Self::emit_completion(
                                    &completion_tx,
                                    &job,
                                    "schedule",
                                    Ok(&result),
                                );
                                job.mark_executed(now);
                                job.last_output = latest_assistant_output(&result)
                                    .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
                            }
                            Err(e) => {
                                tracing::error!("Cron job '{}' failed: {}", job.id, e);
                                Self::emit_completion(
                                    &completion_tx,
                                    &job,
                                    "schedule",
                                    Err(e.to_string()),
                                );
                                job.mark_failed();
                            }
                        }

                        // Update the job in memory and persist
                        guard = jobs.lock().await;
                        guard.insert(job.id.clone(), job.clone());
                        drop(guard);

                        if let Err(e) = persistence.save_job(&job).await {
                            tracing::error!("Failed to persist job '{}': {}", job.id, e);
                        }

                        guard = jobs.lock().await;
                    }
                }
            }

            tracing::info!("Scheduler loop exited");
        });
    }

    /// Stop the scheduler loop.
    pub async fn stop(&self) {
        let mut running = self.running.lock().await;
        if !*running {
            tracing::warn!("Scheduler is not running");
            return;
        }
        *running = false;
        drop(running);

        self.stop_notify.notify_waiters();
        tracing::info!("Cron scheduler stopped");
    }

    /// Check if the scheduler is currently running.
    pub async fn is_running(&self) -> bool {
        *self.running.lock().await
    }

    /// Create a new cron job.
    ///
    /// Validates the job, generates a UUID if needed, computes the next run
    /// time, persists it, and registers it in memory.
    pub async fn create_job(&self, mut job: CronJob) -> Result<String, CronError> {
        // Validate
        job.validate().map_err(CronError::InvalidJob)?;

        // Generate UUID if empty
        if job.id.is_empty() {
            job.id = uuid::Uuid::new_v4().to_string();
        }

        let id = job.id.clone();
        let schedule = job.schedule.clone();

        // Check for duplicate
        {
            let guard = self.jobs.lock().await;
            if guard.contains_key(&id) {
                return Err(CronError::JobAlreadyExists(id));
            }
        }

        // Compute next_run if not set
        if job.next_run.is_none() && job.status == JobStatus::Active {
            job.next_run = job.compute_next_run(Utc::now());
        }

        // Persist
        self.persistence
            .save_job(&job)
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        // Register in memory
        {
            let mut guard = self.jobs.lock().await;
            guard.insert(id.clone(), job);
        }

        tracing::info!("Created cron job '{}' ({})", id, schedule);
        Ok(id)
    }

    /// List all registered cron jobs.
    pub async fn list_jobs(&self) -> Vec<CronJob> {
        let guard = self.jobs.lock().await;
        guard.values().cloned().collect()
    }

    /// Get a specific job by ID.
    pub async fn get_job(&self, id: &str) -> Option<CronJob> {
        let guard = self.jobs.lock().await;
        guard.get(id).cloned()
    }

    /// Update an existing cron job.
    pub async fn update_job(&self, id: &str, updated: CronJob) -> Result<(), CronError> {
        // Validate the updated job
        updated.validate().map_err(CronError::InvalidJob)?;

        let mut guard = self.jobs.lock().await;
        if !guard.contains_key(id) {
            return Err(CronError::JobNotFound(id.to_string()));
        }

        // Ensure the ID stays consistent
        let mut job = updated;
        job.id = id.to_string();

        // Recompute next_run if schedule changed
        if job.status == JobStatus::Active && job.next_run.is_none() {
            job.next_run = job.compute_next_run(Utc::now());
        }

        // Persist
        self.persistence
            .save_job(&job)
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        guard.insert(id.to_string(), job);
        Ok(())
    }

    /// Pause a cron job.
    pub async fn pause_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        let job = guard
            .get_mut(id)
            .ok_or_else(|| CronError::JobNotFound(id.to_string()))?;

        if job.status == JobStatus::Paused {
            return Err(CronError::JobPaused(id.to_string()));
        }
        if job.status == JobStatus::Completed {
            return Err(CronError::JobCompleted(id.to_string()));
        }

        job.status = JobStatus::Paused;
        drop(guard);

        // Persist the change
        if let Some(job) = self.get_job(id).await {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))?;
        }

        tracing::info!("Paused cron job '{}'", id);
        Ok(())
    }

    /// Resume a paused cron job.
    pub async fn resume_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        let job = guard
            .get_mut(id)
            .ok_or_else(|| CronError::JobNotFound(id.to_string()))?;

        if job.status != JobStatus::Paused {
            return Err(CronError::Scheduler(format!(
                "Job '{}' is not paused (status: {})",
                id, job.status
            )));
        }

        job.status = JobStatus::Active;
        job.next_run = job.compute_next_run(Utc::now());
        drop(guard);

        // Persist the change
        if let Some(job) = self.get_job(id).await {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))?;
        }

        tracing::info!("Resumed cron job '{}'", id);
        Ok(())
    }

    /// Remove a cron job.
    pub async fn remove_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        if guard.remove(id).is_none() {
            return Err(CronError::JobNotFound(id.to_string()));
        }
        drop(guard);

        // Delete from persistence
        self.persistence
            .delete_job(id)
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        tracing::info!("Removed cron job '{}'", id);
        Ok(())
    }

    /// Manually trigger a cron job to run immediately.
    ///
    /// The job is executed regardless of its schedule or status (except
    /// Completed jobs). The run count is not incremented for manual triggers.
    pub async fn run_job(&self, id: &str) -> Result<AgentResult, CronError> {
        let (job, runnable_job) = {
            let guard = self.jobs.lock().await;
            let job = guard
                .get(id)
                .cloned()
                .ok_or_else(|| CronError::JobNotFound(id.to_string()))?;
            let mut runnable_job = job.clone();
            if let Some(ctx_prefix) = build_context_prefix_for_job(&runnable_job, &guard) {
                runnable_job.prompt = format!("{}\n\n{}", ctx_prefix, runnable_job.prompt);
            }
            (job, runnable_job)
        };

        if job.status == JobStatus::Completed {
            return Err(CronError::JobCompleted(id.to_string()));
        }

        tracing::info!("Manually triggering cron job '{}'", id);
        let run_result = self.runner.run_job(&runnable_job).await;
        match &run_result {
            Ok(result) => Self::emit_completion(&self.completion_tx, &job, "manual", Ok(result)),
            Err(e) => {
                Self::emit_completion(&self.completion_tx, &job, "manual", Err(e.to_string()))
            }
        }
        let result = run_result?;

        // Update last_run but don't increment run_count for manual triggers
        {
            let mut guard = self.jobs.lock().await;
            if let Some(j) = guard.get_mut(id) {
                j.last_run = Some(Utc::now());
                j.last_output = latest_assistant_output(&result)
                    .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
                if j.status == JobStatus::Failed {
                    // Reset status to Active on successful manual run
                    j.status = JobStatus::Active;
                }
            }
        }

        // Persist the updated job
        if let Some(job) = self.get_job(id).await {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::tempdir;

    use crate::cli_support::cron_scheduler_for_data_dir;
    use crate::job::CronJob;

    fn make_test_scheduler() -> CronScheduler {
        let dir = tempdir().expect("tempdir");
        cron_scheduler_for_data_dir(dir.path().to_path_buf())
    }

    #[tokio::test]
    async fn test_create_job_validates() {
        let mut job = CronJob::new("", "");
        job.prompt = "".to_string();
        // Validation should fail for empty schedule
        assert!(job.validate().is_err());
    }

    #[tokio::test]
    async fn test_job_status_active_by_default() {
        let job = CronJob::new("* * * * *", "test");
        assert_eq!(job.status, JobStatus::Active);
    }

    #[tokio::test]
    async fn test_scheduler_create_job_roundtrip() {
        let sched = make_test_scheduler();
        let job = CronJob::new("0 * * * *", "hello");
        let id = sched.create_job(job).await.expect("create");
        let loaded = sched.get_job(&id).await.expect("get");
        assert_eq!(loaded.prompt, "hello");
        assert_eq!(sched.list_jobs().await.len(), 1);
    }

    #[test]
    fn test_build_context_prefix_injects_recent_output() {
        let source = CronJob {
            last_output: Some("Latest digest".to_string()),
            ..CronJob::new("0 * * * *", "collect")
        };
        let target = CronJob {
            context_from: Some(vec![source.id.clone()]),
            ..CronJob::new("0 * * * *", "summarize")
        };
        let mut jobs = HashMap::new();
        jobs.insert(source.id.clone(), source.clone());
        jobs.insert(target.id.clone(), target.clone());

        let prefix = build_context_prefix_for_job(&target, &jobs).expect("prefix");
        assert!(prefix.contains("Output from job"));
        assert!(prefix.contains("Latest digest"));
    }

    #[test]
    fn test_build_context_prefix_silent_when_no_output() {
        let source = CronJob::new("0 * * * *", "collect");
        let target = CronJob {
            context_from: Some(vec![source.id.clone()]),
            ..CronJob::new("0 * * * *", "summarize")
        };
        let mut jobs = HashMap::new();
        jobs.insert(source.id.clone(), source.clone());
        jobs.insert(target.id.clone(), target.clone());

        assert!(build_context_prefix_for_job(&target, &jobs).is_none());
    }

    #[test]
    fn test_build_context_prefix_skips_invalid_job_ids() {
        let target = CronJob {
            context_from: Some(vec!["../../../etc/passwd".to_string()]),
            ..CronJob::new("0 * * * *", "summarize")
        };
        let jobs = HashMap::new();
        assert!(build_context_prefix_for_job(&target, &jobs).is_none());
    }
}
