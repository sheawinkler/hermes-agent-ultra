//! Credential pool for managing multiple API keys.
//!
//! Supports round-robin key selection with rate-limit-aware skipping.
//! When a key is rate-limited, it is temporarily skipped in favor of
//! available keys.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Manages multiple API keys with round-robin selection and rate-limit tracking.
#[derive(Debug)]
pub struct CredentialPool {
    inner: Mutex<PoolInner>,
}

#[derive(Debug)]
struct PoolInner {
    keys: Vec<KeyEntry>,
    /// Index of the next key to try (round-robin).
    next_index: usize,
    /// Keys temporarily removed from rotation due to persistent failures.
    failed_keys: Vec<FailedKey>,
}

#[derive(Debug, Clone)]
struct FailedKey {
    key: String,
    failed_at: Instant,
    cooldown: Duration,
}

#[derive(Debug, Clone)]
struct KeyEntry {
    /// The API key.
    key: String,
    /// If rate-limited, when the limit expires.
    rate_limited_until: Option<Instant>,
    /// Number of times this key has been used.
    use_count: u64,
}

impl CredentialPool {
    /// Create a new credential pool with the given API keys.
    pub fn new(keys: Vec<String>) -> Self {
        let entries = keys
            .into_iter()
            .map(|key| KeyEntry {
                key,
                rate_limited_until: None,
                use_count: 0,
            })
            .collect();

        Self {
            inner: Mutex::new(PoolInner {
                keys: entries,
                next_index: 0,
                failed_keys: Vec::new(),
            }),
        }
    }

    /// Create a pool with a single key.
    pub fn single(key: impl Into<String>) -> Self {
        Self::new(vec![key.into()])
    }

    /// Get the best available API key.
    ///
    /// Uses round-robin selection, skipping rate-limited keys.
    /// If all keys are rate-limited, returns the one that will be
    /// available soonest.
    pub fn get_key(&self) -> String {
        let mut inner = match self.inner.lock() {
            Ok(i) => i,
            Err(e) => {
                let guard = e.into_inner();
                return guard.keys.first().map(|k| k.key.clone()).unwrap_or_default();
            }
        };

        // Auto-recover failed keys whose cooldown has elapsed.
        Self::recover_failed_inner(&mut inner);

        if inner.keys.is_empty() {
            return String::new();
        }

        let now = Instant::now();
        let len = inner.keys.len();
        let start = inner.next_index % len;

        // Try to find a non-rate-limited key starting from next_index
        for offset in 0..len {
            let idx = (start + offset) % len;
            let entry = &inner.keys[idx];
            if entry.rate_limited_until.map_or(true, |until| until <= now) {
                inner.next_index = (idx + 1) % len;
                inner.keys[idx].use_count += 1;
                inner.keys[idx].rate_limited_until = None; // Clear expired limit
                return inner.keys[idx].key.clone();
            }
        }

        // All keys are rate-limited — pick the one expiring soonest
        let best_idx = inner
            .keys
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| entry.rate_limited_until.unwrap_or(now))
            .map(|(idx, _)| idx)
            .unwrap_or(0);

