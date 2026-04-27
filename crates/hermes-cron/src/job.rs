//! Cron job definition and related types.

use chrono::{DateTime, Utc};
use cron::Schedule;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// JobStatus
// ---------------------------------------------------------------------------

/// Status of a cron job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Job is active and will be scheduled.
    Active,
    /// Job is paused and will not be scheduled.
    Paused,
    /// Job has completed its repeat count.
    Completed,
    /// Job failed during its last execution.
    Failed,
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JobStatus::Active => write!(f, "active"),
            JobStatus::Paused => write!(f, "paused"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
        }
    }
}

// ---------------------------------------------------------------------------
// ModelConfig
// ---------------------------------------------------------------------------

/// Optional model configuration override for a cron job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    /// Provider name override (e.g. "openai", "anthropic").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model name override (e.g. "gpt-4o", "claude-3-5-sonnet").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

// ---------------------------------------------------------------------------
// DeliverTarget / DeliverConfig
// ---------------------------------------------------------------------------

/// Target platform for delivering cron job results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverTarget {
    /// Return results to the origin (caller).
    Origin,
    /// Deliver locally (e.g. write to file, log).
    Local,
    /// Deliver via Telegram.
    Telegram,
    /// Deliver via Discord.
    Discord,
    /// Deliver via Slack.
    Slack,
    /// Deliver via Email.
    Email,
    /// Deliver via WhatsApp.
    WhatsApp,
    /// Deliver via Signal.
    Signal,
    /// Deliver via Matrix.
    Matrix,
    /// Deliver via Mattermost.
    Mattermost,
    /// Deliver via DingTalk.
    DingTalk,
    /// Deliver via Feishu.
    Feishu,
    /// Deliver via WeCom.
    WeCom,
    /// Deliver via WeChat.
    Weixin,
    /// Deliver via BlueBubbles (iMessage).
    BlueBubbles,
    /// Deliver via SMS.
    Sms,
    /// Deliver via Home Assistant.
    HomeAssistant,
}

/// Delivery configuration for a cron job's results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeliverConfig {
    /// The target platform for delivery.
    pub target: DeliverTarget,
    /// Platform-specific identifier (e.g. chat_id, channel, email address).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

// ---------------------------------------------------------------------------
// CronJob
// ---------------------------------------------------------------------------

/// A scheduled cron job that runs an agent prompt on a recurring basis.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronJob {
    /// Unique identifier (UUID).
    pub id: String,
    /// Human-readable name for this job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Cron expression schedule (e.g. "0 9 * * *" for 9am daily).
    pub schedule: String,
    /// The prompt to send to the agent when this job fires.
    pub prompt: String,
    /// Skills to load for the agent (by name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    /// Optional model configuration override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    /// Optional delivery configuration for results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver: Option<DeliverConfig>,
    /// Current status of the job.
    #[serde(default = "default_job_status")]
    pub status: JobStatus,
    /// When this job was created.
    pub created_at: DateTime<Utc>,
    /// When this job was last executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Utc>>,
    /// When this job is next scheduled to run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Utc>>,
    /// Maximum number of times to repeat this job (None = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<u32>,
    /// Number of times this job has been executed so far.
    #[serde(default)]
    pub run_count: u32,
    /// Optional script content to execute instead of an agent prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Optional source cron job IDs whose most recent output should be injected
    /// into this job prompt before execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_from: Option<Vec<String>>,
    /// Last assistant output captured from this job's most recent successful run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
}

fn default_job_status() -> JobStatus {
    JobStatus::Active
}

