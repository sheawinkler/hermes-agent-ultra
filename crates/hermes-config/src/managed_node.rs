//! Hermes-managed Node.js resolver.
//!
//! Upstream Python prefers a Hermes-managed portable Node/npm before PATH so
//! Windows installs are not broken by an elevation-triggering or stale system
//! Node. Keep this Rust helper in `hermes-config` so CLI, gateway, and tools
//! can share the same lookup and PATH-prepend rules.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::paths::hermes_home;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodePlatform {
    Windows,
    Posix,
}

impl NodePlatform {
    fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Return Hermes-managed Node.js directories in preferred lookup order.
///
/// Windows portable installs unpack directly into `<HERMES_HOME>/node`; POSIX
/// installs usually expose binaries under `<HERMES_HOME>/node/bin`. Include
/// both shapes on every platform so migrated installs keep working.
pub fn iter_hermes_node_dirs() -> Vec<PathBuf> {
    iter_hermes_node_dirs_in(hermes_home())
}

/// Return managed Node lookup directories for an explicit Hermes home.
pub fn iter_hermes_node_dirs_in(home: impl AsRef<Path>) -> Vec<PathBuf> {
    iter_hermes_node_dirs_for_platform(home.as_ref(), NodePlatform::current())
}

fn iter_hermes_node_dirs_for_platform(home: &Path, platform: NodePlatform) -> Vec<PathBuf> {
    let node_dir = home.join("node");
    let bin_dir = node_dir.join("bin");
    match platform {
        NodePlatform::Windows => vec![node_dir, bin_dir],
        NodePlatform::Posix => vec![bin_dir, node_dir],
    }
}

/// Return a Hermes-managed Node/npm executable path, if installed.
pub fn find_hermes_node_executable(command: &str) -> Option<PathBuf> {
    find_hermes_node_executable_in(command, hermes_home())
}

/// Return a Hermes-managed Node/npm executable path under an explicit home.
pub fn find_hermes_node_executable_in(command: &str, home: impl AsRef<Path>) -> Option<PathBuf> {
    find_hermes_node_executable_for_platform(command, home.as_ref(), NodePlatform::current())
}

fn find_hermes_node_executable_for_platform(
    command: &str,
    home: &Path,
    platform: NodePlatform,
) -> Option<PathBuf> {
    let names = candidate_node_command_names_for_platform(command, platform);
    for directory in iter_hermes_node_dirs_for_platform(home, platform) {
        for name in &names {
            let candidate = directory.join(name);
            if executable_candidate_exists_for_platform(&candidate, platform) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Resolve a Node.js command, preferring Hermes-managed installs before PATH.
pub fn find_node_executable(command: &str) -> Option<PathBuf> {
    find_hermes_node_executable(command).or_else(|| find_node_executable_on_path(command))
}

fn find_node_executable_on_path(command: &str) -> Option<PathBuf> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let command_path = Path::new(command);
    if command_path.components().count() > 1 || command_path.is_absolute() {
        return executable_candidate_exists_for_platform(command_path, NodePlatform::current())
            .then(|| command_path.to_path_buf());
    }

    let names = candidate_node_command_names_for_platform(command, NodePlatform::current());
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        for name in &names {
            let candidate = dir.join(name);
            if executable_candidate_exists_for_platform(&candidate, NodePlatform::current()) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Return PATH with existing Hermes-managed Node directories prepended.
pub fn with_hermes_node_path_var(existing_path: Option<OsString>) -> OsString {
    with_hermes_node_path_var_for_home(existing_path, hermes_home())
}

/// Return PATH with explicit-home managed Node directories prepended.
pub fn with_hermes_node_path_var_for_home(
    existing_path: Option<OsString>,
    home: impl AsRef<Path>,
) -> OsString {
    let existing_dirs: Vec<PathBuf> = existing_path
        .as_ref()
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    let mut seen: HashSet<OsString> = existing_dirs
        .iter()
        .map(|path| path.as_os_str().to_os_string())
        .collect();
    let mut merged = Vec::new();
    for dir in iter_hermes_node_dirs_in(home) {
        if dir.is_dir() && seen.insert(dir.as_os_str().to_os_string()) {
            merged.push(dir);
        }
    }
    merged.extend(existing_dirs);
    std::env::join_paths(merged).unwrap_or_default()
}

fn candidate_node_command_names_for_platform(command: &str, platform: NodePlatform) -> Vec<String> {
    let base = command.rsplit(['/', '\\']).next().unwrap_or(command).trim();
    if base.is_empty() {
        return Vec::new();
    }
    if platform != NodePlatform::Windows || base.contains('.') {
        return vec![base.to_string()];
    }
    match base.to_ascii_lowercase().as_str() {
        "npm" => vec!["npm.cmd".into(), "npm.exe".into(), "npm".into()],
        "npx" => vec!["npx.cmd".into(), "npx.exe".into(), "npx".into()],
        "node" => vec!["node.exe".into(), "node".into()],
        _ => vec![
            format!("{base}.cmd"),
            format!("{base}.exe"),
            base.to_string(),
        ],
    }
}

fn executable_candidate_exists_for_platform(path: &Path, platform: NodePlatform) -> bool {
    if !path.is_file() {
        return false;
    }
    if platform == NodePlatform::Windows {
        return true;
    }
    is_unix_executable(path)
}

#[cfg(unix)]
fn is_unix_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_unix_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn touch(path: &Path, executable: bool) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if executable { 0o755 } else { 0o644 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
        }
        #[cfg(not(unix))]
        {
            let _ = executable;
        }
    }

    #[test]
    fn windows_prefers_root_node_and_cmd_shims() {
        let tmp = tempdir().expect("tempdir");
        let root_npm = tmp.path().join("node").join("npm.cmd");
        let bin_npm = tmp.path().join("node").join("bin").join("npm.exe");
        touch(&root_npm, false);
        touch(&bin_npm, false);

        let resolved =
            find_hermes_node_executable_for_platform("npm", tmp.path(), NodePlatform::Windows)
                .expect("npm");

        assert_eq!(resolved, root_npm);
        assert_eq!(
            candidate_node_command_names_for_platform("npx", NodePlatform::Windows),
            vec!["npx.cmd", "npx.exe", "npx"]
        );
    }

    #[test]
    fn posix_prefers_bin_and_requires_executable_bit() {
        let tmp = tempdir().expect("tempdir");
        let root_node = tmp.path().join("node").join("node");
        let bin_node = tmp.path().join("node").join("bin").join("node");
        touch(&root_node, true);
        touch(&bin_node, false);

        let resolved =
            find_hermes_node_executable_for_platform("node", tmp.path(), NodePlatform::Posix)
                .expect("node");

        assert_eq!(resolved, root_node);
    }

    #[test]
    fn path_var_prepends_existing_managed_dirs_without_duplicates() {
        let tmp = tempdir().expect("tempdir");
        let node_dir = tmp.path().join("node");
        let bin_dir = node_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("mkdir");
        let existing =
            std::env::join_paths([bin_dir.clone(), PathBuf::from("/usr/bin")]).expect("join");

        let merged = with_hermes_node_path_var_for_home(Some(existing), tmp.path());
        let parts: Vec<PathBuf> = std::env::split_paths(&merged).collect();

        assert_eq!(parts[0], node_dir);
        assert_eq!(parts[1], bin_dir);
        assert_eq!(parts[2], PathBuf::from("/usr/bin"));
    }
}
