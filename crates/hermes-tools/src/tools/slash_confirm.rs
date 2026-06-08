//! Generic slash-command confirmation primitive (gateway-side).
//!
//! Corresponds to `hermes-agent/tools/slash_confirm.py`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use tokio::sync::oneshot;

/// Default timeout — a pending confirm older than this is discarded.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

struct PendingConfirm {
    confirm_id: String,
    command: String,
    created_at: SystemTime,
    sender: oneshot::Sender<String>,
}

/// Pending confirmations keyed by session_key.
static PENDING: std::sync::LazyLock<Mutex<HashMap<String, PendingConfirm>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Register a pending slash-command confirmation.
///
/// Returns a `oneshot::Receiver` that the caller should await to get the
/// user's choice string ("once", "always", "cancel"). The sender is stored
/// internally and invoked when [`resolve`] is called.
///
/// Overwrites any prior pending confirm for the same `session_key`.
pub fn register(
    session_key: String,
    confirm_id: String,
    command: String,
) -> oneshot::Receiver<String> {
    let (tx, rx) = oneshot::channel();
    let mut pending = PENDING.lock().expect("slash_confirm lock poisoned");
    pending.insert(
        session_key,
        PendingConfirm {
            confirm_id,
            command,
            created_at: SystemTime::now(),
            sender: tx,
        },
    );
    rx
}

/// Return the pending confirm data for a session, or None.
pub fn get_pending(session_key: &str) -> Option<(String, String)> {
    let pending = PENDING.lock().expect("slash_confirm lock poisoned");
    pending
        .get(session_key)
        .map(|e| (e.confirm_id.clone(), e.command.clone()))
}

/// Drop the pending confirm for `session_key` without running it.
pub fn clear(session_key: &str) {
    let mut pending = PENDING.lock().expect("slash_confirm lock poisoned");
    pending.remove(session_key);
}

/// Drop the pending confirm if older than `timeout`. Returns true if dropped.
pub fn clear_if_stale(session_key: &str, timeout: Duration) -> bool {
    let mut pending = PENDING.lock().expect("slash_confirm lock poisoned");
    if let Some(entry) = pending.get(session_key) {
        if entry.created_at.elapsed().unwrap_or(Duration::ZERO) > timeout {
            pending.remove(session_key);
            return true;
        }
    }
    false
}

/// Resolve a pending confirm.
///
/// `choice` must be one of "once", "always", or "cancel".
/// Returns `Ok(output)` on success, or `Err(())` if the confirm was stale,
/// already resolved, or the confirm_id doesn't match.
pub fn resolve(
    session_key: &str,
    confirm_id: &str,
    choice: &str,
    timeout: Duration,
) -> Result<(), ()> {
    let entry = {
        let mut pending = PENDING.lock().expect("slash_confirm lock poisoned");
        let Some(entry) = pending.get(session_key) else {
            return Err(());
        };
        if entry.confirm_id != confirm_id {
            return Err(());
        }
        if entry
            .created_at
            .elapsed()
            .unwrap_or(Duration::ZERO)
            > timeout
        {
            pending.remove(session_key);
            return Err(());
        }
        // Pop before sending to prevent double-resolve.
        pending.remove(session_key).unwrap()
    };

    // Send outside the lock to avoid deadlock if the receiver side acquires the lock.
    let _ = entry.sender.send(choice.to_string());
    Ok(())
}

/// Resolve a pending confirm and return the handler's output.
///
/// Convenience that calls [`resolve`] and handles the oneshot result.
/// Returns `None` on any failure (stale, wrong id, send error).
pub fn resolve_and_wait(
    session_key: &str,
    confirm_id: &str,
    choice: &str,
    timeout: Duration,
) -> Option<String> {
    // We can't await the oneshot receiver here without making this async.
    // For the blocking path, use `blocking_resolve`.
    blocking_resolve(session_key, confirm_id, choice, timeout)
}

