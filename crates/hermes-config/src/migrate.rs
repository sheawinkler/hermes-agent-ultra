//! One-time migration from legacy `.hermes` / `hermes` home directories to
//! `.hermes-agent-ultra` / `hermes-agent-ultra`.

use std::path::{Path, PathBuf};

use crate::paths::{
    self, legacy_home_basename, primary_home_basename, user_home_dir, LEGACY_HOME_DIR,
    PRIMARY_HOME_DIR,
};

/// Ensure the effective Hermes home exists and legacy data has been copied forward.
///
/// Resolution order:
/// 1. `home_dir` CLI override
/// 2. `HERMES_HOME`
/// 3. `HERMES_AGENT_ULTRA_HOME`
/// 4. Default `~/.hermes-agent-ultra` with legacy fallback
///
/// When the resolved path uses a legacy directory name, map to the primary name,
/// copy if needed, and return the primary path.
pub fn ensure_migrated_hermes_home(home_dir: Option<&str>) -> PathBuf {
    let requested = resolve_requested_home(home_dir);
    ensure_migrated_path(&requested)
}

/// Project-local Hermes directory under `cwd` (`.hermes-agent-ultra`, with read
/// fallback to legacy `.hermes`).
pub fn project_hermes_dir(cwd: &Path) -> PathBuf {
    let primary = cwd.join(PRIMARY_HOME_DIR);
    let legacy = cwd.join(LEGACY_HOME_DIR);
    if primary.exists() || !legacy.exists() {
        primary
    } else {
        legacy
    }
}

/// Read-only legacy session roots for resume fallback (newest naming last).
pub fn legacy_hermes_home_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let base = user_home_dir();

    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = local.trim();
        if !local.is_empty() {
            candidates.push(
                PathBuf::from(local)
                    .join(legacy_home_basename())
                    .join("sessions"),
            );
        }
    }

    candidates.push(base.join(LEGACY_HOME_DIR).join("sessions"));
    candidates.push(base.join(PRIMARY_HOME_DIR).join("sessions"));
    candidates
}

fn resolve_requested_home(home_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = home_dir.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(dir);
    }
    if let Some(home) = paths::env_var_path("HERMES_HOME") {
        return home;
    }
    if let Some(home) = paths::env_var_path("HERMES_AGENT_ULTRA_HOME") {
        return home;
    }

    let base = user_home_dir();
    let primary = base.join(PRIMARY_HOME_DIR);
    let legacy = base.join(LEGACY_HOME_DIR);
    if primary.exists() || !legacy.exists() {
        primary
    } else {
        legacy
    }
}

fn ensure_migrated_path(requested: &Path) -> PathBuf {
    let (primary, legacy) = primary_and_legacy_pair(requested);

    if primary.exists() {
        let _ = std::fs::create_dir_all(&primary);
        return primary;
    }

    if legacy.exists() {
        if let Err(err) = copy_home_tree(&legacy, &primary) {
            tracing::warn!(
                "Failed to migrate Hermes home {} -> {}: {err}",
                legacy.display(),
                primary.display()
            );
            return legacy;
        }
        tracing::info!(
            "Migrated Hermes home: {} -> {}",
            legacy.display(),
            primary.display()
        );
        return primary;
    }

    let _ = std::fs::create_dir_all(&primary);
    primary
}

fn primary_and_legacy_pair(path: &Path) -> (PathBuf, PathBuf) {
    if let Some(file_name) = path.file_name().and_then(|s| s.to_str()) {
        if is_legacy_basename(file_name) {
            return (
                remap_basename(path, paired_primary_name(file_name)),
                path.to_path_buf(),
            );
        }
        if is_primary_basename(file_name) {
            return (
                path.to_path_buf(),
                remap_basename(path, paired_legacy_name(file_name)),
            );
        }
    }
    (path.to_path_buf(), path.to_path_buf())
}

fn is_legacy_basename(name: &str) -> bool {
    name == LEGACY_HOME_DIR || name == legacy_home_basename()
}

