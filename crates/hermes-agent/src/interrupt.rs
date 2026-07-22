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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterruptCause {
    Cancellation,
    Timeout,
    Steer,
}

impl InterruptCause {
    const fn is_fatal(self) -> bool {
        matches!(self, Self::Cancellation | Self::Timeout)
    }
}

#[derive(Debug)]
struct PendingInterrupt {
    cause: InterruptCause,
    message: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InterruptController {
    /// Atomic flag — set to `true` when an interrupt is requested.
    flag: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<PendingInterrupt>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl InterruptController {
    /// Create a new interrupt controller in the non-interrupted state.
    pub fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(Mutex::new(None)),
            notify: Arc::new(tokio::sync::Notify::new()),
        }
    }

    /// Request the agent to interrupt its current loop.
    ///
    /// Optionally provide a redirect message (e.g. new user input that
    /// should be processed instead of continuing the current tool chain).
    pub fn interrupt(&self, message: Option<String>) {
        self.signal(message, InterruptCause::Cancellation);
    }

    /// Request interruption because a governing deadline expired.
    pub fn interrupt_timeout(&self, message: Option<String>) {
        self.signal(message, InterruptCause::Timeout);
    }

    /// Deliver mid-turn user steering without cancelling governed persistence.
    pub fn steer(&self, message: String) {
        self.signal(Some(message), InterruptCause::Steer);
    }

