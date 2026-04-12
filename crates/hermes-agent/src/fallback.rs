//! Fallback model switching.
//!
//! When the primary model fails (rate limit, auth error, etc.), automatically
//! switch to a configured fallback model, then restore the primary after a
//! cooldown period.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermes_core::LlmProvider;

/// A chain of LLM providers with automatic fallback.
pub struct FallbackChain {
    /// Primary provider.
    primary: Arc<dyn LlmProvider>,
    /// Ordered list of fallback providers.
    fallbacks: Vec<Arc<dyn LlmProvider>>,
    /// State tracking which provider is active.
    state: Mutex<FallbackState>,
}

struct FallbackState {
    /// Index into the chain: 0 = primary, 1+ = fallbacks[i-1]
    active_index: usize,
    /// When the fallback was activated (for cooldown).
    fallback_activated_at: Option<Instant>,
    /// How long to wait before trying the primary again.
    cooldown: Duration,
}

impl FallbackChain {
    pub fn new(primary: Arc<dyn LlmProvider>) -> Self {
        Self {
            primary,
            fallbacks: Vec::new(),
            state: Mutex::new(FallbackState {
                active_index: 0,
                fallback_activated_at: None,
                cooldown: Duration::from_secs(60),
            }),
        }
    }

    pub fn with_fallback(mut self, provider: Arc<dyn LlmProvider>) -> Self {
        self.fallbacks.push(provider);
        self
    }

    pub fn with_cooldown(self, cooldown: Duration) -> Self {
        self.state.lock().unwrap().cooldown = cooldown;
        self
    }

    /// Get the currently active provider.
    pub fn active_provider(&self) -> Arc<dyn LlmProvider> {
        let state = self.state.lock().unwrap();
        if state.active_index == 0 {
            self.primary.clone()
        } else {
            self.fallbacks
                .get(state.active_index - 1)
                .cloned()
                .unwrap_or_else(|| self.primary.clone())
        }
    }

    /// Activate the next fallback in the chain.
    pub fn activate_fallback(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.active_index < self.fallbacks.len() {
            state.active_index += 1;
            state.fallback_activated_at = Some(Instant::now());
            tracing::warn!(
                "Activated fallback provider (index {})",
                state.active_index
            );
            true
        } else {
            tracing::error!("No more fallback providers available");
            false
        }
    }

    /// Try to restore the primary provider if the cooldown has elapsed.
    pub fn try_restore_primary(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.active_index == 0 {
            return true;
        }
        if let Some(activated_at) = state.fallback_activated_at {
            if activated_at.elapsed() >= state.cooldown {
                tracing::info!("Cooldown elapsed, restoring primary provider");
                state.active_index = 0;
                state.fallback_activated_at = None;
                return true;
            }
        }
        false
    }

    /// Check if we're currently using a fallback.
    pub fn is_on_fallback(&self) -> bool {
        self.state.lock().unwrap().active_index > 0
    }
}
