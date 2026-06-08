//! Persistent channel directory for discovery, lookup, and alias resolution.
//!
//! Provides:
//! - Cross-platform channel parsing (`"telegram:12345"`, `"discord:67890"`)
//! - Channel alias / nickname system
//! - JSON persistence to `~/.hermes/channel_directory.json`
//! - Atomic writes (write temp file + rename) for crash safety
//! - Startup reload from disk

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// ChannelEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelEntry {
    /// Canonical id: `"platform:chat_id"` (e.g. `"telegram:12345"`).
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Platform identifier (e.g. `"telegram"`, `"discord"`).
    pub platform: String,
    /// Raw chat/channel/user id on the platform.
    #[serde(default)]
    pub chat_id: String,
    /// Optional aliases for quick lookup (e.g. `["home", "family-chat"]`).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Optional metadata (avatar URL, member count, etc.).
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

impl ChannelEntry {
    /// Create a new entry from platform + chat_id.
    pub fn new(
        platform: impl Into<String>,
        chat_id: impl Into<String>,
        name: impl Into<String>,
    ) -> Self {
        let platform = platform.into();
        let chat_id = chat_id.into();
        let id = format!("{}:{}", platform, chat_id);
        Self {
            id,
            name: name.into(),
            platform,
            chat_id,
            aliases: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add an alias.
    pub fn with_alias(mut self, alias: impl Into<String>) -> Self {
        self.aliases.push(alias.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Persistence format
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DirectorySnapshot {
    version: u32,
    channels: Vec<ChannelEntry>,
}

// ---------------------------------------------------------------------------
// ChannelDirectory
// ---------------------------------------------------------------------------

/// Persistent channel directory with alias resolution and cross-platform parsing.
#[derive(Clone)]
pub struct ChannelDirectory {
    channels: Arc<RwLock<HashMap<String, ChannelEntry>>>,
    /// Alias → canonical id mapping.
    aliases: Arc<RwLock<HashMap<String, String>>>,
    /// Path to the persistence file. `None` = in-memory only.
    persist_path: Option<PathBuf>,
}

impl Default for ChannelDirectory {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelDirectory {
    /// Create an in-memory-only directory.
    pub fn new() -> Self {
        Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            persist_path: None,
        }
    }

    /// Create a directory backed by a JSON file. Loads existing data on creation.
    pub fn with_persistence(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut dir = Self {
            channels: Arc::new(RwLock::new(HashMap::new())),
            aliases: Arc::new(RwLock::new(HashMap::new())),
            persist_path: Some(path.clone()),
        };
        if path.exists() {
            if let Err(e) = dir.load_from_disk() {
                tracing::warn!(path = %path.display(), error = %e, "Failed to load channel directory");
            }
        }
        dir
    }

    /// Load the default path: `~/.hermes/channel_directory.json`.
    pub fn with_default_persistence() -> Self {
        let path = default_persist_path();
        Self::with_persistence(path)
    }

    // -- CRUD ----------------------------------------------------------------

    /// Insert or update a channel entry. Persists to disk if configured.
    pub fn upsert(&self, entry: ChannelEntry) {
        // Update alias index
        if let Ok(mut aliases) = self.aliases.write() {
            for alias in &entry.aliases {
                aliases.insert(alias.to_lowercase(), entry.id.clone());
            }
        }
        // Update main index
        if let Ok(mut channels) = self.channels.write() {
            channels.insert(entry.id.clone(), entry);
        }
        self.persist_if_configured();
    }

    /// Remove a channel by id.
    pub fn remove(&self, id: &str) -> Option<ChannelEntry> {
        let removed = {
            let mut channels = self.channels.write().ok()?;
            channels.remove(id)
        };
        if let Some(ref entry) = removed {
            if let Ok(mut aliases) = self.aliases.write() {
                for alias in &entry.aliases {
                    aliases.remove(&alias.to_lowercase());
                }
            }
            self.persist_if_configured();
        }
        removed
    }

    /// Get a channel by canonical id.
    pub fn get(&self, id: &str) -> Option<ChannelEntry> {
        self.channels.read().ok().and_then(|c| c.get(id).cloned())
    }

    /// List all channels.
    pub fn list(&self) -> Vec<ChannelEntry> {
        self.channels
            .read()
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default()
    }

    /// List channels filtered by platform.
    pub fn list_by_platform(&self, platform: &str) -> Vec<ChannelEntry> {
        self.channels
            .read()
            .map(|c| {
                c.values()
                    .filter(|e| e.platform == platform)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Number of channels.
    pub fn len(&self) -> usize {
        self.channels.read().map(|c| c.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    // -- Alias management ----------------------------------------------------

    /// Add an alias for a channel.
    pub fn add_alias(&self, channel_id: &str, alias: &str) -> bool {
        let alias_lower = alias.to_lowercase();
        // Verify channel exists
        let exists = self
            .channels
            .read()
            .map(|c| c.contains_key(channel_id))
            .unwrap_or(false);
        if !exists {
            return false;
        }
        // Add to alias index
        if let Ok(mut aliases) = self.aliases.write() {
            aliases.insert(alias_lower.clone(), channel_id.to_string());
        }
        // Add to channel entry
        if let Ok(mut channels) = self.channels.write() {
            if let Some(entry) = channels.get_mut(channel_id) {
                if !entry
                    .aliases
                    .iter()
                    .any(|a| a.to_lowercase() == alias_lower)
                {
                    entry.aliases.push(alias.to_string());
                }
            }
        }
        self.persist_if_configured();
        true
    }

    /// Remove an alias.
    pub fn remove_alias(&self, alias: &str) -> bool {
        let alias_lower = alias.to_lowercase();
        let channel_id = {
            let mut aliases = match self.aliases.write() {
                Ok(a) => a,
                Err(_) => return false,
            };
            aliases.remove(&alias_lower)
        };
        if let Some(ref cid) = channel_id {
            if let Ok(mut channels) = self.channels.write() {
                if let Some(entry) = channels.get_mut(cid) {
                    entry.aliases.retain(|a| a.to_lowercase() != alias_lower);
                }
            }
            self.persist_if_configured();
            true
        } else {
            false
        }
    }

    // -- Resolution ----------------------------------------------------------

    /// Resolve a channel reference. Tries in order:
    /// 1. Exact canonical id (`"telegram:12345"`)
    /// 2. Alias lookup
    /// 3. Cross-platform parse (`"platform:id"` format, auto-register)
    pub fn resolve(&self, reference: &str) -> Option<ChannelEntry> {
        let trimmed = reference.trim();

        // 1. Exact id match
        if let Some(entry) = self.get(trimmed) {
            return Some(entry);
        }

        // 2. Alias lookup
        let alias_id = self
            .aliases
            .read()
            .ok()
            .and_then(|a| a.get(&trimmed.to_lowercase()).cloned());
        if let Some(id) = alias_id {
            return self.get(&id);
        }

        // 3. Parse "platform:chat_id" format
        if let Some((platform, chat_id)) = trimmed.split_once(':') {
            let platform = platform.trim().to_lowercase();
            let chat_id = chat_id.trim();
            if !chat_id.is_empty() {
                let canonical = format!("{}:{}", platform, chat_id);
                // Check if it exists now
                if let Some(entry) = self.get(&canonical) {
                    return Some(entry);
                }
                // Auto-register
                let entry = ChannelEntry::new(&platform, chat_id, chat_id);
                self.upsert(entry.clone());
                return Some(entry);
            }
        }

        None
    }

    // -- Persistence ---------------------------------------------------------

    fn persist_if_configured(&self) {
        if let Some(ref path) = self.persist_path {
            if let Err(e) = self.save_to_disk(path) {
                tracing::warn!(path = %path.display(), error = %e, "Failed to persist channel directory");
            }
        }
    }

    /// Save to disk using atomic write (temp file + rename).
    fn save_to_disk(&self, path: &Path) -> Result<(), std::io::Error> {
        let channels = self
            .channels
            .read()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let snapshot = DirectorySnapshot {
            version: 1,
            channels: channels.values().cloned().collect(),
        };

        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Atomic write: write to temp file, then rename
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json.as_bytes())?;
        std::fs::rename(&tmp_path, path)?;

        tracing::debug!(path = %path.display(), count = channels.len(), "Channel directory persisted");
        Ok(())
    }

    /// Load from disk.
    fn load_from_disk(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let path = self
            .persist_path
            .as_ref()
            .ok_or("No persist path configured")?;

        let content = std::fs::read_to_string(path)?;
        let snapshot: DirectorySnapshot = serde_json::from_str(&content)?;

        let mut channels = self.channels.write().map_err(|e| e.to_string())?;
        let mut aliases = self.aliases.write().map_err(|e| e.to_string())?;

        channels.clear();
        aliases.clear();

        for entry in snapshot.channels {
            for alias in &entry.aliases {
                aliases.insert(alias.to_lowercase(), entry.id.clone());
            }
            channels.insert(entry.id.clone(), entry);
        }

        tracing::info!(
            path = %path.display(),
            count = channels.len(),
            "Channel directory loaded from disk"
        );
        Ok(())
    }
}

fn default_persist_path() -> PathBuf {
    hermes_config::hermes_home().join("channel_directory.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_upsert_and_get() {
        let dir = ChannelDirectory::new();
        let entry = ChannelEntry::new("telegram", "12345", "My Chat");
        dir.upsert(entry);

        let found = dir.get("telegram:12345").unwrap();
        assert_eq!(found.name, "My Chat");
        assert_eq!(found.platform, "telegram");
        assert_eq!(found.chat_id, "12345");
    }

    #[test]
    fn list_and_list_by_platform() {
        let dir = ChannelDirectory::new();
        dir.upsert(ChannelEntry::new("telegram", "1", "TG Chat"));
        dir.upsert(ChannelEntry::new("discord", "2", "DC Chat"));
        dir.upsert(ChannelEntry::new("telegram", "3", "TG Chat 2"));

        assert_eq!(dir.list().len(), 3);
        assert_eq!(dir.list_by_platform("telegram").len(), 2);
        assert_eq!(dir.list_by_platform("discord").len(), 1);
        assert_eq!(dir.list_by_platform("slack").len(), 0);
    }

    #[test]
    fn remove_channel() {
        let dir = ChannelDirectory::new();
        dir.upsert(ChannelEntry::new("telegram", "1", "Chat").with_alias("home"));
        assert_eq!(dir.len(), 1);

        let removed = dir.remove("telegram:1").unwrap();
        assert_eq!(removed.name, "Chat");
        assert_eq!(dir.len(), 0);
        // Alias should also be gone
        assert!(dir.resolve("home").is_none());
    }

    #[test]
    fn alias_resolution() {
        let dir = ChannelDirectory::new();
        dir.upsert(ChannelEntry::new("telegram", "12345", "Family").with_alias("family"));

        // Resolve by alias
        let found = dir.resolve("family").unwrap();
        assert_eq!(found.chat_id, "12345");

        // Case-insensitive
        let found = dir.resolve("FAMILY").unwrap();
        assert_eq!(found.chat_id, "12345");
    }

    #[test]
    fn add_and_remove_alias() {
        let dir = ChannelDirectory::new();
        dir.upsert(ChannelEntry::new("discord", "abc", "Server"));

        assert!(dir.add_alias("discord:abc", "gaming"));
        let found = dir.resolve("gaming").unwrap();
        assert_eq!(found.chat_id, "abc");

        assert!(dir.remove_alias("gaming"));
        assert!(dir.resolve("gaming").is_none());
    }

    #[test]
    fn add_alias_nonexistent_channel() {
        let dir = ChannelDirectory::new();
        assert!(!dir.add_alias("nope:123", "alias"));
    }

    #[test]
    fn resolve_platform_colon_id() {
        let dir = ChannelDirectory::new();
        // Not pre-registered — should auto-register
        let found = dir.resolve("slack:C12345").unwrap();
        assert_eq!(found.platform, "slack");
        assert_eq!(found.chat_id, "C12345");

        // Now it should be in the directory
        assert_eq!(dir.len(), 1);

        // Second resolve should find the existing entry
        let found2 = dir.resolve("slack:C12345").unwrap();
        assert_eq!(found2.id, found.id);
    }

    #[test]
    fn resolve_exact_id() {
        let dir = ChannelDirectory::new();
        dir.upsert(ChannelEntry::new("telegram", "999", "Direct"));

        let found = dir.resolve("telegram:999").unwrap();
        assert_eq!(found.name, "Direct");
    }

    #[test]
    fn resolve_unknown_returns_none() {
        let dir = ChannelDirectory::new();
        assert!(dir.resolve("unknown_alias").is_none());
    }

    // -- Persistence tests ---------------------------------------------------

    #[test]
    fn persist_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("channels.json");

        // Create and populate
        {
            let dir = ChannelDirectory::with_persistence(&path);
            dir.upsert(ChannelEntry::new("telegram", "1", "Chat A").with_alias("home"));
            dir.upsert(ChannelEntry::new("discord", "2", "Chat B"));
        }

        // File should exist
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Chat A"));
        assert!(content.contains("Chat B"));

        // Reload into a new directory
        let dir2 = ChannelDirectory::with_persistence(&path);
        assert_eq!(dir2.len(), 2);
        let found = dir2.get("telegram:1").unwrap();
        assert_eq!(found.name, "Chat A");
        assert!(found.aliases.contains(&"home".to_string()));

        // Alias should work after reload
        let found = dir2.resolve("home").unwrap();
        assert_eq!(found.chat_id, "1");
    }

    #[test]
    fn persist_atomic_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("channels.json");

        let dir = ChannelDirectory::with_persistence(&path);
        dir.upsert(ChannelEntry::new("telegram", "1", "Chat"));

        // Temp file should not exist after successful write
        let tmp_path = path.with_extension("json.tmp");
        assert!(!tmp_path.exists());
        assert!(path.exists());
    }

    #[test]
    fn persist_survives_kill() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("channels.json");

        // Write some data
        let dir = ChannelDirectory::with_persistence(&path);
        dir.upsert(ChannelEntry::new("telegram", "1", "Chat"));
        dir.upsert(ChannelEntry::new("discord", "2", "Server"));

        // Simulate "kill -9" by just dropping and reloading
        drop(dir);

        let dir2 = ChannelDirectory::with_persistence(&path);
        assert_eq!(dir2.len(), 2);
    }

    #[test]
    fn empty_directory_no_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nonexistent.json");

        let dir = ChannelDirectory::with_persistence(&path);
        assert_eq!(dir.len(), 0);
        assert!(dir.is_empty());
    }
}
