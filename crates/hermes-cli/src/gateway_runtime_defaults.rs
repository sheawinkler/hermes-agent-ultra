//! Runtime defaults applied when the gateway starts.
//!
//! Values are set only when the corresponding environment variable is unset,
//! so explicit user configuration always wins.

use std::path::MAIN_SEPARATOR;

/// Set an environment variable if it is not already defined.
pub fn set_var_if_unset(key: &str, value: &str) {
    if std::env::var_os(key).is_none() {
        // SAFETY: Gateway startup is single-threaded before worker pools fan out.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Prepend Hermes-managed tool directories to the process `PATH`.
pub fn prepend_hermes_tool_path_to_process() {
    let supplemental = hermes_config::dep_supplemental_path_entries();
    if supplemental.is_empty() {
        return;
    }
    let current = std::env::var_os("PATH").map(|p| p.to_string_lossy().into_owned());
    let mut parts: Vec<String> = supplemental
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if let Some(current) = current.filter(|p| !p.trim().is_empty()) {
        parts.push(current);
    }
    let joined = parts.join(&MAIN_SEPARATOR.to_string());
    // SAFETY: Gateway startup is single-threaded before worker pools fan out.
    unsafe { std::env::set_var("PATH", joined) };
}

/// Apply gateway-friendly defaults for browser and web tooling.
pub fn apply_gateway_runtime_defaults() {
    prepend_hermes_tool_path_to_process();
    set_var_if_unset("HERMES_BROWSER_AUTO_START", "1");
    set_var_if_unset("HERMES_DDGS_BACKENDS", "lite,html,yandex,mojeek");
    set_var_if_unset("HERMES_DDGS_TIMEOUT_SECS", "8");
    set_var_if_unset("HERMES_DDGS_REGION", "cn-zh");
    set_var_if_unset("HERMES_TOOL_PROGRESS_INITIAL_DELAY_MS", "4000");
    set_var_if_unset("HERMES_TOOL_PROGRESS_INTERVAL_MS", "15000");
    // Per-tool run budgets default in hermes-agent `web_tool_budget` (browser=2, extract=5, search=2).
    // Optional aggregate backstop: set HERMES_WEB_TOOL_BUDGET_MAX_CALLS explicitly if needed.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_test_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn apply_sets_browser_auto_start_when_unset() {
        let _guard = env_test_lock();
        let key = "HERMES_BROWSER_AUTO_START";
        let prior = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        apply_gateway_runtime_defaults();
        assert_eq!(
            std::env::var(key).ok().as_deref(),
            Some("1"),
            "expected browser auto-start default"
        );
        unsafe { std::env::remove_var(key) };
        if let Some(v) = prior {
            unsafe { std::env::set_var(key, v) };
        }
    }

    #[test]
    fn apply_does_not_override_existing_browser_auto_start() {
        let _guard = env_test_lock();
        let key = "HERMES_BROWSER_AUTO_START";
        let prior = std::env::var_os(key);
        unsafe { std::env::set_var(key, "0") };
        apply_gateway_runtime_defaults();
        assert_eq!(std::env::var(key).ok().as_deref(), Some("0"));
        unsafe { std::env::remove_var(key) };
        if let Some(v) = prior {
            unsafe { std::env::set_var(key, v) };
        } else {
            unsafe { std::env::remove_var(key) };
        }
    }
}
