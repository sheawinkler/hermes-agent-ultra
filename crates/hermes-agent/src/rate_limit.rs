//! Rate limit tracking for LLM providers.
//!
//! Parses `x-ratelimit-remaining`, `x-ratelimit-reset`, and `retry-after`
//! headers from HTTP responses and tracks rate limit state per provider.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use reqwest::header::HeaderMap;

/// Tracks rate limit state for a single provider endpoint.
#[derive(Debug)]
pub struct RateLimitTracker {
    state: Mutex<RateLimitState>,
}

#[derive(Debug, Clone)]
struct RateLimitState {
    /// Number of remaining requests before hitting the limit.
    remaining: Option<u64>,
    /// When the rate limit window resets (as an Instant).
    reset_at: Option<Instant>,
    /// Explicit retry-after duration from the server.
    retry_after: Option<Instant>,
}

impl RateLimitTracker {
    /// Create a new rate limit tracker with no initial state.
    pub fn new() -> Self {
        Self {
            state: Mutex::new(RateLimitState {
                remaining: None,
                reset_at: None,
                retry_after: None,
            }),
        }
    }

    /// Check if we should wait before making the next request.
    ///
    /// Returns `Some(duration)` if we need to wait, `None` if we can proceed.
    pub fn should_wait(&self) -> Option<Duration> {
        let state = self.state.lock().ok()?;
        let now = Instant::now();

        // Check explicit retry-after first
        if let Some(retry_at) = state.retry_after {
            if retry_at > now {
                return Some(retry_at - now);
            }
        }

        // Check if remaining requests are exhausted
        if let Some(remaining) = state.remaining {
            if remaining == 0 {
                if let Some(reset_at) = state.reset_at {
                    if reset_at > now {
                        return Some(reset_at - now);
                    }
                } else {
                    // No reset time known, use a default backoff
                    return Some(Duration::from_secs(1));
                }
            }
        }

        None
    }

    /// Update rate limit state from HTTP response headers.
    ///
    /// Parses the following headers:
    /// - `x-ratelimit-remaining`: remaining requests in current window
    /// - `x-ratelimit-reset`: Unix timestamp when the window resets
    /// - `retry-after`: seconds to wait before retrying (from 429 responses)
    /// - `x-ratelimit-reset-requests`: alternative reset header
    /// - `x-ratelimit-reset-tokens`: alternative reset header for token limits
    pub fn update_from_headers(&self, headers: &HeaderMap) {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(_) => return,
        };

        let now = Instant::now();

        // Parse x-ratelimit-remaining
        if let Some(remaining) = headers
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
        {
            state.remaining = Some(remaining);
        }

        // Parse x-ratelimit-reset (Unix timestamp in seconds)
        if let Some(reset_at) = headers
            .get("x-ratelimit-reset")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_reset_value)
            .and_then(checked_positive_duration)
            .and_then(|duration| now.checked_add(duration))
        {
            state.reset_at = Some(reset_at);
        }

        // Parse retry-after (seconds)
        if let Some(retry_at) = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<f64>().ok())
            .and_then(checked_positive_duration)
            .and_then(|duration| now.checked_add(duration))
        {
            state.retry_after = Some(retry_at);
        }

        // Parse x-ratelimit-reset-requests (duration string like "1s" or "200ms")
        if let Some(new_reset) = headers
            .get("x-ratelimit-reset-requests")
            .and_then(|v| v.to_str().ok())
            .and_then(parse_duration_string)
            .and_then(|duration| now.checked_add(duration))
        {
            // Use the shorter of existing reset_at and this value
            state.reset_at = Some(match state.reset_at {
                Some(existing) if existing < new_reset => existing,
                _ => new_reset,
            });
        }
    }

    /// Reset the rate limit state (e.g., after switching API keys).
    pub fn reset(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.remaining = None;
            state.reset_at = None;
            state.retry_after = None;
        }
    }

    /// Get the current remaining request count, if known.
    pub fn remaining(&self) -> Option<u64> {
        self.state.lock().ok().and_then(|s| s.remaining)
    }
}

impl Default for RateLimitTracker {
    fn default() -> Self {
        Self::new()
    }
}

fn checked_positive_duration(seconds: f64) -> Option<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Duration::try_from_secs_f64(seconds).ok()
}

/// Parse a reset value that could be a Unix timestamp or a relative seconds value.
fn parse_reset_value(s: &str) -> Option<f64> {
    let val = s.parse::<f64>().ok()?;
    if val > 1_000_000_000.0 {
        // Looks like a Unix timestamp — convert to relative seconds
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_secs_f64();
        let diff = val - now_unix;
        Some(if diff > 0.0 { diff } else { 0.0 })
    } else {
        // Already relative seconds
        Some(val)
    }
}

