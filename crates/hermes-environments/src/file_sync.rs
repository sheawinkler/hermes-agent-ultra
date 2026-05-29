//! File synchronization between local and remote environments.
//!
//! Provides bidirectional file sync between the local filesystem and a
//! remote `TerminalBackend`, with optional watch-and-sync capability.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use hermes_core::{AgentError, TerminalBackend};
use tokio::io::AsyncWriteExt;

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

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

            atomic_write_text(&local_path, &content).await?;

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
                tracing::info!(
                    "watch_and_sync: {} files changed, syncing...",
                    changed.len()
                );
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

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to read dir entry: {}", e)))?
            {
                let path = entry.path();
                if path.is_dir() {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
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

fn atomic_temp_path(path: &Path) -> Result<PathBuf, AgentError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| AgentError::Io(format!("Missing filename for '{}'", path.display())))?
        .to_string_lossy();
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Io(format!("Missing parent for '{}'", path.display())))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let seq = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(parent.join(format!(
        ".{}.hermes-sync-{}-{}-{}.tmp",
        file_name,
        std::process::id(),
        nanos,
        seq
    )))
}

async fn atomic_write_text(path: &Path, content: &str) -> Result<(), AgentError> {
    let tmp_path = atomic_temp_path(path)?;
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await
            .map_err(|e| {
                AgentError::Io(format!(
                    "Failed to create temporary sync file '{}': {}",
                    tmp_path.display(),
                    e
                ))
            })?;

        file.write_all(content.as_bytes()).await.map_err(|e| {
            AgentError::Io(format!(
                "Failed to write temporary sync file '{}': {}",
                tmp_path.display(),
                e
            ))
        })?;
        file.sync_all().await.map_err(|e| {
            AgentError::Io(format!(
                "Failed to sync temporary sync file '{}': {}",
                tmp_path.display(),
                e
            ))
        })?;
        drop(file);

        tokio::fs::rename(&tmp_path, path).await.map_err(|e| {
            AgentError::Io(format!(
                "Failed to replace local file '{}' from '{}': {}",
                path.display(),
                tmp_path.display(),
                e
            ))
        })?;
        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use hermes_core::CommandOutput;
    use std::collections::HashMap;

    struct MockBackend {
        files: HashMap<String, String>,
    }

    #[async_trait]
    impl TerminalBackend for MockBackend {
        async fn execute_command(
            &self,
            _command: &str,
            _timeout: Option<u64>,
            _workdir: Option<&str>,
            _background: bool,
            _pty: bool,
        ) -> Result<CommandOutput, AgentError> {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }

        async fn read_file(
            &self,
            path: &str,
            _offset: Option<u64>,
            _limit: Option<u64>,
        ) -> Result<String, AgentError> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| AgentError::Io(format!("missing mock file: {path}")))
        }

        async fn write_file(&self, _path: &str, _content: &str) -> Result<(), AgentError> {
            Ok(())
        }

        async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
            Ok(self.files.contains_key(path))
        }
    }

    #[tokio::test]
    async fn atomic_write_text_replaces_existing_file_and_cleans_temp() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("nested").join("example.txt");
        tokio::fs::create_dir_all(target.parent().unwrap())
            .await
            .expect("mkdir");
        tokio::fs::write(&target, "old").await.expect("seed");

        atomic_write_text(&target, "new contents")
            .await
            .expect("atomic write");

        let actual = tokio::fs::read_to_string(&target).await.expect("read");
        assert_eq!(actual, "new contents");

        let mut entries = tokio::fs::read_dir(target.parent().unwrap())
            .await
            .expect("read dir");
        while let Some(entry) = entries.next_entry().await.expect("entry") {
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.contains(".hermes-sync-"),
                "temporary sync file was not cleaned up: {name}"
            );
        }
    }

    #[tokio::test]
    async fn sync_from_remote_uses_atomic_local_write() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut files = HashMap::new();
        files.insert("subdir/file.txt".to_string(), "remote contents".to_string());
        let sync = FileSync::new(dir.path().to_path_buf(), Arc::new(MockBackend { files }));

        sync.sync_from_remote(&[PathBuf::from("subdir/file.txt")])
            .await
            .expect("sync");

        let actual = tokio::fs::read_to_string(dir.path().join("subdir/file.txt"))
            .await
            .expect("read synced file");
        assert_eq!(actual, "remote contents");
    }
}
