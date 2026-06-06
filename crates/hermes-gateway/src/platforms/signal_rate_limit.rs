//! Signal attachment rate-limit scheduler.
//!
//! Signal's server-side attachment bucket is per account.  This module keeps a
//! process-wide token-bucket model so concurrent gateway sessions pace uploads
//! before the server starts returning 429s.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Mutex as AsyncMutex;

pub const SIGNAL_MAX_ATTACHMENTS_PER_MSG: usize = 32;
pub const SIGNAL_RATE_LIMIT_BUCKET_CAPACITY: f64 = 50.0;
pub const SIGNAL_RATE_LIMIT_DEFAULT_RETRY_AFTER: f64 = 4.0;
pub const SIGNAL_RATE_LIMIT_MAX_ATTEMPTS: usize = 2;
pub const SIGNAL_BATCH_PACING_NOTICE_THRESHOLD: f64 = 10.0;

static SCHEDULER: Mutex<Option<Arc<AsyncMutex<SignalAttachmentScheduler>>>> = Mutex::new(None);

#[derive(Debug, Clone, PartialEq)]
pub struct SignalSchedulerState {
    pub tokens: f64,
    pub capacity: usize,
    pub refill_rate: f64,
    pub refill_seconds_per_token: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalSchedulerError {
    message: String,
}

impl std::fmt::Display for SignalSchedulerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SignalSchedulerError {}

/// Token-bucket simulator for Signal attachment sends.
#[derive(Debug, Clone)]
pub struct SignalAttachmentScheduler {
    pub capacity: f64,
    pub tokens: f64,
    pub refill_rate: f64,
    pub last_refill: Instant,
}

impl Default for SignalAttachmentScheduler {
    fn default() -> Self {
        Self::new(
            SIGNAL_RATE_LIMIT_BUCKET_CAPACITY,
            SIGNAL_RATE_LIMIT_DEFAULT_RETRY_AFTER,
        )
    }
}

impl SignalAttachmentScheduler {
    pub fn new(capacity: f64, default_retry_after: f64) -> Self {
        let capacity = capacity.max(1.0);
        let retry_after = default_retry_after.max(0.001);
        Self {
            capacity,
            tokens: capacity,
            refill_rate: 1.0 / retry_after,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_refill);
        if elapsed > Duration::ZERO && self.tokens < self.capacity {
            self.tokens =
                (self.tokens + elapsed.as_secs_f64() * self.refill_rate).min(self.capacity);
        }
        self.last_refill = now;
    }

    pub fn estimate_wait(&self, n: usize) -> f64 {
        if n == 0 {
            return 0.0;
        }
        let elapsed = Instant::now()
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        let projected = if elapsed > 0.0 && self.tokens < self.capacity {
            (self.tokens + elapsed * self.refill_rate).min(self.capacity)
        } else {
            self.tokens
        };
        let deficit = n as f64 - projected;
        if deficit <= 0.0 {
            0.0
        } else {
            deficit / self.refill_rate
        }
    }

    /// Wait until `n` attachment tokens should be available.
    ///
    /// This mirrors upstream behavior: acquire waits but does not deduct.  The
    /// caller records the completed RPC with `report_rpc_duration`, because the
    /// server checks capacity at RPC start and upload time should not be
    /// credited as refill.
    pub async fn acquire(&mut self, n: usize) -> Result<f64, SignalSchedulerError> {
        if n == 0 {
            return Ok(0.0);
        }
        if n as f64 > self.capacity {
            return Err(SignalSchedulerError {
                message: format!(
                    "Signal scheduler requested {n} tokens but capacity is {}",
                    self.capacity as usize
                ),
            });
        }

        let mut slept = 0.0;
        loop {
            self.refill();
            if self.tokens >= n as f64 {
                return Ok(slept);
            }

            let wait = (n as f64 - self.tokens) / self.refill_rate;
            tokio::time::sleep(Duration::from_secs_f64(wait)).await;
            slept += wait;
        }
    }

    pub fn report_rpc_duration(&mut self, _rpc_duration: Duration, n_attachments: usize) {
        if n_attachments == 0 {
            return;
        }
        self.tokens = (self.tokens - n_attachments as f64).max(0.0);
        self.last_refill = Instant::now();
    }

    pub fn feedback(&mut self, retry_after: Option<f64>, _n_attempted: usize) {
        if let Some(retry_after) = retry_after.filter(|v| *v > 0.0) {
            self.refill_rate = 1.0 / retry_after;
        }
        self.tokens = 0.0;
        self.last_refill = Instant::now();
    }

