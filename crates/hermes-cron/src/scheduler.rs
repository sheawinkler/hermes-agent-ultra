//! Cron scheduler engine.
//!
//! The `CronScheduler` manages the lifecycle of cron jobs: creation, listing,
//! updating, pausing, resuming, removal, and execution. It runs a background
//! loop that polls for due jobs (default every 60s, overridable via
//! `HERMES_CRON_TICK_SECS`) and dispatches them to the `CronRunner`.

use std::collections::{HashMap, HashSet};
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

#[async_trait::async_trait]
trait CronJobExecutor: Send + Sync {
    async fn run_job(&self, job: &CronJob) -> Result<AgentResult, CronError>;
    async fn deliver_error(
        &self,
        error_text: &str,
        deliver: &crate::job::DeliverConfig,
    ) -> Result<(), CronError>;
}

#[async_trait::async_trait]
impl CronJobExecutor for CronRunner {
    async fn run_job(&self, job: &CronJob) -> Result<AgentResult, CronError> {
        CronRunner::run_job(self, job).await
    }

    async fn deliver_error(
        &self,
        error_text: &str,
        deliver: &crate::job::DeliverConfig,
    ) -> Result<(), CronError> {
        CronRunner::deliver_error(self, error_text, deliver).await
    }
}

struct ScheduledCronRun {
    job: CronJob,
    runnable_job: CronJob,
}

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
    runner: Arc<dyn CronJobExecutor>,
    /// Optional broadcast of job completion (e.g. gateway HTTP webhooks).
    completion_tx: Option<broadcast::Sender<CronCompletionEvent>>,
    /// Notification handle to stop the scheduler loop.
    stop_notify: Arc<Notify>,
    /// Whether the scheduler loop is running.
    running: Arc<Mutex<bool>>,
    /// Job IDs currently executing. Due ticks skip these instead of queuing
    /// duplicate runs behind a long task.
    running_job_ids: Arc<Mutex<HashSet<String>>>,
    /// Jobs with per-run process-global context (currently workdir) are
    /// serialized without blocking the scheduler tick.
    sequential_run_lock: Arc<Mutex<()>>,
}

impl CronScheduler {
    /// Create a new cron scheduler.
    pub fn new(persistence: Arc<dyn JobPersistence>, runner: Arc<CronRunner>) -> Self {
        Self::new_with_executor(persistence, runner)
    }

