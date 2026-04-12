//! Persistent multi-credential pool for same-provider failover.
//!
//! Manages multiple API keys per provider with round-robin, fill-first,
//! random, and least-used selection strategies.  Tracks rate limit
//! exhaustion and automatic recovery.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const STATUS_OK: &str = "ok";
pub const STATUS_EXHAUSTED: &str = "exhausted";

pub const AUTH_TYPE_OAUTH: &str = "oauth";
pub const AUTH_TYPE_API_KEY: &str = "api_key";

pub const SOURCE_MANUAL: &str = "manual";

/// Cooldown before retrying an exhausted credential (1 hour).
pub const EXHAUSTED_TTL_429_SECONDS: u64 = 3600;
pub const EXHAUSTED_TTL_DEFAULT_SECONDS: u64 = 3600;

// ---------------------------------------------------------------------------
// Selection strategy
// ---------------------------------------------------------------------------

/// How the pool selects the next credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolStrategy {
    FillFirst,
    RoundRobin,
    Random,
    LeastUsed,
}

impl Default for PoolStrategy {
    fn default() -> Self {
        Self::FillFirst
    }
}

impl PoolStrategy {
    pub fn from_str_loose(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "round_robin" | "roundrobin" => Self::RoundRobin,
            "random" => Self::Random,
            "least_used" | "leastused" => Self::LeastUsed,
            _ => Self::FillFirst,
        }
    }
}

// ---------------------------------------------------------------------------
// PooledCredential
// ---------------------------------------------------------------------------

/// A single credential entry in the pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PooledCredential {
    pub provider: String,
    pub id: String,
    pub label: String,
    pub auth_type: String,
    pub priority: i32,
    pub source: String,
    pub access_token: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,

    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_status_at: Option<f64>,
    #[serde(default)]
    pub last_error_code: Option<u16>,
    #[serde(default)]
    pub last_error_reason: Option<String>,
    #[serde(default)]
    pub last_error_message: Option<String>,
    #[serde(default)]
    pub last_error_reset_at: Option<f64>,

    #[serde(default)]
    pub request_count: u64,
}

impl PooledCredential {
    /// Create a minimal credential entry.
    pub fn new(provider: &str, id: &str, access_token: &str) -> Self {
        Self {
            provider: provider.to_string(),
            id: id.to_string(),
            label: id.to_string(),
            auth_type: AUTH_TYPE_API_KEY.to_string(),
            priority: 0,
            source: SOURCE_MANUAL.to_string(),
            access_token: access_token.to_string(),
            refresh_token: None,
            base_url: None,
            expires_at: None,
            expires_at_ms: None,
            last_status: None,
            last_status_at: None,
            last_error_code: None,
            last_error_reason: None,
            last_error_message: None,
            last_error_reset_at: None,
            request_count: 0,
        }
    }

    /// The key to use at runtime (access_token for most providers).
    pub fn runtime_api_key(&self) -> &str {
        &self.access_token
    }

    /// The base URL to use at runtime.
    pub fn runtime_base_url(&self) -> Option<&str> {
        self.base_url.as_deref()
    }

    /// Whether this credential is currently in exhaustion cooldown.
    pub fn is_exhausted(&self) -> bool {
        self.last_status.as_deref() == Some(STATUS_EXHAUSTED)
    }

    /// The unix timestamp until which this credential is exhausted.
    pub fn exhausted_until(&self) -> Option<f64> {
        if !self.is_exhausted() {
            return None;
        }
        if let Some(reset_at) = self.last_error_reset_at {
            return Some(reset_at);
        }
        if let Some(status_at) = self.last_status_at {
            let ttl = exhausted_ttl(self.last_error_code);
            return Some(status_at + ttl as f64);
        }
        None
    }
}