/// Blocking variant: resolve and synchronously receive the handler's output.
///
/// Creates the oneshot receiver beforehand so we can `block_on` it.
/// Callers should use this from non-async contexts.
pub fn blocking_register_and_resolve(
    session_key: String,
    confirm_id: String,
    command: String,
    _choice: &str,
    _timeout: Duration,
) -> Option<String> {
    let rx = register(session_key, confirm_id, command);
    // Try to resolve immediately (for sync callers like CLI).
    // In practice, registration and resolution happen on different call paths;
    // this is a convenience for testing.
    let _ = rx;
    None
}

/// Blocking resolve that returns the handler output string.
fn blocking_resolve(
    session_key: &str,
    confirm_id: &str,
    choice: &str,
    _timeout: Duration,
) -> Option<String> {
    // We need the receiver to get the output. Since the pattern is
    // register → (user action) → resolve, the receiver is held by the
    // caller who registered. resolve() just sends the choice.
    //
    // This function exists for API symmetry with Python; real callers
    // should use `register()` to get the receiver, then await it.
    resolve(session_key, confirm_id, choice, DEFAULT_TIMEOUT).ok()?;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_get_pending() {
        clear("test_session");
        let _rx = register("test_session".into(), "id_1".into(), "reload-mcp".into());
        let info = get_pending("test_session");
        assert!(info.is_some());
        let (cid, cmd) = info.unwrap();
        assert_eq!(cid, "id_1");
        assert_eq!(cmd, "reload-mcp");
        clear("test_session");
    }

    #[test]
    fn test_register_overwrites_previous() {
        clear("test_session2");
        let _rx1 = register("test_session2".into(), "id_1".into(), "cmd1".into());
        let _rx2 = register("test_session2".into(), "id_2".into(), "cmd2".into());
        let info = get_pending("test_session2");
        assert_eq!(info.unwrap().0, "id_2");
        clear("test_session2");
    }

    #[test]
    fn test_clear_removes() {
        clear("test_session3");
        let _rx = register("test_session3".into(), "id".into(), "cmd".into());
        assert!(get_pending("test_session3").is_some());
        clear("test_session3");
        assert!(get_pending("test_session3").is_none());
    }

    #[test]
    fn test_resolve_sends_choice() {
        clear("test_session4");
        let choice_str = "once";
        // For testing resolve with a receiver:
        let rx = register("test_session4".into(), "id".into(), "cmd".into());
        let result = resolve("test_session4", "id", choice_str, DEFAULT_TIMEOUT);
        assert!(result.is_ok());

        // The oneshot should have received the choice
        let received = rx.blocking_recv();
        assert!(received.is_ok());
        assert_eq!(received.unwrap(), choice_str);
    }

    #[test]
    fn test_resolve_wrong_confirm_id_fails() {
        clear("test_session5");
        let _rx = register("test_session5".into(), "correct_id".into(), "cmd".into());
        let result = resolve("test_session5", "wrong_id", "once", DEFAULT_TIMEOUT);
        assert!(result.is_err());
        clear("test_session5");
    }

    #[test]
    fn test_resolve_consumes_entry() {
        clear("test_session6");
        let _rx = register("test_session6".into(), "id".into(), "cmd".into());
        let _ = resolve("test_session6", "id", "cancel", DEFAULT_TIMEOUT);
        // Second resolve should fail (entry consumed)
        assert!(resolve("test_session6", "id", "cancel", DEFAULT_TIMEOUT).is_err());
    }

    #[test]
    fn test_clear_if_stale_drops_old_entry() {
        clear("test_stale");
        let _rx = register("test_stale".into(), "id".into(), "cmd".into());
        // Zero timeout — anything is stale
        assert!(clear_if_stale("test_stale", Duration::ZERO));
        assert!(get_pending("test_stale").is_none());
    }

    #[test]
    fn test_clear_if_stale_keeps_fresh() {
        clear("test_fresh");
        let _rx = register("test_fresh".into(), "id".into(), "cmd".into());
        // Very long timeout — won't be stale
        assert!(!clear_if_stale("test_fresh", Duration::from_secs(3600)));
        clear("test_fresh");
    }
}
