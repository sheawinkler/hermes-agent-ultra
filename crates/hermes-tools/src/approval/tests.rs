use super::*;

static TEST_STATE_LOCK: LazyLock<std::sync::Mutex<()>> =
    LazyLock::new(|| std::sync::Mutex::new(()));

fn lock_test_state() -> std::sync::MutexGuard<'static, ()> {
    TEST_STATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn remove(key: &'static str) -> Self {
        let old = std::env::var(key).ok();
        std::env::remove_var(key);
        Self { key, old }
    }

    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(old) = &self.old {
            std::env::set_var(self.key, old);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn reset_approval_state_unlocked() {
    SESSION_APPROVED.lock().clear();
    SESSION_YOLO.lock().clear();
    PERMANENT_APPROVED.lock().clear();
    GATEWAY_QUEUES.lock().clear();
    GATEWAY_NOTIFY_CBS.lock().clear();
    APPROVAL_OBSERVERS.lock().clear();
    NEXT_APPROVAL_OBSERVER_ID.store(1, Ordering::SeqCst);
}

fn interactive_context(tirith_result: TirithResult) -> CommandGuardContext {
    CommandGuardContext::interactive_with_tirith(tirith_result)
}

include!("tests/basics.rs");
include!("tests/session_and_gateway.rs");
include!("tests/tirith_guards.rs");
include!("tests/patterns.rs");
