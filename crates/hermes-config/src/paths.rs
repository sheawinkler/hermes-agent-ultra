//! Path management for the hermes home directory and its files.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// hermes_home
// ---------------------------------------------------------------------------

const DEFAULT_HOME_DIR: &str = ".hermes-agent-ultra";
const LEGACY_HOME_DIR: &str = ".hermes";

/// Return the hermes home directory.
///
/// - If the `HERMES_HOME` environment variable is set, use that.
/// - Else if `HERMES_AGENT_ULTRA_HOME` is set, use that.
/// - Otherwise default to `~/.hermes-agent-ultra`.
/// - Backward-compat: if `~/.hermes-agent-ultra` does not exist but
///   `~/.hermes` exists, use `~/.hermes`.
pub fn hermes_home() -> PathBuf {
    if let Some(home) = env_var_path("HERMES_HOME") {
        return home;
    }
    if let Some(home) = env_var_path("HERMES_AGENT_ULTRA_HOME") {
        return home;
    }

    let home_dir = user_home_dir();
    let primary = home_dir.join(DEFAULT_HOME_DIR);
    let legacy = home_dir.join(LEGACY_HOME_DIR);
    if primary.exists() || !legacy.exists() {
        primary
    } else {
        legacy
    }
}

fn env_var_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Best-effort home directory resolution.
fn user_home_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
    } else if let Ok(home) = std::env::var("USERPROFILE") {
        PathBuf::from(home)
    } else {
        PathBuf::from(".")
    }
}

/// Hermes state root directory (same rule as CLI `--config-dir` / gateway data).
///
/// If `config_dir_override` is set, that path is used; otherwise [`hermes_home`].
/// Use this for `cron/`, `webhooks.json`, and other machine-local state so CLI
/// and gateway stay aligned.
pub fn state_dir(config_dir_override: Option<&Path>) -> PathBuf {
    config_dir_override
        .map(|p| p.to_path_buf())
        .unwrap_or_else(hermes_home)
}

// ---------------------------------------------------------------------------
// Derived paths
// ---------------------------------------------------------------------------

/// `$hermes_home/config.yaml`
pub fn config_path() -> PathBuf {
    hermes_home().join("config.yaml")
}

/// `$hermes_home/cli-config.yaml`
pub fn cli_config_path() -> PathBuf {
    hermes_home().join("cli-config.yaml")
}

/// `$hermes_home/gateway.json`
pub fn gateway_json_path() -> PathBuf {
    hermes_home().join("gateway.json")
}

/// PID file written by `hermes gateway start` (same directory as `config.yaml`).
pub fn gateway_pid_path() -> PathBuf {
    hermes_home().join("gateway.pid")
}

/// Gateway PID file under an explicit Hermes home directory (e.g. `HERMES_HOME` or `-C`).
pub fn gateway_pid_path_in(home: impl AsRef<std::path::Path>) -> PathBuf {
    home.as_ref().join("gateway.pid")
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

/// `$hermes_home/auth.json` — credential store written by `hermes auth login`.
///
/// Mirrors Python's `tools.managed_tool_gateway.auth_json_path()`. Used by
/// the managed-tool-gateway resolver to read provider OAuth tokens.
pub fn auth_json_path() -> PathBuf {
    hermes_home().join("auth.json")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermes_home_respects_env() {
        // Ensure helper always yields a concrete path.
        let home = hermes_home();
        assert!(!home.as_os_str().is_empty());
    }

    /// Combined test for all path helpers.
    ///
    /// Environment-variable mutations are not thread-safe, so we test
    /// both "derived paths" and "explicit home" in a single test to
    /// avoid races with parallel test threads.
    #[test]
    fn derived_paths_and_explicit_home() {
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        // -- Part 1: derived paths are consistent --
        std::env::set_var("HERMES_HOME", "/tmp/hermes-path-test");
        let home = hermes_home();
        assert_eq!(home, PathBuf::from("/tmp/hermes-path-test"));
        assert_eq!(state_dir(None), home);
        assert_eq!(config_path(), home.join("config.yaml"));
        assert_eq!(cli_config_path(), home.join("cli-config.yaml"));
        assert_eq!(gateway_json_path(), home.join("gateway.json"));
        assert_eq!(gateway_pid_path(), home.join("gateway.pid"));
        assert_eq!(memory_path(), home.join("MEMORY.md"));
        assert_eq!(user_path(), home.join("USER.md"));
        assert_eq!(skills_dir(), home.join("skills"));
        assert_eq!(sessions_dir(), home.join("sessions"));
        assert_eq!(cron_dir(), home.join("cron"));
        assert_eq!(env_path(), home.join(".env"));

        // -- Part 2: explicit home override --
        std::env::set_var("HERMES_HOME", "/tmp/test-hermes");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes"));
        assert_eq!(config_path(), PathBuf::from("/tmp/test-hermes/config.yaml"));

        // -- Part 3: ultra env alias works when HERMES_HOME is absent --
        std::env::remove_var("HERMES_HOME");
        std::env::set_var("HERMES_AGENT_ULTRA_HOME", "/tmp/test-hermes-ultra");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes-ultra"));
        assert_eq!(
            config_path(),
            PathBuf::from("/tmp/test-hermes-ultra/config.yaml")
        );
        std::env::remove_var("HERMES_AGENT_ULTRA_HOME");

        // Restore
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