        inner.next_index = (best_idx + 1) % len;
        inner.keys[best_idx].use_count += 1;
        inner.keys[best_idx].key.clone()
    }

    /// Mark a key as rate-limited for the given duration.
    pub fn mark_rate_limited(&self, key: &str, duration: Duration) {
        if let Ok(mut inner) = self.inner.lock() {
            let until = Instant::now() + duration;
            for entry in &mut inner.keys {
                if entry.key == key {
                    entry.rate_limited_until = Some(until);
                    break;
                }
            }
        }
    }

    /// Clear the rate-limit status for a specific key.
    pub fn clear_rate_limit(&self, key: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            for entry in &mut inner.keys {
                if entry.key == key {
                    entry.rate_limited_until = None;
                    break;
                }
            }
        }
    }

    /// Get the number of keys in the pool.
    pub fn len(&self) -> usize {
        self.inner.lock().map(|i| i.keys.len()).unwrap_or(0)
    }

    /// Check if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get usage counts for all keys (for diagnostics).
    pub fn usage_counts(&self) -> Vec<(String, u64)> {
        self.inner
            .lock()
            .map(|i| {
                i.keys
                    .iter()
                    .map(|e| (e.key.clone(), e.use_count))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Temporarily remove a key from rotation due to a persistent failure
    /// (e.g. auth error, revoked key). The key will be automatically
    /// re-added after `cooldown` elapses.
    pub fn mark_failed(&self, key: &str, cooldown: Duration) {
        if let Ok(mut inner) = self.inner.lock() {
            if let Some(pos) = inner.keys.iter().position(|e| e.key == key) {
                let removed = inner.keys.remove(pos);
                if inner.next_index > 0 && inner.next_index > pos {
                    inner.next_index -= 1;
                }
                if !inner.keys.is_empty() {
                    inner.next_index %= inner.keys.len();
                } else {
                    inner.next_index = 0;
                }
                inner.failed_keys.push(FailedKey {
                    key: removed.key,
                    failed_at: Instant::now(),
                    cooldown,
                });
                tracing::warn!(
                    "Credential marked as failed, will recover after {}s ({} active keys remaining)",
                    cooldown.as_secs(),
                    inner.keys.len(),
                );
            }
        }
    }

    /// Re-add all failed keys whose cooldown has elapsed back into rotation.
    /// Returns the number of keys recovered.
    pub fn recover_failed(&self) -> usize {
        if let Ok(mut inner) = self.inner.lock() {
            Self::recover_failed_inner(&mut inner)
        } else {
            0
        }
    }

    fn recover_failed_inner(inner: &mut PoolInner) -> usize {
        let now = Instant::now();
        let mut recovered = 0;

        inner.failed_keys.retain(|fk| {
            if now.duration_since(fk.failed_at) >= fk.cooldown {
                inner.keys.push(KeyEntry {
                    key: fk.key.clone(),
                    rate_limited_until: None,
                    use_count: 0,
                });
                tracing::info!("Recovered previously failed credential back into pool");
                recovered += 1;
                false
            } else {
                true
            }
        });

        recovered
    }

    /// Get the number of currently failed (cooling-down) keys.
    pub fn failed_count(&self) -> usize {
        self.inner
            .lock()
            .map(|i| i.failed_keys.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_key() {
        let pool = CredentialPool::single("sk-test-123");
        assert_eq!(pool.get_key(), "sk-test-123");
        assert_eq!(pool.get_key(), "sk-test-123");
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn test_round_robin() {
        let pool = CredentialPool::new(vec![
            "key-a".to_string(),
            "key-b".to_string(),
            "key-c".to_string(),
        ]);

        let k1 = pool.get_key();
        let k2 = pool.get_key();
        let k3 = pool.get_key();
        let k4 = pool.get_key();

        assert_eq!(k1, "key-a");
        assert_eq!(k2, "key-b");
        assert_eq!(k3, "key-c");
        assert_eq!(k4, "key-a"); // wraps around
    }

    #[test]
    fn test_skip_rate_limited() {
        let pool = CredentialPool::new(vec![
            "key-a".to_string(),
            "key-b".to_string(),
            "key-c".to_string(),
        ]);

        // Rate-limit key-a
        pool.mark_rate_limited("key-a", Duration::from_secs(60));

        let k1 = pool.get_key();
        assert_ne!(k1, "key-a");
        // Should get key-b (skipping key-a)
        assert_eq!(k1, "key-b");

        let k2 = pool.get_key();
        assert_eq!(k2, "key-c");

        let k3 = pool.get_key();
        // key-a is still rate-limited, skip to key-b
        assert_eq!(k3, "key-b");
    }

    #[test]
    fn test_all_rate_limited_picks_soonest() {
        let pool = CredentialPool::new(vec![
            "key-a".to_string(),
            "key-b".to_string(),
        ]);

        pool.mark_rate_limited("key-a", Duration::from_secs(60));
        pool.mark_rate_limited("key-b", Duration::from_secs(10));

        // Should pick key-b since it expires sooner
        let key = pool.get_key();
        assert_eq!(key, "key-b");
    }

    #[test]
    fn test_clear_rate_limit() {
        let pool = CredentialPool::new(vec![
            "key-a".to_string(),
            "key-b".to_string(),
        ]);

        pool.mark_rate_limited("key-a", Duration::from_secs(60));
        pool.clear_rate_limit("key-a");

        // key-a should be available again
        let key = pool.get_key();
        assert_eq!(key, "key-a");
    }

    #[test]
    fn test_usage_counts() {
        let pool = CredentialPool::new(vec![
            "key-a".to_string(),
            "key-b".to_string(),
        ]);

        pool.get_key(); // key-a
        pool.get_key(); // key-b
        pool.get_key(); // key-a

        let counts = pool.usage_counts();
        assert_eq!(counts.len(), 2);
        let a_count = counts.iter().find(|(k, _)| k == "key-a").unwrap().1;
        let b_count = counts.iter().find(|(k, _)| k == "key-b").unwrap().1;
        assert_eq!(a_count, 2);
        assert_eq!(b_count, 1);
    }

    #[test]
    fn test_empty_pool() {
        let pool = CredentialPool::new(vec![]);
        assert!(pool.is_empty());
        assert_eq!(pool.get_key(), "");
    }
}
