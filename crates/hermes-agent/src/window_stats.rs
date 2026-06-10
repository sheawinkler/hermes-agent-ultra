//! Simple sliding-window statistics helpers.
//!
//! Extracted from `agent_loop.rs` — these are independent utility
//! functions used by the governor and conversation loop.

use std::collections::VecDeque;

/// Push a `u64` value onto a sliding window, trimming to `limit`.
pub(crate) fn push_window_u64(window: &mut VecDeque<u64>, value: u64, limit: usize) {
    window.push_back(value);
    while window.len() > limit {
        let _ = window.pop_front();
    }
}

/// Push an `f64` value onto a sliding window, trimming to `limit`.
pub(crate) fn push_window_f64(window: &mut VecDeque<f64>, value: f64, limit: usize) {
    window.push_back(value);
    while window.len() > limit {
        let _ = window.pop_front();
    }
}
