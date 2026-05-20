//! Python-parity schedule parsing and next-run computation (`cron/jobs.py`).

use chrono::{DateTime, Duration, Utc};
use cron::Schedule;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// One-shot jobs within this many seconds of `now` may still fire (Python `ONESHOT_GRACE_SECONDS`).
pub const ONESHOT_GRACE_SECONDS: i64 = 120;

const MIN_GRACE_SECONDS: i64 = 120;
const MAX_GRACE_SECONDS: i64 = 7200;

static DURATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(\d+)\s*(m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)$",
    )
    .expect("valid regex")
});

static CRON_FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[\d\*\-,/]+$").expect("valid regex")
});

/// Parsed schedule (Python `parse_schedule` output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScheduleSpec {
    Once { run_at: DateTime<Utc> },
    Interval {
        minutes: u32,
    },
    Cron {
        expr: String,
    },
}

impl ScheduleSpec {
    pub fn display(&self) -> String {
        match self {
            ScheduleSpec::Once { run_at } => format!("once at {}", run_at.format("%Y-%m-%d %H:%M")),
            ScheduleSpec::Interval { minutes } => format!("every {minutes}m"),
            ScheduleSpec::Cron { expr } => expr.clone(),
        }
    }
}

/// Errors from schedule parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleParseError(pub String);

impl std::fmt::Display for ScheduleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ScheduleParseError {}

/// Parse a duration string into minutes (Python `parse_duration`).
pub fn parse_duration(s: &str) -> Result<u32, ScheduleParseError> {
    let caps = DURATION_RE
        .captures(s.trim())
        .ok_or_else(|| ScheduleParseError(format!("Invalid duration: '{s}'")))?;
    let value: u32 = caps[1]
        .parse()
        .map_err(|_| ScheduleParseError(format!("Invalid duration: '{s}'")))?;
    let unit = caps[2].chars().next().unwrap_or('m');
    let mult = match unit {
        'm' | 'M' => 1,
        'h' | 'H' => 60,
        'd' | 'D' => 1440,
        _ => 1,
    };
    Ok(value.saturating_mul(mult))
}

/// Parse schedule string (Python `parse_schedule`).
pub fn parse_schedule(schedule: &str) -> Result<ScheduleSpec, ScheduleParseError> {
    let original = schedule.trim();
    if original.is_empty() {
        return Err(ScheduleParseError("Schedule cannot be empty".into()));
    }
    let lower = original.to_ascii_lowercase();

    if let Some(rest) = lower.strip_prefix("every ") {
        let minutes = parse_duration(rest.trim())?;
        return Ok(ScheduleSpec::Interval { minutes });
    }

    let parts: Vec<&str> = original.split_whitespace().collect();
    if parts.len() >= 5 && parts.iter().take(5).all(|p| CRON_FIELD_RE.is_match(p)) {
        let expr = original.to_string();
        let normalized = normalize_cron_expr(&expr);
        if normalized.parse::<Schedule>().is_err() {
            return Err(ScheduleParseError(format!(
                "Invalid cron expression '{expr}'"
            )));
        }
        return Ok(ScheduleSpec::Cron { expr });
    }

    if original.contains('T') || Regex::new(r"^\d{4}-\d{2}-\d{2}")
        .expect("valid")
        .is_match(original)
    {
        let run_at = parse_iso_timestamp(original)?;
        return Ok(ScheduleSpec::Once { run_at });
    }

    if let Ok(minutes) = parse_duration(original) {
        let run_at = Utc::now() + Duration::minutes(i64::from(minutes));
        return Ok(ScheduleSpec::Once { run_at });
    }

    Err(ScheduleParseError(format!(
        "Invalid schedule '{original}'. Use duration (30m), interval (every 2h), cron (0 9 * * *), or ISO timestamp"
    )))
}

/// Parse schedule from Python jobs.json object or legacy string.
pub fn parse_schedule_value(value: &serde_json::Value) -> Result<ScheduleSpec, ScheduleParseError> {
    match value {
        serde_json::Value::String(s) => parse_schedule(s),
        serde_json::Value::Object(map) => {
            let kind = map
                .get("kind")
                .and_then(|v| v.as_str())
                .ok_or_else(|| ScheduleParseError("schedule object missing kind".into()))?;
            match kind {
                "once" => {
                    let run_at = map
                        .get("run_at")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ScheduleParseError("once schedule missing run_at".into()))?;
                    Ok(ScheduleSpec::Once {
                        run_at: parse_iso_timestamp(run_at)?,
                    })
                }
                "interval" => {
                    let minutes = map
                        .get("minutes")
                        .and_then(|v| v.as_u64())
                        .ok_or_else(|| {
                            ScheduleParseError("interval schedule missing minutes".into())
                        })? as u32;
                    Ok(ScheduleSpec::Interval { minutes })
                }
                "cron" => {
                    let expr = map
                        .get("expr")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ScheduleParseError("cron schedule missing expr".into()))?;
                    parse_schedule(expr)
                }
                other => Err(ScheduleParseError(format!("unknown schedule kind: {other}"))),
            }
        }
        _ => Err(ScheduleParseError("schedule must be string or object".into())),
    }
}

