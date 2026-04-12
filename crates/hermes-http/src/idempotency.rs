//! In-memory idempotency cache for policy HTTP mutations.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Caches successful HTTP responses keyed by route + idempotency key.
pub struct PolicyIdempotencyCache {
    inner: Mutex<HashMap<String, (Instant, u16, String)>>,
    ttl: Duration,
    max_entries: usize,
}

impl PolicyIdempotencyCache {
    pub fn from_env() -> Self {
        let ttl_secs = std::env::var("HERMES_HTTP_POLICY_IDEMPOTENCY_TTL_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0 && n <= 86400 * 7)
            .unwrap_or(3600);
        let max_entries = std::env::var("HERMES_HTTP_POLICY_IDEMPOTENCY_MAX_ENTRIES")
            .ok()
            .and_then(|s| s.parse().ok())
            .filter(|&n| n > 0)
            .unwrap_or(4096);
        Self {
            inner: Mutex::new(HashMap::new()),
            ttl: Duration::from_secs(ttl_secs),
            max_entries,
        }
    }

    /// Returns cached `(status, body)` when still fresh.
    pub fn get(&self, key: &str) -> Option<(u16, String)> {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.retain(|_, (t, _, _)| now.duration_since(*t) < self.ttl);
        g.get(key)
            .filter(|(t, _, _)| now.duration_since(*t) < self.ttl)
            .map(|(_, status, body)| (*status, body.clone()))
    }

    pub fn insert(&self, key: String, status: u16, body: String) {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        g.retain(|_, (t, _, _)| now.duration_since(*t) < self.ttl);
        if g.len() >= self.max_entries {
            if let Some(k) = g.keys().next().cloned() {
                g.remove(&k);
            }
        }
        g.insert(key, (now, status, body));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip() {
        let c = PolicyIdempotencyCache::from_env();
        c.insert(
            "POST /v1/policy/update\nk1".to_string(),
            200,
            r#"{"ok":true}"#.to_string(),
        );
        let got = c.get("POST /v1/policy/update\nk1").unwrap();
        assert_eq!(got.0, 200);
        assert_eq!(got.1, r#"{"ok":true}"#);
    }
}
