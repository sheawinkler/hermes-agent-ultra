//! Path management for the hermes home directory and its files.

use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// hermes_home
// ---------------------------------------------------------------------------

/// Primary Hermes Ultra home directory name under the user profile.
pub const PRIMARY_HOME_DIR: &str = ".hermes-agent-ultra";
/// Brief misnamed ultra directory (read-only remap target; never copied forward).
pub const INTERMEDIATE_HOME_DIR: &str = ".hermes-ultra-agent";
/// Legacy Hermes home directory name (pre-ultra branding; never copied forward).
pub const LEGACY_HOME_DIR: &str = ".hermes";
/// Project-local Hermes directory name in the working tree.
pub const PROJECT_HOME_DIR: &str = ".hermes-agent-ultra";
/// Legacy project-local directory name.
pub const LEGACY_PROJECT_HOME_DIR: &str = ".hermes";
/// Windows `%LOCALAPPDATA%` subdirectory for Hermes Ultra data.
pub const LOCALAPPDATA_SUBDIR_NEW: &str = "hermes-agent-ultra";
/// Brief misnamed Windows `%LOCALAPPDATA%` ultra subdirectory.
pub const LOCALAPPDATA_SUBDIR_INTERMEDIATE: &str = "hermes-ultra-agent";
/// Legacy Windows `%LOCALAPPDATA%` subdirectory.
pub const LOCALAPPDATA_SUBDIR_LEGACY: &str = "hermes";

/// Return the hermes home directory.
///
/// - If the `HERMES_HOME` environment variable is set, use that.
/// - Else if `HERMES_AGENT_ULTRA_HOME` is set, use that.
/// - Otherwise default to `~/.hermes-agent-ultra`.
pub fn hermes_home() -> PathBuf {
    if let Some(home) = env_var_path("HERMES_HOME") {
        return home;
    }
    if let Some(home) = env_var_path("HERMES_AGENT_ULTRA_HOME") {
        return home;
    }

    default_home_without_migration()
}

/// Resolve the default home without touching legacy directories (read-only).
pub fn default_home_without_migration() -> PathBuf {
    user_home_dir().join(PRIMARY_HOME_DIR)
}

/// Basename for the primary Hermes home directory (`.hermes-agent-ultra` or `hermes-agent-ultra`).
pub fn primary_home_basename() -> &'static str {
    PRIMARY_HOME_DIR.trim_start_matches('.')
}

/// Basename for the intermediate ultra home directory.
pub fn intermediate_home_basename() -> &'static str {
    INTERMEDIATE_HOME_DIR.trim_start_matches('.')
}

/// Basename for the legacy Hermes home directory (`.hermes` or `hermes`).
pub fn legacy_home_basename() -> &'static str {
    LEGACY_HOME_DIR.trim_start_matches('.')
}

