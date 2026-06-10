//! Cron execution timing: delivery lead time and ping (zero-LLM) reminders.

use chrono::{DateTime, Duration, Utc};
use hermes_core::format_wall_datetime_precise;

use crate::job::CronJob;
use crate::schedule::{ScheduleSpec, parse_schedule};

const DEFAULT_DELIVERY_LEAD_SECS: i64 = 15;
const MAX_DELIVERY_LEAD_SECS: i64 = 300;

/// Seconds to start execution before `next_run` so LLM + gateway delivery lands on time.
///
/// Override with **`HERMES_CRON_DELIVERY_LEAD_SECS`** (0–300). Ping reminders use **0**.
pub fn cron_delivery_lead_seconds() -> i64 {
    std::env::var("HERMES_CRON_DELIVERY_LEAD_SECS")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(DEFAULT_DELIVERY_LEAD_SECS)
        .clamp(0, MAX_DELIVERY_LEAD_SECS)
}

/// One-shot reminder jobs with no skills/tools — deliver task text directly (no LLM).
pub fn is_ping_reminder(job: &CronJob) -> bool {
    if job.no_agent {
        return false;
    }
    if job.script.as_ref().is_some_and(|s| !s.trim().is_empty()) {
        return false;
    }
    if job.skills.as_ref().is_some_and(|s| !s.is_empty()) {
        return false;
    }
    if job.context_from.as_ref().is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if job.enabled_toolsets.as_ref().is_some_and(|v| !v.is_empty()) {
        return false;
    }
    if job.workdir.as_ref().is_some_and(|s| !s.trim().is_empty()) {
        return false;
    }
    if job.profile.as_ref().is_some_and(|s| !s.trim().is_empty()) {
        return false;
    }
    if job.prompt.trim().is_empty() || job.prompt.chars().count() > 2000 {
        return false;
    }
    let spec = job
        .schedule_spec
        .clone()
        .or_else(|| parse_schedule(&job.schedule).ok());
    if !matches!(spec, Some(ScheduleSpec::Once { .. })) {
        return false;
    }
    reminder_like_prompt(&job.prompt)
}

fn reminder_like_prompt(prompt: &str) -> bool {
    let lower = prompt.to_ascii_lowercase();
    [
        "remind",
        "reminder",
        "alert",
        "notify",
        "ping",
        "nudge",
        "提醒",
        "记得",
        "别忘了",
        "通知",
    ]
    .iter()
    .any(|k| lower.contains(k))
}

/// Lead seconds subtracted from `next_run` before the runner starts.
pub fn execution_lead_seconds(job: &CronJob) -> i64 {
    if is_ping_reminder(job) {
        0
    } else {
        cron_delivery_lead_seconds()
    }
}

/// When the scheduler should start running this job (may be before user-facing `next_run`).
pub fn execution_fire_at(job: &CronJob) -> Option<DateTime<Utc>> {
    job.next_run
        .map(|nr| nr - Duration::seconds(execution_lead_seconds(job)))
}

/// Format a ping reminder for direct IM delivery.
pub fn format_ping_reminder_text(prompt: &str) -> String {
    let body = user_facing_reminder_text(prompt.trim());
    if body.starts_with('⏰') {
        body
    } else {
        format!("⏰ {body}")
    }
}

