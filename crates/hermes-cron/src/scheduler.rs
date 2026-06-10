//! Cron scheduler engine.
//!
//! The `CronScheduler` manages the lifecycle of cron jobs: creation, listing,
//! updating, pausing, resuming, removal, and execution. It runs a background
//! loop that polls for due jobs (default every 60s, overridable via
//! `HERMES_CRON_TICK_SECS`) and dispatches them to the `CronRunner`.

use std::collections::HashMap;
use std::sync::Arc;

use hermes_core::{now_utc, AgentResult, MessageRole};
use tokio::sync::{broadcast, Mutex, Notify};
use tokio::time::{self, Duration};

use crate::completion::CronCompletionEvent;
use crate::job::{CronJob, JobStatus};
use crate::persistence::JobPersistence;
use crate::runner::{CronRunOutcome, CronRunner};
use crate::schedule::{
    advance_next_run_before_execute, compute_next_run as schedule_compute_next_run,
    normalize_schedule_input, parse_duration, parse_schedule,
};
use crate::timing::{log_job_registered, log_job_trigger, log_scheduler_tick};

/// Maximum idle poll interval when no due jobs are imminent. Default **60** seconds.
///
/// Override with **`HERMES_CRON_TICK_SECS`** (integer **1–300**) for integration tests
/// or local debugging; values outside the range are clamped.
fn cron_max_poll_interval() -> Duration {
    let secs = std::env::var("HERMES_CRON_TICK_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60)
        .clamp(1, 300);
    Duration::from_secs(secs)
}

const CRON_MIN_POLL_SECS: u64 = 1;

/// Compute how long the scheduler loop should sleep before the next due-job check.
///
/// When an active job has `next_run` soon, sleep only until that instant (capped by
/// [`cron_max_poll_interval`] for long-range schedules). This avoids the up-to-60s
/// alignment skew of a fixed poll cadence when jobs are created mid-interval.
fn cron_sleep_duration(
    earliest_next_run: Option<chrono::DateTime<chrono::Utc>>,
    max_poll: Duration,
) -> Duration {
    let min_poll = Duration::from_secs(CRON_MIN_POLL_SECS);
    let Some(next) = earliest_next_run else {
        return max_poll;
    };
    let now = now_utc();
    if now >= next {
        return Duration::ZERO;
    }
    let remaining_secs = (next - now).num_seconds().max(0) as u64;
    let capped = remaining_secs.min(max_poll.as_secs()).max(CRON_MIN_POLL_SECS);
    Duration::from_secs(capped).max(min_poll)
}

fn earliest_active_execution_fire(
    jobs: &HashMap<String, CronJob>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    jobs.values()
        .filter(|job| job.status == JobStatus::Active)
        .filter_map(|job| crate::timing::execution_fire_at(job))
        .min()
}

/// Relative duration / interval schedules must be anchored at job registration time.
fn should_refresh_schedule_at_registration(schedule: &str) -> bool {
    let normalized = normalize_schedule_input(schedule);
    if normalized.to_ascii_lowercase().starts_with("every ") {
        return true;
    }
    parse_duration(&normalized).is_ok()
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
    /// Wake the scheduler loop early (new/updated job with sooner `next_run`).
    tick_notify: Arc<Notify>,
    /// Whether the scheduler loop is running.
    running: Arc<Mutex<bool>>,
    /// Generation counter: incremented on structural changes (remove_job).
    /// The scheduler loop checks this after job execution to avoid
    /// re-inserting a job that was removed while it was running.
    generation: Arc<Mutex<u64>>,
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
            tick_notify: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(false)),
            generation: Arc::new(Mutex::new(0)),
        }
    }

    /// Wake the background loop so it recomputes sleep until the next due job.
    fn wake_scheduler(&self) {
        self.tick_notify.notify_one();
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
        for mut job in jobs {
            job.normalize_schedule();
            job.refresh_next_run();
            tracing::debug!(
                job_id = %job.id,
                job_name = job.name.as_deref().unwrap_or(&job.id),
                "Loaded persisted cron job"
            );
            guard.insert(job.id.clone(), job);
        }

        tracing::info!(job_count = guard.len(), "Loaded persisted cron jobs");
        self.wake_scheduler();
        Ok(())
    }

    /// Start the scheduler loop in the background.
    ///
    /// The loop sleeps until the earliest active `next_run` (see [`cron_sleep_duration`])
    /// then dispatches due jobs. Returns immediately; the loop runs as a spawned task.
    pub async fn start(&self) {
        let mut running = self.running.lock().await;
        if *running {
            tracing::warn!("Scheduler is already running");
            return;
        }
        *running = true;
        drop(running);

        tracing::info!(event = "cron.start", "Starting cron scheduler");

        let jobs = self.jobs.clone();
        let runner = self.runner.clone();
        let persistence = self.persistence.clone();
        let completion_tx = self.completion_tx.clone();
        let stop_notify = self.stop_notify.clone();
        let tick_notify = self.tick_notify.clone();
        let running_flag = self.running.clone();
        let generation = self.generation.clone();

        self.wake_scheduler();

        tokio::spawn(async move {
            let max_poll = cron_max_poll_interval();
            loop {
                // Check if we should stop
                if !*running_flag.lock().await {
                    break;
                }

                let sleep_for = {
                    let guard = jobs.lock().await;
                    cron_sleep_duration(earliest_active_execution_fire(&guard), max_poll)
                };

                // Wait until the next due job, a schedule change, or stop.
                let wake_reason: &str;
                tokio::select! {
                    _ = time::sleep(sleep_for) => {
                        wake_reason = "timer";
                    }
                    _ = tick_notify.notified() => {
                        wake_reason = "schedule_change";
                    }
                    _ = stop_notify.notified() => {
                        tracing::info!(event = "cron.stop", "cron scheduler stopping");
                        break;
                    }
                }

                let now = now_utc();
                let mut guard = jobs.lock().await;
                let active_jobs = guard.values().filter(|j| j.status == JobStatus::Active).count();
                let mut tick_dirty = false;
                for job in guard.values_mut() {
                    if job.prepare_for_tick(now) {
                        tick_dirty = true;
                    }
                }
                if tick_dirty {
                    let snapshot: Vec<CronJob> = guard.values().cloned().collect();
                    drop(guard);
                    for job in snapshot {
                        if let Err(e) = persistence.save_job(&job).await {
                            tracing::error!("Failed to persist tick-adjusted job '{}': {}", job.id, e);
                        }
                    }
                    guard = jobs.lock().await;
                }
                let due_job_ids: Vec<String> = guard
                    .iter()
                    .filter(|(_, job)| job.is_due(now))
                    .map(|(id, _)| id.clone())
                    .collect();

                log_scheduler_tick(
                    now,
                    wake_reason,
                    sleep_for.as_secs(),
                    active_jobs,
                    due_job_ids.len(),
                );

                // Phase 1: pre-process all due jobs while holding the lock.
                // Advance next_run timestamps and build context-enriched runnables.
                // Jobs with workdir/profile mutate process-global env vars (TERMINAL_CWD,
                // HERMES_HOME) so they must run serially; all others can run concurrently.
                let mut parallel_queue: Vec<(CronJob, CronJob)> = Vec::new();
                let mut sequential_queue: Vec<(CronJob, CronJob)> = Vec::new();

                for job_id in &due_job_ids {
                    let Some(mut job) = guard.get(job_id).cloned() else {
                        continue;
                    };
                    if let Some(spec) = job.schedule_spec.clone() {
                        if let Some(advanced) =
                            advance_next_run_before_execute(&spec, job.next_run, now)
                        {
                            job.next_run = Some(advanced);
                            guard.insert(job.id.clone(), job.clone());
                        }
                    }
                    let mut runnable = job.clone();
                    if let Some(ctx_prefix) = build_context_prefix_for_job(&job, &guard) {
                        runnable.prompt = format!("{}\n\n{}", ctx_prefix, runnable.prompt);
                    }
                    let needs_serial = job
                        .workdir
                        .as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .is_some()
                        || job
                            .profile
                            .as_deref()
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .is_some();
                    if needs_serial {
                        sequential_queue.push((job, runnable));
                    } else {
                        parallel_queue.push((job, runnable));
                    }
                }
                drop(guard);

                // Persist all advanced next_run timestamps before any job runs.
                for (job, _) in parallel_queue.iter().chain(sequential_queue.iter()) {
                    if let Err(e) = persistence.save_job(job).await {
                        tracing::error!(
                            "Failed to persist advanced next_run for '{}': {}",
                            job.id,
                            e
                        );
                    }
                }

                // Phase 2: spawn independent (no workdir/profile) jobs concurrently.
                let mut parallel_handles: Vec<
                    tokio::task::JoinHandle<(
                        CronJob,
                        Result<CronRunOutcome, crate::scheduler::CronError>,
                        u64,
                    )>,
                > = Vec::new();
                for (original, runnable) in parallel_queue {
                    let runner_clone = runner.clone();
                    let gen_before = *generation.lock().await;
                    log_job_trigger(&runnable, now, "schedule");
                    parallel_handles.push(tokio::spawn(async move {
                        let outcome = runner_clone.run_job(&runnable).await;
                        (original, outcome, gen_before)
                    }));
                }

                // Phase 3: run workdir/profile jobs serially (env-mutation safety).
                for (mut job, runnable) in sequential_queue {
                    let gen_before = *generation.lock().await;
                    log_job_trigger(&runnable, now, "schedule");
                    match runner.run_job(&runnable).await {
                        Ok(outcome) => {
                            Self::emit_completion(
                                &completion_tx,
                                &job,
                                "schedule",
                                Ok(&outcome.result),
                            );
                            job.mark_executed(now);
                            job.last_output = latest_assistant_output(&outcome.result)
                                .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
                            job.last_delivery_error = outcome.delivery_error;
                        }
                        Err(e) => {
                            tracing::error!("Cron job '{}' failed: {}", job.id, e);
                            let err_msg = e.to_string();
                            job.last_delivery_error =
                                runner.delivery_error_for_failure(&job, &err_msg).await;
                            if let Some(ref del_err) = job.last_delivery_error {
                                tracing::warn!(
                                    "Cron job '{}' failed to deliver error alert: {}",
                                    job.id,
                                    del_err
                                );
                            }
                            Self::emit_completion(
                                &completion_tx,
                                &job,
                                "schedule",
                                Err(err_msg),
                            );
                            job.mark_failed();
                        }
                    }
                    let gen_after = *generation.lock().await;
                    if gen_after != gen_before {
                        tracing::info!(
                            "Skipping re-insert of job '{}' — removed during execution",
                            job.id
                        );
                        continue;
                    }
                    {
                        let mut g = jobs.lock().await;
                        g.insert(job.id.clone(), job.clone());
                    }
                    if let Err(e) = persistence.save_job(&job).await {
                        tracing::error!("Failed to persist job '{}': {}", job.id, e);
                    }
                }

                // Phase 4: collect results from concurrent jobs.
                for handle in parallel_handles {
                    match handle.await {
                        Ok((mut job, Ok(outcome), gen_before)) => {
                            Self::emit_completion(
                                &completion_tx,
                                &job,
                                "schedule",
                                Ok(&outcome.result),
                            );
                            job.mark_executed(now);
                            job.last_output = latest_assistant_output(&outcome.result)
                                .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
                            job.last_delivery_error = outcome.delivery_error;
                            let gen_after = *generation.lock().await;
                            if gen_after != gen_before {
                                tracing::info!(
                                    "Skipping re-insert of job '{}' — removed during execution",
                                    job.id
                                );
                                continue;
                            }
                            {
                                let mut g = jobs.lock().await;
                                g.insert(job.id.clone(), job.clone());
                            }
                            if let Err(e) = persistence.save_job(&job).await {
                                tracing::error!("Failed to persist job '{}': {}", job.id, e);
                            }
                        }
                        Ok((mut job, Err(e), gen_before)) => {
                            tracing::error!("Cron job '{}' failed: {}", job.id, e);
                            let err_msg = e.to_string();
                            job.last_delivery_error =
                                runner.delivery_error_for_failure(&job, &err_msg).await;
                            if let Some(ref del_err) = job.last_delivery_error {
                                tracing::warn!(
                                    "Cron job '{}' failed to deliver error alert: {}",
                                    job.id,
                                    del_err
                                );
                            }
                            Self::emit_completion(
                                &completion_tx,
                                &job,
                                "schedule",
                                Err(err_msg),
                            );
                            job.mark_failed();
                            let gen_after = *generation.lock().await;
                            if gen_after != gen_before {
                                tracing::info!(
                                    "Skipping re-insert of job '{}' — removed during execution",
                                    job.id
                                );
                                continue;
                            }
                            {
                                let mut g = jobs.lock().await;
                                g.insert(job.id.clone(), job.clone());
                            }
                            if let Err(e) = persistence.save_job(&job).await {
                                tracing::error!("Failed to persist job '{}': {}", job.id, e);
                            }
                        }
                        Err(e) => {
                            tracing::error!("Cron parallel task panicked: {}", e);
                        }
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

        // Anchor relative schedules (e.g. `2m`, `every 30m`) to registration time.
        job.normalize_schedule();
        if job.status == JobStatus::Active && should_refresh_schedule_at_registration(&schedule) {
            if let Ok(spec) = parse_schedule(&job.schedule) {
                job.next_run = schedule_compute_next_run(&spec, None);
                job.schedule_spec = Some(spec);
            }
        } else if job.next_run.is_none() && job.status == JobStatus::Active {
            job.next_run = job.compute_next_run(now_utc());
        }

        // Persist
        self.persistence
            .save_job(&job)
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        // Register in memory
        log_job_registered(&job, "create");
        {
            let mut guard = self.jobs.lock().await;
            guard.insert(id.clone(), job);
        }
        self.wake_scheduler();

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

        let mut job = updated;
        job.id = id.to_string();
        job.normalize_schedule();
        if job.status == JobStatus::Active {
            job.refresh_next_run();
        }

        // Persist
        self.persistence
            .save_job(&job)
            .await
            .map_err(|e| CronError::Persistence(e.to_string()))?;

        guard.insert(id.to_string(), job);
        self.wake_scheduler();
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
        job.next_run = job.compute_next_run(now_utc());
        drop(guard);

        // Persist the change
        if let Some(job) = self.get_job(id).await {
            self.persistence
                .save_job(&job)
                .await
                .map_err(|e| CronError::Persistence(e.to_string()))?;
        }

        tracing::info!("Resumed cron job '{}'", id);
        self.wake_scheduler();
        Ok(())
    }

    /// Remove a cron job.
    pub async fn remove_job(&self, id: &str) -> Result<(), CronError> {
        let mut guard = self.jobs.lock().await;
        if guard.remove(id).is_none() {
            return Err(CronError::JobNotFound(id.to_string()));
        }
        // Bump generation so the scheduler loop knows the map was structurally
        // modified and must not re-insert a stale clone of this job.
        *self.generation.lock().await += 1;
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
        log_job_trigger(&job, now_utc(), "manual");
        let run_result = self.runner.run_job(&runnable_job).await;
        let delivery_error = match &run_result {
            Ok(outcome) => {
                Self::emit_completion(
                    &self.completion_tx,
                    &job,
                    "manual",
                    Ok(&outcome.result),
                );
                outcome.delivery_error.clone()
            }
            Err(e) => {
                let err_msg = e.to_string();
                let delivery_error = self
                    .runner
                    .delivery_error_for_failure(&job, &err_msg)
                    .await;
                if let Some(ref del_err) = delivery_error {
                    tracing::warn!(
                        "Cron job '{}' failed to deliver manual error alert: {}",
                        job.id,
                        del_err
                    );
                }
                Self::emit_completion(&self.completion_tx, &job, "manual", Err(err_msg));
                delivery_error
            }
        };
        let outcome = run_result?;

        // Update last_run but don't increment run_count for manual triggers
        {
            let mut guard = self.jobs.lock().await;
            if let Some(j) = guard.get_mut(id) {
                j.last_run = Some(now_utc());
                j.last_output = latest_assistant_output(&outcome.result)
                    .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
                j.last_delivery_error = delivery_error;
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

        Ok(outcome.result)
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
    fn cron_sleep_duration_zero_when_due() {
        let past = now_utc() - chrono::Duration::minutes(1);
        assert_eq!(
            cron_sleep_duration(Some(past), Duration::from_secs(60)),
            Duration::ZERO
        );
    }

    #[test]
    fn cron_sleep_duration_matches_short_reminder() {
        let next = now_utc() + chrono::Duration::seconds(90);
        let sleep = cron_sleep_duration(Some(next), Duration::from_secs(60));
        assert!(sleep >= Duration::from_secs(1));
        assert!(sleep <= Duration::from_secs(60));
    }

    #[test]
    fn cron_sleep_duration_falls_back_without_jobs() {
        assert_eq!(
            cron_sleep_duration(None, Duration::from_secs(45)),
            Duration::from_secs(45)
        );
    }

    #[test]
    fn should_refresh_relative_duration_schedules() {
        assert!(should_refresh_schedule_at_registration("2m"));
        assert!(should_refresh_schedule_at_registration("every 30m"));
        assert!(!should_refresh_schedule_at_registration("0 9 * * *"));
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
