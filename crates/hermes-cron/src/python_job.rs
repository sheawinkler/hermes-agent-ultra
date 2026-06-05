//! Map Python `jobs.json` records to [`CronJob`](crate::job::CronJob).

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::job::{deliver_target_from_str, CronJob, DeliverConfig, JobStatus, ModelConfig};
use crate::schedule::parse_schedule_value;

/// Job origin for `deliver: origin` (Python `origin` dict).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JobOrigin {
    pub platform: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
}

/// Convert one Python job dict from `jobs.json` into a [`CronJob`].
pub fn cron_job_from_python_value(raw: &Value) -> Result<CronJob, String> {
    let id = raw
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or("job missing id")?
        .to_string();

    let prompt = raw
        .get("prompt")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let schedule_val = raw.get("schedule").ok_or("job missing schedule")?;
    let schedule_spec = parse_schedule_value(schedule_val).map_err(|e| e.to_string())?;
    let schedule_display = raw
        .get("schedule_display")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| schedule_spec.display());

    let schedule = raw
        .get("schedule")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or(schedule_display.clone());

    let status = python_job_status(raw);
    let created_at =
        parse_time_field(raw.get("created_at")).unwrap_or_else(hermes_core::now_utc);
    let last_run = parse_time_field(raw.get("last_run_at"));
    let next_run = parse_time_field(raw.get("next_run_at"))
        .or_else(|| crate::schedule::compute_next_run(&schedule_spec, last_run));

    let repeat = raw
        .get("repeat")
        .and_then(|r| r.get("times"))
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let run_count = raw
        .get("repeat")
        .and_then(|r| r.get("completed"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    let deliver = raw
        .get("deliver")
        .and_then(|v| v.as_str())
        .and_then(parse_deliver_string);

    let origin = raw.get("origin").and_then(parse_origin);

    let skills = raw
        .get("skills")
        .and_then(|v| {
            if let Some(arr) = v.as_array() {
                Some(
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>(),
                )
            } else {
                None
            }
        })
        .filter(|v| !v.is_empty());

    let model = {
        let m = raw.get("model").and_then(|v| v.as_str());
        let p = raw.get("provider").and_then(|v| v.as_str());
        let b = raw.get("base_url").and_then(|v| v.as_str());
        if m.is_some() || p.is_some() || b.is_some() {
            Some(ModelConfig {
                model: m.map(|s| s.to_string()),
                provider: p.map(|s| s.to_string()),
                base_url: b.map(|s| s.to_string()),
            })
        } else {
            None
        }
    };

    Ok(CronJob {
        id,
        name: raw
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        schedule,
        schedule_display: Some(schedule_display),
        schedule_spec: Some(schedule_spec),
        prompt,
        skills,
        model,
        deliver,
        origin,
        status,
        created_at,
        last_run,
        next_run,
        repeat,
        run_count,
        script: raw
            .get("script")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        no_agent: raw.get("no_agent").and_then(|v| v.as_bool()).unwrap_or(false),
        script_timeout_seconds: None,
        script_shell: None,
        context_from: raw
            .get("context_from")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty()),
        enabled_toolsets: raw
            .get("enabled_toolsets")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
            })
            .filter(|v| !v.is_empty()),
        workdir: raw
            .get("workdir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        profile: raw
            .get("profile")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        last_output: raw
            .get("last_output")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        last_error: raw
            .get("last_error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        last_delivery_error: raw
            .get("last_delivery_error")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

fn python_job_status(raw: &Value) -> JobStatus {
    let enabled = raw.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let state = raw
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("scheduled");
    if !enabled || state == "paused" {
        return JobStatus::Paused;
    }
    if state == "completed" {
        return JobStatus::Completed;
    }
    if state == "error" {
        return JobStatus::Failed;
    }
    JobStatus::Active
}

use hermes_core::{ensure_aware_naive, ensure_aware_utc};

fn parse_time_field(v: Option<&Value>) -> Option<DateTime<Utc>> {
    let s = v?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(&s.replace('Z', "+00:00"))
        .ok()
        .map(|dt| ensure_aware_utc(dt.with_timezone(&Utc)))
        .or_else(|| {
            DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%z")
                .ok()
                .map(|dt| ensure_aware_utc(dt.with_timezone(&Utc)))
        })
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                .ok()
                .map(ensure_aware_naive)
        })
}

pub fn parse_deliver_string(raw: &str) -> Option<DeliverConfig> {
    let value = raw.trim().to_ascii_lowercase();
    let (target, platform) = if let Some((platform, rest)) = value.split_once(':') {
        let chat = rest.split(':').next().unwrap_or(rest).trim();
        let target = match platform {
            "telegram" | "discord" | "slack" | "wecom" | "weixin" | "feishu" | "dingtalk"
            | "matrix" | "signal" | "mattermost" | "email" | "whatsapp" | "sms" => platform,
            _ => return None,
        };
        (deliver_target_from_str(target)?, Some(chat.to_string()))
    } else {
        (deliver_target_from_str(&value)?, None)
    };
    Some(DeliverConfig { target, platform })
}

fn parse_origin(v: &Value) -> Option<JobOrigin> {
    let platform = v.get("platform").and_then(|x| x.as_str())?.to_string();
    Some(JobOrigin {
        platform,
        chat_id: v
            .get("chat_id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
        thread_id: v
            .get("thread_id")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string()),
    })
}

/// Load Python-shaped `jobs.json` wrapper.
pub fn load_python_jobs_file(contents: &str) -> Result<Vec<CronJob>, String> {
    let data: Value =
        serde_json::from_str(contents).map_err(|e| format!("jobs.json parse error: {e}"))?;
    let jobs = data
        .get("jobs")
        .and_then(|v| v.as_array())
        .ok_or("jobs.json missing jobs array")?;
    jobs.iter()
        .map(cron_job_from_python_value)
        .collect()
}
