//! Process environment helpers.
//!
//! `std::env::set_var` / `remove_var` are `unsafe` from Rust 1.95 onward because
//! mutating the environment is not thread-safe with concurrent readers.

use std::ffi::OsStr;

/// Set a process environment variable (see [`std::env::set_var`]).
// SAFETY: Hermes CLI runs env mutation from the main thread during startup
// or under `test_env_lock`; concurrent `std::env::var` during set is rare.
#[inline]
pub fn set_var<K: AsRef<OsStr>, V: AsRef<OsStr>>(key: K, value: V) {
    unsafe {
        std::env::set_var(key, value);
    }
}

/// Remove a process environment variable (see [`std::env::remove_var`]).
// SAFETY: Same as [`set_var`].
#[inline]
pub fn remove_var<K: AsRef<OsStr>>(key: K) {
    unsafe {
        std::env::remove_var(key);
    }
}
