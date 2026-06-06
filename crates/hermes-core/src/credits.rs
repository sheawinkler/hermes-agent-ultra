//! Nous credits telemetry captured from inference response headers.
//!
//! This module is intentionally shared from `hermes-core` so providers can
//! capture headers at the HTTP boundary while CLI and gateway surfaces can render
//! the last-known state without depending on `hermes-agent`.

use chrono::Utc;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, OnceLock};

static LAST_NOUS_CREDITS: OnceLock<Mutex<Option<NousCreditsState>>> = OnceLock::new();
static YIELDED_FLASH_NOTICES: OnceLock<Mutex<YieldedFlashNotices>> = OnceLock::new();
static USD_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+\.\d{2}$").unwrap());

#[derive(Debug, Default)]
struct YieldedFlashNotices {
    usage_band: Option<u8>,
    grant_spent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NousCreditsState {
    pub version: i64,
    pub remaining_micros: i64,
    pub remaining_usd: String,
    pub subscription_micros: i64,
    pub subscription_usd: String,
    pub subscription_limit_micros: Option<i64>,
    pub subscription_limit_usd: Option<String>,
    pub rollover_micros: i64,
    pub purchased_micros: i64,
    pub purchased_usd: String,
    pub tool_pool_micros: i64,
    pub tool_pool_gated_off: bool,
    pub denominator_kind: String,
    pub paid_access: bool,
    pub disabled_reason: Option<String>,
    pub as_of_ms: Option<i64>,
    pub captured_at_ms: i64,
}

impl NousCreditsState {
    pub fn depleted(&self) -> bool {
        !self.paid_access
    }

    pub fn used_fraction(&self) -> Option<f64> {
        let limit = self.subscription_limit_micros?;
        if limit <= 0 {
            return None;
        }
        let used = limit.saturating_sub(self.subscription_micros);
        Some(((used as f64) / (limit as f64)).clamp(0.0, 1.0))
    }
}

pub fn capture_nous_credits_from_pairs<I, K, V>(headers: I) -> Option<NousCreditsState>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let state = parse_nous_credits_headers(headers)?;
    reconcile_yielded_flash_notices(&state);
    let store = LAST_NOUS_CREDITS.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = store.lock() {
        *guard = Some(state.clone());
    }
    Some(state)
}

pub fn last_nous_credits_state() -> Option<NousCreditsState> {
    LAST_NOUS_CREDITS
        .get_or_init(|| Mutex::new(None))
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
}

pub fn clear_last_nous_credits_state() {
    if let Ok(mut guard) = LAST_NOUS_CREDITS.get_or_init(|| Mutex::new(None)).lock() {
        *guard = None;
    }
    if let Ok(mut guard) = YIELDED_FLASH_NOTICES
        .get_or_init(|| Mutex::new(YieldedFlashNotices::default()))
        .lock()
    {
        *guard = YieldedFlashNotices::default();
    }
}

pub fn render_last_nous_credits_lines() -> Vec<String> {
    let Some(state) = last_nous_credits_state() else {
        return Vec::new();
    };
    render_nous_credits_lines(&state)
}

pub fn last_nous_credits_notice_line() -> Option<String> {
    last_nous_credits_state().and_then(|state| {
        let notice = nous_credits_notice(&state)?;
        if notice_is_yielded(&notice) {
            return None;
        }
        Some(notice.line)
    })
}

pub fn nous_credits_notice_line(state: &NousCreditsState) -> Option<String> {
    nous_credits_notice(state).map(|notice| notice.line)
}

