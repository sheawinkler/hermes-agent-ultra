//! Interactive dependency install orchestration
//! mirrors python `hermes_cli/dep_ensure.py`.
//!
//! Uses [`hermes_config::dep_check`] for availability detection and
//! [`crate::runtime_dep_install`] for silent installs.

use std::io::{self, BufRead, Write};

use hermes_config::dep_check::{RuntimeDep, description, is_available};
use tracing::{debug, warn};

use crate::runtime_dep_install::{auto_ensure_enabled, ensure_runtime_dep};

/// Parse a runtime dependency name (`ffmpeg`, `node`, ...).
pub fn parse_runtime_dep_name(name: &str) -> Option<RuntimeDep> {
    match name.trim().to_ascii_lowercase().as_str() {
        "node" => Some(RuntimeDep::Node),
        "browser" => Some(RuntimeDep::Browser),
        "ripgrep" | "rg" => Some(RuntimeDep::Ripgrep),
        "ffmpeg" => Some(RuntimeDep::Ffmpeg),
        _ => None,
    }
}

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
pub async fn ensure_dependency(dep: RuntimeDep, interactive: bool) -> bool {
    if is_available(dep) {
        debug!(%dep, "dependency already available");
        return true;
    }

    if !interactive {
        if auto_ensure_enabled() {
            return ensure_runtime_dep(dep, true).await;
        }
        warn!(%dep, "{} is not installed", description(dep));
        return false;
    }

    if !atty_is_tty() {
        warn!("not a TTY, skipping install prompt for {}", dep);
        return false;
    }
    if !prompt_yes_no(&format!(
        "{} is not installed. Install now?",
        description(dep)
    )) {
        return false;
    }

    ensure_runtime_dep(dep, false).await
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

fn atty_is_tty() -> bool {
    if cfg!(windows) {
        return true;
    }
    std::env::var("TERM").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_runtime_dep_names() {
        assert_eq!(parse_runtime_dep_name("ffmpeg"), Some(RuntimeDep::Ffmpeg));
        assert_eq!(parse_runtime_dep_name("rg"), Some(RuntimeDep::Ripgrep));
        assert_eq!(parse_runtime_dep_name("unknown"), None);
    }

    #[tokio::test]
    async fn ensure_returns_true_when_available() {
        if is_available(RuntimeDep::Node) {
            assert!(ensure_dependency(RuntimeDep::Node, false).await);
        }
    }
}