/// Parse a duration string like "1s", "200ms", "1.5s", "2m".
fn parse_duration_string(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(ms_str) = s.strip_suffix("ms") {
        let ms: f64 = ms_str.parse().ok()?;
        return checked_positive_duration(ms / 1000.0);
    }
    if let Some(s_str) = s.strip_suffix('s') {
        let secs: f64 = s_str.parse().ok()?;
        return checked_positive_duration(secs);
    }
    if let Some(m_str) = s.strip_suffix('m') {
        let mins: f64 = m_str.parse().ok()?;
        return checked_positive_duration(mins * 60.0);
    }
    // Try parsing as plain seconds
    let secs: f64 = s.parse().ok()?;
    checked_positive_duration(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderName;

    fn state_for_test(tracker: &RateLimitTracker) -> std::sync::MutexGuard<'_, RateLimitState> {
        match tracker.state.lock() {
            Ok(state) => state,
            Err(_) => panic!("rate limit tracker state lock should not be poisoned"),
        }
    }

    fn assert_duration_header_is_ignored(header: &str, value: &str) {
        let tracker = RateLimitTracker::new();
        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_bytes(header.as_bytes()).expect("valid test header name"),
            value.parse().expect("valid test header value"),
        );

        tracker.update_from_headers(&headers);

        let state = state_for_test(&tracker);
        match header {
            "retry-after" => assert!(state.retry_after.is_none(), "{value} should be ignored"),
            _ => assert!(state.reset_at.is_none(), "{value} should be ignored"),
        }
    }

    #[test]
    fn test_new_tracker_no_wait() {
        let tracker = RateLimitTracker::new();
        assert!(tracker.should_wait().is_none());
    }

    #[test]
    fn test_remaining_zero_triggers_wait() {
        let tracker = RateLimitTracker::new();
        {
            let mut state = state_for_test(&tracker);
            state.remaining = Some(0);
            state.reset_at = Some(Instant::now() + Duration::from_secs(5));
        }
        let wait = tracker.should_wait();
        assert!(wait.is_some());
        assert!(wait.unwrap() <= Duration::from_secs(5));
    }

    #[test]
    fn test_remaining_nonzero_no_wait() {
        let tracker = RateLimitTracker::new();
        {
            let mut state = state_for_test(&tracker);
            state.remaining = Some(10);
        }
        assert!(tracker.should_wait().is_none());
    }

    #[test]
    fn test_retry_after_triggers_wait() {
        let tracker = RateLimitTracker::new();
        {
            let mut state = state_for_test(&tracker);
            state.retry_after = Some(Instant::now() + Duration::from_secs(3));
        }
        let wait = tracker.should_wait();
        assert!(wait.is_some());
        assert!(wait.unwrap() <= Duration::from_secs(3));
    }

    #[test]
    fn test_expired_retry_after_no_wait() {
        let tracker = RateLimitTracker::new();
        {
            let mut state = state_for_test(&tracker);
            // Set retry_after in the past
            state.retry_after = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(tracker.should_wait().is_none());
    }

    #[test]
    fn test_update_from_headers() {
        let tracker = RateLimitTracker::new();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-remaining", "5".parse().unwrap());
        headers.insert("retry-after", "2".parse().unwrap());

        tracker.update_from_headers(&headers);

        assert_eq!(tracker.remaining(), Some(5));
    }

    #[test]
    fn test_update_from_headers_parses_fractional_reset_and_retry_after() {
        let tracker = RateLimitTracker::new();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset", "1.25".parse().unwrap());
        headers.insert("retry-after", "1.5".parse().unwrap());

        let before_update = Instant::now();
        tracker.update_from_headers(&headers);

        let state = state_for_test(&tracker);
        let reset_wait = state
            .reset_at
            .and_then(|reset_at| reset_at.checked_duration_since(before_update))
            .expect("fractional reset should be recorded");
        let retry_wait = state
            .retry_after
            .and_then(|retry_at| retry_at.checked_duration_since(before_update))
            .expect("fractional retry-after should be recorded");
        assert!(reset_wait >= Duration::from_secs(1));
        assert!(retry_wait >= Duration::from_secs(1));
    }

    #[test]
    fn test_update_from_headers_parses_suffixed_duration() {
        let tracker = RateLimitTracker::new();
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-reset-requests", "1.25s".parse().unwrap());

        let before_update = Instant::now();
        tracker.update_from_headers(&headers);

        let state = state_for_test(&tracker);
        let reset_wait = state
            .reset_at
            .and_then(|reset_at| reset_at.checked_duration_since(before_update))
            .expect("suffixed reset should be recorded");
        assert!(reset_wait >= Duration::from_secs(1));
    }

    #[test]
    fn test_update_from_headers_ignores_invalid_duration_values() {
        for value in ["1e300", "1e308", "NaN", "inf", "-inf", "-1", "0"] {
            assert_duration_header_is_ignored("x-ratelimit-reset", value);
            assert_duration_header_is_ignored("retry-after", value);
            assert_duration_header_is_ignored("x-ratelimit-reset-requests", &format!("{value}s"));
        }
    }

    #[test]
    fn test_reset() {
        let tracker = RateLimitTracker::new();
        {
            let mut state = state_for_test(&tracker);
            state.remaining = Some(0);
        }
        tracker.reset();
        assert!(tracker.remaining().is_none());
    }

    #[test]
    fn test_parse_duration_string() {
        assert_eq!(parse_duration_string("1s"), Some(Duration::from_secs(1)));
        assert_eq!(
            parse_duration_string("200ms"),
            Some(Duration::from_millis(200))
        );
        assert_eq!(
            parse_duration_string("1.5s"),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(parse_duration_string("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_duration_string("5"), Some(Duration::from_secs(5)));
    }

    #[test]
    fn test_parse_reset_value_relative() {
        let val = parse_reset_value("10").unwrap();
        assert!((val - 10.0).abs() < 0.01);
    }
}
