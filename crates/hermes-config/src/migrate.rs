//! Resolve the Hermes Ultra home directory without copying legacy data.
//!
//! Legacy `.hermes` / `hermes` and the brief misnamed ultra directory
//! (`.hermes-ultra-agent` / `hermes-ultra-agent`) are **not** migrated.
//! When those paths are requested, the process remaps to a fresh
//! `.hermes-agent-ultra` / `hermes-agent-ultra` directory instead.

use std::path::{Path, PathBuf};

use crate::paths::{
    self, INTERMEDIATE_HOME_DIR, LEGACY_HOME_DIR, LOCALAPPDATA_SUBDIR_INTERMEDIATE,
    LOCALAPPDATA_SUBDIR_LEGACY, LOCALAPPDATA_SUBDIR_NEW, PRIMARY_HOME_DIR,
    intermediate_home_basename, legacy_home_basename, primary_home_basename, user_home_dir,
};

/// Standard Hermes Ultra home subdirectories (same set as `hermes setup`).
pub const HOME_LAYOUT_SUBDIRS: &[&str] =
    &["profiles", "sessions", "logs", "skills", "cron", "cache"];

const DEFAULT_CONFIG_YAML: &str = include_str!("../config.example.yaml");

/// Ensure the effective Hermes Ultra home exists with the standard directory layout.
///
/// Idempotent: creates missing roots only; never overwrites existing files.
pub fn ensure_hermes_home_layout(home_dir: Option<&str>) -> PathBuf {
    let home = ensure_migrated_hermes_home(home_dir);
    for sub in HOME_LAYOUT_SUBDIRS {
        let _ = std::fs::create_dir_all(home.join(sub));
    }
    seed_default_config_yaml(&home);
    home
}

/// Write `config.yaml` when missing (never overwrites an existing file).
fn seed_default_config_yaml(home: &Path) {
    let path = home.join("config.yaml");
    if path.exists() {
        return;
    }
    if let Err(err) = std::fs::write(&path, DEFAULT_CONFIG_YAML) {
        tracing::warn!(path = %path.display(), error = %err, "failed to seed config.yaml");
        return;
    }
    tracing::info!(path = %path.display(), "created default config.yaml");
}

/// Ensure the effective Hermes Ultra home exists (empty if newly created).
///
/// Resolution order:
/// 1. `home_dir` CLI override
/// 2. `HERMES_HOME`
/// 3. `HERMES_AGENT_ULTRA_HOME`
/// 4. Default `~/.hermes-agent-ultra`
///
/// Known legacy/intermediate directory names are remapped to the primary ultra
/// name; custom absolute paths are left unchanged.
pub fn ensure_migrated_hermes_home(home_dir: Option<&str>) -> PathBuf {
    let requested = resolve_requested_home(home_dir);
    ensure_primary_home(&requested)
}

/// Project-local Hermes directory under `cwd` (`.hermes-agent-ultra` only).
pub fn project_hermes_dir(cwd: &Path) -> PathBuf {
    cwd.join(PRIMARY_HOME_DIR)
}

/// Read-only legacy session roots for resume fallback (newest naming last).
pub fn legacy_hermes_home_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let base = user_home_dir();

    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let local = local.trim();
        if !local.is_empty() {
            let local = PathBuf::from(local);
            candidates.push(local.join(legacy_home_basename()).join("sessions"));
            candidates.push(local.join(intermediate_home_basename()).join("sessions"));
        }
    }

    candidates.push(base.join(LEGACY_HOME_DIR).join("sessions"));
    candidates.push(base.join(INTERMEDIATE_HOME_DIR).join("sessions"));
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

    user_home_dir().join(PRIMARY_HOME_DIR)
}

fn ensure_primary_home(requested: &Path) -> PathBuf {
    let primary = remap_to_primary_if_needed(requested);
    let _ = std::fs::create_dir_all(&primary);
    primary
}

fn remap_to_primary_if_needed(path: &Path) -> PathBuf {
    let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
        return path.to_path_buf();
    };
    if is_primary_basename(file_name) {
        return path.to_path_buf();
    }
    if is_legacy_or_intermediate_basename(file_name) {
        return remap_basename(path, paired_primary_name(file_name));
    }
    path.to_path_buf()
}

fn is_legacy_or_intermediate_basename(name: &str) -> bool {
    name == LEGACY_HOME_DIR
        || name == legacy_home_basename()
        || name == INTERMEDIATE_HOME_DIR
        || name == intermediate_home_basename()
        || name == LOCALAPPDATA_SUBDIR_LEGACY
        || name == LOCALAPPDATA_SUBDIR_INTERMEDIATE
}

fn is_primary_basename(name: &str) -> bool {
    name == PRIMARY_HOME_DIR || name == primary_home_basename() || name == LOCALAPPDATA_SUBDIR_NEW
}

fn paired_primary_name(requested_name: &str) -> &'static str {
    if requested_name.starts_with('.') {
        PRIMARY_HOME_DIR
    } else {
        LOCALAPPDATA_SUBDIR_NEW
    }
}

fn remap_basename(path: &Path, new_base: &str) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.join(new_base),
        None => PathBuf::from(new_base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;
    use tempfile::tempdir;

    #[test]
    fn remaps_legacy_dot_hermes_to_fresh_primary_without_copy() {
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
        assert!(primary.is_dir());
        assert!(!primary.join("config.yaml").exists());
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
    fn remaps_localappdata_hermes_to_fresh_ultra_agent() {
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
        assert!(primary.is_dir());
        assert!(!primary.join("config.yaml").exists());

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn remaps_intermediate_ultra_name_without_copy() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let intermediate = tmp.path().join("hermes-ultra-agent");
        let primary = tmp.path().join("hermes-agent-ultra");
        std::fs::create_dir_all(intermediate.join("logs")).expect("intermediate");
        std::fs::write(intermediate.join("config.yaml"), "platforms: {}\n").expect("config");

        unsafe {
            std::env::set_var("HERMES_HOME", intermediate.to_string_lossy().as_ref());
        }
        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, primary);
        assert!(!primary.join("config.yaml").exists());

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn preserves_custom_absolute_home() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let custom = tmp.path().join("my-portable-config");
        std::fs::create_dir_all(&custom).expect("custom");

        unsafe {
            std::env::set_var("HERMES_HOME", custom.to_string_lossy().as_ref());
        }
        let resolved = ensure_migrated_hermes_home(None);
        assert_eq!(resolved, custom);

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }

    #[test]
    fn initializes_empty_primary_when_unset() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let primary = tmp.path().join(".hermes-agent-ultra");

        unsafe {
            std::env::remove_var("HERMES_HOME");
            std::env::remove_var("HERMES_AGENT_ULTRA_HOME");
            std::env::set_var("HOME", tmp.path().to_string_lossy().as_ref());
            std::env::set_var("USERPROFILE", tmp.path().to_string_lossy().as_ref());
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

    #[test]
    fn ensure_home_layout_creates_standard_subdirs() {
        let _g = test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();

        let tmp = tempdir().expect("tempdir");
        let home = tmp.path().join(".hermes-agent-ultra");

        unsafe {
            std::env::set_var("HERMES_HOME", home.to_string_lossy().as_ref());
        }

        let resolved = ensure_hermes_home_layout(None);
        assert_eq!(resolved, home);
        for sub in HOME_LAYOUT_SUBDIRS {
            assert!(home.join(sub).is_dir(), "missing subdir {sub}");
        }
        assert!(home.join("config.yaml").is_file(), "missing config.yaml");

        unsafe {
            match original {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
        }
    }
}