impl CronJob {
    /// Create a new CronJob with a generated UUID and the given schedule/prompt.
    pub fn new(schedule: impl Into<String>, prompt: impl Into<String>) -> Self {
        let schedule = schedule.into();
        let now = Utc::now();
        let next_run = Self::parse_next_run(&schedule, now);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: None,
            schedule,
            prompt: prompt.into(),
            skills: None,
            model: None,
            deliver: None,
            status: JobStatus::Active,
            created_at: now,
            last_run: None,
            next_run,
            repeat: None,
            run_count: 0,
            script: None,
            context_from: None,
            last_output: None,
        }
    }

    /// Validate the cron job definition.
    ///
    /// Checks:
    /// - The schedule is a valid cron expression
    /// - The prompt is non-empty (or a script is provided)
    /// - The repeat value is not zero
    pub fn validate(&self) -> Result<(), String> {
        // Validate schedule
        if self.schedule.trim().is_empty() {
            return Err("Schedule expression cannot be empty".to_string());
        }
        if Self::parse_next_run(&self.schedule, Utc::now()).is_none() {
            return Err(format!(
                "Invalid cron schedule expression: '{}'",
                self.schedule
            ));
        }

        // Validate prompt or script
        if self.prompt.trim().is_empty()
            && self.script.as_ref().map_or(true, |s| s.trim().is_empty())
        {
            return Err("Either prompt or script must be non-empty".to_string());
        }

        // Validate repeat
        if let Some(repeat) = self.repeat {
            if repeat == 0 {
                return Err("Repeat count must be greater than zero if specified".to_string());
            }
        }

        Ok(())
    }

    /// Compute the next run time based on the cron schedule, starting from `after`.
    pub fn compute_next_run(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        Self::parse_next_run(&self.schedule, after)
    }

    /// Parse a cron expression and return the next fire time after `after`.
    ///
    /// The `cron` crate requires 7-field format (sec min hour dom month dow year).
    /// If the user provides a 5-field expression (min hour dom month dow), we
    /// automatically prepend "0 " (run at second 0) and append " *" (any year).
    fn parse_next_run(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let normalized = Self::normalize_cron_expr(schedule);
        match normalized.parse::<Schedule>() {
            Ok(sched) => sched.after(&after).next().map(|dt: DateTime<Utc>| dt),
            Err(e) => {
                tracing::warn!("Failed to parse cron expression '{}': {}", normalized, e);
                None
            }
        }
    }

    /// Normalize a cron expression to 7-field format.
    ///
    /// - 5 fields: `min hour dom month dow` -> `0 min hour dom month dow *`
    /// - 6 fields: `sec min hour dom month dow` -> `sec min hour dom month dow *`
    /// - 7 fields: already correct, return as-is
    fn normalize_cron_expr(expr: &str) -> String {
        let parts: Vec<&str> = expr.trim().split_whitespace().collect();
        match parts.len() {
            5 => format!("0 {} *", expr.trim()),
            6 => format!("{} *", expr.trim()),
            7 => expr.trim().to_string(),
            _ => expr.trim().to_string(),
        }
    }

    /// Check whether this job is due to run at the given time.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if self.status != JobStatus::Active {
            return false;
        }
        match self.next_run {
            Some(next) => now >= next,
            None => false,
        }
    }

    /// Mark the job as having just been run: increment run_count, set last_run,
    /// and compute next_run. Returns false if the job has reached its repeat limit.
    pub fn mark_executed(&mut self, now: DateTime<Utc>) -> bool {
        self.run_count += 1;
        self.last_run = Some(now);

        // Check repeat limit
        if let Some(repeat) = self.repeat {
            if self.run_count >= repeat {
                self.status = JobStatus::Completed;
                self.next_run = None;
                return false;
            }
        }

        // Schedule next run
        self.next_run = self.compute_next_run(now);
        true
    }

    /// Mark the job as failed.
    pub fn mark_failed(&mut self) {
        self.status = JobStatus::Failed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_new() {
        let job = CronJob::new("0 9 * * *", "Say hello");
        assert_eq!(job.status, JobStatus::Active);
        assert_eq!(job.run_count, 0);
        assert!(job.next_run.is_some());
        assert!(job.id.len() > 0);
    }

    #[test]
    fn test_validate_valid() {
        let job = CronJob::new("0 9 * * *", "Say hello");
        assert!(job.validate().is_ok());
    }

    #[test]
    fn test_validate_empty_schedule() {
        let mut job = CronJob::new("", "Say hello");
        job.schedule = "".to_string();
        assert!(job.validate().is_err());
    }

    #[test]
    fn test_validate_empty_prompt() {
        let mut job = CronJob::new("0 9 * * *", "");
        job.prompt = "".to_string();
        assert!(job.validate().is_err());
    }

    #[test]
    fn test_validate_zero_repeat() {
        let mut job = CronJob::new("0 9 * * *", "Say hello");
        job.repeat = Some(0);
        assert!(job.validate().is_err());
    }

    #[test]
    fn test_is_due() {
        let job = CronJob::new("* * * * *", "Say hello");
        // A minutely job: next_run is in the next minute.
        // is_due returns true when now >= next_run, so it might not be due yet
        // if we just created it. Instead, verify that next_run is set and is
        // in the near future.
        assert!(job.next_run.is_some());
        let next = job.next_run.unwrap();
        // next_run should be within the next 2 minutes
        let diff = next - Utc::now();
        assert!(
            diff.num_seconds() >= 0 && diff.num_seconds() <= 120,
            "next_run should be within the next 2 minutes, got {}s",
            diff.num_seconds()
        );
    }

    #[test]
    fn test_is_due_paused() {
        let mut job = CronJob::new("* * * * *", "Say hello");
        job.status = JobStatus::Paused;
        assert!(!job.is_due(Utc::now()));
    }

    #[test]
    fn test_mark_executed_increments() {
        let mut job = CronJob::new("* * * * *", "Say hello");
        let now = Utc::now();
        let result = job.mark_executed(now);
        assert!(result);
        assert_eq!(job.run_count, 1);
        assert_eq!(job.last_run, Some(now));
        assert!(job.next_run.is_some());
    }

    #[test]
    fn test_mark_executed_repeat_limit() {
        let mut job = CronJob::new("* * * * *", "Say hello");
        job.repeat = Some(1);
        let now = Utc::now();
        let result = job.mark_executed(now);
        assert!(!result);
        assert_eq!(job.status, JobStatus::Completed);
        assert!(job.next_run.is_none());
    }

    #[test]
    fn test_mark_failed() {
        let mut job = CronJob::new("* * * * *", "Say hello");
        job.mark_failed();
        assert_eq!(job.status, JobStatus::Failed);
    }

    #[test]
    fn test_job_status_display() {
        assert_eq!(JobStatus::Active.to_string(), "active");
        assert_eq!(JobStatus::Paused.to_string(), "paused");
        assert_eq!(JobStatus::Completed.to_string(), "completed");
        assert_eq!(JobStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn test_serde_roundtrip() {
        let job = CronJob::new("0 9 * * *", "Say hello");
        let json = serde_json::to_string(&job).unwrap();
        let parsed: CronJob = serde_json::from_str(&json).unwrap();
        assert_eq!(job, parsed);
    }
}
