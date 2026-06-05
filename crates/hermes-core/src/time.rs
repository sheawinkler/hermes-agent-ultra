//! Timezone-aware wall clock for Hermes (Python `hermes_time.py` parity).
//!
//! **(user wall clock):** system prompt date line, cron scheduling, execute_code `TZ`.
//! **(internal UTC):** session timestamps, auth expiry, telemetry — use `Utc::now()` directly.

use std::sync::{Mutex, OnceLock};

use chrono::{
    DateTime, FixedOffset, Local, NaiveDateTime, Offset, TimeZone, Utc,
};
use chrono_tz::Tz;
use tracing::warn;

/// Process-wide Hermes clock state (config timezone + resolved IANA zone).
struct GlobalTimeState {
    config_timezone: Option<String>,
    clock: HermesClock,
}

static GLOBAL_TIME: OnceLock<Mutex<GlobalTimeState>> = OnceLock::new();

fn global_state() -> &'static Mutex<GlobalTimeState> {
    GLOBAL_TIME.get_or_init(|| {
        Mutex::new(GlobalTimeState {
            config_timezone: None,
            clock: HermesClock::from_env_and_config(None),
        })
    })
}

/// Initialize or refresh the process-wide clock from config (call after `load_config`).
pub fn init_global_clock(config_timezone: Option<&str>) {
    let mut state = global_state().lock().expect("hermes time lock poisoned");
    state.config_timezone = config_timezone.map(str::to_string);
    state.clock = HermesClock::from_env_and_config(config_timezone);
}

/// Force re-resolution from env + last config timezone (Python `reset_cache()`).
pub fn reset_global_clock_cache() {
    let mut state = global_state().lock().expect("hermes time lock poisoned");
    let cfg = state.config_timezone.clone();
    state.clock = HermesClock::from_env_and_config(cfg.as_deref());
}

/// Python `hermes_time.reset_cache()`.
pub fn reset_cache() {
    reset_global_clock_cache();
}

/// User wall-clock "now" as a fixed offset (Hermes tz or server-local).
pub fn now() -> DateTime<FixedOffset> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .now()
}

/// Same instant as [`now`], stored/compared as UTC.
pub fn now_utc() -> DateTime<Utc> {
    now().with_timezone(&Utc)
}

/// Configured IANA timezone name, if any (empty when using server-local fallback).
pub fn timezone_name() -> Option<String> {
    get_timezone()
}

/// Python `hermes_time.get_timezone()` — configured IANA name, or None (server-local).
pub fn get_timezone() -> Option<String> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .timezone_name()
        .map(str::to_string)
}

/// Value for child-process `TZ` env (does not include `HERMES_TIMEZONE`).
pub fn tz_for_child_env() -> Option<String> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .tz_for_child_env()
}

/// Date-only string for system prompt (no hour/minute — upstream PR #20451).
pub fn format_conversation_started_date() -> String {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .format_conversation_started_date()
}

/// Normalize through Hermes wall timezone (Python `cron.jobs._ensure_aware` for aware values).
pub fn ensure_aware(dt: DateTime<Utc>) -> DateTime<Utc> {
    ensure_aware_utc(dt)
}

/// Normalize a UTC instant through the Hermes timezone (Python `_ensure_aware` for aware values).
pub fn ensure_aware_utc(dt: DateTime<Utc>) -> DateTime<Utc> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .ensure_aware_utc(dt)
}

/// Interpret naive wall time as system-local, then normalize to UTC (Python `_ensure_aware` for naive).
pub fn ensure_aware_naive(naive: NaiveDateTime) -> DateTime<Utc> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .ensure_aware_naive(naive)
}

