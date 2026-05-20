//! Test-only helpers for mutating process environment variables.
//!
//! Rust 1.95+ marks `std::env::set_var` / `remove_var` as unsafe because concurrent
//! reads during mutation are undefined behavior. Unit tests that toggle env vars
//! must use these wrappers (typically under an env mutex).

use std::ffi::OsStr;

/// Set an environment variable in tests (`unsafe` centralized here).
pub fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
    // SAFETY: Callers must hold a test-wide env lock when other threads may read env.
    unsafe { std::env::set_var(key, value) };
}

/// Remove an environment variable in tests (`unsafe` centralized here).
pub fn remove_var<K: AsRef<OsStr>>(key: K) {
    // SAFETY: Callers must hold a test-wide env lock when other threads may read env.
    unsafe { std::env::remove_var(key) };
}