    fn new_with_executor(
        persistence: Arc<dyn JobPersistence>,
        runner: Arc<dyn CronJobExecutor>,
    ) -> Self {
        Self {
            jobs: Arc::new(Mutex::new(HashMap::new())),
            persistence,
            runner,
            completion_tx: None,
            stop_notify: Arc::new(Notify::new()),
            running: Arc::new(Mutex::new(false)),
            running_job_ids: Arc::new(Mutex::new(HashSet::new())),
            sequential_run_lock: Arc::new(Mutex::new(())),
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

    fn requires_sequential_run(job: &CronJob) -> bool {
        job.workdir
            .as_deref()
            .map(str::trim)
            .is_some_and(|s| !s.is_empty())
    }

    async fn execute_with_optional_sequential_guard(
        runner: Arc<dyn CronJobExecutor>,
        sequential_run_lock: Arc<Mutex<()>>,
        job: CronJob,
    ) -> Result<AgentResult, CronError> {
        if Self::requires_sequential_run(&job) {
            let _guard = sequential_run_lock.lock().await;
            runner.run_job(&job).await
        } else {
            runner.run_job(&job).await
        }
    }

    async fn mark_running_if_idle(
        running_job_ids: &Arc<Mutex<HashSet<String>>>,
        job_id: &str,
    ) -> bool {
        let mut running = running_job_ids.lock().await;
        running.insert(job_id.to_string())
    }

    async fn clear_running(running_job_ids: &Arc<Mutex<HashSet<String>>>, job_id: &str) {
        running_job_ids.lock().await.remove(job_id);
    }

    async fn finish_scheduled_job(
        jobs: Arc<Mutex<HashMap<String, CronJob>>>,
        persistence: Arc<dyn JobPersistence>,
        completion_tx: Option<broadcast::Sender<CronCompletionEvent>>,
        runner: Arc<dyn CronJobExecutor>,
        mut job: CronJob,
        scheduled_at: chrono::DateTime<Utc>,
        run_result: Result<AgentResult, CronError>,
    ) {
        match run_result {
            Ok(result) => {
                tracing::info!(
                    "Cron job '{}' completed successfully (turns: {})",
                    job.id,
                    result.total_turns
                );
                Self::emit_completion(&completion_tx, &job, "schedule", Ok(&result));
                job.mark_executed(scheduled_at);
                job.last_output = latest_assistant_output(&result)
                    .map(|s| truncate_chars(&s, MAX_STORED_OUTPUT_CHARS));
            }
            Err(e) => {
                tracing::error!("Cron job '{}' failed: {}", job.id, e);
                if let Some(ref deliver) = job.deliver {
                    if let Err(deliver_err) = runner.deliver_error(&e.to_string(), deliver).await {
                        tracing::warn!(
                            "Cron job '{}' failed to deliver error alert: {}",
                            job.id,
                            deliver_err
                        );
                    }
                }
                Self::emit_completion(&completion_tx, &job, "schedule", Err(e.to_string()));
                job.mark_failed();
            }
        }

        {
            let mut guard = jobs.lock().await;
            guard.insert(job.id.clone(), job.clone());
        }

        if let Err(e) = persistence.save_job(&job).await {
            tracing::error!("Failed to persist job '{}': {}", job.id, e);
        }
    }

    async fn collect_due_runs(
        jobs: Arc<Mutex<HashMap<String, CronJob>>>,
        running_job_ids: Arc<Mutex<HashSet<String>>>,
        now: chrono::DateTime<Utc>,
    ) -> Vec<ScheduledCronRun> {
        let guard = jobs.lock().await;
        let mut running = running_job_ids.lock().await;
        let mut due = Vec::new();

        for (job_id, job) in guard.iter().filter(|(_, job)| job.is_due(now)) {
            if running.contains(job_id) {
                tracing::info!(
                    "Cron job '{}' already running; skipping duplicate scheduled dispatch",
                    job.name.as_deref().unwrap_or(job_id)
                );
                continue;
            }

            running.insert(job_id.clone());
            let mut runnable_job = job.clone();
            if let Some(ctx_prefix) = build_context_prefix_for_job(job, &guard) {
                runnable_job.prompt = format!("{}\n\n{}", ctx_prefix, runnable_job.prompt);
            }
            due.push(ScheduledCronRun {
                job: job.clone(),
                runnable_job,
            });
        }

        due
    }

    async fn tick_due_jobs_from_parts(
        jobs: Arc<Mutex<HashMap<String, CronJob>>>,
        runner: Arc<dyn CronJobExecutor>,
        persistence: Arc<dyn JobPersistence>,
        completion_tx: Option<broadcast::Sender<CronCompletionEvent>>,
        running_job_ids: Arc<Mutex<HashSet<String>>>,
        sequential_run_lock: Arc<Mutex<()>>,
    ) -> usize {
        let now = Utc::now();
        let due_jobs = Self::collect_due_runs(jobs.clone(), running_job_ids.clone(), now).await;
        let dispatched = due_jobs.len();

        for due_run in due_jobs {
            let job_id = due_run.job.id.clone();
            tracing::info!(
                "Dispatching cron job '{}' ({})",
                due_run.job.name.as_deref().unwrap_or(&due_run.job.id),
                due_run.job.id
            );

            let jobs = jobs.clone();
            let runner = runner.clone();
            let persistence = persistence.clone();
            let completion_tx = completion_tx.clone();
            let running_job_ids = running_job_ids.clone();
            let sequential_run_lock = sequential_run_lock.clone();

            tokio::spawn(async move {
                let job = due_run.job;
                let runnable_job = due_run.runnable_job;
                let run_result = Self::execute_with_optional_sequential_guard(
                    runner.clone(),
                    sequential_run_lock,
                    runnable_job,
                )
                .await;

                Self::finish_scheduled_job(
                    jobs,
                    persistence,
                    completion_tx,
                    runner,
                    job,
                    now,
                    run_result,
                )
                .await;
                Self::clear_running(&running_job_ids, &job_id).await;
            });
        }

        dispatched
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
        let running_job_ids = self.running_job_ids.clone();
        let sequential_run_lock = self.sequential_run_lock.clone();

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

                Self::tick_due_jobs_from_parts(
                    jobs.clone(),
                    runner.clone(),
                    persistence.clone(),
                    completion_tx.clone(),
                    running_job_ids.clone(),
                    sequential_run_lock.clone(),
                )
                .await;
            }

            tracing::info!("Scheduler loop exited");
        });
    }

    /// Dispatch currently due jobs without waiting for them to complete.
    ///
    /// Due jobs are marked in-flight before their background task is spawned, so
    /// later ticks do not enqueue duplicate runs while a prior invocation is
    /// still active.
    pub async fn tick_due_jobs(&self) -> usize {
        Self::tick_due_jobs_from_parts(
            self.jobs.clone(),
            self.runner.clone(),
            self.persistence.clone(),
            self.completion_tx.clone(),
            self.running_job_ids.clone(),
            self.sequential_run_lock.clone(),
        )
        .await
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
        job.normalize_paths().map_err(CronError::InvalidJob)?;
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
    pub async fn update_job(&self, id: &str, mut updated: CronJob) -> Result<(), CronError> {
        updated.normalize_paths().map_err(CronError::InvalidJob)?;
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
        if !Self::mark_running_if_idle(&self.running_job_ids, id).await {
            return Err(CronError::Scheduler(format!("Job already running: {id}")));
        }

        tracing::info!("Manually triggering cron job '{}'", id);
        let outcome: Result<AgentResult, CronError> = async {
            let run_result = Self::execute_with_optional_sequential_guard(
                self.runner.clone(),
                self.sequential_run_lock.clone(),
                runnable_job,
            )
            .await;
            match &run_result {
                Ok(result) => {
                    Self::emit_completion(&self.completion_tx, &job, "manual", Ok(result))
                }
                Err(e) => {
                    if let Some(ref deliver) = job.deliver {
                        if let Err(deliver_err) =
                            self.runner.deliver_error(&e.to_string(), deliver).await
                        {
                            tracing::warn!(
                                "Cron job '{}' failed to deliver manual error alert: {}",
                                job.id,
                                deliver_err
                            );
                        }
                    }
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
        .await;
        Self::clear_running(&self.running_job_ids, id).await;
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::time::Duration as StdDuration;

    use chrono::Duration as ChronoDuration;
    use hermes_core::Message;
    use tempfile::tempdir;
    use tokio::sync::Semaphore;
    use tokio::time::sleep;

    use crate::cli_support::cron_scheduler_for_data_dir;
    use crate::job::CronJob;
    use crate::persistence::FileJobPersistence;

    fn make_test_scheduler() -> CronScheduler {
        let dir = tempdir().expect("tempdir");
        cron_scheduler_for_data_dir(dir.path().to_path_buf())
    }

    struct BlockingTestExecutor {
        started: AtomicUsize,
        completed: AtomicUsize,
        release: Semaphore,
        prompts: StdMutex<Vec<String>>,
    }

    impl Default for BlockingTestExecutor {
        fn default() -> Self {
            Self {
                started: AtomicUsize::new(0),
                completed: AtomicUsize::new(0),
                release: Semaphore::new(0),
                prompts: StdMutex::new(Vec::new()),
            }
        }
    }

    impl BlockingTestExecutor {
        fn record_prompt(&self, prompt: &str) {
            let mut prompts = self.prompts.lock().expect("prompts lock");
            prompts.push(prompt.to_string());
        }

        fn recorded_prompts(&self) -> Vec<String> {
            self.prompts.lock().expect("prompts lock").clone()
        }
    }

    #[async_trait::async_trait]
    impl CronJobExecutor for BlockingTestExecutor {
        async fn run_job(&self, job: &CronJob) -> Result<AgentResult, CronError> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.record_prompt(&job.prompt);
            let permit = self
                .release
                .acquire()
                .await
                .map_err(|e| CronError::Scheduler(format!("test semaphore closed: {e}")))?;
            drop(permit);
            self.completed.fetch_add(1, Ordering::SeqCst);
            Ok(AgentResult {
                messages: vec![Message::assistant(format!("finished {}", job.id))],
                finished_naturally: true,
                total_turns: 1,
                ..AgentResult::default()
            })
        }

        async fn deliver_error(
            &self,
            _error_text: &str,
            _deliver: &crate::job::DeliverConfig,
        ) -> Result<(), CronError> {
            Ok(())
        }
    }

    fn make_executor_scheduler(
        executor: Arc<BlockingTestExecutor>,
    ) -> (CronScheduler, tempfile::TempDir) {
        let dir = tempdir().expect("tempdir");
        let persistence = Arc::new(FileJobPersistence::with_dir(dir.path().to_path_buf()));
        (CronScheduler::new_with_executor(persistence, executor), dir)
    }

    fn due_test_job(prompt: &str) -> CronJob {
        let mut job = CronJob::new("*/5 * * * * *", prompt);
        job.next_run = Some(Utc::now() - ChronoDuration::seconds(5));
        job
    }

    async fn wait_for_count(counter: &AtomicUsize, expected: usize) {
        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                if counter.load(Ordering::SeqCst) >= expected {
                    return;
                }
                sleep(StdDuration::from_millis(10)).await;
            }
        })
        .await
        .expect("counter reached expected value");
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

    #[tokio::test]
    async fn test_scheduler_create_job_normalizes_workdir() {
        let sched = make_test_scheduler();
        let dir = tempdir().expect("workdir");
        let mut job = CronJob::new("0 * * * *", "hello");
        job.workdir = Some(dir.path().to_string_lossy().to_string());

        let id = sched.create_job(job).await.expect("create");
        let loaded = sched.get_job(&id).await.expect("get");
        assert_eq!(
            loaded.workdir,
            Some(
                std::fs::canonicalize(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn test_scheduler_rejects_invalid_workdir() {
        let sched = make_test_scheduler();
        let mut job = CronJob::new("0 * * * *", "hello");
        job.workdir = Some("relative/path".to_string());

        let err = sched.create_job(job).await.expect_err("invalid workdir");
        assert!(err.to_string().contains("absolute path"));
    }

    #[tokio::test]
    async fn tick_due_jobs_dispatches_without_waiting_for_completion() {
        let executor = Arc::new(BlockingTestExecutor::default());
        let (sched, _dir) = make_executor_scheduler(executor.clone());
        let id = sched
            .create_job(due_test_job("slow"))
            .await
            .expect("create due job");

        let dispatched = sched.tick_due_jobs().await;
        assert_eq!(dispatched, 1);
        wait_for_count(&executor.started, 1).await;
        assert_eq!(executor.completed.load(Ordering::SeqCst), 0);

        executor.release.add_permits(1);
        wait_for_count(&executor.completed, 1).await;
        let completed = sched.get_job(&id).await.expect("job");
        assert_eq!(completed.run_count, 1);
        assert!(completed
            .last_output
            .as_deref()
            .is_some_and(|out| out.contains("finished")));
    }

    #[tokio::test]
    async fn tick_due_jobs_skips_duplicate_while_job_is_running() {
        let executor = Arc::new(BlockingTestExecutor::default());
        let (sched, _dir) = make_executor_scheduler(executor.clone());
        sched
            .create_job(due_test_job("dedupe"))
            .await
            .expect("create due job");

        assert_eq!(sched.tick_due_jobs().await, 1);
        wait_for_count(&executor.started, 1).await;

        assert_eq!(sched.tick_due_jobs().await, 0);
        assert_eq!(executor.started.load(Ordering::SeqCst), 1);

        executor.release.add_permits(1);
        wait_for_count(&executor.completed, 1).await;
    }

    #[tokio::test]
    async fn tick_due_jobs_serializes_workdir_jobs_without_blocking_dispatch() {
        let executor = Arc::new(BlockingTestExecutor::default());
        let (sched, _dir) = make_executor_scheduler(executor.clone());
        let workdir = tempdir().expect("workdir");
        for prompt in ["first", "second"] {
            let mut job = due_test_job(prompt);
            job.workdir = Some(workdir.path().to_string_lossy().to_string());
            sched.create_job(job).await.expect("create due workdir job");
        }

        let dispatched = sched.tick_due_jobs().await;
        assert_eq!(dispatched, 2);
        wait_for_count(&executor.started, 1).await;
        sleep(StdDuration::from_millis(50)).await;
        assert_eq!(
            executor.started.load(Ordering::SeqCst),
            1,
            "second workdir job must wait for the sequential run lock"
        );

        executor.release.add_permits(1);
        wait_for_count(&executor.started, 2).await;
        executor.release.add_permits(1);
        wait_for_count(&executor.completed, 2).await;
    }

    #[tokio::test]
    async fn tick_due_jobs_injects_context_without_persisting_augmented_prompt() {
        let executor = Arc::new(BlockingTestExecutor::default());
        let (sched, _dir) = make_executor_scheduler(executor.clone());
        let mut source = CronJob::new("0 * * * *", "collect");
        source.last_output = Some("latest source output".to_string());
        let source_id = source.id.clone();
        sched.create_job(source).await.expect("create source job");

        let mut target = due_test_job("summarize");
        target.context_from = Some(vec![source_id]);
        let target_id = sched.create_job(target).await.expect("create target job");

        assert_eq!(sched.tick_due_jobs().await, 1);
        wait_for_count(&executor.started, 1).await;
        executor.release.add_permits(1);
        wait_for_count(&executor.completed, 1).await;

        let prompts = executor.recorded_prompts();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("latest source output"));

        let stored = sched.get_job(&target_id).await.expect("stored target");
        assert_eq!(stored.prompt, "summarize");
        assert!(!stored.prompt.contains("latest source output"));
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
