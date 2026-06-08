//! Per-thread interrupt signaling for all tools.
//!
//! Provides thread-scoped interrupt tracking so that interrupting one agent
//! session does not kill tools running in other sessions.
//!
//! Corresponds to `hermes-agent/tools/interrupt.py`.
//!
//! Usage in tools:
//! ```ignore
//! use hermes_tools::tools::interrupt::is_interrupted;
//! if is_interrupted() {
//!     return serde_json::json!({"output": "[interrupted]", "returncode": 130});
//! }
//! ```

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};
use std::thread::ThreadId;

/// Set of thread ids that have been interrupted.
static INTERRUPTED_THREADS: LazyLock<Mutex<HashSet<ThreadId>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

/// Set or clear interrupt for a specific thread.
///
/// When `thread_id` is `None`, targets the current thread.
pub fn set_interrupt(active: bool, thread_id: Option<ThreadId>) {
    let tid = thread_id.unwrap_or_else(|| std::thread::current().id());
    let mut set = INTERRUPTED_THREADS.lock().expect("interrupt lock poisoned");
    if active {
        set.insert(tid);
    } else {
        set.remove(&tid);
    }
}

/// Check if an interrupt has been requested for the current thread.
///
/// Safe to call from any thread — each thread only sees its own interrupt state.
pub fn is_interrupted() -> bool {
    let tid = std::thread::current().id();
    INTERRUPTED_THREADS
        .lock()
        .expect("interrupt lock poisoned")
        .contains(&tid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_not_interrupted_initially() {
        assert!(!is_interrupted());
    }

    #[test]
    fn test_set_and_check_interrupt() {
        set_interrupt(true, None);
        assert!(is_interrupted());
        set_interrupt(false, None);
        assert!(!is_interrupted());
    }

    #[test]
    fn test_clear_interrupt() {
        set_interrupt(true, None);
        assert!(is_interrupted());
        set_interrupt(false, None);
        assert!(!is_interrupted());
        // Clearing again is a no-op
        set_interrupt(false, None);
        assert!(!is_interrupted());
    }

    #[test]
    fn test_target_specific_thread() {
        let child_tid = std::thread::spawn(|| {
            let tid = std::thread::current().id();
            set_interrupt(true, Some(tid));
            assert!(is_interrupted());
            set_interrupt(false, Some(tid));
            assert!(!is_interrupted());
            tid
        })
        .join()
        .expect("child thread");

        // Main thread was not affected
        assert!(!is_interrupted());
        // Can still set interrupt on another thread externally
        set_interrupt(true, Some(child_tid));
        set_interrupt(false, Some(child_tid));
    }

    #[test]
    fn test_interrupt_isolation_between_threads() {
        let handle = std::thread::spawn(|| {
            set_interrupt(true, None);
            assert!(is_interrupted());
        });
        handle.join().expect("child thread");

        // Main thread should be unaffected
        assert!(!is_interrupted());
    }
}
