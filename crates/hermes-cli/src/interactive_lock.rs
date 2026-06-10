//! Interactive session lock — prevents multiple interactive Hermes sessions
//! from running concurrently against the same state directory.
//!
//! On Unix, stale locks held by orphaned (ppid=1, no tty) processes are
//! automatically reaped. On Windows, orphan reaping is not supported, so
//! stale locks must be removed manually.
//!
//! ## Design
//!
//! Uses `create_new` for atomic lock file acquisition (O_CREAT | O_EXCL on
//! Unix, `CREATE_NEW` on Windows). The lock file contains the holder's PID
//! as a plain integer or a JSON object `{"pid": …}`. On drop, the lock is
//! released only if the PID stored in the file still matches our PID.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use hermes_core::AgentError;

/// Name of the lock file inside the Hermes state directory.
pub(crate) const LOCK_FILE_NAME: &str = "interactive.session.lock";

/// Environment variable that, when set to a truthy value, bypasses the
/// interactive session lock entirely.
pub(crate) const BYPASS_ENV: &str = "HERMES_ALLOW_PARALLEL_INTERACTIVE";

// ---------------------------------------------------------------------------
// PID reading
// ---------------------------------------------------------------------------

/// Read the PID stored in a lock file. Supports plain integer and
/// `{"pid": …}` JSON formats.
pub(crate) fn read_lock_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return Some(pid);
    }
    let json: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = json.get("pid")?.as_u64()?;
    u32::try_from(pid).ok()
}

// ---------------------------------------------------------------------------
// Cross-platform PID aliveness check
// ---------------------------------------------------------------------------

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(not(unix))]
fn pid_is_alive(_pid: u32) -> bool {
    false
}

// ---------------------------------------------------------------------------
// Unix orphan reaping (ps-based process inspection + SIGTERM/SIGKILL)
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[derive(Debug, Clone)]
struct PidSnapshot {
    ppid: u32,
    tty: String,
    command: String,
}

#[cfg(unix)]
fn parse_snapshot_line(line: &str) -> Option<PidSnapshot> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let ppid = parts.next()?.parse::<u32>().ok()?;
    let tty = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    if command.is_empty() {
        return None;
    }
    Some(PidSnapshot { ppid, tty, command })
}

#[cfg(unix)]
fn pid_snapshot(pid: u32) -> Option<PidSnapshot> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid=,tty=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8_lossy(&output.stdout);
    parse_snapshot_line(line.as_ref())
}

#[cfg(unix)]
fn looks_like_interactive_hermes(command: &str) -> bool {
    let cmd = command.to_ascii_lowercase();
    (cmd.contains("hermes-agent-ultra") || cmd.contains("hermes-ultra")) && !cmd.contains("gateway")
}

#[cfg(unix)]
fn is_reapable_orphan(pid: u32) -> bool {
    let snapshot = match pid_snapshot(pid) {
        Some(s) => s,
        None => return false,
    };
    // Reap only obvious abandoned interactive agents:
    // orphaned from shell (ppid=1) and detached from a terminal.
    looks_like_interactive_hermes(&snapshot.command)
        && snapshot.ppid == 1
        && (snapshot.tty == "??" || snapshot.tty == "?")
}

#[cfg(unix)]
fn reap_orphan(pid: u32) -> bool {
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    std::thread::sleep(std::time::Duration::from_millis(250));
    if !pid_is_alive(pid) {
        return true;
    }
    let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    std::thread::sleep(std::time::Duration::from_millis(150));
    !pid_is_alive(pid)
}

#[cfg(unix)]
fn reap_orphans_except(own_pid: u32) -> usize {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return 0,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut reaped = 0usize;
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|p| p.parse::<u32>().ok()) else {
            continue;
        };
        if pid == own_pid || ppid != 1 {
            continue;
        }
        let command = parts.collect::<Vec<_>>().join(" ");
        if looks_like_interactive_hermes(&command) && reap_orphan(pid) {
            reaped = reaped.saturating_add(1);
        }
    }
    reaped
}

// ---------------------------------------------------------------------------
// Lock guard
// ---------------------------------------------------------------------------

/// RAII guard for an interactive session lock file.
///
/// Acquired atomically via `create_new`. Released on drop (removes the lock
/// file only if the PID stored in it still matches ours).
pub(crate) struct InteractiveSessionLockGuard {
    lock_path: PathBuf,
    pid: u32,
    _lock_file: std::fs::File,
}

