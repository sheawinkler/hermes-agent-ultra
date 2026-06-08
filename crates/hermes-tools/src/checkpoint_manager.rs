//! Filesystem checkpoint manager — parity with Python `tools/checkpoint_manager.py`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Max files under a working directory before skipping snapshot (Python `_MAX_FILES`).
const MAX_FILES: usize = 50_000;

static PROJECT_ROOT_MARKERS: &[&str] = &[
    ".git",
    "pyproject.toml",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "Makefile",
    "pom.xml",
    ".hg",
    "Gemfile",
];

/// Shadow project id: first 16 hex chars of SHA-256(abs path).
pub fn checkpoint_shadow_dir_id(abs_path_str: &str) -> String {
    let digest = Sha256::digest(abs_path_str.as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Transparent filesystem snapshots before mutating tools.
#[derive(Debug)]
pub struct CheckpointManager {
    enabled: bool,
    store_root: PathBuf,
    default_workdir: PathBuf,
    checkpointed_dirs: HashSet<String>,
    git_available: Option<bool>,
}

impl CheckpointManager {
    pub fn new(
        enabled: bool,
        hermes_home: Option<&Path>,
        workdir: impl AsRef<Path>,
    ) -> Self {
        let home = hermes_home
            .map(Path::to_path_buf)
            .or_else(|| Some(hermes_config::paths::hermes_home()));
        let store_root = home
            .map(|h| h.join("checkpoints").join("store"))
            .unwrap_or_else(|| hermes_config::hermes_home().join("checkpoints").join("store"));
        let default_workdir = normalize_path(workdir.as_ref());
        Self {
            enabled,
            store_root,
            default_workdir,
            checkpointed_dirs: HashSet::new(),
            git_available: None,
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Reset per-turn dedup. Call at the start of each agent iteration (Python `new_turn`).
    pub fn new_turn(&mut self) {
        self.checkpointed_dirs.clear();
    }

    /// Resolve a file path to its project working directory for checkpointing.
    pub fn get_working_dir_for_path(&self, file_path: &Path) -> PathBuf {
        let path = normalize_path(file_path);
        let candidate = if path.is_dir() {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.default_workdir.clone())
        };

        let mut check = candidate.clone();
        loop {
            if PROJECT_ROOT_MARKERS
                .iter()
                .any(|marker| check.join(marker).exists())
            {
                return check;
            }
            if !check.pop() {
                break;
            }
        }
        candidate
    }

    /// Ensure a checkpoint exists for `path` before mutation.
    ///
    /// At most one snapshot per working directory per turn. Never raises — errors are logged
    /// at debug and ignored (Python parity).
    pub fn ensure_checkpoint(&mut self, path: &Path, label: &str) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }

        if self.git_available.is_none() {
            let available = Command::new("git")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            self.git_available = Some(available);
            if !available {
                tracing::debug!("Checkpoints disabled: git not found");
            }
        }
        if !self.git_available.unwrap_or(false) {
            return Ok(());
        }

        let working_dir = self.get_working_dir_for_path(path);
        let abs_dir = normalize_path(&working_dir);
        let abs_key = abs_dir.to_string_lossy().into_owned();

        if is_broad_checkpoint_dir(&abs_dir) {
            tracing::debug!(
                directory = %abs_dir.display(),
                "Checkpoint skipped: directory too broad"
            );
            return Ok(());
        }

        if self.checkpointed_dirs.contains(&abs_key) {
            return Ok(());
        }

        if dir_file_count(&abs_dir) > MAX_FILES {
            tracing::debug!(
                directory = %abs_dir.display(),
                max_files = MAX_FILES,
                "Checkpoint skipped: too many files"
            );
            return Ok(());
        }

        self.checkpointed_dirs.insert(abs_key);

        match self.snapshot_worktree(&abs_dir, label) {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::debug!(
                    directory = %abs_dir.display(),
                    error = %err,
                    "Checkpoint failed (non-fatal)"
                );
                Ok(())
            }
        }
    }

    pub fn restore_latest(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        let project_id = checkpoint_shadow_dir_id(
            self.default_workdir
                .to_str()
                .ok_or("invalid workdir")?,
        );
        let git_dir = &self.store_root;
        if !git_dir.join("HEAD").exists() {
            return Err("checkpoint: no snapshots yet".into());
        }
        let ref_name = format!("refs/hermes/{project_id}");
        let output = Command::new("git")
            .args([
                "--git-dir",
                git_dir.to_str().ok_or("invalid git dir")?,
                "--work-tree",
                self.default_workdir.to_str().ok_or("invalid workdir")?,
                "checkout",
                ref_name.as_str(),
                "--",
                ".",
            ])
            .output()
            .map_err(|e| format!("checkpoint restore failed: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "checkpoint restore: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn checkpointed_dir_count(&self) -> usize {
        self.checkpointed_dirs.len()
    }

    #[cfg(test)]
    pub(crate) fn reserve_checkpoint_dir_for_test(&mut self, abs_dir: &str) -> bool {
        if self.checkpointed_dirs.contains(abs_dir) {
            return false;
        }
        self.checkpointed_dirs.insert(abs_dir.to_string());
        true
    }

    fn snapshot_worktree(&self, workdir: &Path, message: &str) -> Result<(), String> {
        let project_id = workdir
            .to_str()
            .map(checkpoint_shadow_dir_id)
            .ok_or("invalid workdir")?;
        std::fs::create_dir_all(&self.store_root).map_err(|e| e.to_string())?;
        let git_dir = &self.store_root;
        if !git_dir.join("HEAD").exists() {
            Command::new("git")
                .args(["init", "--bare"])
                .current_dir(git_dir)
                .output()
                .map_err(|e| format!("git init: {e}"))?;
        }
        let ref_name = format!("refs/hermes/{project_id}");
        let workdir_str = workdir.to_str().ok_or("invalid workdir")?;
        let status = Command::new("git")
            .env("GIT_DIR", git_dir)
            .env("GIT_WORK_TREE", workdir_str)
            .args(["add", "-A"])
            .output()
            .map_err(|e| format!("git add: {e}"))?;
        if !status.status.success() {
            return Err(String::from_utf8_lossy(&status.stderr).into());
        }
        let commit = Command::new("git")
            .env("GIT_DIR", git_dir)
            .env("GIT_WORK_TREE", workdir_str)
            .args(["commit", "-m", message, "--allow-empty"])
            .output()
            .map_err(|e| format!("git commit: {e}"))?;
        if !commit.status.success() {
            return Err(String::from_utf8_lossy(&commit.stderr).into());
        }
        let update_ref = Command::new("git")
            .env("GIT_DIR", git_dir)
            .args(["update-ref", ref_name.as_str(), "HEAD"])
            .output()
            .map_err(|e| format!("git update-ref: {e}"))?;
        if !update_ref.status.success() {
            return Err(String::from_utf8_lossy(&update_ref.stderr).into());
        }
        Ok(())
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn is_broad_checkpoint_dir(abs_dir: &Path) -> bool {
    if abs_dir == Path::new("/") {
        return true;
    }
    if let Some(home) = home_dir() {
        if abs_dir == home {
            return true;
        }
    }
    false
}

fn dir_file_count(dir: &Path) -> usize {
    let mut count = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            count = count.saturating_add(1);
            if count > MAX_FILES {
                return count;
            }
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                stack.push(entry.path());
            }
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn shadow_dir_hash_matches_fixture() {
        assert_eq!(
            checkpoint_shadow_dir_id("/workspace/demo"),
            "4de1d2f8b60db00a"
        );
    }

    #[test]
    fn new_turn_clears_per_turn_dedup() {
        let mut mgr = CheckpointManager::new(false, None, ".");
        mgr.reserve_checkpoint_dir_for_test("/tmp/project-a");
        assert_eq!(mgr.checkpointed_dir_count(), 1);
        mgr.new_turn();
        assert_eq!(mgr.checkpointed_dir_count(), 0);
    }

    #[test]
    fn dedup_blocks_second_reservation_same_turn() {
        let mut mgr = CheckpointManager::new(false, None, ".");
        assert!(mgr.reserve_checkpoint_dir_for_test("/tmp/project-a"));
        assert!(!mgr.reserve_checkpoint_dir_for_test("/tmp/project-a"));
        assert_eq!(mgr.checkpointed_dir_count(), 1);
        mgr.new_turn();
        assert!(mgr.reserve_checkpoint_dir_for_test("/tmp/project-a"));
    }

    #[test]
    fn get_working_dir_finds_cargo_toml_root() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("crates").join("demo");
        fs::create_dir_all(&nested).unwrap();
        fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        let file = nested.join("src").join("lib.rs");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "pub fn demo() {}").unwrap();

        let mgr = CheckpointManager::new(false, None, tmp.path());
        let resolved = mgr.get_working_dir_for_path(&file);
        assert_eq!(normalize_path(&resolved), normalize_path(tmp.path()));
    }

    #[test]
    fn dir_file_count_respects_limit() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..5 {
            fs::write(tmp.path().join(format!("file{i}.txt")), "x").unwrap();
        }
        assert_eq!(dir_file_count(tmp.path()), 5);
    }

    #[test]
    fn broad_dirs_are_detected() {
        assert!(is_broad_checkpoint_dir(Path::new("/")));
        if let Some(home) = home_dir() {
            assert!(is_broad_checkpoint_dir(&home));
        }
    }
}
