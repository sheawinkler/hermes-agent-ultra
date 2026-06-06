//! Repository discovery helpers shared by tool-facing runtime surfaces.

use std::path::{Path, PathBuf};

pub fn detect_repo_root_from_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    detect_repo_root_from(&cwd)
}

pub fn detect_repo_root_from(start: &Path) -> Option<PathBuf> {
    for candidate in start.ancestors() {
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_nearest_parent_with_git_directory() {
        let temp = tempfile::tempdir().expect("temp");
        let repo = temp.path().join("repo");
        let nested = repo.join("a/b/c");
        std::fs::create_dir_all(repo.join(".git")).expect("mkdir git");
        std::fs::create_dir_all(&nested).expect("mkdir nested");

        assert_eq!(
            detect_repo_root_from(&nested).as_deref(),
            Some(repo.as_path())
        );
    }
}