/// Mark the current flash-style Nous credits notice as yielded.
///
/// Usage-band and grant-spent notices are heads-ups: they should remain visible
/// until the next prompt starts, then stay quiet until their condition clears or
/// the usage band changes. Depletion is intentionally sticky and is never yielded.
pub fn yield_current_nous_credits_flash_notice() -> bool {
    let Some(state) = last_nous_credits_state() else {
        return false;
    };
    let Some(notice) = nous_credits_notice(&state) else {
        return false;
    };
    let mut guard = match YIELDED_FLASH_NOTICES
        .get_or_init(|| Mutex::new(YieldedFlashNotices::default()))
        .lock()
    {
        Ok(guard) => guard,
        Err(_) => return false,
    };
    match notice.kind {
        NousCreditsNoticeKind::Usage { band } => {
            guard.usage_band = Some(band);
            true
        }
        NousCreditsNoticeKind::GrantSpent => {
            guard.grant_spent = true;
            true
        }
        NousCreditsNoticeKind::Depleted => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NousCreditsNoticeKind {
    Usage { band: u8 },
    GrantSpent,
    Depleted,
}

struct NousCreditsNotice {
    kind: NousCreditsNoticeKind,
    line: String,
}

fn nous_credits_notice(state: &NousCreditsState) -> Option<NousCreditsNotice> {
    if state.depleted() {
        return Some(NousCreditsNotice {
            kind: NousCreditsNoticeKind::Depleted,
            line: "credits: depleted - run /usage".to_string(),
        });
    }
    let used_fraction = state.used_fraction()?;
    if grant_spent_condition(state, used_fraction) {
        let top_up = if state.purchased_usd.trim().is_empty() {
            format!("{} micros", state.purchased_micros)
        } else {
            state.purchased_usd.clone()
        };
        return Some(NousCreditsNotice {
            kind: NousCreditsNoticeKind::GrantSpent,
            line: format!("credits: grant spent - {top_up} top-up left"),
        });
    }
    let band = if used_fraction >= 0.90 {
        90
    } else if used_fraction >= 0.75 {
        75
    } else if used_fraction >= 0.50 {
        50
    } else {
        return None;
    };
    Some(NousCreditsNotice {
        kind: NousCreditsNoticeKind::Usage { band },
        line: format!("credits: {band}% used - run /usage"),
    })
}

fn grant_spent_condition(state: &NousCreditsState, used_fraction: f64) -> bool {
    used_fraction >= 1.0 && state.purchased_micros > 0
}

fn notice_is_yielded(notice: &NousCreditsNotice) -> bool {
    let Ok(guard) = YIELDED_FLASH_NOTICES
        .get_or_init(|| Mutex::new(YieldedFlashNotices::default()))
        .lock()
    else {
        return false;
    };
    match notice.kind {
        NousCreditsNoticeKind::Usage { band } => guard.usage_band == Some(band),
        NousCreditsNoticeKind::GrantSpent => guard.grant_spent,
        NousCreditsNoticeKind::Depleted => false,
    }
}

fn reconcile_yielded_flash_notices(state: &NousCreditsState) {
    let Ok(mut guard) = YIELDED_FLASH_NOTICES
        .get_or_init(|| Mutex::new(YieldedFlashNotices::default()))
        .lock()
    else {
        return;
    };
    if state.depleted() {
        guard.usage_band = None;
        guard.grant_spent = false;
        return;
    }
    let used_fraction = state.used_fraction();
    let current_usage_band = used_fraction.and_then(|used| {
        if used >= 0.90 {
            Some(90)
        } else if used >= 0.75 {
            Some(75)
        } else if used >= 0.50 {
            Some(50)
        } else {
            None
        }
    });
    if guard.usage_band != current_usage_band {
        guard.usage_band = None;
    }
    let current_grant_spent = used_fraction
        .map(|used| grant_spent_condition(state, used))
        .unwrap_or(false);
    if !current_grant_spent {
        guard.grant_spent = false;
    }
}

pub fn render_nous_credits_lines(state: &NousCreditsState) -> Vec<String> {
    let mut lines = vec!["Nous credits".to_string(), "Provider: nous".to_string()];

    if let Some(used_fraction) = state.used_fraction() {
        let used_pct = (used_fraction * 100.0).round().clamp(0.0, 100.0) as u64;
        let remaining_pct = 100u64.saturating_sub(used_pct);
        let detail = match (
            &state.subscription_limit_usd,
            state.subscription_usd.as_str(),
        ) {
            (Some(limit), subscription) if !subscription.is_empty() => {
                format!(" - {subscription} of {limit} left")
            }
            _ => String::new(),
        };
        lines.push(format!(
            "Subscription: {remaining_pct}% remaining ({used_pct}% used){detail}"
        ));
    }

    if !state.remaining_usd.is_empty() {
        lines.push(format!("Total usable: {}", state.remaining_usd));
    } else {
        lines.push(format!("Total usable: {} micros", state.remaining_micros));
    }
    if !state.subscription_usd.is_empty() {
        lines.push(format!("Subscription credits: {}", state.subscription_usd));
    }
    if !state.purchased_usd.is_empty() {
        lines.push(format!("Top-up credits: {}", state.purchased_usd));
    }
    if state.rollover_micros > 0 {
        lines.push(format!("Rollover: {} micros", state.rollover_micros));
    }
    if state.tool_pool_micros > 0 || state.tool_pool_gated_off {
        lines.push(format!(
            "Tool pool: {} micros{}",
            state.tool_pool_micros,
            if state.tool_pool_gated_off {
                " (gated off)"
            } else {
                ""
            }
        ));
    }
    if state.depleted() {
        let reason = state
            .disabled_reason
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("paid access paused");
        lines.push(format!("Status: access depleted - {reason}"));
    }
    if let Some(as_of_ms) = state.as_of_ms {
        lines.push(format!("As of: {as_of_ms} ms"));
    }
    lines
}

pub fn parse_nous_credits_headers<I, K, V>(headers: I) -> Option<NousCreditsState>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let map = headers
        .into_iter()
        .map(|(key, value)| {
            (
                key.as_ref().trim().to_ascii_lowercase(),
                value.as_ref().trim().to_string(),
            )
        })
        .collect::<HashMap<_, _>>();

    let version = parse_i64(required(&map, "x-nous-credits-version")?)?;
    if version != 1 {
        tracing::warn!(version, "unsupported Nous credits header schema version");
        return None;
    }

    let remaining_micros =
        parse_nonnegative_i64(required(&map, "x-nous-credits-remaining-micros")?)?;
    let subscription_micros = parse_i64(required(&map, "x-nous-credits-subscription-micros")?)?;
    let rollover_micros = parse_nonnegative_i64(required(&map, "x-nous-credits-rollover-micros")?)?;
    let purchased_micros =
        parse_nonnegative_i64(required(&map, "x-nous-credits-purchased-micros")?)?;
    let denominator_kind = required(&map, "x-nous-credits-denominator-kind")?.to_string();
    if !matches!(denominator_kind.as_str(), "subscription_cap" | "none") {
        return None;
    }
    let paid_access = parse_bool_string(required(&map, "x-nous-credits-paid-access")?)?;

    let remaining_usd = optional_usd(&map, "x-nous-credits-remaining-usd")?.unwrap_or_default();
    let subscription_usd =
        optional_usd(&map, "x-nous-credits-subscription-usd")?.unwrap_or_default();
    let purchased_usd = optional_usd(&map, "x-nous-credits-purchased-usd")?.unwrap_or_default();

    let limit_micros_raw = map.get("x-nous-credits-subscription-limit-micros");
    let limit_usd_raw = map.get("x-nous-credits-subscription-limit-usd");
    let (subscription_limit_micros, subscription_limit_usd) =
        match (limit_micros_raw, limit_usd_raw) {
            (Some(micros), Some(usd)) => {
                let parsed = parse_nonnegative_i64(micros)?;
                if !valid_usd(usd) {
                    return None;
                }
                (Some(parsed), Some(usd.clone()))
            }
            _ => (None, None),
        };

    let tool_pool_micros = match map.get("x-nous-tool-pool-micros") {
        Some(value) => parse_nonnegative_i64(value)?,
        None => 0,
    };
    let tool_pool_gated_off = match map.get("x-nous-tool-pool-gated-off") {
        Some(value) => parse_bool_string(value)?,
        None => false,
    };
    let disabled_reason = map
        .get("x-nous-credits-disabled-reason")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let as_of_ms = match map.get("x-nous-credits-as-of-ms") {
        Some(value) => Some(parse_nonnegative_i64(value)?),
        None => None,
    };

    Some(NousCreditsState {
        version,
        remaining_micros,
        remaining_usd,
        subscription_micros,
        subscription_usd,
        subscription_limit_micros,
        subscription_limit_usd,
        rollover_micros,
        purchased_micros,
        purchased_usd,
        tool_pool_micros,
        tool_pool_gated_off,
        denominator_kind,
        paid_access,
        disabled_reason,
        as_of_ms,
        captured_at_ms: Utc::now().timestamp_millis(),
    })
}

fn required<'a>(map: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    map.get(key).map(String::as_str).filter(|v| !v.is_empty())
}

