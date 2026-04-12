//! Interrupt mechanism for the agent loop.
//!
//! Provides a thread-safe interrupt controller that allows external threads
//! (TUI input handler, message receiver, Ctrl+C signal handler) to request
//! the agent to stop its current tool-calling loop and optionally redirect
//! to a new message.
//!
//! Corresponds to Python `run_agent.py`'s `interrupt()` / `clear_interrupt()`
//! / `_interrupt_requested` system.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hermes_core::AgentError;

/// Thread-safe interrupt controller for the agent loop.
///
/// Clone-friendly: all clones share the same underlying state via `Arc`.
///
/// # Usage
///
/// ```rust,ignore
/// let ctrl = InterruptController::new();
///
/// // From another thread (e.g. Ctrl+C handler):
/// ctrl.interrupt(Some("New user message".into()));
///
/// // In the agent loop, before each LLM call / tool execution:
/// ctrl.check_interrupt()?;
/// ```
#[derive(Debug, Clone)]
pub struct InterruptController {
    /// Atomic flag — set to `true` when an interrupt is requested.
    flag: Arc<AtomicBool>,
    /// Optional redirect message that triggered the interrupt.
    redirect_message: Arc<Mutex<Option<String>>>,
}

impl InterruptController {
    /// Create a new interrupt controller in the non-interrupted state.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            redirect_message: Arc::new(Mutex::new(None)),
        }
    }

    /// Request the agent to interrupt its current loop.
    ///
    /// Optionally provide a redirect message (e.g. new user input that
    /// should be processed instead of continuing the current tool chain).
    pub fn interrupt(&self, message: Option<String>) {
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = message;
        }
        // Set the flag AFTER storing the message so readers always see
        // a consistent (flag=true, message=Some) pair.
        self.flag.store(true, Ordering::Release);
    }

    /// Clear any pending interrupt request.
    pub fn clear_interrupt(&self) {
        self.flag.store(false, Ordering::Release);
        if let Ok(mut guard) = self.redirect_message.lock() {
            *guard = None;
        }
    }

    /// Check whether an interrupt has been requested.
    pub fn is_interrupted(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Take the redirect message (if any), leaving `None` in its place.
    pub fn take_redirect_message(&self) -> Option<String> {
        if let Ok(mut guard) = self.redirect_message.lock() {
            guard.take()
        } else {
            None
        }
    }

    /// Check the interrupt flag and return `Err(AgentError::Interrupted)`
    /// if set. This is the primary check point used inside the agent loop
    /// before each LLM call and tool execution.
    pub fn check_interrupt(&self) -> Result<(), AgentError> {
        if self.is_interrupted() {
            let message = self.take_redirect_message();
            Err(AgentError::Interrupted { message })
        } else {
            Ok(())
        }
    }
}

impl Default for InterruptController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_controller_is_not_interrupted() {
        let ctrl = InterruptController::new();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_interrupt_sets_flag() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(None);
        assert!(ctrl.is_interrupted());
    }

    #[test]
    fn test_interrupt_with_message() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("new input".into()));
        assert!(ctrl.is_interrupted());

        match ctrl.check_interrupt() {
            Err(AgentError::Interrupted { message }) => {
                assert_eq!(message.as_deref(), Some("new input"));
            }
            other => panic!("expected Interrupted, got {:?}", other),
        }
    }

    #[test]
    fn test_clear_interrupt() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("msg".into()));
        assert!(ctrl.is_interrupted());

        ctrl.clear_interrupt();
        assert!(!ctrl.is_interrupted());
        assert!(ctrl.check_interrupt().is_ok());
    }

    #[test]
    fn test_take_redirect_message_consumes() {
        let ctrl = InterruptController::new();
        ctrl.interrupt(Some("hello".into()));

        let msg = ctrl.take_redirect_message();
        assert_eq!(msg.as_deref(), Some("hello"));

        // Second take returns None
        let msg2 = ctrl.take_redirect_message();
        assert!(msg2.is_none());
    }

    #[test]
    fn test_clone_shares_state() {
        let ctrl1 = InterruptController::new();
        let ctrl2 = ctrl1.clone();

        ctrl1.interrupt(Some("shared".into()));
        assert!(ctrl2.is_interrupted());
    }
}
