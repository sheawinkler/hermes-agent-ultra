//! Events emitted when a cron job finishes (for gateway HTTP webhooks, metrics, etc.).

use chrono::Utc;
use hermes_core::{AgentResult, MessageRole};
use serde::Serialize;

use crate::job::CronJob;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

/// Payload broadcast after a scheduled or manual cron run completes.
#[derive(Debug, Clone, Serialize)]
pub struct CronCompletionEvent {
    /// Always `cron_job_finished`.
    pub event: &'static str,
    /// `schedule` or `manual`.
    pub trigger: &'static str,
    pub job_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_name: Option<String>,
    pub schedule: String,
    pub prompt: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_turns: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_snippet: Option<String>,
    pub finished_at: String,
}

impl CronCompletionEvent {
    /// Build a webhook-friendly completion record.
    pub fn new(
        job: &CronJob,
        trigger: &'static str,
        outcome: Result<&AgentResult, String>,
    ) -> Self {
        let finished_at = Utc::now().to_rfc3339();
        let (ok, error, total_turns, assistant_snippet) = match outcome {
            Ok(r) => {
                let snippet = r.messages.iter().rev().find_map(|m| {
                    if m.role == MessageRole::Assistant {
                        m.content.as_ref().map(|c| truncate_chars(c, 2000))
                    } else {
                        None
                    }
                });
                (true, None, Some(r.total_turns), snippet)
            }
            Err(msg) => (false, Some(msg), None, None),
        };
        Self {
            event: "cron_job_finished",
            trigger,
            job_id: job.id.clone(),
            job_name: job.name.clone(),
            schedule: job.schedule.clone(),
            prompt: truncate_chars(&job.prompt, 4096),
            ok,
            error,
            total_turns,
            assistant_snippet,
            finished_at,
        }
    }
}
