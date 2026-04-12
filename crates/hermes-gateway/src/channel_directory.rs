//! In-memory channel directory for discovery and lookup.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct ChannelEntry {
    pub id: String,
    pub name: String,
    pub platform: String,
}

#[derive(Clone, Default)]
pub struct ChannelDirectory {
    channels: Arc<RwLock<HashMap<String, ChannelEntry>>>,
}

impl ChannelDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert(&self, entry: ChannelEntry) {
        if let Ok(mut channels) = self.channels.write() {
            channels.insert(entry.id.clone(), entry);
        }
    }

    pub fn get(&self, id: &str) -> Option<ChannelEntry> {
        self.channels.read().ok().and_then(|c| c.get(id).cloned())
    }

    pub fn list(&self) -> Vec<ChannelEntry> {
        self.channels
            .read()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default()
    }
}