    fn signal(&self, message: Option<String>, cause: InterruptCause) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cause == InterruptCause::Steer
            && pending
                .as_ref()
                .is_some_and(|signal| signal.cause.is_fatal())
        {
            return;
        }
        *pending = Some(PendingInterrupt { cause, message });
        self.flag.store(true, Ordering::Release);
        drop(pending);
        self.notify.notify_waiters();
    }

    /// Clear any pending interrupt request.
    pub fn clear_interrupt(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *pending = None;
        self.flag.store(false, Ordering::Release);
    }

    /// Check whether an interrupt has been requested.
    pub fn is_interrupted(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Wait until any interrupt is observed without consuming its message.
    pub async fn cancelled(&self) {
        self.wait_until_pending(|_| true).await;
    }

    /// Wait only for a terminal cancellation or deadline. Steering remains
    /// pending for the tool-batch boundary that durably attaches it.
    pub async fn fatal_cancelled(&self) {
        self.wait_until_pending(InterruptCause::is_fatal)
            .await;
    }

    async fn wait_until_pending(&self, accepts: impl Fn(InterruptCause) -> bool) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let accepted = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .is_some_and(|signal| accepts(signal.cause));
            if accepted {
                return;
            }
            notified.await;
        }
    }

    /// If an interrupt is pending, consume it (clear flag + redirect) and return `Some(redirect)`.
    ///
    /// Used by the agent loop for **graceful** shutdown with
    /// [`hermes_core::AgentResult::interrupted`] instead of
    /// [`AgentError::Interrupted`]. Call sites that still need the legacy error
    /// should use [`Self::check_interrupt`].
    pub fn take_interrupt_graceful(&self) -> Option<Option<String>> {
        self.take_pending().map(|signal| signal.message)
    }

    /// Consume a terminal cancellation or timeout while leaving steer pending.
    pub fn take_fatal_graceful(&self) -> Option<Option<String>> {
        self.take_pending_if(InterruptCause::is_fatal)
            .map(|signal| signal.message)
    }

    /// Consume a terminal cancellation or timeout while preserving its exact
    /// error type. Steering remains pending for its durable batch boundary.
    pub fn take_fatal_error(&self) -> Option<AgentError> {
        self.take_pending_if(InterruptCause::is_fatal)
            .map(|signal| Self::agent_error_from_parts(signal.cause, signal.message))
    }

    /// Consume the current signal while preserving cancellation vs timeout.
    pub fn take_interrupt_error(&self) -> Option<AgentError> {
        self.take_pending()
            .map(|signal| Self::agent_error_from_parts(signal.cause, signal.message))
    }

    pub fn take_redirect_message(&self) -> Option<String> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.as_mut().and_then(|signal| signal.message.take())
    }

    /// Check the interrupt flag and return `Err(AgentError::Interrupted)`
    /// if set. This is the primary check point used inside the agent loop
    /// before each LLM call and tool execution.
    pub fn check_interrupt(&self) -> Result<(), AgentError> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(signal) = pending.as_mut() else {
            return Ok(());
        };
        if signal.cause == InterruptCause::Steer {
            return Ok(());
        }
        let cause = signal.cause;
        let message = signal.message.take();
        Err(Self::agent_error_from_parts(cause, message))
    }

    fn take_pending(&self) -> Option<PendingInterrupt> {
        self.take_pending_if(|_| true)
    }

    fn take_pending_if(
        &self,
        accepts: impl FnOnce(InterruptCause) -> bool,
    ) -> Option<PendingInterrupt> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !pending
            .as_ref()
            .is_some_and(|signal| accepts(signal.cause))
        {
            return None;
        }
        let signal = pending.take();
        self.flag.store(false, Ordering::Release);
        signal
    }

    fn agent_error_from_parts(cause: InterruptCause, message: Option<String>) -> AgentError {
        match cause {
            InterruptCause::Timeout => AgentError::Timeout(
                message.unwrap_or_else(|| "agent deadline expired".to_string()),
            ),
            InterruptCause::Cancellation | InterruptCause::Steer => {
                AgentError::Interrupted { message }
            }
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

    #[tokio::test]
    async fn cancelled_does_not_lose_concurrent_interrupts() {
        for sequence in 0..512 {
            let ctrl = InterruptController::new();
            let waiter_ctrl = ctrl.clone();
            let waiter = tokio::spawn(async move {
                waiter_ctrl.cancelled().await;
                waiter_ctrl.take_interrupt_graceful().flatten()
            });
            if sequence % 2 == 0 {
                tokio::task::yield_now().await;
            }
            ctrl.interrupt(Some(sequence.to_string()));

            let message = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
                .await
                .expect("interrupt waiter lost a wakeup")
                .expect("interrupt waiter panicked");
            assert_eq!(message.as_deref(), Some(sequence.to_string().as_str()));
            assert!(!ctrl.is_interrupted());
        }
    }

    #[test]
    fn fatal_error_preserves_timeout_and_cancellation_causes() {
        let timeout = InterruptController::new();
        timeout.interrupt_timeout(Some("deadline".into()));
        assert!(matches!(
            timeout.take_fatal_error(),
            Some(AgentError::Timeout(message)) if message == "deadline"
        ));
        assert!(!timeout.is_interrupted());

        let cancellation = InterruptController::new();
        cancellation.interrupt(Some("stop".into()));
        assert!(matches!(
            cancellation.take_fatal_error(),
            Some(AgentError::Interrupted { message }) if message.as_deref() == Some("stop")
        ));
        assert!(!cancellation.is_interrupted());
    }
    #[test]
    fn steer_survives_terminal_checks_until_batch_consumption() {
        let ctrl = InterruptController::new();
        ctrl.steer("bounded steer marker".to_string());

        assert!(ctrl.check_interrupt().is_ok());
        assert!(ctrl.take_fatal_error().is_none());
        assert!(ctrl.is_interrupted());
        assert_eq!(
            ctrl.take_interrupt_graceful().flatten().as_deref(),
            Some("bounded steer marker")
        );
        assert!(!ctrl.is_interrupted());
    }

    #[tokio::test]
    async fn fatal_waiter_ignores_steer_and_wakes_for_timeout() {
        let ctrl = InterruptController::new();
        let waiter_ctrl = ctrl.clone();
        let mut waiter = tokio::spawn(async move {
            waiter_ctrl.fatal_cancelled().await;
        });
        ctrl.steer("steer".to_string());

        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiter)
                .await
                .is_err(),
            "steer unexpectedly completed the fatal waiter"
        );
        ctrl.interrupt_timeout(Some("deadline".to_string()));
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("fatal waiter lost the timeout wakeup")
            .expect("fatal waiter panicked");
        assert!(matches!(
            ctrl.take_interrupt_error(),
            Some(AgentError::Timeout(message)) if message == "deadline"
        ));
    }

    #[test]
    fn steer_cannot_overwrite_pending_timeout() {
        let ctrl = InterruptController::new();
        ctrl.interrupt_timeout(Some("deadline".to_string()));
        ctrl.steer("late steer".to_string());

        assert!(matches!(
            ctrl.take_interrupt_error(),
            Some(AgentError::Timeout(message)) if message == "deadline"
        ));
    }


    #[test]
    fn concurrent_consume_never_erases_a_second_signal() {
        fn error_message(error: AgentError) -> String {
            match error {
                AgentError::Interrupted { message } => message.unwrap_or_default(),
                AgentError::Timeout(message) => message,
                other => panic!("unexpected interrupt error: {other:?}"),
            }
        }

        for sequence in 0..128 {
            let ctrl = InterruptController::new();
            ctrl.interrupt(Some(format!("first-{sequence}")));
            let barrier = Arc::new(std::sync::Barrier::new(3));
            let consumed = std::thread::scope(|scope| {
                let consumer_ctrl = ctrl.clone();
                let consumer_barrier = barrier.clone();
                let consumer = scope.spawn(move || {
                    consumer_barrier.wait();
                    consumer_ctrl.take_interrupt_error()
                });
                let producer_ctrl = ctrl.clone();
                let producer_barrier = barrier.clone();
                let producer = scope.spawn(move || {
                    producer_barrier.wait();
                    producer_ctrl.interrupt_timeout(Some(format!("second-{sequence}")));
                });
                barrier.wait();
                let consumed = consumer.join().expect("consumer thread");
                producer.join().expect("producer thread");
                consumed
            });
            let pending = ctrl.take_interrupt_error();
            let seen = consumed
                .into_iter()
                .chain(pending)
                .map(error_message)
                .collect::<Vec<_>>();
            assert!(
                seen.iter().any(|message| message == &format!("second-{sequence}")),
                "second signal was erased: {seen:?}"
            );
        }
    }
}