fn exhausted_ttl(error_code: Option<u16>) -> u64 {
    match error_code {
        Some(429) => EXHAUSTED_TTL_429_SECONDS,
        _ => EXHAUSTED_TTL_DEFAULT_SECONDS,
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

// ---------------------------------------------------------------------------
// CredentialPool
// ---------------------------------------------------------------------------

/// Manages multiple API keys for a single provider.
pub struct CredentialPool {
    pub provider: String,
    entries: Vec<PooledCredential>,
    current_id: Option<String>,
    strategy: PoolStrategy,
    active_leases: HashMap<String, u32>,
}

impl CredentialPool {
    /// Create a new pool from a list of credentials.
    pub fn new(provider: &str, mut entries: Vec<PooledCredential>, strategy: PoolStrategy) -> Self {
        entries.sort_by_key(|e| e.priority);
        Self {
            provider: provider.to_string(),
            entries,
            current_id: None,
            strategy,
            active_leases: HashMap::new(),
        }
    }

    /// Whether the pool has any credentials at all.
    pub fn has_credentials(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Whether at least one credential is not currently exhausted.
    pub fn has_available(&self) -> bool {
        !self.available_entries().is_empty()
    }

    /// Return all entries.
    pub fn entries(&self) -> &[PooledCredential] {
        &self.entries
    }

    /// Return the currently selected credential.
    pub fn current(&self) -> Option<&PooledCredential> {
        self.current_id
            .as_ref()
            .and_then(|id| self.entries.iter().find(|e| &e.id == id))
    }

    /// Select the next available credential.
    pub fn select(&mut self) -> Option<&PooledCredential> {
        let available = self.available_entries();
        if available.is_empty() {
            self.current_id = None;
            return None;
        }

        let chosen_id = match self.strategy {
            PoolStrategy::Random => {
                use std::collections::hash_map::DefaultHasher;
                use std::hash::{Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                now_secs().to_bits().hash(&mut hasher);
                let idx = hasher.finish() as usize % available.len();
                available[idx].to_string()
            }
            PoolStrategy::LeastUsed => {
                let min_entry = available
                    .iter()
                    .min_by_key(|id| {
                        self.entries
                            .iter()
                            .find(|e| &e.id == *id)
                            .map(|e| e.request_count)
                            .unwrap_or(u64::MAX)
                    })
                    .cloned();
                min_entry.unwrap_or_else(|| available[0].clone())
            }
            PoolStrategy::RoundRobin => {
                // Pick next after current
                if let Some(ref current) = self.current_id {
                    let pos = available.iter().position(|id| id == current);
                    match pos {
                        Some(p) => available[(p + 1) % available.len()].clone(),
                        None => available[0].clone(),
                    }
                } else {
                    available[0].clone()
                }
            }
            PoolStrategy::FillFirst => available[0].clone(),
        };

        self.current_id = Some(chosen_id.clone());
        self.entries.iter().find(|e| e.id == chosen_id)
    }

    /// Mark the current credential as exhausted and rotate to next.
    pub fn mark_exhausted_and_rotate(
        &mut self,
        status_code: Option<u16>,
        error_reason: Option<&str>,
        error_message: Option<&str>,
        reset_at: Option<f64>,
    ) -> Option<&PooledCredential> {
        if let Some(ref current_id) = self.current_id.clone() {
            if let Some(entry) = self.entries.iter_mut().find(|e| &e.id == current_id) {
                entry.last_status = Some(STATUS_EXHAUSTED.to_string());
                entry.last_status_at = Some(now_secs());
                entry.last_error_code = status_code;
                entry.last_error_reason = error_reason.map(|s| s.to_string());
                entry.last_error_message = error_message.map(|s| s.to_string());
                entry.last_error_reset_at = reset_at;
            }
        }
        self.current_id = None;
        self.select()
    }

    /// Mark a specific key as recovered (clear exhaustion status).
    pub fn mark_key_recovered(&mut self, key_id: &str) {
        if let Some(entry) = self.entries.iter_mut().find(|e| e.id == key_id) {
            entry.last_status = Some(STATUS_OK.to_string());
            entry.last_status_at = None;
            entry.last_error_code = None;
            entry.last_error_reason = None;
            entry.last_error_message = None;
            entry.last_error_reset_at = None;
        }
    }

    /// Reset all exhaustion statuses.
    pub fn reset_statuses(&mut self) -> usize {
        let mut count = 0;
        for entry in &mut self.entries {
            if entry.last_status.is_some() || entry.last_error_code.is_some() {
                entry.last_status = None;
                entry.last_status_at = None;
                entry.last_error_code = None;
                entry.last_error_reason = None;
                entry.last_error_message = None;
                entry.last_error_reset_at = None;
                count += 1;
            }
        }
        count
    }

    /// Add a new credential to the pool.
    pub fn add_entry(&mut self, mut entry: PooledCredential) {
        let next_priority = self.entries.iter().map(|e| e.priority).max().unwrap_or(-1) + 1;
        entry.priority = next_priority;
        self.entries.push(entry);
    }

    /// Remove a credential by 1-based index.
    pub fn remove_index(&mut self, index: usize) -> Option<PooledCredential> {
        if index < 1 || index > self.entries.len() {
            return None;
        }
        let removed = self.entries.remove(index - 1);
        // Re-normalize priorities
        for (i, entry) in self.entries.iter_mut().enumerate() {
            entry.priority = i as i32;
        }
        if self.current_id.as_deref() == Some(&removed.id) {
            self.current_id = None;
        }
        Some(removed)
    }

    /// Acquire a lease on a credential for concurrent use.
    pub fn acquire_lease(&mut self, credential_id: Option<&str>) -> Option<String> {
        if let Some(id) = credential_id {
            *self.active_leases.entry(id.to_string()).or_insert(0) += 1;
            self.current_id = Some(id.to_string());
            return Some(id.to_string());
        }

        let available = self.available_entries();
        if available.is_empty() {
            return None;
        }

        // Pick the credential with fewest active leases
        let chosen = available
            .iter()
            .min_by_key(|id| self.active_leases.get(*id).copied().unwrap_or(0))
            .cloned()?;

        *self.active_leases.entry(chosen.clone()).or_insert(0) += 1;
        self.current_id = Some(chosen.clone());
        Some(chosen)
    }

    /// Release a previously acquired lease.
    pub fn release_lease(&mut self, credential_id: &str) {
        if let Some(count) = self.active_leases.get_mut(credential_id) {
            if *count <= 1 {
                self.active_leases.remove(credential_id);
            } else {
                *count -= 1;
            }
        }
    }

    /// Increment the request count for the current credential.
    pub fn increment_request_count(&mut self) {
        if let Some(ref current_id) = self.current_id.clone() {
            if let Some(entry) = self.entries.iter_mut().find(|e| &e.id == current_id) {
                entry.request_count += 1;
            }
        }
    }

    // -- internal --

    fn available_entries(&self) -> Vec<String> {
        let now = now_secs();
        self.entries
            .iter()
            .filter(|e| {
                if !e.is_exhausted() {
                    return true;
                }
                match e.exhausted_until() {
                    Some(until) if now >= until => true,
                    Some(_) => false,
                    None => true, // No cooldown info — treat as available
                }
            })
            .map(|e| e.id.clone())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Thread-safe wrapper
// ---------------------------------------------------------------------------

/// Thread-safe credential pool manager.
pub struct PoolManager {
    pools: Mutex<HashMap<String, CredentialPool>>,
}

impl PoolManager {
    pub fn new() -> Self {
        Self {
            pools: Mutex::new(HashMap::new()),
        }
    }

    /// Register a pool for a provider.
    pub fn register(&self, pool: CredentialPool) {
        let mut pools = self.pools.lock().unwrap();
        pools.insert(pool.provider.clone(), pool);
    }

    /// Get the next key for a provider (round-robin/strategy-based).
    pub fn get_next_key(&self, provider: &str) -> Option<String> {
        let mut pools = self.pools.lock().unwrap();
        let pool = pools.get_mut(provider)?;
        pool.select().map(|e| e.access_token.clone())
    }

    /// Mark a key as exhausted (rate limited).
    pub fn mark_key_exhausted(&self, provider: &str, status_code: Option<u16>) -> Option<String> {
        let mut pools = self.pools.lock().unwrap();
        let pool = pools.get_mut(provider)?;
        pool.mark_exhausted_and_rotate(status_code, None, None, None)
            .map(|e| e.access_token.clone())
    }

    /// Mark a key as recovered.
    pub fn mark_key_recovered(&self, provider: &str, key_id: &str) {
        let mut pools = self.pools.lock().unwrap();
        if let Some(pool) = pools.get_mut(provider) {
            pool.mark_key_recovered(key_id);
        }
    }

    /// List all providers with pools.
    pub fn list_providers(&self) -> Vec<String> {
        let pools = self.pools.lock().unwrap();
        pools.keys().cloned().collect()
    }
}

impl Default for PoolManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cred(provider: &str, id: &str, token: &str) -> PooledCredential {
        PooledCredential::new(provider, id, token)
    }

    #[test]
    fn test_fill_first() {
        let entries = vec![
            make_cred("openai", "a", "key-a"),
            make_cred("openai", "b", "key-b"),
        ];
        let mut pool = CredentialPool::new("openai", entries, PoolStrategy::FillFirst);
        let selected = pool.select().unwrap();
        assert_eq!(selected.id, "a");
    }

    #[test]
    fn test_exhaustion_and_rotation() {
        let entries = vec![
            make_cred("openai", "a", "key-a"),
            make_cred("openai", "b", "key-b"),
        ];
        let mut pool = CredentialPool::new("openai", entries, PoolStrategy::FillFirst);
        pool.select();
        let next = pool.mark_exhausted_and_rotate(Some(429), None, None, None);
        assert!(next.is_some());
        assert_eq!(next.unwrap().id, "b");
    }

    #[test]
    fn test_empty_pool() {
        let mut pool = CredentialPool::new("openai", vec![], PoolStrategy::FillFirst);
        assert!(!pool.has_credentials());
        assert!(pool.select().is_none());
    }

    #[test]
    fn test_reset_statuses() {
        let mut entries = vec![make_cred("openai", "a", "key-a")];
        entries[0].last_status = Some(STATUS_EXHAUSTED.to_string());
        entries[0].last_error_code = Some(429);
        let mut pool = CredentialPool::new("openai", entries, PoolStrategy::FillFirst);
        let count = pool.reset_statuses();
        assert_eq!(count, 1);
        assert!(!pool.entries()[0].is_exhausted());
    }

    #[test]
    fn test_add_and_remove() {
        let mut pool = CredentialPool::new("openai", vec![], PoolStrategy::FillFirst);
        pool.add_entry(make_cred("openai", "a", "key-a"));
        pool.add_entry(make_cred("openai", "b", "key-b"));
        assert_eq!(pool.entries().len(), 2);
        pool.remove_index(1);
        assert_eq!(pool.entries().len(), 1);
        assert_eq!(pool.entries()[0].id, "b");
    }

    #[test]
    fn test_pool_manager() {
        let manager = PoolManager::new();
        let entries = vec![make_cred("openai", "a", "key-a")];
        let pool = CredentialPool::new("openai", entries, PoolStrategy::FillFirst);
        manager.register(pool);
        let key = manager.get_next_key("openai");
        assert_eq!(key, Some("key-a".to_string()));
    }
}
