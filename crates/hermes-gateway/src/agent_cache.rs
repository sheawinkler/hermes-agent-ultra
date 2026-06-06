//! Rust-native gateway agent cache helpers.
//!
//! The Python gateway caches an agent per session and invalidates it when the
//! runtime model/provider/tool/config signature changes. This module keeps the
//! same contract without depending on the concrete Rust agent type.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const CACHE_BUSTING_CONFIG_KEYS: &[(&str, &str)] = &[
    ("model", "context_length"),
    ("model", "max_tokens"),
    ("compression", "enabled"),
    ("compression", "threshold"),
    ("compression", "target_ratio"),
    ("compression", "protect_last_n"),
    ("memory", "provider"),
    ("kanban", "dispatch_in_gateway"),
];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfigSignatureInput {
    pub model: String,
    #[serde(default)]
    pub runtime: BTreeMap<String, Value>,
    #[serde(default)]
    pub toolsets: Vec<String>,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub cache_keys: BTreeMap<String, Value>,
}

fn secretish_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("credential")
}

fn fingerprint_secret(raw: &str) -> Value {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    serde_json::json!({
        "len": raw.chars().count(),
        "sha256": format!("{:x}", hasher.finalize()),
    })
}

fn canonical_value(key_hint: &str, value: &Value) -> Value {
    if secretish_key(key_hint) {
        if let Some(s) = value.as_str() {
            return fingerprint_secret(s);
        }
    }

    match value {
        Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical_value(key, value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| canonical_value(key_hint, value))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn canonical_map(input: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    input
        .iter()
        .map(|(key, value)| (key.clone(), canonical_value(key, value)))
        .collect()
}

/// Stable, secret-safe config signature.
///
/// Full token values affect the digest, but raw secrets are first converted to
/// SHA-256 fingerprints so diagnostic callers never need to retain plaintext.
pub fn agent_config_signature(input: &AgentConfigSignatureInput) -> String {
    let mut toolsets = input.toolsets.clone();
    toolsets.sort();
    let payload = serde_json::json!({
        "model": input.model,
        "runtime": canonical_map(&input.runtime),
        "toolsets": toolsets,
        "system_prompt": input.system_prompt,
        "cache_keys": canonical_map(&input.cache_keys),
    });
    let encoded = serde_json::to_vec(&payload).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(encoded);
    format!("{:x}", hasher.finalize())
}

fn object_subkey<'a>(config: &'a Value, section: &str, key: &str) -> Option<&'a Value> {
    config
        .get(section)
        .and_then(Value::as_object)
        .and_then(|section| section.get(key))
}

/// Extract the config subset that must bust a cached gateway agent.
///
/// Missing keys are included as `null` so adding a value later changes the
/// signature deterministically. `tools.registry_generation` models MCP/tool
/// reloads that mutate the effective toolset without changing config.yaml.
pub fn extract_cache_busting_config(
    config: Option<&Value>,
    tools_registry_generation: Option<u64>,
) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (section, key) in CACHE_BUSTING_CONFIG_KEYS {
        let value = config
            .and_then(|cfg| object_subkey(cfg, section, key))
            .cloned()
            .unwrap_or(Value::Null);
        out.insert(format!("{section}.{key}"), value);
    }
    out.insert(
        "tools.registry_generation".to_string(),
        tools_registry_generation
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    out
}

struct AgentCacheEntry<T> {
    agent: T,
    signature: String,
    last_activity: Instant,
}

type ReleaseHook<T> = Arc<dyn Fn(&T) + Send + Sync>;

/// Session-keyed LRU cache for gateway agents.
pub struct GatewayAgentCache<T> {
    entries: HashMap<String, AgentCacheEntry<T>>,
    lru: VecDeque<String>,
    max_size: usize,
    idle_ttl: Duration,
    release_hook: Option<ReleaseHook<T>>,
}

impl<T> GatewayAgentCache<T> {
    pub fn new(max_size: usize, idle_ttl: Duration) -> Self {
        Self {
            entries: HashMap::new(),
            lru: VecDeque::new(),
            max_size,
            idle_ttl,
            release_hook: None,
        }
    }