    pub fn state(&self) -> SignalSchedulerState {
        let projected = (self.tokens
            + Instant::now()
                .saturating_duration_since(self.last_refill)
                .as_secs_f64()
                * self.refill_rate)
            .min(self.capacity);
        SignalSchedulerState {
            tokens: (projected * 10.0).round() / 10.0,
            capacity: self.capacity as usize,
            refill_rate: self.refill_rate,
            refill_seconds_per_token: if self.refill_rate > 0.0 {
                1.0 / self.refill_rate
            } else {
                f64::INFINITY
            },
        }
    }
}

pub fn get_scheduler() -> Arc<AsyncMutex<SignalAttachmentScheduler>> {
    let mut guard = SCHEDULER.lock().expect("signal scheduler lock poisoned");
    guard
        .get_or_insert_with(|| Arc::new(AsyncMutex::new(SignalAttachmentScheduler::default())))
        .clone()
}

#[cfg(test)]
pub fn reset_scheduler() {
    *SCHEDULER.lock().expect("signal scheduler lock poisoned") = None;
}

pub fn extract_retry_after_seconds(message: &str) -> Option<f64> {
    let lower = message.to_ascii_lowercase();
    let marker = "retry after ";
    let idx = lower.find(marker)?;
    let rest = &message[idx + marker.len()..];
    let number: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    number.parse::<f64>().ok()
}

pub fn is_signal_rate_limit_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.contains("[429]")
        || lower.contains("ratelimit")
        || lower.contains("retrylaterexception")
        || lower.contains("retry after")
}

pub fn format_wait(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 90.0 {
        format!("{}s", seconds.round() as u64)
    } else {
        format!("{} min", ((seconds / 60.0).round() as u64).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_defaults_match_signal_bucket_contract() {
        let scheduler = SignalAttachmentScheduler::default();
        assert_eq!(scheduler.capacity, SIGNAL_RATE_LIMIT_BUCKET_CAPACITY);
        assert_eq!(scheduler.tokens, scheduler.capacity);
        assert_eq!(
            scheduler.refill_rate,
            1.0 / SIGNAL_RATE_LIMIT_DEFAULT_RETRY_AFTER
        );
    }

    #[test]
    fn estimate_wait_is_zero_when_bucket_has_enough_tokens() {
        let scheduler = SignalAttachmentScheduler::default();
        assert_eq!(scheduler.estimate_wait(10), 0.0);
        assert_eq!(
            scheduler.estimate_wait(SIGNAL_RATE_LIMIT_BUCKET_CAPACITY as usize),
            0.0
        );
    }

    #[test]
    fn estimate_wait_is_proportional_to_deficit() {
        let scheduler = SignalAttachmentScheduler {
            tokens: 0.0,
            last_refill: Instant::now(),
            ..Default::default()
        };
        let wait = scheduler.estimate_wait(32);
        let expected = 32.0 / scheduler.refill_rate;
        assert!((wait - expected).abs() < 0.001, "{wait} != {expected}");
    }

    #[tokio::test]
    async fn acquire_zero_is_noop() {
        let mut scheduler = SignalAttachmentScheduler::default();
        let original = scheduler.tokens;
        let waited = scheduler.acquire(0).await.expect("zero acquire");
        assert_eq!(waited, 0.0);
        assert_eq!(scheduler.tokens, original);
    }

    #[tokio::test]
    async fn acquire_within_capacity_does_not_deduct_until_reported() {
        let mut scheduler = SignalAttachmentScheduler::default();
        let waited = scheduler.acquire(10).await.expect("acquire");
        assert_eq!(waited, 0.0);
        assert_eq!(scheduler.tokens, scheduler.capacity);
        scheduler.report_rpc_duration(Duration::from_millis(1), 10);
        assert_eq!(scheduler.tokens, scheduler.capacity - 10.0);
    }

    #[tokio::test]
    async fn acquire_rejects_more_than_capacity() {
        let mut scheduler = SignalAttachmentScheduler::default();
        let err = scheduler
            .acquire(SIGNAL_RATE_LIMIT_BUCKET_CAPACITY as usize + 1)
            .await
            .expect_err("over capacity should fail");
        assert!(err.to_string().contains("capacity"));
    }

    #[test]
    fn feedback_calibrates_refill_rate_and_zeros_tokens() {
        let mut scheduler = SignalAttachmentScheduler::default();
        scheduler.feedback(Some(42.0), 1);
        assert_eq!(scheduler.refill_rate, 1.0 / 42.0);
        assert_eq!(scheduler.tokens, 0.0);
    }

    #[test]
    fn refill_clamps_at_capacity() {
        let mut scheduler = SignalAttachmentScheduler {
            tokens: 0.0,
            last_refill: Instant::now() - Duration::from_secs(365 * 24 * 3600),
            ..Default::default()
        };
        scheduler.refill();
        assert_eq!(scheduler.tokens, scheduler.capacity);
    }

    #[test]
    fn retry_after_and_rate_limit_detection_cover_signal_cli_shapes() {
        assert_eq!(
            extract_retry_after_seconds("AttachmentInvalidException: Retry after 42 seconds"),
            Some(42.0)
        );
        assert!(is_signal_rate_limit_error("[429] RateLimitException"));
        assert!(is_signal_rate_limit_error("RetryLaterException"));
        assert!(!is_signal_rate_limit_error("network unavailable"));
    }

    #[test]
    fn singleton_can_be_reset_for_tests() {
        reset_scheduler();
        let first = get_scheduler();
        reset_scheduler();
        let second = get_scheduler();
        assert!(!Arc::ptr_eq(&first, &second));
    }
}
