//! Memory provider plugin for local user interest prefetch.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use hermes_config::InterestConfig;
use serde_json::Value;

use crate::memory_manager::MemoryProviderPlugin;

use super::store::InterestStore;

/// Prefetch hook for [`MemoryManager`] (persistence is via [`AgentLoop`] interest store).
pub struct InterestMemoryPlugin {
    store: Arc<Mutex<InterestStore>>,
    config: InterestConfig,
    db_path: PathBuf,
}

impl InterestMemoryPlugin {
    pub fn new(store: Arc<Mutex<InterestStore>>, config: InterestConfig, db_path: PathBuf) -> Self {
        Self {
            store,
            config,
            db_path,
        }
    }

    pub fn open(hermes_home: &str, config: InterestConfig) -> Option<Arc<Self>> {
        if !config.enabled {
            return None;
        }
        let db_path = PathBuf::from(hermes_home).join("interest.db");
        let store = InterestStore::open(&db_path, config.clone()).ok()?;
        Some(Arc::new(Self::new(
            Arc::new(Mutex::new(store)),
            config,
            db_path,
        )))
    }

    pub fn from_store(
        store: Arc<Mutex<InterestStore>>,
        config: InterestConfig,
        hermes_home: &str,
    ) -> Arc<Self> {
        Arc::new(Self::new(
            store,
            config,
            PathBuf::from(hermes_home).join("interest.db"),
        ))
    }

    fn with_store<F, T>(&self, f: F) -> Option<T>
    where
        F: FnOnce(&InterestStore) -> T,
    {
        let guard = self.store.lock().ok()?;
        Some(f(&guard))
    }
}

impl MemoryProviderPlugin for InterestMemoryPlugin {
    fn name(&self) -> &str {
        "interest"
    }

    fn system_prompt_block(&self) -> String {
        String::new()
    }

    fn prefetch(&self, query: &str, _session_id: &str) -> String {
        self.with_store(|store| store.render_prefetch_block(query).unwrap_or_default())
            .unwrap_or_default()
    }

    fn sync_turn(&self, _user_content: &str, _assistant_content: &str, _session_id: &str) {
        // Per-turn POI ingest: `AgentLoop::interest_sync_user_messages`.
    }

    fn on_session_end(&self, _messages: &[Value]) {
        // Session-end POI: `AgentLoop::interest_on_session_end`.
    }

    fn is_available(&self) -> bool {
        self.config.enabled && self.db_path.parent().is_some()
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(serde_json::json!([
            {"key": "enabled", "description": "Enable local interest store", "default": true},
            {"key": "extract_mode", "description": "llm (default) | hybrid | rules", "default": "llm"},
            {"key": "per_turn_buffer", "description": "Buffer high-confidence signals per turn", "default": true},
            {"key": "per_turn_persist", "description": "Persist POI every user message (legacy)", "default": false},
            {"key": "max_topics", "description": "Max retained topics", "default": 40},
            {"key": "snapshot_top_k", "description": "Topics in frozen system prompt", "default": 5},
            {"key": "prefetch_top_k", "description": "Topics per-turn prefetch", "default": 3}
        ]))
    }
}