impl InteractiveSessionLockGuard {
    /// Try to acquire the interactive session lock.
    ///
    /// Returns `Ok(None)` when `HERMES_ALLOW_PARALLEL_INTERACTIVE` is set.
    /// Returns `Ok(Some(guard))` on success. Returns `Err` when another live
    /// interactive session holds the lock (and it could not be reaped as an
    /// orphan).
    pub(crate) fn acquire(state_root: &Path) -> Result<Option<Self>, AgentError> {
        if hermes_config::env_var_enabled(BYPASS_ENV) {
            return Ok(None);
        }

        let lock_path = state_root.join(LOCK_FILE_NAME);

        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!(
                    "failed to create lock parent {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let own_pid = std::process::id();

        #[cfg(unix)]
        {
            let _ = reap_orphans_except(own_pid);
        }

        // Use create_new for atomic lock acquisition. This closes the race where
        // two interactive sessions read "no lock" and both write concurrently.
        let lock_file = loop {
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(file) => break file,
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(existing_pid) = read_lock_pid(&lock_path) {
                        if existing_pid != own_pid && pid_is_alive(existing_pid) {
                            #[cfg(unix)]
                            {
                                if is_reapable_orphan(existing_pid) && reap_orphan(existing_pid) {
                                    let _ = std::fs::remove_file(&lock_path);
                                    continue;
                                }
                            }
                            return Err(AgentError::Config(format!(
                                "Another Hermes interactive session is running (PID {}). \
                                 Close it first or set {}=1 to allow parallel sessions.",
                                existing_pid, BYPASS_ENV
                            )));
                        }
                    }
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                Err(err) => {
                    return Err(AgentError::Io(format!(
                        "failed to create interactive lock {}: {}",
                        lock_path.display(),
                        err
                    )));
                }
            }
        };

        let mut lock_file = lock_file;
        lock_file
            .write_all(format!("{}\n", own_pid).as_bytes())
            .map_err(|e| {
                AgentError::Io(format!(
                    "failed to write interactive lock {}: {}",
                    lock_path.display(),
                    e
                ))
            })?;
        let _ = lock_file.flush();

        Ok(Some(Self {
            lock_path,
            pid: own_pid,
            _lock_file: lock_file,
        }))
    }
}

impl Drop for InteractiveSessionLockGuard {
    fn drop(&mut self) {
        if let Some(current_pid) = read_lock_pid(&self.lock_path) {
            if current_pid == self.pid {
                let _ = std::fs::remove_file(&self.lock_path);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    static ENV_LOCK: OnceLock<()> = OnceLock::new();

    #[test]
    fn read_lock_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("interactive.lock");
        std::fs::write(&plain, "12345\n").expect("write plain lock");
        assert_eq!(read_lock_pid(&plain), Some(12345));

        let json = tmp.path().join("interactive.json");
        std::fs::write(&json, r#"{"pid":23456}"#).expect("write json lock");
        assert_eq!(read_lock_pid(&json), Some(23456));
    }

    #[test]
    fn interactive_session_lock_guard_replaces_stale_pid_and_cleans_up() {
        let _lock = ENV_LOCK.get_or_init(|| ());
        let old_bypass = std::env::var_os(BYPASS_ENV);
        hermes_cli::env_vars::remove_var(BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_root = tmp.path().to_path_buf();
        let lock_path = state_root.join(LOCK_FILE_NAME);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        std::fs::write(&lock_path, "999999").expect("write stale lock");
        let guard = InteractiveSessionLockGuard::acquire(&state_root)
            .expect("acquire lock")
            .expect("guard enabled");
        assert_eq!(read_lock_pid(&lock_path), Some(std::process::id()));
        drop(guard);
        assert!(!lock_path.exists(), "lock file should be removed on drop");
        if let Some(value) = old_bypass {
            hermes_cli::env_vars::set_var(BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn interactive_session_lock_guard_rejects_live_pid() {
        let _lock = ENV_LOCK.get_or_init(|| ());
        let old_bypass = std::env::var_os(BYPASS_ENV);
        hermes_cli::env_vars::remove_var(BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_root = tmp.path().to_path_buf();
        let lock_path = state_root.join(LOCK_FILE_NAME);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        // PID 1 should always be alive on Unix systems.
        std::fs::write(&lock_path, "1").expect("write lock");
        let err = match InteractiveSessionLockGuard::acquire(&state_root) {
            Err(err) => err,
            Ok(_) => panic!("must reject live lock holder"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Another Hermes interactive session is running"));
        assert_eq!(read_lock_pid(&lock_path), Some(1));
        if let Some(value) = old_bypass {
            hermes_cli::env_vars::set_var(BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_snapshot_line_parses_ppid_tty_and_command() {
        let snap = parse_snapshot_line("1 ?? /Users/user/.cargo/bin/hermes-agent-ultra")
            .expect("snapshot");
        assert_eq!(snap.ppid, 1);
        assert_eq!(snap.tty, "??");
        assert!(snap.command.contains("hermes-agent-ultra"));
    }

    #[cfg(unix)]
    #[test]
    fn looks_like_interactive_hermes_matches_cli_and_not_gateway() {
        assert!(looks_like_interactive_hermes(
            "/Users/user/.cargo/bin/hermes-agent-ultra"
        ));
        assert!(looks_like_interactive_hermes("hermes-ultra"));
        assert!(!looks_like_interactive_hermes(
            "/Users/user/.cargo/bin/hermes-gateway"
        ));
    }
}