fn user_facing_reminder_text(prompt: &str) -> String {
    if prompt.is_empty() {
        return String::new();
    }
    let lower = prompt.to_ascii_lowercase();
    for prefix in [
        "remind the user to ",
        "remind user to ",
        "remind the user that ",
        "remind user that ",
        "remind the user ",
        "notify the user to ",
        "提醒用户",
        "提醒：",
        "提醒:",
    ] {
        if lower.starts_with(prefix) {
            let rest = prompt[prefix.len()..].trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    prompt.to_string()
}

/// Label for cron execution path (`ping` / `script` / `agent`).
pub fn execution_mode_label(job: &CronJob) -> &'static str {
    if job.no_agent {
        "script"
    } else if is_ping_reminder(job) {
        "ping"
    } else {
        "agent"
    }
}

fn optional_wall(dt: Option<DateTime<Utc>>) -> Option<String> {
    dt.map(format_wall_datetime_precise)
}

/// Scheduler loop woke and is about to scan for due jobs.
pub fn log_scheduler_tick(
    now: DateTime<Utc>,
    wake_reason: &str,
    sleep_secs: u64,
    active_jobs: usize,
    due_count: usize,
) {
    tracing::info!(
        event = "cron.tick",
        wake_reason,
        sleep_secs,
        active_jobs,
        due_count,
        now_utc = %now.to_rfc3339(),
        now_wall = %format_wall_datetime_precise(now),
        "cron scheduler tick"
    );
}

/// A job matched `is_due` and is being dispatched to the runner.
pub fn log_job_trigger(job: &CronJob, now: DateTime<Utc>, trigger: &str) {
    let fire_at = execution_fire_at(job);
    let fire_late_ms = fire_at.map(|f| (now - f).num_milliseconds());
    let target_late_ms = job.next_run.map(|nr| (now - nr).num_milliseconds());
    tracing::info!(
        event = "cron.trigger",
        trigger,
        job_id = %job.id,
        job_name = job.name.as_deref().unwrap_or(&job.id),
        schedule = %job.schedule,
        execution_mode = execution_mode_label(job),
        lead_seconds = execution_lead_seconds(job),
        next_run_utc = job.next_run.as_ref().map(|t| t.to_rfc3339()).as_deref(),
        next_run_wall = optional_wall(job.next_run).as_deref(),
        execution_fire_utc = fire_at.as_ref().map(|t| t.to_rfc3339()).as_deref(),
        execution_fire_wall = optional_wall(fire_at).as_deref(),
        fire_late_ms,
        target_late_ms,
        now_utc = %now.to_rfc3339(),
        now_wall = %format_wall_datetime_precise(now),
        "cron job triggered"
    );
}

/// Runner started executing a job.
pub fn log_job_execute_start(job: &CronJob, now: DateTime<Utc>) {
    tracing::info!(
        event = "cron.execute.start",
        job_id = %job.id,
        job_name = job.name.as_deref().unwrap_or(&job.id),
        execution_mode = execution_mode_label(job),
        now_utc = %now.to_rfc3339(),
        now_wall = %format_wall_datetime_precise(now),
        "cron job execution started"
    );
}

/// Runner finished (success or failure).
pub fn log_job_execute_finish(
    job: &CronJob,
    now: DateTime<Utc>,
    started_at: DateTime<Utc>,
    elapsed_ms: i64,
    turns: u32,
    delivery_error: Option<&str>,
    failed: bool,
) {
    let target_late_ms = job.next_run.map(|nr| (now - nr).num_milliseconds());
    if failed {
        tracing::error!(
            event = "cron.execute.finish",
            job_id = %job.id,
            job_name = job.name.as_deref().unwrap_or(&job.id),
            execution_mode = execution_mode_label(job),
            elapsed_ms,
            turns,
            delivery_error,
            target_late_ms,
            now_utc = %now.to_rfc3339(),
            now_wall = %format_wall_datetime_precise(now),
            started_utc = %started_at.to_rfc3339(),
            "cron job execution failed"
        );
    } else if delivery_error.is_some() {
        tracing::warn!(
            event = "cron.execute.finish",
            job_id = %job.id,
            job_name = job.name.as_deref().unwrap_or(&job.id),
            execution_mode = execution_mode_label(job),
            elapsed_ms,
            turns,
            delivery_error,
            target_late_ms,
            now_utc = %now.to_rfc3339(),
            now_wall = %format_wall_datetime_precise(now),
            started_utc = %started_at.to_rfc3339(),
            "cron job finished with delivery error"
        );
    } else {
        tracing::info!(
            event = "cron.execute.finish",
            job_id = %job.id,
            job_name = job.name.as_deref().unwrap_or(&job.id),
            execution_mode = execution_mode_label(job),
            elapsed_ms,
            turns,
            target_late_ms,
            now_utc = %now.to_rfc3339(),
            now_wall = %format_wall_datetime_precise(now),
            started_utc = %started_at.to_rfc3339(),
            "cron job execution finished"
        );
    }
}

/// Job persisted with a new schedule / next run.
pub fn log_job_registered(job: &CronJob, action: &str) {
    tracing::info!(
        event = "cron.register",
        action,
        job_id = %job.id,
        job_name = job.name.as_deref().unwrap_or(&job.id),
        schedule = %job.schedule,
        execution_mode = execution_mode_label(job),
        next_run_utc = job.next_run.as_ref().map(|t| t.to_rfc3339()).as_deref(),
        next_run_wall = optional_wall(job.next_run).as_deref(),
        execution_fire_utc = execution_fire_at(job).as_ref().map(|t| t.to_rfc3339()).as_deref(),
        execution_fire_wall = optional_wall(execution_fire_at(job)).as_deref(),
        lead_seconds = execution_lead_seconds(job),
        "cron job registered"
    );
}

/// Outbound IM delivery for a cron result.
pub fn log_job_delivery(
    job_id: &str,
    platform: &str,
    chat_id: &str,
    phase: &str,
    elapsed_ms: Option<i64>,
    error: Option<&str>,
) {
    match (phase, error) {
        ("start", _) => tracing::info!(
            event = "cron.delivery",
            phase,
            job_id,
            platform,
            chat_id,
            "cron delivery started"
        ),
        (_, Some(err)) => tracing::warn!(
            event = "cron.delivery",
            phase,
            job_id,
            platform,
            chat_id,
            elapsed_ms,
            error = err,
            "cron delivery failed"
        ),
        _ => tracing::info!(
            event = "cron.delivery",
            phase,
            job_id,
            platform,
            chat_id,
            elapsed_ms,
            "cron delivery finished"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CronJob;

    #[test]
    fn ping_reminder_detects_one_shot_remind_task() {
        let job = CronJob::new("2m", "Remind the user to drink water");
        assert!(is_ping_reminder(&job));
        assert_eq!(execution_lead_seconds(&job), 0);
    }

    #[test]
    fn ping_reminder_rejects_recurring() {
        let job = CronJob::new("every 2h", "Remind the user to stretch");
        assert!(!is_ping_reminder(&job));
    }

    #[test]
    fn ping_reminder_rejects_skill_jobs() {
        let mut job = CronJob::new("2m", "Remind the user to check feed");
        job.skills = Some(vec!["blogwatcher".to_string()]);
        assert!(!is_ping_reminder(&job));
    }

    #[test]
    fn format_ping_strips_remind_prefix() {
        assert_eq!(
            format_ping_reminder_text("Remind the user to drink water"),
            "⏰ drink water"
        );
    }

    #[test]
    fn execution_fire_at_applies_lead_for_agent_jobs() {
        let job = CronJob::new("every 2h", "Summarize overnight email");
        let nr = job.next_run.expect("next_run");
        let fire = execution_fire_at(&job).expect("fire");
        assert_eq!(
            (nr - fire).num_seconds(),
            cron_delivery_lead_seconds()
        );
    }
}
