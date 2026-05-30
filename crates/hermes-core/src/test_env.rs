//! Safe wrappers for mutating process environment variables in tests.
//!
//! Rust 1.95+ marks [`std::env::set_var`] / [`remove_var`] as `unsafe` because
//! concurrent reads during mutation are undefined behavior.

use std::ffi::OsStr;

/// Set a process environment variable (see [`std::env::set_var`]).
#[inline]
pub fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
    // SAFETY: Test code should serialize env mutation (e.g. via `test_lock`).
    unsafe { std::env::set_var(key, value) };
}

/// Remove a process environment variable (see [`std::env::remove_var`]).
#[inline]
pub fn remove_var<K: AsRef<OsStr>>(key: K) {
    // SAFETY: Same as [`set_var`].
    unsafe { std::env::remove_var(key) };
}
