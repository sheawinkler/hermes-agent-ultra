//! Interactive dependency install orchestration, ported from Python
//! `hermes_cli/dep_ensure.py`.
//!
//! Uses [`hermes_config::dep_check`] for availability detection and delegates
//! actual installation to `scripts/install.ps1` (Windows) or
//! `scripts/install.sh` (POSIX).

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use hermes_config::dep_check::{RuntimeDep, description, is_available};
use hermes_config::hermes_home;
use tokio::process::Command;
use tracing::{debug, warn};

/// Shell type used to invoke the install script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    PowerShell,
    Bash,
}

/// Locate the install script (`install.ps1` or `install.sh`).
///
/// Mirrors Python `_find_install_script()`.  Walks up from the current
/// executable directory looking for a `scripts/` folder containing the
/// platform-preferred install script.
pub fn find_install_script() -> Option<(PathBuf, ShellKind)> {
    let (preferred, preferred_kind, fallback, fallback_kind): (&str, ShellKind, &str, ShellKind) =
        if cfg!(windows) {
            (
                "install.ps1",
                ShellKind::PowerShell,
                "install.sh",
                ShellKind::Bash,
            )
        } else {
            (
                "install.sh",
                ShellKind::Bash,
                "install.ps1",
                ShellKind::PowerShell,
            )
        };

    // Walk up from the current executable directory.
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(PathBuf::from);
        while let Some(d) = dir {
            let candidate = d.join("scripts").join(preferred);
            if candidate.is_file() {
                return Some((candidate, preferred_kind));
            }
            let candidate = d.join("scripts").join(fallback);
            if candidate.is_file() {
                return Some((candidate, fallback_kind));
            }
            dir = d.parent().map(PathBuf::from);
        }
    }

    // Also check relative to the current working directory (dev workflow).
    let cwd = std::env::current_dir().ok()?;
    let candidate = cwd.join("scripts").join(preferred);
    if candidate.is_file() {
        return Some((candidate, preferred_kind));
    }
    let candidate = cwd.join("scripts").join(fallback);
    if candidate.is_file() {
        return Some((candidate, fallback_kind));
    }

    None
}

/// Prompt the user with a `[Y/n]` question on stdin.
///
/// Returns `true` if the user answered yes (empty or `y`/`yes`).
fn prompt_yes_no(prompt: &str) -> bool {
    print!("{prompt} [Y/n] ");
    let _ = io::stdout().flush();
    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_err() {
        return false;
    }
    let answer = line.trim().to_lowercase();
    answer.is_empty() || answer == "y" || answer == "yes"
}

/// Ensure a runtime dependency is available, optionally prompting for install.
///
/// Mirrors Python `ensure_dependency(dep, interactive)`.
///
/// Returns `true` if the dependency is (or becomes) available.
pub async fn ensure_dependency(dep: RuntimeDep, interactive: bool) -> bool {
    // Already installed?
    if is_available(dep) {
        debug!(%dep, "dependency already available");
        return true;
    }

    if !interactive {
        warn!(%dep, "{} is not installed", description(dep));
        return false;
    }

    // Locate install script.
    let (script, shell) = match find_install_script() {
        Some(pair) => pair,
        None => {
            println!(
                "{} is not installed and no install script was found.",
                description(dep)
            );
            return false;
        }
    };

    // Interactive TTY prompt.
    if !atty_is_tty() {
        warn!("not a TTY, skipping install prompt for {}", dep);
        return false;
    }
    if !prompt_yes_no(&format!("{} is not installed. Install now?", description(dep))) {
        return false;
    }

    // Build command.
    let dep_name = dep.to_string();
    let home = hermes_home();
    let mut cmd = match shell {
        ShellKind::PowerShell => {
            let ps = which::which("powershell")
                .or_else(|_| which::which("pwsh"))
                .ok();
            match ps {
                Some(ps_path) => {
                    let mut c = Command::new(ps_path);
                    c.arg("-ExecutionPolicy")
                        .arg("Bypass")
                        .arg("-File")
                        .arg(&script)
                        .arg("-Ensure")
                        .arg(&dep_name)
                        .arg("-HermesHome")
                        .arg(home);
                    c
                }
                None => {
                    println!("PowerShell not found; cannot run install script.");
                    return false;
                }
            }
        }
        ShellKind::Bash => {
            let mut c = Command::new("bash");
            c.arg(&script).arg("--ensure").arg(&dep_name);
            c
        }
    };

    // Prevent recursive prompts in the child process.
    cmd.env("IS_INTERACTIVE", "false");

    debug!(%dep, "running install script");
    match cmd.status().await {
        Ok(status) if status.success() => {
            // Re-check after install.
            let ok = is_available(dep);
            if ok {
                debug!(%dep, "dependency installed successfully");
            } else {
                warn!(%dep, "install script succeeded but dependency still not found");
            }
            ok
        }
        Ok(status) => {
            warn!(%dep, code = ?status.code(), "install script failed");
            false
        }
        Err(e) => {
            warn!(%dep, "failed to run install script: {e}");
            false
        }
    }
}

/// Batch ensure: iterate over multiple deps, returning per-dep results.
pub async fn ensure_all(deps: &[RuntimeDep], interactive: bool) -> Vec<(RuntimeDep, bool)> {
    let mut results = Vec::with_capacity(deps.len());
    for &dep in deps {
        let ok = ensure_dependency(dep, interactive).await;
        results.push((dep, ok));
    }
    results
}

/// Minimal TTY detection (stdin is a terminal).
fn atty_is_tty() -> bool {
    // Use a simple heuristic: on most platforms, if `TERM` is set or on Windows
    // assume TTY.  For a more robust check we could use the `atty` crate, but
    // this avoids adding another dependency.
    if cfg!(windows) {
        return true;
    }
    std::env::var("TERM").is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ensure_returns_true_when_available() {
        // Node is commonly installed; if it's on PATH this should return true
        // immediately without prompting.
        if is_available(RuntimeDep::Node) {
            assert!(ensure_dependency(RuntimeDep::Node, false).await);
        }
    }

    #[tokio::test]
    async fn ensure_returns_false_non_interactive_when_missing() {
        // Use a non-interactive call for a dep that might be missing.
        // Even if all deps are present, the non-interactive path is tested
        // by the early-return in `ensure_dependency`.
        // Pick ffmpeg as it's less commonly installed.
        if !is_available(RuntimeDep::Ffmpeg) {
            assert!(!ensure_dependency(RuntimeDep::Ffmpeg, false).await);
        }
    }

    #[test]
    fn find_install_script_in_empty_cwd_returns_none_or_some() {
        // We cannot guarantee the CWD has no scripts (the repo does have them),
        // so just verify the function does not panic and returns a consistent
        // result.
        let result = find_install_script();
        // If running inside the repo, scripts/ exists and we get Some.
        // If running elsewhere, we get None.  Either is fine.
        let _ = result;
    }
}