fn parse_iso_timestamp(s: &str) -> Result<DateTime<Utc>, ScheduleParseError> {
    let trimmed = s.trim().replace('Z', "+00:00");
    if let Ok(dt) = DateTime::parse_from_rfc3339(&trimmed) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&trimmed, "%Y-%m-%dT%H:%M:%S") {
        return Ok(ndt.and_utc());
    }
    if let Ok(ndt) = chrono::NaiveDateTime::parse_from_str(&trimmed, "%Y-%m-%d %H:%M:%S") {
        return Ok(ndt.and_utc());
    }
    Err(ScheduleParseError(format!("Invalid timestamp '{s}'")))
}

/// Compute next run (Python `compute_next_run`).
pub fn compute_next_run(
    spec: &ScheduleSpec,
    last_run_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    let now = Utc::now();
    match spec {
        ScheduleSpec::Once { run_at } => recoverable_oneshot_run_at(*run_at, now, last_run_at),
        ScheduleSpec::Interval { minutes } => {
            let delta = Duration::minutes(i64::from(*minutes));
            Some(if let Some(last) = last_run_at {
                last + delta
            } else {
                now + delta
            })
        }
        ScheduleSpec::Cron { expr } => {
            let base = last_run_at.unwrap_or(now);
            parse_cron_next(expr, base)
        }
    }
}

fn recoverable_oneshot_run_at(
    run_at: DateTime<Utc>,
    now: DateTime<Utc>,
    last_run_at: Option<DateTime<Utc>>,
) -> Option<DateTime<Utc>> {
    if last_run_at.is_some() {
        return None;
    }
    if run_at >= now - Duration::seconds(ONESHOT_GRACE_SECONDS) {
        Some(run_at)
    } else {
        None
    }
}

/// Grace window for stale fast-forward (Python `_compute_grace_seconds`).
pub fn compute_grace_seconds(spec: &ScheduleSpec) -> i64 {
    match spec {
        ScheduleSpec::Interval { minutes } => {
            let period = i64::from(*minutes) * 60;
            (period / 2).clamp(MIN_GRACE_SECONDS, MAX_GRACE_SECONDS)
        }
        ScheduleSpec::Cron { expr } => {
            let now = Utc::now();
            if let (Some(first), Some(second)) = (
                parse_cron_next(expr, now),
                parse_cron_next(expr, parse_cron_next(expr, now).unwrap_or(now)),
            ) {
                let period = (second - first).num_seconds().max(60);
                (period / 2).clamp(MIN_GRACE_SECONDS, MAX_GRACE_SECONDS)
            } else {
                MIN_GRACE_SECONDS
            }
        }
        ScheduleSpec::Once { .. } => MIN_GRACE_SECONDS,
    }
}

/// If recurring job is past grace, fast-forward `next_run` (Python `get_due_jobs` stale skip).
pub fn fast_forward_if_stale(
    spec: &ScheduleSpec,
    next_run: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match spec {
        ScheduleSpec::Once { .. } => None,
        ScheduleSpec::Interval { .. } | ScheduleSpec::Cron { .. } => {
            let grace = compute_grace_seconds(spec);
            if (now - next_run).num_seconds() > grace {
                compute_next_run(spec, Some(now))
            } else {
                None
            }
        }
    }
}

/// Pre-execution advance for recurring jobs (Python `advance_next_run`).
pub fn advance_next_run_before_execute(
    spec: &ScheduleSpec,
    current_next: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match spec {
        ScheduleSpec::Once { .. } => None,
        ScheduleSpec::Interval { .. } | ScheduleSpec::Cron { .. } => {
            let new_next = compute_next_run(spec, Some(now))?;
            if current_next != Some(new_next) {
                Some(new_next)
            } else {
                None
            }
        }
    }
}

pub fn normalize_cron_expr(expr: &str) -> String {
    let parts: Vec<&str> = expr.trim().split_whitespace().collect();
    match parts.len() {
        5 => format!("0 {} *", expr.trim()),
        6 => format!("{} *", expr.trim()),
        7 => expr.trim().to_string(),
        _ => expr.trim().to_string(),
    }
}

fn parse_cron_next(expr: &str, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let normalized = normalize_cron_expr(expr);
    normalized
        .parse::<Schedule>()
        .ok()?
        .after(&after)
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_every_2h() {
        let spec = parse_schedule("every 2h").unwrap();
        assert_eq!(spec, ScheduleSpec::Interval { minutes: 120 });
    }

    #[test]
    fn parse_duration_once() {
        let spec = parse_schedule("30m").unwrap();
        match spec {
            ScheduleSpec::Once { run_at } => {
                assert!(run_at > Utc::now());
            }
            _ => panic!("expected once"),
        }
    }

    #[test]
    fn parse_cron_expr() {
        let spec = parse_schedule("0 9 * * *").unwrap();
        assert_eq!(
            spec,
            ScheduleSpec::Cron {
                expr: "0 9 * * *".into()
            }
        );
    }

    #[test]
    fn interval_next_run_uses_last_run() {
        let spec = ScheduleSpec::Interval { minutes: 60 };
        let last = Utc::now() - Duration::hours(1);
        let next = compute_next_run(&spec, Some(last)).unwrap();
        assert!(next >= last);
    }

    #[test]
    fn invalid_schedule_rejected() {
        assert!(parse_schedule("not_a_schedule").is_err());
    }
}
