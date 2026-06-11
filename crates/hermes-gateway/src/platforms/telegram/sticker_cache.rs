//! Persistent sticker metadata cache.
//!
//! Caches sticker metadata (id, name, mime type, file path) to avoid
//! re-downloading stickers on every message. Persists to JSON for
//! cross-restart survival.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StickerMeta {
    pub id: String,
    pub name: String,
    pub mime_type: Option<String>,
    /// Local file path if the sticker has been downloaded.
    #[serde(default)]
    pub file_path: Option<String>,
    /// Platform this sticker belongs to.
    #[serde(default)]
    pub platform: Option<String>,
}

#[derive(Clone)]
pub struct StickerCache {
    entries: Arc<RwLock<HashMap<String, StickerMeta>>>,
    persist_path: Option<PathBuf>,
}

impl Default for StickerCache {
    fn default() -> Self {
        Self::new()
    }
}

impl StickerCache {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(RwLock::new(HashMap::new())),
            persist_path: None,
        }
    }

    /// Create with JSON persistence.
    pub fn with_persistence(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<HashMap<String, StickerMeta>>(&s).ok())
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        Self {
            entries: Arc::new(RwLock::new(entries)),
            persist_path: Some(path),
        }
    }

    pub fn put(&self, meta: StickerMeta) {
        if let Ok(mut entries) = self.entries.write() {
            entries.insert(meta.id.clone(), meta);
        }
        self.persist();
    }

    pub fn get(&self, id: &str) -> Option<StickerMeta> {
        self.entries.read().ok().and_then(|m| m.get(id).cloned())
    }

    pub fn remove(&self, id: &str) -> Option<StickerMeta> {
        let removed = self.entries.write().ok().and_then(|mut m| m.remove(id));
        if removed.is_some() {
            self.persist();
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.entries.read().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn persist(&self) {
        if let Some(ref path) = self.persist_path {
            if let Ok(entries) = self.entries.read() {
                if let Ok(json) = serde_json::to_string_pretty(&*entries) {
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let tmp = path.with_extension("json.tmp");
                    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
                        let _ = std::fs::rename(&tmp, path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_put_get() {
        let cache = StickerCache::new();
        cache.put(StickerMeta {
            id: "s1".into(),
            name: "smile".into(),
            mime_type: Some("image/webp".into()),
            file_path: None,
            platform: Some("telegram".into()),
        });
        let found = cache.get("s1").unwrap();
        assert_eq!(found.name, "smile");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn remove_entry() {
        let cache = StickerCache::new();
        cache.put(StickerMeta {
            id: "s1".into(),
            name: "x".into(),
            mime_type: None,
            file_path: None,
            platform: None,
        });
        assert!(cache.remove("s1").is_some());
        assert!(cache.is_empty());
    }

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("stickers.json");

        {
            let cache = StickerCache::with_persistence(&path);
            cache.put(StickerMeta {
                id: "s1".into(),
                name: "wave".into(),
                mime_type: Some("image/png".into()),
                file_path: None,
                platform: None,
            });
        }

        let cache2 = StickerCache::with_persistence(&path);
        assert_eq!(cache2.len(), 1);
        assert_eq!(cache2.get("s1").unwrap().name, "wave");
    }
}
