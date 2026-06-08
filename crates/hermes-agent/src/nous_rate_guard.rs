//! Cross-session Nous Portal rate limit guard — parity with `agent/nous_rate_guard.py`.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const STATE_SUBDIR: &str = "rate_limits";
const STATE_FILENAME: &str = "nous.json";
const MIN_RESET_FOR_BREAKER_SECONDS: f64 = 60.0;

#[derive(Debug, Serialize, Deserialize)]
struct NousRateLimitState {
    reset_at: f64,
    recorded_at: f64,
    reset_seconds: f64,
}

fn state_path(hermes_home: Option<&str>) -> PathBuf {
    let base = hermes_home
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HERMES_HOME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(hermes_config::hermes_home);
    base.join(STATE_SUBDIR).join(STATE_FILENAME)
}

pub fn parse_reset_seconds(headers: Option<&HashMap<String, String>>) -> Option<f64> {
    let headers = headers?;
    let lowered: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    for key in [
        "x-ratelimit-reset-requests-1h",
        "x-ratelimit-reset-requests",
        "retry-after",
    ] {
        if let Some(raw) = lowered.get(key) {
            if let Ok(val) = raw.trim().parse::<f64>() {
                if val > 0.0 {
                    return Some(val);
                }
            }
        }
    }
    None
}

/// Record that Nous Portal is rate-limited (Python `record_nous_rate_limit`).
pub fn record_nous_rate_limit(
    hermes_home: Option<&str>,
    headers: Option<&HashMap<String, String>>,
    reset_at_from_context: Option<f64>,
    default_cooldown: f64,
) {
    let now = unix_now();
    let mut reset_at = parse_reset_seconds(headers).map(|secs| now + secs);
    if reset_at.is_none() {
        if let Some(ctx_reset) = reset_at_from_context {
            if ctx_reset > now {
                reset_at = Some(ctx_reset);
            }
        }
    }
    let reset_at = reset_at.unwrap_or(now + default_cooldown);
    let path = state_path(hermes_home);
    let Some(dir) = path.parent() else {
        return;
    };
    if fs::create_dir_all(dir).is_err() {
        return;
    }
    let state = NousRateLimitState {
        reset_at,
        recorded_at: now,
        reset_seconds: reset_at - now,
    };
    let tmp = dir.join(format!(
        "nous-{}.tmp",
        std::process::id()
    ));
    if let Ok(mut f) = fs::File::create(&tmp) {
        if serde_json::to_writer(&mut f, &state).is_ok() {
            let _ = f.flush();
            let _ = fs::rename(&tmp, &path);
        }
    }
    let _ = fs::remove_file(&tmp);
    tracing::info!(
        reset_in_secs = state.reset_seconds,
        reset_at = state.reset_at,
        "Nous rate limit recorded"
    );
}

/// Seconds until reset, or `None` when not rate-limited (Python `nous_rate_limit_remaining`).
pub fn nous_rate_limit_remaining(hermes_home: Option<&str>) -> Option<f64> {
    let path = state_path(hermes_home);
    let data = fs::read_to_string(&path).ok()?;
    let state: NousRateLimitState = serde_json::from_str(&data).ok()?;
    let remaining = state.reset_at - unix_now();
    if remaining > 0.0 {
        return Some(remaining);
    }
    let _ = fs::remove_file(&path);
    None
}

pub fn clear_nous_rate_limit(hermes_home: Option<&str>) {
    let path = state_path(hermes_home);
    let _ = fs::remove_file(&path);
}

/// Human-readable duration (Python `format_remaining`).
pub fn format_remaining(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s < 60 {
        return format!("{s}s");
    }
    if s < 3600 {
        let m = s / 60;
        let sec = s % 60;
        return if sec == 0 {
            format!("{m}m")
        } else {
            format!("{m}m {sec}s")
        };
    }
    let h = s / 3600;
    let m = (s % 3600) / 60;
    if m == 0 {
        format!("{h}h")
    } else {
        format!("{h}h {m}m")
    }
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn parse_buckets_from_headers(
    headers: Option<&HashMap<String, String>>,
) -> HashMap<String, (Option<i64>, Option<f64>)> {
    let mut result = HashMap::new();
    let Some(headers) = headers else {
        return result;
    };
    let lowered: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.clone()))
        .collect();
    if !lowered.keys().any(|k| k.starts_with("x-ratelimit-")) {
        return result;
    }
    for tag in ["requests", "requests-1h", "tokens", "tokens-1h"] {
        let remaining = lowered
            .get(&format!("x-ratelimit-remaining-{tag}"))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .map(|v| v as i64);
        let reset = lowered
            .get(&format!("x-ratelimit-reset-{tag}"))
            .and_then(|v| v.trim().parse::<f64>().ok());
        if remaining.is_some() || reset.is_some() {
            result.insert(tag.to_string(), (remaining, reset));
        }
    }
    result
}

fn has_exhausted_bucket(buckets: &HashMap<String, (Option<i64>, Option<f64>)>) -> bool {
    for (remaining, reset) in buckets.values() {
        let Some(remaining) = remaining else {
            continue;
        };
        if *remaining > 0 {
            continue;
        }
        let Some(reset) = reset else {
            continue;
        };
        if *reset >= MIN_RESET_FOR_BREAKER_SECONDS {
            return true;
        }
    }
    false
}

/// Python `is_genuine_nous_rate_limit`.
/// Parse `x-ratelimit-*` headers embedded in provider error strings.
pub fn parse_rate_limit_headers_from_llm_error(err: &str) -> Option<HashMap<String, String>> {
    const MARKER: &str = "__HERMES_RL_HEADERS__:";
    if let Some(idx) = err.find(MARKER) {
        let json = err[idx + MARKER.len()..].trim();
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(json) {
            if !map.is_empty() {
                return Some(map);
            }
        }
    }
    if let Some(start) = err.find('{') {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&err[start..]) {
            return headers_from_json_value(&v);
        }
    }
    None
}

fn headers_from_json_value(v: &serde_json::Value) -> Option<HashMap<String, String>> {
    let mut out = HashMap::new();
    if let Some(obj) = v.as_object() {
        for (k, val) in obj {
            let kl = k.to_ascii_lowercase();
            if kl.starts_with("x-ratelimit-") || kl == "retry-after" {
                if let Some(s) = val.as_str() {
                    out.insert(kl, s.to_string());
                }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

pub fn is_genuine_nous_rate_limit(headers: Option<&HashMap<String, String>>) -> bool {
    let state = parse_buckets_from_headers(headers);
    has_exhausted_bucket(&state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_remaining_matches_python_style() {
        assert_eq!(format_remaining(45.0), "45s");
        assert_eq!(format_remaining(125.0), "2m 5s");
    }

    #[test]
    fn genuine_rate_limit_detects_exhausted_hour_bucket() {
        let mut headers = HashMap::new();
        headers.insert("x-ratelimit-remaining-requests-1h".into(), "0".into());
        headers.insert("x-ratelimit-reset-requests-1h".into(), "120".into());
        assert!(is_genuine_nous_rate_limit(Some(&headers)));
    }
}
