//! Lightweight sticker metadata cache.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct StickerMeta {
    pub id: String,
    pub name: String,
    pub mime_type: Option<String>,
}

#[derive(Clone, Default)]
pub struct StickerCache {
    entries: Arc<RwLock<HashMap<String, StickerMeta>>>,
}

impl StickerCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, meta: StickerMeta) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(meta.id.clone(), meta);
        }
    }

    pub fn get(&self, id: &str) -> Option<StickerMeta> {
        self.entries.read().ok().and_then(|m| m.get(id).cloned())
    }
}