fn parse_i64(raw: &str) -> Option<i64> {
    if raw.contains('.') {
        return None;
    }
    raw.parse::<i64>().ok()
}

fn parse_nonnegative_i64(raw: &str) -> Option<i64> {
    let value = parse_i64(raw)?;
    (value >= 0).then_some(value)
}

fn parse_bool_string(raw: &str) -> Option<bool> {
    match raw.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn optional_usd(map: &HashMap<String, String>, key: &str) -> Option<Option<String>> {
    let Some(value) = map
        .get(key)
        .map(|value| value.trim())
        .filter(|v| !v.is_empty())
    else {
        return Some(None);
    };
    valid_usd(value).then_some(Some(value.to_string()))
}

fn valid_usd(value: &str) -> bool {
    USD_RE.is_match(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_CREDITS_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn valid_headers() -> Vec<(&'static str, &'static str)> {
        vec![
            ("x-nous-credits-version", "1"),
            ("x-nous-credits-remaining-micros", "12000000"),
            ("x-nous-credits-remaining-usd", "12.00"),
            ("x-nous-credits-subscription-micros", "5000000"),
            ("x-nous-credits-subscription-usd", "5.00"),
            ("x-nous-credits-subscription-limit-micros", "10000000"),
            ("x-nous-credits-subscription-limit-usd", "10.00"),
            ("x-nous-credits-rollover-micros", "1000000"),
            ("x-nous-credits-purchased-micros", "6000000"),
            ("x-nous-credits-purchased-usd", "6.00"),
            ("x-nous-credits-denominator-kind", "subscription_cap"),
            ("x-nous-credits-paid-access", "true"),
            ("x-nous-credits-as-of-ms", "1710000000000"),
            ("x-nous-tool-pool-micros", "250000"),
            ("x-nous-tool-pool-gated-off", "false"),
        ]
    }

    #[test]
    fn parse_valid_nous_credits_headers() {
        let state = parse_nous_credits_headers(valid_headers()).expect("credits state");
        assert_eq!(state.remaining_micros, 12_000_000);
        assert_eq!(state.subscription_micros, 5_000_000);
        assert_eq!(state.subscription_limit_micros, Some(10_000_000));
        assert_eq!(state.used_fraction(), Some(0.5));
        assert_eq!(state.tool_pool_micros, 250_000);
        assert!(!state.tool_pool_gated_off);
    }

    #[test]
    fn parse_rejects_bool_traps_and_bad_money() {
        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-paid-access");
        headers.push(("x-nous-credits-paid-access", "1"));
        assert!(parse_nous_credits_headers(headers).is_none());

        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-remaining-usd");
        headers.push(("x-nous-credits-remaining-usd", "12"));
        assert!(parse_nous_credits_headers(headers).is_none());
    }

    #[test]
    fn parse_allows_subscription_debt_only() {
        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-micros");
        headers.push(("x-nous-credits-subscription-micros", "-1000000"));
        assert!(parse_nous_credits_headers(headers).is_some());

        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-remaining-micros");
        headers.push(("x-nous-credits-remaining-micros", "-1"));
        assert!(parse_nous_credits_headers(headers).is_none());
    }

    #[test]
    fn half_subscription_limit_pair_is_ignored() {
        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-limit-usd");
        let state = parse_nous_credits_headers(headers).expect("credits state");
        assert_eq!(state.subscription_limit_micros, None);
        assert_eq!(state.used_fraction(), None);
    }

    #[test]
    fn capture_and_render_last_state() {
        let _guard = TEST_CREDITS_LOCK.lock().unwrap();
        clear_last_nous_credits_state();
        capture_nous_credits_from_pairs(valid_headers()).expect("captured");
        let lines = render_last_nous_credits_lines();
        assert!(lines.iter().any(|line| line == "Nous credits"));
        assert!(lines.iter().any(|line| line.contains("50% remaining")));
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: 50% used - run /usage")
        );
        clear_last_nous_credits_state();
    }

    #[test]
    fn notice_line_prefers_depleted_over_usage_band() {
        let mut state = parse_nous_credits_headers(valid_headers()).expect("credits state");
        state.paid_access = false;
        assert_eq!(
            nous_credits_notice_line(&state).as_deref(),
            Some("credits: depleted - run /usage")
        );
    }

    #[test]
    fn usage_notice_yields_until_band_changes() {
        let _guard = TEST_CREDITS_LOCK.lock().unwrap();
        clear_last_nous_credits_state();
        capture_nous_credits_from_pairs(valid_headers()).expect("captured");
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: 50% used - run /usage")
        );

        assert!(yield_current_nous_credits_flash_notice());
        assert_eq!(last_nous_credits_notice_line(), None);

        capture_nous_credits_from_pairs(valid_headers()).expect("captured");
        assert_eq!(last_nous_credits_notice_line(), None);

        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-micros");
        headers.push(("x-nous-credits-subscription-micros", "2000000"));
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-usd");
        headers.push(("x-nous-credits-subscription-usd", "2.00"));
        capture_nous_credits_from_pairs(headers).expect("captured");
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: 75% used - run /usage")
        );
        clear_last_nous_credits_state();
    }

    #[test]
    fn grant_spent_notice_yields_but_depleted_stays_sticky() {
        let _guard = TEST_CREDITS_LOCK.lock().unwrap();
        clear_last_nous_credits_state();
        let mut headers = valid_headers();
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-micros");
        headers.push(("x-nous-credits-subscription-micros", "0"));
        headers.retain(|(key, _)| *key != "x-nous-credits-subscription-usd");
        headers.push(("x-nous-credits-subscription-usd", "0.00"));
        headers.retain(|(key, _)| *key != "x-nous-credits-purchased-micros");
        headers.push(("x-nous-credits-purchased-micros", "990000000"));
        headers.retain(|(key, _)| *key != "x-nous-credits-purchased-usd");
        headers.push(("x-nous-credits-purchased-usd", "990.00"));

        capture_nous_credits_from_pairs(headers.clone()).expect("captured");
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: grant spent - 990.00 top-up left")
        );
        assert!(yield_current_nous_credits_flash_notice());
        assert_eq!(last_nous_credits_notice_line(), None);
        capture_nous_credits_from_pairs(headers.clone()).expect("captured");
        assert_eq!(last_nous_credits_notice_line(), None);

        headers.retain(|(key, _)| *key != "x-nous-credits-paid-access");
        headers.push(("x-nous-credits-paid-access", "false"));
        headers.push(("x-nous-credits-disabled-reason", "out_of_credits"));
        capture_nous_credits_from_pairs(headers).expect("captured");
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: depleted - run /usage")
        );
        assert!(!yield_current_nous_credits_flash_notice());
        assert_eq!(
            last_nous_credits_notice_line().as_deref(),
            Some("credits: depleted - run /usage")
        );
        clear_last_nous_credits_state();
    }
}
