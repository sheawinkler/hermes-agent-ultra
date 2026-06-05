//! Cron job definition and related types.

use chrono::{DateTime, Utc};
use hermes_core::now_utc;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::python_job::JobOrigin;
use crate::schedule::{
    compute_next_run, fast_forward_if_stale, normalize_schedule_input, parse_schedule, ScheduleSpec,
};

// ---------------------------------------------------------------------------
// JobStatus
// ---------------------------------------------------------------------------

/// Status of a cron job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Active,
    Paused,
    Completed,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

// ---------------------------------------------------------------------------
// DeliverTarget / DeliverConfig
// ---------------------------------------------------------------------------

/// Delivery platform slug — JSON uses Python/gateway names (`wecom`, `dingtalk`), not Rust `snake_case` (`we_com`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverTarget {
    Origin,
    Local,
    Telegram,
    Discord,
    Slack,
    Email,
    WhatsApp,
    Signal,
    Matrix,
    Mattermost,
    DingTalk,
    Feishu,
    WeCom,
    Weixin,
    BlueBubbles,
    Sms,
    HomeAssistant,
}

impl DeliverTarget {
    /// Canonical on-disk / Python `deliver` string (see `cron/scheduler.py` `_KNOWN_DELIVERY_PLATFORMS`).
    pub fn as_str(self) -> &'static str {
        match self {
            DeliverTarget::Origin => "origin",
            DeliverTarget::Local => "local",
            DeliverTarget::Telegram => "telegram",
            DeliverTarget::Discord => "discord",
            DeliverTarget::Slack => "slack",
            DeliverTarget::Email => "email",
            DeliverTarget::WhatsApp => "whatsapp",
            DeliverTarget::Signal => "signal",
            DeliverTarget::Matrix => "matrix",
            DeliverTarget::Mattermost => "mattermost",
            DeliverTarget::DingTalk => "dingtalk",
            DeliverTarget::Feishu => "feishu",
            DeliverTarget::WeCom => "wecom",
            DeliverTarget::Weixin => "weixin",
            DeliverTarget::BlueBubbles => "bluebubbles",
            DeliverTarget::Sms => "sms",
            DeliverTarget::HomeAssistant => "homeassistant",
        }
    }
}

/// Parse Python-style `deliver` field (`"wecom"`, `"origin"`, …).
pub fn deliver_target_from_str(value: &str) -> Option<DeliverTarget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "origin" => Some(DeliverTarget::Origin),
        "local" => Some(DeliverTarget::Local),
        "telegram" => Some(DeliverTarget::Telegram),
        "discord" => Some(DeliverTarget::Discord),
        "slack" => Some(DeliverTarget::Slack),
        "email" => Some(DeliverTarget::Email),
        "whatsapp" => Some(DeliverTarget::WhatsApp),
        "signal" => Some(DeliverTarget::Signal),
        "matrix" => Some(DeliverTarget::Matrix),
        "mattermost" => Some(DeliverTarget::Mattermost),
        "dingtalk" => Some(DeliverTarget::DingTalk),
        "feishu" => Some(DeliverTarget::Feishu),
        "wecom" | "wecom_callback" => Some(DeliverTarget::WeCom),
        "weixin" | "wechat" | "wx" => Some(DeliverTarget::Weixin),
        "bluebubbles" | "imessage" => Some(DeliverTarget::BlueBubbles),
        "sms" => Some(DeliverTarget::Sms),
        "homeassistant" | "ha" => Some(DeliverTarget::HomeAssistant),
        _ => None,
    }
}

impl Serialize for DeliverTarget {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for DeliverTarget {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        deliver_target_from_str(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "unknown deliver target '{}'; expected a platform slug like wecom, telegram, origin",
                raw
            ))
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliverConfig {
    pub target: DeliverTarget,
    pub platform: Option<String>,
}

impl DeliverConfig {
    pub fn new(target: DeliverTarget) -> Self {
        Self {
            target,
            platform: None,
        }
    }
}

impl Serialize for DeliverConfig {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = serializer.serialize_struct("DeliverConfig", 2)?;
        st.serialize_field("target", &self.target)?;
        if let Some(ref platform) = self.platform {
            st.serialize_field("platform", platform)?;
        }
        st.end()
    }
}

impl<'de> Deserialize<'de> for DeliverConfig {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            target: DeliverTarget,
            #[serde(default)]
            platform: Option<String>,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self {
            target: raw.target,
            platform: raw.platform,
        })
    }
}