/// Wall-clock `YYYY-MM-DD HH:MM:SS` (Python `_hermes_now().strftime("%Y-%m-%d %H:%M:%S")`).
pub fn format_wall_ymd_hms() -> String {
    now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Wall-clock `HH:MM:SS` for scheduler tick logs (Python `_hermes_now().strftime("%H:%M:%S")`).
pub fn format_wall_hms() -> String {
    now().format("%H:%M:%S").to_string()
}

/// Compact wall-clock stamp for cron session ids (Python `_hermes_now().strftime("%Y%m%d_%H%M%S")`).
pub fn format_wall_compact() -> String {
    now().format("%Y%m%d_%H%M%S").to_string()
}

/// Format a UTC instant for human display in the Hermes wall timezone.
pub fn format_wall_datetime(dt: DateTime<Utc>) -> String {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .format_wall_datetime(dt)
}

/// Fixed offset for cron expression evaluation at `instant` (IANA + DST, or legacy fallbacks).
pub fn cron_wall_offset_at(instant: DateTime<Utc>) -> Option<FixedOffset> {
    global_state()
        .lock()
        .expect("hermes time lock poisoned")
        .clock
        .cron_wall_offset_at(instant)
}

/// Resolved Hermes wall clock (timezone-aware datetime helper).
#[derive(Debug, Clone)]
pub struct HermesClock {
    timezone: Option<Tz>,
    timezone_name: Option<String>,
}

impl HermesClock {
    /// Resolve from `HERMES_TIMEZONE` → config `timezone` → server-local.
    pub fn from_env_and_config(config_timezone: Option<&str>) -> Self {
        let name = resolve_timezone_name(config_timezone);
        let tz = name.as_deref().and_then(parse_iana_timezone);
        Self {
            timezone: tz,
            timezone_name: name,
        }
    }

    /// Test helper: fixed IANA zone.
    pub fn with_fixed_tz(name: &str) -> Self {
        let tz = parse_iana_timezone(name);
        Self {
            timezone: tz,
            timezone_name: if tz.is_some() {
                Some(name.to_string())
            } else {
                None
            },
        }
    }

    pub fn timezone_name(&self) -> Option<&str> {
        self.timezone_name.as_deref()
    }

    pub fn now(&self) -> DateTime<FixedOffset> {
        match &self.timezone {
            Some(tz) => Utc::now().with_timezone(tz).fixed_offset(),
            None => Local::now().fixed_offset(),
        }
    }

    pub fn tz_for_child_env(&self) -> Option<String> {
        self.timezone_name.clone()
    }

    pub fn format_conversation_started_date(&self) -> String {
        self.now().format("%A, %B %d, %Y").to_string()
    }

    pub fn format_wall_datetime(&self, dt: DateTime<Utc>) -> String {
        match &self.timezone {
            Some(tz) => dt
                .with_timezone(tz)
                .format("%B %d, %Y at %I:%M %p")
                .to_string(),
            None => dt
                .with_timezone(&Local)
                .format("%B %d, %Y at %I:%M %p")
                .to_string(),
        }
    }

    pub fn ensure_aware_utc(&self, dt: DateTime<Utc>) -> DateTime<Utc> {
        match &self.timezone {
            Some(tz) => dt.with_timezone(tz).with_timezone(&Utc),
            None => dt,
        }
    }

    pub fn ensure_aware_naive(&self, naive: NaiveDateTime) -> DateTime<Utc> {
        let local_offset = Local::now().offset().fix();
        let as_local = local_offset
            .from_local_datetime(&naive)
            .single()
            .unwrap_or_else(|| naive.and_local_timezone(local_offset).unwrap());
        match &self.timezone {
            Some(tz) => as_local.with_timezone(tz).with_timezone(&Utc),
            None => as_local.with_timezone(&Utc),
        }
    }

    pub fn cron_wall_offset_at(&self, instant: DateTime<Utc>) -> Option<FixedOffset> {
        if let Some(tz) = &self.timezone {
            return Some(instant.with_timezone(tz).offset().fix());
        }

        if let Some(offset) = deprecated_cron_tz_offset() {
            return Some(offset);
        }

        let local_offset = Local::now().offset().fix();
        if local_offset.local_minus_utc() == 0 {
            None
        } else {
            Some(local_offset)
        }
    }
}

fn resolve_timezone_name(config_timezone: Option<&str>) -> Option<String> {
    if let Ok(env) = std::env::var("HERMES_TIMEZONE") {
        let trimmed = env.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(cfg) = config_timezone {
        let trimmed = cfg.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn parse_iana_timezone(name: &str) -> Option<Tz> {
    match name.parse::<Tz>() {
        Ok(tz) => Some(tz),
        Err(err) => {
            warn!(
                "Invalid timezone '{name}': {err}. Falling back to server local time.",
            );
            None
        }
    }
}

fn deprecated_cron_tz_offset() -> Option<FixedOffset> {
    let raw = std::env::var("HERMES_CRON_TZ")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    warn!(
        "HERMES_CRON_TZ is deprecated; set `timezone` in config.yaml or HERMES_TIMEZONE instead"
    );
    parse_fixed_offset(&raw)
}

fn parse_fixed_offset(raw: &str) -> Option<FixedOffset> {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix('+') {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let hours: i32 = parts[0].parse().ok()?;
        let mins: i32 = parts.get(1).unwrap_or(&"0").parse().ok()?;
        return FixedOffset::east_opt(hours * 3600 + mins * 60);
    }
    if let Some(rest) = trimmed.strip_prefix('-') {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let hours: i32 = parts[0].parse().ok()?;
        let mins: i32 = parts.get(1).unwrap_or(&"0").parse().ok()?;
        return FixedOffset::west_opt(hours * 3600 + mins * 60);
    }
    if let Ok(hours) = trimmed.parse::<i32>() {
        if hours >= 0 {
            return FixedOffset::east_opt(hours * 3600);
        }
        return FixedOffset::west_opt((-hours) * 3600);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Offset;
    use std::sync::{Mutex, MutexGuard};

    static TIME_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn time_test_lock() -> MutexGuard<'static, ()> {
        TIME_TEST_LOCK.lock().expect("time test lock poisoned")
    }

    fn with_env<F: FnOnce()>(key: &str, value: Option<&str>, f: F) {
        let _guard = time_test_lock();
        let prior = std::env::var(key).ok();
        match value {
            Some(v) => crate::test_env::set_var(key, v),
            None => crate::test_env::remove_var(key),
        }
        f();
        match prior {
            Some(v) => crate::test_env::set_var(key, &v),
            None => crate::test_env::remove_var(key),
        }
    }

    #[test]
    fn env_timezone_beats_config() {
        with_env("HERMES_TIMEZONE", Some("UTC"), || {
            let clock = HermesClock::from_env_and_config(Some("Asia/Shanghai"));
            assert_eq!(clock.now().offset().fix().local_minus_utc(), 0);
        });
    }

    #[test]
    fn invalid_timezone_falls_back_without_panic() {
        with_env("HERMES_TIMEZONE", Some("Mars/Olympus_Mons"), || {
            let clock = HermesClock::from_env_and_config(None);
            assert!(clock.now().offset().local_minus_utc().abs() <= 14 * 3600);
        });
    }

    #[test]
    fn conversation_started_date_has_no_time_component() {
        let clock = HermesClock::with_fixed_tz("UTC");
        let line = clock.format_conversation_started_date();
        assert!(!line.contains(':'));
    }

    #[test]
    fn ensure_aware_naive_preserves_absolute_instant() {
        let clock = HermesClock::with_fixed_tz("Asia/Kolkata");
        let naive = NaiveDateTime::parse_from_str("2026-03-11 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap();
        let local_offset = Local::now().offset().fix();
        let expected = local_offset
            .from_local_datetime(&naive)
            .single()
            .unwrap()
            .with_timezone(&Utc);
        let actual = clock.ensure_aware_naive(naive);
        assert_eq!(actual, expected);
    }

    #[test]
    fn cron_wall_offset_uses_iana() {
        let clock = HermesClock::with_fixed_tz("Asia/Shanghai");
        let instant = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
        let offset = clock.cron_wall_offset_at(instant).expect("offset");
        assert_eq!(offset.fix().local_minus_utc(), 8 * 3600);
    }

    #[test]
    fn tz_for_child_env_returns_iana_name() {
        let clock = HermesClock::with_fixed_tz("Asia/Kolkata");
        assert_eq!(clock.tz_for_child_env().as_deref(), Some("Asia/Kolkata"));
    }

    #[test]
    fn reset_cache_picks_up_env_change() {
        with_env("HERMES_TIMEZONE", Some("UTC"), || {
            let clock = HermesClock::from_env_and_config(None);
            assert_eq!(clock.now().offset().fix().local_minus_utc(), 0);
            crate::test_env::set_var("HERMES_TIMEZONE", "Asia/Kolkata");
            let refreshed = HermesClock::from_env_and_config(None);
            assert_eq!(
                refreshed.now().offset().fix().local_minus_utc(),
                5 * 3600 + 30 * 60
            );
        });
    }

    #[test]
    fn get_timezone_matches_clock_name() {
        let clock = HermesClock::with_fixed_tz("Asia/Kolkata");
        assert_eq!(clock.timezone_name(), Some("Asia/Kolkata"));
    }

    #[test]
    fn reset_cache_alias_picks_up_env_change() {
        with_env("HERMES_TIMEZONE", Some("UTC"), || {
            init_global_clock(None);
            assert_eq!(now().offset().fix().local_minus_utc(), 0);
            crate::test_env::set_var("HERMES_TIMEZONE", "Asia/Kolkata");
            reset_cache();
            assert_eq!(
                now().offset().fix().local_minus_utc(),
                5 * 3600 + 30 * 60
            );
        });
    }

    #[test]
    fn global_clock_init_and_reset() {
        with_env("HERMES_TIMEZONE", Some("UTC"), || {
            init_global_clock(Some("Asia/Shanghai"));
            assert_eq!(now().offset().fix().local_minus_utc(), 0);
            crate::test_env::set_var("HERMES_TIMEZONE", "Asia/Kolkata");
            reset_global_clock_cache();
            assert_eq!(
                now().offset().fix().local_minus_utc(),
                5 * 3600 + 30 * 60
            );
        });
    }
}
