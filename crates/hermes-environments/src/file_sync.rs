//! File synchronization between local and remote environments.
//!
//! Provides bidirectional file sync between the local filesystem and a
//! remote `TerminalBackend`, with optional watch-and-sync capability.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::{AgentError, TerminalBackend};

/// Bidirectional file synchronization between local and remote environments.
pub struct FileSync {
    local_root: PathBuf,
    remote_backend: Arc<dyn TerminalBackend>,
}

impl FileSync {
    /// Create a new FileSync instance.
    ///
    /// - `local_root`: Root directory on the local filesystem.
    /// - `remote_backend`: A `TerminalBackend` used to read/write files remotely.
    pub fn new(local_root: PathBuf, remote_backend: Arc<dyn TerminalBackend>) -> Self {
        Self {
            local_root,
            remote_backend,
        }
    }

    /// Get the local root path.
    pub fn local_root(&self) -> &Path {
        &self.local_root
    }

    /// Sync local files to the remote environment.
    ///
    /// For each path (relative to `local_root`), reads the local file and
    /// writes it to the same relative path on the remote backend.
    pub async fn sync_to_remote(&self, paths: &[PathBuf]) -> Result<(), AgentError> {
        for path in paths {
            let local_path = self.local_root.join(path);
            let content = tokio::fs::read_to_string(&local_path).await.map_err(|e| {
                AgentError::Io(format!(
                    "Failed to read local file '{}': {}",
                    local_path.display(),
                    e
                ))
            })?;

            let remote_path = path.to_string_lossy();
            self.remote_backend
                .write_file(&remote_path, &content)
                .await?;

            tracing::debug!("Synced to remote: {}", remote_path);
        }
        Ok(())
    }

    /// Sync remote files to the local environment.
    ///
    /// For each path (relative to `local_root`), reads from the remote
    /// backend and writes to the local filesystem.
    pub async fn sync_from_remote(&self, paths: &[PathBuf]) -> Result<(), AgentError> {
        for path in paths {
            let remote_path = path.to_string_lossy();
            let content = self
                .remote_backend
                .read_file(&remote_path, None, None)
                .await?;

            let local_path = self.local_root.join(path);
            if let Some(parent) = local_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    AgentError::Io(format!(
                        "Failed to create directory '{}': {}",
                        parent.display(),
                        e
                    ))
                })?;
            }

            tokio::fs::write(&local_path, &content).await.map_err(|e| {
                AgentError::Io(format!(
                    "Failed to write local file '{}': {}",
                    local_path.display(),
                    e
                ))
            })?;

            tracing::debug!("Synced from remote: {}", remote_path);
        }
        Ok(())
    }

    /// Watch local files for changes and sync them to the remote environment.
    ///
    /// This runs an infinite polling loop that checks for modifications.
    /// Call from a spawned task; cancel via the returned `JoinHandle`.
    pub async fn watch_and_sync(&self) -> Result<(), AgentError> {
        use std::collections::HashMap;

        let mut last_modified: HashMap<PathBuf, std::time::SystemTime> = HashMap::new();

        loop {
            let entries = Self::walk_dir(&self.local_root).await?;

            let mut changed = Vec::new();
            for entry in &entries {
                let meta = tokio::fs::metadata(entry).await.map_err(|e| {
                    AgentError::Io(format!("Failed to stat '{}': {}", entry.display(), e))
                })?;
                let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                let relative = entry
                    .strip_prefix(&self.local_root)
                    .unwrap_or(entry)
                    .to_path_buf();

                if last_modified.get(&relative) != Some(&modified) {
                    last_modified.insert(relative.clone(), modified);
                    changed.push(relative);
                }
            }

            if !changed.is_empty() {
                tracing::info!("watch_and_sync: {} files changed, syncing...", changed.len());
                self.sync_to_remote(&changed).await?;
            }

            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    async fn walk_dir(dir: &Path) -> Result<Vec<PathBuf>, AgentError> {
        let mut result = Vec::new();
        let mut stack = vec![dir.to_path_buf()];

        while let Some(current) = stack.pop() {
            let mut entries = tokio::fs::read_dir(&current).await.map_err(|e| {
                AgentError::Io(format!(
                    "Failed to read directory '{}': {}",
                    current.display(),
                    e
                ))
            })?;

            while let Some(entry) = entries.next_entry().await.map_err(|e| {
                AgentError::Io(format!("Failed to read dir entry: {}", e))
            })? {
                let path = entry.path();
                if path.is_dir() {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy();
                    if !name.starts_with('.') && name != "target" && name != "node_modules" {
                        stack.push(path);
                    }
                } else {
                    result.push(path);
                }
            }
        }

        Ok(result)
    }
}