fn is_primary_basename(name: &str) -> bool {
    name == PRIMARY_HOME_DIR || name == primary_home_basename()
}

fn paired_primary_name(legacy_name: &str) -> &'static str {
    if legacy_name.starts_with('.') {
        PRIMARY_HOME_DIR
    } else {
        paths::LOCALAPPDATA_SUBDIR_NEW
    }
}

fn paired_legacy_name(primary_name: &str) -> &'static str {
    if primary_name.starts_with('.') {
        LEGACY_HOME_DIR
    } else {
        paths::LOCALAPPDATA_SUBDIR_LEGACY
    }
}

fn remap_basename(path: &Path, new_base: &str) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.join(new_base),
        None => PathBuf::from(new_base),
    }
}

fn copy_home_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        return Ok(());
    }

    let lock = dst.with_extension("migrate.lock");
    if lock.exists() {
        // Another process may be migrating; wait briefly for dst to appear.
        for _ in 0..20 {
            if dst.exists() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    std::fs::write(&lock, std::process::id().to_string())?;
    let result = copy_dir_all(src, dst);
    let _ = std::fs::remove_file(&lock);
    result
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&src_path, &dst_path)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            if file_type.is_symlink() {
                #[cfg(unix)]
                {
                    let target = std::fs::read_link(&src_path)?;
                    std::os::unix::fs::symlink(&target, &dst_path)?;
                }
                #[cfg(not(unix))]
                {
                    std::fs::copy(&src_path, &dst_path)?;
                }
            } else {
                std::fs::copy(&src_path, &dst_path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;
    use tempfile::tempdir;

    #[test]
    fn migrates_legacy_dot_hermes_to_primary() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let legacy = tmp.path().join(".hermes");
        let primary = tmp.path().join(".hermes-agent-ultra");
        std::fs::create_dir_all(legacy.join("sessions")).expect("legacy");
        std::fs::write(legacy.join("config.yaml"), "memory: {}\n").expect("config");

        unsafe {
            std::env::remove_var("HERMES_HOME");
            std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
            std::env::set_var("HERMES_HOME", legacy.to_string_lossy().as_ref());
        }

        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, primary);
        assert!(primary.join("config.yaml").exists());
        assert!(legacy.join("config.yaml").exists());

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
            match original_ultra {
                Some(v) => std::env::set_var("HERMES_AGENT_ULTRA_HOME", v),
                None => std::env::remove_var("HERMES_AGENT_ULTRA_HOME"),
            }
        }
    }

    #[test]
    fn uses_primary_when_already_present() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let primary = tmp.path().join(".hermes-agent-ultra");
        std::fs::create_dir_all(&primary).expect("primary");
        std::fs::write(primary.join("marker"), "ok").expect("marker");

        unsafe {
            std::env::set_var("HERMES_HOME", primary.to_string_lossy().as_ref());
        }
        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, primary);

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn migrates_localappdata_hermes_to_hermes_agent_ultra() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let legacy = tmp.path().join("hermes");
        let primary = tmp.path().join("hermes-agent-ultra");
        std::fs::create_dir_all(legacy.join("logs")).expect("legacy");
        std::fs::write(legacy.join("config.yaml"), "gateway: {}\n").expect("config");

        unsafe {
            std::env::set_var("HERMES_HOME", legacy.to_string_lossy().as_ref());
        }
        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, primary);
        assert!(primary.join("config.yaml").exists());

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn initializes_empty_primary_when_no_legacy() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let primary = tmp.path().join(".hermes-agent-ultra");

        unsafe {
            std::env::remove_var("HERMES_HOME");
            std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
            std::env::set_var("HERMES_HOME", primary.to_string_lossy().as_ref());
        }

        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, primary);
        assert!(primary.is_dir());

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
            match original_ultra {
                Some(v) => std::env::set_var("HERMES_AGENT_ULTRA_HOME", v),
                None => std::env::remove_var("HERMES_AGENT_ULTRA_HOME"),
            }
        }
    }
}
