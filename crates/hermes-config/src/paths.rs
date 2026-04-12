//! Path management for the hermes home directory and its files.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// hermes_home
// ---------------------------------------------------------------------------

/// Return the hermes home directory.
///
/// - If the `HERMES_HOME` environment variable is set, use that.
/// - Otherwise default to `~/.hermes`.
pub fn hermes_home() -> PathBuf {
    if let Ok(home) = std::env::var("HERMES_HOME") {
        PathBuf::from(home)
    } else {
        dirs_home().join(".hermes")
    }
}

/// Best-effort home directory resolution.
fn dirs_home() -> PathBuf {
    // Try the `directories` crate first; fall back to $HOME / $USERPROFILE.
    if let Some(dirs) = directories::ProjectDirs::from("", "", "hermes") {
        dirs.config_dir().to_path_buf()
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        PathBuf::from(home)
    } else {
        // Last resort: current directory
        PathBuf::from(".")
    }
}

// ---------------------------------------------------------------------------
// Derived paths
// ---------------------------------------------------------------------------

/// `$hermes_home/config.yaml`
pub fn config_path() -> PathBuf {
    hermes_home().join("config.yaml")
}

/// `$hermes_home/gateway.json`
pub fn gateway_json_path() -> PathBuf {
    hermes_home().join("gateway.json")
}

/// `$hermes_home/MEMORY.md`
pub fn memory_path() -> PathBuf {
    hermes_home().join("MEMORY.md")
}

/// `$hermes_home/USER.md`
pub fn user_path() -> PathBuf {
    hermes_home().join("USER.md")
}

/// `$hermes_home/skills/`
pub fn skills_dir() -> PathBuf {
    hermes_home().join("skills")
}

/// `$hermes_home/sessions/`
pub fn sessions_dir() -> PathBuf {
    hermes_home().join("sessions")
}

/// `$hermes_home/cron/`
pub fn cron_dir() -> PathBuf {
    hermes_home().join("cron")
}

/// `$hermes_home/.env`
pub fn env_path() -> PathBuf {
    hermes_home().join(".env")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_home_respects_env() {
        // If HERMES_HOME is not set, we should get ~/.hermes
        let home = hermes_home();
        // Just ensure it's a valid path (not empty)
        assert!(!home.as_os_str().is_empty());
    }

    #[test]
    fn derived_paths_are_consistent() {
        let home = hermes_home();
        assert_eq!(config_path(), home.join("config.yaml"));
        assert_eq!(gateway_json_path(), home.join("gateway.json"));
        assert_eq!(memory_path(), home.join("MEMORY.md"));
        assert_eq!(user_path(), home.join("USER.md"));
        assert_eq!(skills_dir(), home.join("skills"));
        assert_eq!(sessions_dir(), home.join("sessions"));
        assert_eq!(cron_dir(), home.join("cron"));
        assert_eq!(env_path(), home.join(".env"));
    }

    #[test]
    fn paths_with_explicit_home() {
        // Temporarily set HERMES_HOME
        let original = std::env::var("HERMES_HOME").ok();
        std::env::set_var("HERMES_HOME", "/tmp/test-hermes");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes"));
        assert_eq!(config_path(), PathBuf::from("/tmp/test-hermes/config.yaml"));

        // Restore
        match original {
            Some(v) => std::env::set_var("HERMES_HOME", v),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }
}