pub(crate) fn env_var_path(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Best-effort home directory resolution.
pub fn user_home_dir() -> PathBuf {
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

/// `$hermes_home/state.db` — session SQLite store (Python `hermes_state` parity).
///
/// Uses legacy `sessions.db` when it exists and `state.db` does not yet.
pub fn state_db_path() -> PathBuf {
    state_db_path_in(hermes_home())
}

/// Resolve session DB path under an explicit Hermes home directory.
pub fn state_db_path_in(home: impl AsRef<Path>) -> PathBuf {
    let home = home.as_ref();
    let state_db = home.join("state.db");
    let legacy = home.join("sessions.db");
    if state_db.exists() {
        state_db
    } else if legacy.exists() {
        legacy
    } else {
        state_db
    }
}

/// `$hermes_home/cron/`
pub fn cron_dir() -> PathBuf {
    hermes_home().join("cron")
}

/// Subdirectory under Hermes home for voice dialog (`hermes talk`).
pub const TALK_SUBDIR: &str = "hermes-talk";

/// `$hermes_home/hermes-talk/`
pub fn talk_dir() -> PathBuf {
    hermes_home().join(TALK_SUBDIR)
}

/// `$hermes_home/hermes-talk/config.toml`
pub fn talk_config_path() -> PathBuf {
    talk_dir().join("config.toml")
}

/// `$hermes_home/interest.db` — local user interest (POI) topic store.
pub fn interest_db_path() -> PathBuf {
    hermes_home().join("interest.db")
}

/// Interest DB under an explicit Hermes home directory.
pub fn interest_db_path_in(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("interest.db")
}

/// `$hermes_home/insights/` — contribution outbox and local state.
pub fn insights_dir() -> PathBuf {
    hermes_home().join("insights")
}

/// Contribution outbox SQLite database.
pub fn insights_outbox_path() -> PathBuf {
    insights_dir().join("outbox.db")
}

/// Persistent pseudo-anonymous installation id for REST API headers.
pub fn insights_installation_id_path() -> PathBuf {
    insights_dir().join("installation_id")
}

/// Audit log for dropped/rejected contributions (JSONL).
pub fn insights_audit_path() -> PathBuf {
    insights_dir().join("audit.jsonl")
}

/// Skill maturity tracking for contribution eligibility.
pub fn insights_skill_state_path() -> PathBuf {
    insights_dir().join("skill_state.json")
}

/// `$hermes_home/evolution/` — background review ledger and evolution state.
pub fn evolution_dir() -> PathBuf {
    hermes_home().join("evolution")
}

/// Append-only background review event log.
pub fn evolution_reviews_path() -> PathBuf {
    evolution_dir().join("reviews.jsonl")
}

/// Evolution directory under an explicit Hermes home.
pub fn evolution_dir_in(home: impl AsRef<Path>) -> PathBuf {
    home.as_ref().join("evolution")
}

/// Review ledger path under an explicit Hermes home.
pub fn evolution_reviews_path_in(home: impl AsRef<Path>) -> PathBuf {
    evolution_dir_in(home).join("reviews.jsonl")
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
// Agent path resolution (Windows /tmp, session temp — parity with Python local.py)
// ---------------------------------------------------------------------------

/// Writable session temp directory for terminal artifacts and agent-generated files.
///
/// On Windows uses `{HERMES_HOME}/cache/terminal` (forward-slash friendly, no spaces).
/// On Unix prefers `/tmp` when writable, else `tempfile::gettempdir()`.
pub fn session_temp_dir() -> PathBuf {
    #[cfg(windows)]
    {
        let cache_dir = hermes_home().join("cache").join("terminal");
        let _ = std::fs::create_dir_all(&cache_dir);
        cache_dir
    }

    #[cfg(not(windows))]
    {
        for key in ["TMPDIR", "TMP", "TEMP"] {
            if let Ok(val) = std::env::var(key) {
                let trimmed = val.trim();
                if !trimmed.is_empty() && trimmed.starts_with('/') {
                    let dir = PathBuf::from(trimmed);
                    if dir.is_dir() {
                        return dir;
                    }
                }
            }
        }
        let tmp = PathBuf::from("/tmp");
        if tmp.is_dir() {
            return tmp;
        }
        std::env::temp_dir()
    }
}

/// Map agent-supplied paths before filesystem or outbound media operations.
///
/// - Expands `~` / `~/` via [`expand_tilde`].
/// - On Windows, rewrites `/tmp/...` and `\tmp\...` to [`session_temp_dir`].
/// - Leaves explicit `C:\...` and other native Windows paths unchanged.
pub fn resolve_agent_path(input: &str) -> PathBuf {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return PathBuf::new();
    }

    if trimmed.starts_with('~') {
        return expand_tilde(trimmed).unwrap_or_else(|_| PathBuf::from(trimmed));
    }

    #[cfg(windows)]
    {
        if trimmed == "/tmp" || trimmed == "\\tmp" {
            return session_temp_dir();
        }
        if let Some(rest) = trimmed.strip_prefix("/tmp/") {
            return session_temp_dir().join(rest);
        }
        if let Some(rest) = trimmed.strip_prefix("/tmp\\") {
            return session_temp_dir().join(rest);
        }
        if let Some(rest) = trimmed.strip_prefix("\\tmp\\") {
            return session_temp_dir().join(rest);
        }
    }

    PathBuf::from(trimmed)
}

/// Expand `~` and `~/suffix` to the user home directory.
pub fn expand_tilde(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim();
    if !trimmed.starts_with('~') {
        return Ok(PathBuf::from(trimmed));
    }
    let rest = &trimmed[1..];
    if rest.is_empty() {
        return Ok(user_home_dir());
    }
    if rest.starts_with('/') || rest.starts_with('\\') {
        let suffix = rest.trim_start_matches(['/', '\\']);
        return Ok(if suffix.is_empty() {
            user_home_dir()
        } else {
            user_home_dir().join(suffix)
        });
    }
    Err(format!("unsupported tilde path form: {path}"))
}

/// Resolve a local media/file path for outbound delivery; returns canonical path when possible.
pub fn resolve_outbound_media_path(input: &str) -> Result<PathBuf, String> {
    let path = resolve_agent_path(input);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    if !canonical.is_file() {
        return Err(format!(
            "Media file not found: '{input}' (resolved: {})",
            canonical.display()
        ));
    }
    Ok(canonical)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::test_env;

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
        // Acquire the workspace-wide env-var lock to prevent races with
        // managed_gateway tests that also mutate HERMES_HOME.
        let _g = crate::managed_gateway::test_lock::lock();

        let _guard = crate::managed_gateway::test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        let original_ultra = std::env::var("HERMES_AGENT_ULTRA_HOME").ok();

        // -- Part 1: derived paths are consistent --
        test_env::set_var("HERMES_HOME", "/tmp/hermes-path-test");
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
        assert_eq!(talk_dir(), home.join("hermes-talk"));
        assert_eq!(talk_config_path(), home.join("hermes-talk/config.toml"));
        assert_eq!(env_path(), home.join(".env"));

        // -- Part 2: explicit home override --
        test_env::set_var("HERMES_HOME", "/tmp/test-hermes");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes"));
        assert_eq!(config_path(), PathBuf::from("/tmp/test-hermes/config.yaml"));

        // -- Part 3: ultra env alias works when HERMES_HOME is absent --
        test_env::remove_var("HERMES_HOME");
        test_env::set_var("HERMES_AGENT_ULTRA_HOME", "/tmp/test-hermes-ultra");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/test-hermes-ultra"));
        assert_eq!(
            config_path(),
            PathBuf::from("/tmp/test-hermes-ultra/config.yaml")
        );
        test_env::remove_var("HERMES_AGENT_ULTRA_HOME");

        // Restore
        match original {
            Some(v) => test_env::set_var("HERMES_HOME", v),
            None => test_env::remove_var("HERMES_HOME"),
        }
        match original_ultra {
            Some(v) => test_env::set_var("HERMES_AGENT_ULTRA_HOME", v),
            None => test_env::remove_var("HERMES_AGENT_ULTRA_HOME"),
        }
    }

    #[test]
    fn resolve_agent_path_expands_tilde() {
        let home = user_home_dir();
        assert_eq!(resolve_agent_path("~/notes.txt"), home.join("notes.txt"));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_agent_path_maps_tmp_prefix_on_windows() {
        let _g = crate::managed_gateway::test_lock::lock();
        let original = std::env::var("HERMES_HOME").ok();
        unsafe {
            std::env::set_var("HERMES_HOME", "/tmp/hermes-path-win-test");
        }

        let mapped = resolve_agent_path("/tmp/memorial.html");
        assert_eq!(
            mapped,
            PathBuf::from("/tmp/hermes-path-win-test/cache/terminal/memorial.html")
        );

        match original {
            Some(v) => unsafe { std::env::set_var("HERMES_HOME", v) },
            None => unsafe { std::env::remove_var("HERMES_HOME") },
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_agent_path_preserves_unix_tmp() {
        assert_eq!(
            resolve_agent_path("/tmp/memorial.html"),
            PathBuf::from("/tmp/memorial.html")
        );
    }

    #[test]
    fn resolve_outbound_media_path_requires_existing_file() {
        let err = resolve_outbound_media_path("/nonexistent/hermes-test-404.bin").unwrap_err();
        assert!(err.contains("Media file not found"));
    }
}
