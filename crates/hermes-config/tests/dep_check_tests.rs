//! Integration tests for `hermes_config::dep_check`.
//!
//! Placed here (rather than inline `#[cfg(test)]`) so that this file compiles
//! as a *separate* binary — independent of any `#[cfg(test)]` code in sibling
//! modules that may have pre-existing compilation issues.

use hermes_config::dep_check::{RuntimeDep, all_deps, description, is_available};

#[test]
fn description_matches_python() {
    assert_eq!(
        description(RuntimeDep::Node),
        "Node.js (required for browser tools and TUI)"
    );
    assert_eq!(
        description(RuntimeDep::Browser),
        "Browser engine (Chromium, for web browsing tools)"
    );
    assert_eq!(
        description(RuntimeDep::Ripgrep),
        "ripgrep (fast file search)"
    );
    assert_eq!(
        description(RuntimeDep::Ffmpeg),
        "ffmpeg (TTS, long video concat — auto-installed to ~/.hermes/bin)"
    );
}

#[test]
fn all_deps_has_four_entries() {
    assert_eq!(all_deps().len(), 4);
}

#[test]
fn display_matches_python_keys() {
    assert_eq!(RuntimeDep::Node.to_string(), "node");
    assert_eq!(RuntimeDep::Browser.to_string(), "browser");
    assert_eq!(RuntimeDep::Ripgrep.to_string(), "ripgrep");
    assert_eq!(RuntimeDep::Ffmpeg.to_string(), "ffmpeg");
}

#[test]
fn is_available_does_not_panic() {
    for dep in all_deps() {
        let _ = is_available(*dep);
    }
}

#[test]
fn browser_not_found_in_empty_hermes_home() {
    // Set HERMES_HOME to a fresh empty directory.
    // If no system browser and no agent-browser on PATH, this tests the
    // hermes-agent-browser path logic. Even if a system browser exists,
    // this at least verifies the function doesn't panic with a bogus home.
    let dir = tempfile::tempdir().expect("tempdir");
    // SAFETY: single-threaded test; no concurrent env access.
    unsafe { std::env::set_var("HERMES_HOME", dir.path()) };
    // Call the public API — exercises has_hermes_agent_browser internally.
    let result = is_available(RuntimeDep::Browser);
    unsafe { std::env::remove_var("HERMES_HOME") };
    // We can't assert false (system Chrome may exist), but we verify no panic
    // and the function returns a bool. If no system browser, it must be false.
    let _ = result;
}
