//! Runtime dependency detection, ported from Python `hermes_cli/dep_ensure.py`.
//!
//! Provides pure, synchronous checks for non-Python tools that hermes
//! optionally relies on: Node.js, a browser engine, ripgrep, and ffmpeg.

use std::path::PathBuf;

use crate::paths::hermes_home;

/// Non-Python runtime dependencies that hermes may need.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuntimeDep {
    /// Node.js — required for browser tools and TUI.
    Node,
    /// Browser engine (Chromium) — for web browsing tools.
    Browser,
    /// ripgrep (`rg`) — fast file search.
    Ripgrep,
    /// ffmpeg — TTS voice messages, video frame sampling.
    Ffmpeg,
}

impl std::fmt::Display for RuntimeDep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Node => "node",
            Self::Browser => "browser",
            Self::Ripgrep => "ripgrep",
            Self::Ffmpeg => "ffmpeg",
        })
    }
}

/// All known runtime dependencies.
pub fn all_deps() -> &'static [RuntimeDep] {
    &[
        RuntimeDep::Node,
        RuntimeDep::Browser,
        RuntimeDep::Ripgrep,
        RuntimeDep::Ffmpeg,
    ]
}

/// Human-readable description matching the Python `_DEP_DESCRIPTIONS`.
pub fn description(dep: RuntimeDep) -> &'static str {
    match dep {
        RuntimeDep::Node => "Node.js (required for browser tools and TUI)",
        RuntimeDep::Browser => "Browser engine (Chromium, for web browsing tools)",
        RuntimeDep::Ripgrep => "ripgrep (fast file search)",
        RuntimeDep::Ffmpeg => "ffmpeg (TTS voice messages)",
    }
}

/// Extra PATH directories under [`hermes_home`] where managed tools may live.
pub fn supplemental_path_entries() -> Vec<PathBuf> {
    let home = hermes_home();
    let candidates = [
        home.join("bin"),
        home.join("node").join("bin"),
        home.join("tools").join("bin"),
    ];
    candidates
        .into_iter()
        .filter(|path| path.is_dir())
        .collect()
}

fn managed_binary(home: &std::path::Path, name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        home.join(format!("{name}.exe"))
    }
    #[cfg(not(windows))]
    {
        home.join(name)
    }
}

fn is_on_path_or_managed(name: &str, managed_dirs: &[PathBuf]) -> bool {
    if which::which(name).is_ok() {
        return true;
    }
    managed_dirs
        .iter()
        .any(|dir| managed_binary(dir, name).is_file())
}

/// Check whether a runtime dependency is available on the current system.
///
/// Mirrors Python `_DEP_CHECKS[dep]()`, extended with Hermes-managed install dirs.
pub fn is_available(dep: RuntimeDep) -> bool {
    let managed = supplemental_path_entries();
    match dep {
        RuntimeDep::Node => is_on_path_or_managed("node", &managed),
        RuntimeDep::Browser => {
            which::which("agent-browser").is_ok()
                || has_system_browser()
                || has_hermes_agent_browser()
        }
        RuntimeDep::Ripgrep => is_on_path_or_managed("rg", &managed),
        RuntimeDep::Ffmpeg => is_on_path_or_managed("ffmpeg", &managed),
    }
}

/// Return deps that are not currently available (for startup diagnostics).
pub fn missing_deps(deps: &[RuntimeDep]) -> Vec<RuntimeDep> {
    deps.iter()
        .copied()
        .filter(|dep| !is_available(*dep))
        .collect()
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Check for a system-installed browser (Chrome / Edge / Chromium).
///
/// First checks PATH (mirrors Python `_has_system_browser()`), then on Windows
/// also probes common Program Files install locations — browsers are rarely
/// added to PATH on Windows.
fn has_system_browser() -> bool {
    let candidates: &[&str] = if cfg!(windows) {
        &["chrome", "msedge", "chromium"]
    } else {
        &[
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "chrome",
        ]
    };

    if candidates.iter().any(|name| which::which(name).is_ok()) {
        return true;
    }

    // Windows fallback: check Program Files directly.
    #[cfg(windows)]
    {
        const WINDOWS_BROWSER_PATHS: &[&str] = &[
            r"Google\Chrome\Application\chrome.exe",
            r"Microsoft\Edge\Application\msedge.exe",
            r"Chromium\Application\chrome.exe",
        ];
        let roots = [
            std::env::var_os("ProgramFiles").map(PathBuf::from),
            std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
            std::env::var_os("LOCALAPPDATA").map(|v| PathBuf::from(v).join("Programs")),
        ];
        for root in roots.into_iter().flatten() {
            for rel in WINDOWS_BROWSER_PATHS {
                if root.join(rel).is_file() {
                    return true;
                }
            }
        }
    }

    false
}

/// Check for the Hermes-managed `agent-browser` installed via npm under
/// `$HERMES_HOME/node/`.
///
/// Mirrors Python `_has_hermes_agent_browser()`.
fn has_hermes_agent_browser() -> bool {
    let home = hermes_home();
    let candidate: PathBuf = if cfg!(windows) {
        home.join("node").join("bin").join("agent-browser.cmd")
    } else {
        home.join("node").join("bin").join("agent-browser")
    };
    candidate.is_file()
}

// ---------------------------------------------------------------------------
// Tests — see `tests/dep_check_tests.rs` (integration test, separate
// compilation unit, avoids pre-existing `unsafe set_var` issues in sibling
// modules).
// ---------------------------------------------------------------------------