    pub fn with_release_hook(mut self, hook: ReleaseHook<T>) -> Self {
        self.release_hook = Some(hook);
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn touch_lru(&mut self, session_key: &str) {
        self.lru.retain(|key| key != session_key);
        self.lru.push_back(session_key.to_string());
    }

    fn release(&self, agent: &T) {
        if let Some(hook) = self.release_hook.as_ref() {
            hook(agent);
        }
    }

    pub fn insert_at_with_active(
        &mut self,
        session_key: impl Into<String>,
        agent: T,
        signature: impl Into<String>,
        now: Instant,
        is_active: impl FnMut(&str, &T) -> bool,
    ) {
        let session_key = session_key.into();
        if let Some(old) = self.entries.remove(&session_key) {
            self.release(&old.agent);
        }
        self.entries.insert(
            session_key.clone(),
            AgentCacheEntry {
                agent,
                signature: signature.into(),
                last_activity: now,
            },
        );
        self.touch_lru(&session_key);
        self.enforce_cap_with_active(is_active);
    }

    pub fn insert_at(
        &mut self,
        session_key: impl Into<String>,
        agent: T,
        signature: impl Into<String>,
        now: Instant,
    ) {
        self.insert_at_with_active(session_key, agent, signature, now, |_, _| false);
    }

    pub fn insert_with_active(
        &mut self,
        session_key: impl Into<String>,
        agent: T,
        signature: impl Into<String>,
        is_active: impl FnMut(&str, &T) -> bool,
    ) {
        self.insert_at_with_active(session_key, agent, signature, Instant::now(), is_active);
    }

    pub fn insert(
        &mut self,
        session_key: impl Into<String>,
        agent: T,
        signature: impl Into<String>,
    ) {
        self.insert_at(session_key, agent, signature, Instant::now());
    }

    pub fn get_if_signature_at(
        &mut self,
        session_key: &str,
        signature: &str,
        now: Instant,
    ) -> Option<&mut T> {
        let matches = self
            .entries
            .get(session_key)
            .map(|entry| entry.signature == signature)
            .unwrap_or(false);
        if !matches {
            return None;
        }
        self.touch_lru(session_key);
        let entry = self.entries.get_mut(session_key)?;
        entry.last_activity = now;
        Some(&mut entry.agent)
    }

    pub fn get_if_signature(&mut self, session_key: &str, signature: &str) -> Option<&mut T> {
        self.get_if_signature_at(session_key, signature, Instant::now())
    }

    pub fn evict(&mut self, session_key: &str) -> bool {
        self.lru.retain(|key| key != session_key);
        if let Some(entry) = self.entries.remove(session_key) {
            self.release(&entry.agent);
            true
        } else {
            false
        }
    }

    pub fn enforce_cap_with_active(
        &mut self,
        mut is_active: impl FnMut(&str, &T) -> bool,
    ) -> usize {
        if self.max_size == 0 {
            return 0;
        }
        let excess = self.entries.len().saturating_sub(self.max_size);
        if excess == 0 {
            return 0;
        }
        let candidates = self.lru.iter().take(excess).cloned().collect::<Vec<_>>();
        let mut evicted = 0usize;
        for key in candidates {
            let active = self
                .entries
                .get(&key)
                .map(|entry| is_active(&key, &entry.agent))
                .unwrap_or(false);
            if active {
                continue;
            }
            if self.evict(&key) {
                evicted += 1;
            }
        }
        evicted
    }

    pub fn enforce_cap(&mut self) -> usize {
        self.enforce_cap_with_active(|_, _| false)
    }

    pub fn sweep_idle_at_with_active(
        &mut self,
        now: Instant,
        mut is_active: impl FnMut(&str, &T) -> bool,
    ) -> usize {
        if self.idle_ttl.is_zero() {
            return 0;
        }
        let stale = self
            .entries
            .iter()
            .filter(|(key, entry)| {
                now.duration_since(entry.last_activity) > self.idle_ttl
                    && !is_active(key, &entry.agent)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let count = stale.len();
        for key in stale {
            self.evict(&key);
        }
        count
    }

    pub fn sweep_idle_at(&mut self, now: Instant) -> usize {
        self.sweep_idle_at_with_active(now, |_, _| false)
    }

    pub fn sweep_idle(&mut self) -> usize {
        self.sweep_idle_at(Instant::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::Mutex;

    fn runtime(api_key: &str, provider: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("api_key".to_string(), Value::String(api_key.to_string())),
            (
                "base_url".to_string(),
                Value::String("https://example.test/v1".to_string()),
            ),
            ("provider".to_string(), Value::String(provider.to_string())),
        ])
    }

    #[test]
    fn signature_is_stable_for_same_input() {
        let input = AgentConfigSignatureInput {
            model: "claude-sonnet-4".to_string(),
            runtime: runtime("sk-test123", "openrouter"),
            toolsets: vec!["hermes-telegram".to_string()],
            system_prompt: String::new(),
            cache_keys: BTreeMap::new(),
        };
        assert_eq!(
            agent_config_signature(&input),
            agent_config_signature(&input)
        );
    }

    #[test]
    fn signature_changes_for_model_provider_toolset_and_full_token() {
        let base = AgentConfigSignatureInput {
            model: "m1".to_string(),
            runtime: runtime("eyJhbGci.token-for-account-a", "openrouter"),
            toolsets: vec!["hermes-telegram".to_string()],
            system_prompt: String::new(),
            cache_keys: BTreeMap::new(),
        };
        let mut changed_model = base.clone();
        changed_model.model = "m2".to_string();
        assert_ne!(
            agent_config_signature(&base),
            agent_config_signature(&changed_model)
        );

        let mut changed_provider = base.clone();
        changed_provider.runtime = runtime("eyJhbGci.token-for-account-a", "anthropic");
        assert_ne!(
            agent_config_signature(&base),
            agent_config_signature(&changed_provider)
        );

        let mut changed_toolset = base.clone();
        changed_toolset.toolsets = vec!["hermes-discord".to_string()];
        assert_ne!(
            agent_config_signature(&base),
            agent_config_signature(&changed_toolset)
        );

        let mut changed_token = base.clone();
        changed_token.runtime = runtime("eyJhbGci.token-for-account-b", "openrouter");
        assert_ne!(
            agent_config_signature(&base),
            agent_config_signature(&changed_token)
        );
    }

    #[test]
    fn reasoning_is_not_part_of_signature_unless_caller_adds_it() {
        let base = AgentConfigSignatureInput {
            model: "m".to_string(),
            runtime: runtime("k", "p"),
            toolsets: vec![],
            system_prompt: String::new(),
            cache_keys: BTreeMap::new(),
        };
        let same = base.clone();
        assert_eq!(agent_config_signature(&base), agent_config_signature(&same));
    }

    #[test]
    fn cache_busting_config_reads_documented_keys_and_missing_nulls() {
        let cfg = serde_json::json!({
            "model": {"context_length": 272000, "max_tokens": 4096},
            "compression": {"enabled": false, "threshold": 0.6, "target_ratio": 0.3, "protect_last_n": 25, "ignored": true},
            "memory": {"provider": "honcho"},
            "kanban": {"dispatch_in_gateway": false}
        });
        let out = extract_cache_busting_config(Some(&cfg), Some(123));
        assert_eq!(out["model.context_length"], Value::from(272000));
        assert_eq!(out["model.max_tokens"], Value::from(4096));
        assert_eq!(out["compression.enabled"], Value::from(false));
        assert_eq!(out["compression.threshold"], Value::from(0.6));
        assert_eq!(out["compression.target_ratio"], Value::from(0.3));
        assert_eq!(out["compression.protect_last_n"], Value::from(25));
        assert_eq!(out["memory.provider"], Value::from("honcho"));
        assert_eq!(out["kanban.dispatch_in_gateway"], Value::from(false));
        assert_eq!(out["tools.registry_generation"], Value::from(123));

        let missing = extract_cache_busting_config(None, None);
        for (section, key) in CACHE_BUSTING_CONFIG_KEYS {
            assert_eq!(missing[&format!("{section}.{key}")], Value::Null);
        }
        assert_eq!(missing["tools.registry_generation"], Value::Null);
    }

    #[test]
    fn cache_keys_change_signature_and_key_order_does_not_matter() {
        let mut a = BTreeMap::new();
        a.insert("model.context_length".to_string(), Value::from(200000));
        a.insert("compression.threshold".to_string(), Value::from(0.5));
        let mut b = BTreeMap::new();
        b.insert("compression.threshold".to_string(), Value::from(0.5));
        b.insert("model.context_length".to_string(), Value::from(200000));

        let first = AgentConfigSignatureInput {
            model: "m".to_string(),
            cache_keys: a,
            ..Default::default()
        };
        let second = AgentConfigSignatureInput {
            model: "m".to_string(),
            cache_keys: b,
            ..Default::default()
        };
        assert_eq!(
            agent_config_signature(&first),
            agent_config_signature(&second)
        );

        let changed = AgentConfigSignatureInput {
            model: "m".to_string(),
            cache_keys: BTreeMap::from([("compression.threshold".to_string(), Value::from(0.75))]),
            ..Default::default()
        };
        assert_ne!(
            agent_config_signature(&first),
            agent_config_signature(&changed)
        );
    }

    #[test]
    fn cache_hit_miss_evict_lru_and_idle_ttl() {
        let released = Arc::new(Mutex::new(Vec::new()));
        let released_for_hook = released.clone();
        let mut cache = GatewayAgentCache::new(2, Duration::from_secs(10)).with_release_hook(
            Arc::new(move |agent: &String| released_for_hook.lock().unwrap().push(agent.clone())),
        );
        let now = Instant::now();
        cache.insert_at("s1", "agent1".to_string(), "sig1", now);
        assert_eq!(
            cache
                .get_if_signature_at("s1", "sig1", now)
                .map(|s| s.as_str()),
            Some("agent1")
        );
        assert!(cache.get_if_signature_at("s1", "sig2", now).is_none());

        cache.insert_at("s2", "agent2".to_string(), "sig2", now);
        cache.get_if_signature_at("s1", "sig1", now);
        cache.insert_at("s3", "agent3".to_string(), "sig3", now);
        assert!(cache.get_if_signature_at("s1", "sig1", now).is_some());
        assert!(cache.get_if_signature_at("s2", "sig2", now).is_none());
        assert_eq!(released.lock().unwrap().as_slice(), ["agent2"]);

        assert_eq!(cache.sweep_idle_at(now + Duration::from_secs(11)), 2);
        assert!(cache.is_empty());
    }

    #[test]
    fn active_aware_cap_skips_mid_turn_lru_and_can_remain_over_limit() {
        let released = Arc::new(Mutex::new(Vec::new()));
        let released_for_hook = released.clone();
        let mut cache = GatewayAgentCache::new(2, Duration::from_secs(10)).with_release_hook(
            Arc::new(move |agent: &String| released_for_hook.lock().unwrap().push(agent.clone())),
        );
        let now = Instant::now();
        cache.insert_at("active", "agent-active".to_string(), "sig", now);
        cache.insert_at("idle-a", "agent-idle-a".to_string(), "sig", now);
        cache.insert_at_with_active(
            "idle-b",
            "agent-idle-b".to_string(),
            "sig",
            now,
            |key, _| key == "active",
        );

        assert_eq!(cache.len(), 3);
        assert!(cache.get_if_signature_at("active", "sig", now).is_some());
        assert!(cache.get_if_signature_at("idle-a", "sig", now).is_some());
        assert!(cache.get_if_signature_at("idle-b", "sig", now).is_some());
        assert!(released.lock().unwrap().is_empty());
    }

    #[test]
    fn active_aware_cap_evicts_inactive_entries_in_excess_window_only() {
        let released = Arc::new(Mutex::new(Vec::new()));
        let released_for_hook = released.clone();
        let mut cache = GatewayAgentCache::new(2, Duration::from_secs(10)).with_release_hook(
            Arc::new(move |agent: &String| released_for_hook.lock().unwrap().push(agent.clone())),
        );
        let now = Instant::now();
        let active_keys = HashSet::from(["s1".to_string()]);
        cache.insert_at("s1", "agent1".to_string(), "sig", now);
        cache.insert_at("s2", "agent2".to_string(), "sig", now);
        cache.insert_at_with_active("s3", "agent3".to_string(), "sig", now, |key, _| {
            active_keys.contains(key)
        });
        cache.insert_at_with_active("s4", "agent4".to_string(), "sig", now, |key, _| {
            active_keys.contains(key)
        });

        assert!(cache.get_if_signature_at("s1", "sig", now).is_some());
        assert!(cache.get_if_signature_at("s2", "sig", now).is_none());
        assert!(cache.get_if_signature_at("s3", "sig", now).is_some());
        assert!(cache.get_if_signature_at("s4", "sig", now).is_some());
        assert_eq!(released.lock().unwrap().as_slice(), ["agent2"]);
    }

    #[test]
    fn idle_sweep_skips_active_agent_and_evicted_session_can_reinsert() {
        let released = Arc::new(Mutex::new(Vec::new()));
        let released_for_hook = released.clone();
        let mut cache = GatewayAgentCache::new(3, Duration::from_secs(10)).with_release_hook(
            Arc::new(move |agent: &String| released_for_hook.lock().unwrap().push(agent.clone())),
        );
        let now = Instant::now();
        cache.insert_at("active", "old-active".to_string(), "sig", now);
        cache.insert_at("idle", "old-idle".to_string(), "sig", now);

        let sweep_at = now + Duration::from_secs(11);
        assert_eq!(
            cache.sweep_idle_at_with_active(sweep_at, |key, _| key == "active"),
            1
        );
        assert!(cache
            .get_if_signature_at("active", "sig", sweep_at)
            .is_some());
        assert!(cache.get_if_signature_at("idle", "sig", sweep_at).is_none());
        assert_eq!(released.lock().unwrap().as_slice(), ["old-idle"]);

        cache.insert_at("idle", "new-idle".to_string(), "sig-new", sweep_at);
        assert_eq!(
            cache
                .get_if_signature_at("idle", "sig-new", sweep_at)
                .map(|agent| agent.as_str()),
            Some("new-idle")
        );
    }
}