/// Python stores `deliver` as a string; Rust CLI may use `{ "target": "wecom", "platform": "..." }`.
fn deserialize_deliver_opt<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DeliverConfig>, D::Error> {
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if let Some((platform, chat)) = trimmed.split_once(':') {
                let target = deliver_target_from_str(platform).ok_or_else(|| {
                    serde::de::Error::custom(format!("unknown deliver target '{platform}'"))
                })?;
                let chat_id = chat.split(':').next().unwrap_or(chat).trim().to_string();
                return Ok(Some(DeliverConfig {
                    target,
                    platform: Some(chat_id),
                }));
            }
            let target = deliver_target_from_str(trimmed).ok_or_else(|| {
                serde::de::Error::custom(format!("unknown deliver target '{trimmed}'"))
            })?;
            Ok(Some(DeliverConfig::new(target)))
        }
        serde_json::Value::Object(_) => {
            let cfg: DeliverConfig = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Some(cfg))
        }
        _ => Err(serde::de::Error::custom(
            "deliver must be a string (Python) or object with target",
        )),
    }
}

fn serialize_deliver_opt<S: Serializer>(
    value: &Option<DeliverConfig>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        None => serializer.serialize_none(),
        Some(cfg) => cfg.serialize(serializer),
    }
}

// ---------------------------------------------------------------------------
// CronJob
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CronJob {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Display / legacy schedule string (e.g. `every 2h`, `0 9 * * *`).
    pub schedule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule_spec: Option<ScheduleSpec>,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelConfig>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_deliver_opt",
        serialize_with = "serialize_deliver_opt"
    )]
    pub deliver: Option<DeliverConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<JobOrigin>,
    #[serde(default = "default_job_status")]
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "last_run_at")]
    pub last_run: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "next_run_at")]
    pub next_run: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<u32>,
    #[serde(default)]
    pub run_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub no_agent: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_timeout_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_shell: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_from: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled_toolsets: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delivery_error: Option<String>,
}

fn default_job_status() -> JobStatus {
    JobStatus::Active
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl CronJob {
    pub fn new(schedule: impl Into<String>, prompt: impl Into<String>) -> Self {
        let schedule_str = normalize_schedule_input(&schedule.into());
        let spec = parse_schedule(&schedule_str).ok();
        let display = spec
            .as_ref()
            .map(|s| s.display())
            .unwrap_or_else(|| schedule_str.clone());
        let now = now_utc();
        let next_run = spec
            .as_ref()
            .and_then(|s| compute_next_run(s, None))
            .or_else(|| Self::legacy_parse_next_run(&schedule_str, now));
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: None,
            schedule: schedule_str,
            schedule_display: Some(display),
            schedule_spec: spec,
            prompt: prompt.into(),
            skills: None,
            model: None,
            deliver: None,
            origin: None,
            status: JobStatus::Active,
            created_at: now,
            last_run: None,
            next_run,
            repeat: None,
            run_count: 0,
            script: None,
            no_agent: false,
            script_timeout_seconds: None,
            script_shell: None,
            context_from: None,
            enabled_toolsets: None,
            workdir: None,
            profile: None,
            last_output: None,
            last_error: None,
            last_delivery_error: None,
        }
    }

    /// Ensure structured schedule exists (after load from disk).
    pub fn normalize_schedule(&mut self) {
        if self.schedule_spec.is_none() {
            if let Ok(spec) = parse_schedule(&self.schedule) {
                self.schedule_spec = Some(spec);
            }
        }
        if self.schedule_display.is_none() {
            self.schedule_display = self
                .schedule_spec
                .as_ref()
                .map(|s| s.display())
                .or(Some(self.schedule.clone()));
        }
    }

    /// Recompute `next_run` for active jobs (e.g. after load).
    pub fn refresh_next_run(&mut self) {
        if self.status != JobStatus::Active {
            return;
        }
        if let Some(spec) = self.schedule_spec.clone() {
            self.next_run = compute_next_run(&spec, self.last_run);
        }
    }

    pub fn schedule_spec(&self) -> Option<ScheduleSpec> {
        self.schedule_spec.clone()
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schedule.trim().is_empty() {
            return Err("Schedule expression cannot be empty".to_string());
        }
        let spec = self
            .schedule_spec
            .clone()
            .or_else(|| parse_schedule(&self.schedule).ok());
        if spec.is_none() && self.compute_next_run(now_utc()).is_none() {
            return Err(format!(
                "Invalid cron schedule expression: '{}'",
                self.schedule
            ));
        }
        if self.prompt.trim().is_empty()
            && self.script.as_ref().map_or(true, |s| s.trim().is_empty())
        {
            return Err("Either prompt or script must be non-empty".to_string());
        }
        if self.no_agent && self.script.as_ref().map_or(true, |s| s.trim().is_empty()) {
            return Err("no_agent mode requires non-empty script".to_string());
        }
        if let Some(repeat) = self.repeat {
            if repeat == 0 {
                return Err("Repeat count must be greater than zero if specified".to_string());
            }
        }
        Ok(())
    }

    pub fn compute_next_run(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        if let Some(spec) = self.schedule_spec.as_ref() {
            return compute_next_run(spec, self.last_run.or(Some(after)));
        }
        Self::legacy_parse_next_run(&self.schedule, after)
    }

    fn legacy_parse_next_run(schedule: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let normalized = crate::schedule::normalize_cron_expr(schedule);
        normalized
            .parse::<cron::Schedule>()
            .ok()?
            .after(&after)
            .next()
    }

    /// Tick preparation: recover missing next_run, fast-forward stale recurring jobs.
    pub fn prepare_for_tick(&mut self, now: DateTime<Utc>) -> bool {
        if self.status != JobStatus::Active {
            return false;
        }
        self.normalize_schedule();
        let Some(spec) = self.schedule_spec.clone() else {
            return false;
        };
        let mut changed = false;
        if self.next_run.is_none() {
            if let Some(nr) = compute_next_run(&spec, self.last_run) {
                self.next_run = Some(nr);
                changed = true;
            }
        }
        if let Some(next) = self.next_run {
            if let Some(ff) = fast_forward_if_stale(&spec, next, now) {
                tracing::info!(
                    "Job '{}' fast-forwarded stale next_run {} -> {}",
                    self.id,
                    next,
                    ff
                );
                self.next_run = Some(ff);
                return true;
            }
        }
        changed
    }

    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if self.status != JobStatus::Active {
            return false;
        }
        match self.next_run {
            Some(next) => now >= next,
            None => false,
        }
    }

    pub fn mark_executed(&mut self, now: DateTime<Utc>) -> bool {
        self.run_count += 1;
        self.last_run = Some(now);
        if let Some(repeat) = self.repeat {
            if self.run_count >= repeat {
                self.status = JobStatus::Completed;
                self.next_run = None;
                return false;
            }
        }
        if let Some(spec) = self.schedule_spec.as_ref() {
            self.next_run = compute_next_run(spec, Some(now));
        } else {
            self.next_run = self.compute_next_run(now);
        }
        true
    }

    pub fn mark_failed(&mut self) {
        self.status = JobStatus::Failed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_new_every_2h() {
        let job = CronJob::new("every 2h", "drink water");
        assert!(job.schedule_spec.is_some());
        assert!(job.next_run.is_some());
        let nr = job.next_run.unwrap();
        let diff = nr - Utc::now();
        assert!(diff.num_hours() <= 2 && diff.num_seconds() >= 0);
    }

    #[test]
    fn test_validate_rejects_garbage_schedule() {
        let job = CronJob::new("not_a_schedule", "x");
        assert!(job.validate().is_err());
    }

    #[test]
    fn test_mark_executed_interval() {
        let mut job = CronJob::new("every 2h", "test");
        let now = Utc::now();
        job.mark_executed(now);
        assert_eq!(job.run_count, 1);
        assert!(job.next_run.is_some());
        assert!(job.next_run.unwrap() > now);
    }

    #[test]
    fn test_deliver_deserialize_python_slug_wecom() {
        let ts = "2026-05-17T17:27:05Z";
        let object = format!(
            r#"{{"id":"x","schedule":"every 2h","prompt":"p","created_at":"{ts}","deliver":{{"target":"wecom"}}}}"#
        );
        let job: CronJob = serde_json::from_str(&object).expect("object deliver");
        assert_eq!(
            job.deliver.as_ref().map(|d| d.target),
            Some(DeliverTarget::WeCom)
        );

        let string = format!(
            r#"{{"id":"y","schedule":"every 2h","prompt":"p","created_at":"{ts}","deliver":"wecom"}}"#
        );
        let job: CronJob = serde_json::from_str(&string).expect("string deliver");
        assert_eq!(
            job.deliver.as_ref().map(|d| d.target),
            Some(DeliverTarget::WeCom)
        );
    }

    #[test]
    fn test_deliver_roundtrip_serializes_wecom_not_we_com() {
        let job = CronJob {
            deliver: Some(DeliverConfig::new(DeliverTarget::WeCom)),
            ..CronJob::new("every 2h", "p")
        };
        let json = serde_json::to_string(&job).unwrap();
        assert!(json.contains(r#""target":"wecom""#));
        assert!(!json.contains("we_com"));
    }
}
