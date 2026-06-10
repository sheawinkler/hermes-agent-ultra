//! Fallback model switching.
//!
//! When the primary model fails (rate limit, auth error, etc.), automatically
//! switch to a configured fallback model, then restore the primary after a
//! cooldown period.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermes_core::LlmProvider;

use crate::smart_model_routing::PrimaryRuntime;

/// Per-session turn-scoped fallback state (Python `_fallback_activated` / `_fallback_index`).
#[derive(Debug, Clone)]
pub struct TurnFallbackState {
    pub fallback_activated: bool,
    pub fallback_chain_index: usize,
    rate_limited_until: Option<Instant>,
    cooldown: Duration,
}

impl TurnFallbackState {
    pub fn new() -> Self {
        Self {
            fallback_activated: false,
            fallback_chain_index: 0,
            rate_limited_until: None,
            cooldown: Duration::from_secs(60),
        }
    }

    /// Python `restore_primary_runtime` gate before mutating runtime fields.
    pub fn should_restore_primary(&self) -> bool {
        if !self.fallback_activated {
            return false;
        }
        if let Some(until) = self.rate_limited_until {
            if Instant::now() < until {
                return false;
            }
        }
        true
    }

    /// Call at the start of each turn (`run` / `run_stream`).
    ///
    /// Returns `true` when the active runtime was reset to the stored primary snapshot.
    pub fn restore_primary_runtime(
        &mut self,
        stored_primary: &PrimaryRuntime,
        active_runtime: &mut PrimaryRuntime,
    ) -> bool {
        if !self.fallback_activated {
            // Regression #20465: reset chain index even when primary was never left.
            self.fallback_chain_index = 0;
            return false;
        }
        if !self.should_restore_primary() {
            return false;
        }
        *active_runtime = stored_primary.clone();
        self.fallback_activated = false;
        self.fallback_chain_index = 0;
        self.rate_limited_until = None;
        tracing::info!(
            model = %stored_primary.model,
            provider = ?stored_primary.provider,
            "Primary runtime restored for new turn"
        );
        true
    }

    pub fn mark_fallback_activated(&mut self) {
        self.fallback_activated = true;
    }

    pub fn note_primary_rate_limited(&mut self) {
        self.rate_limited_until = Some(Instant::now() + self.cooldown);
    }

    pub fn is_fallback_activated(&self) -> bool {
        self.fallback_activated
    }
}

/// A chain of LLM providers with automatic fallback.
pub struct FallbackChain {
    /// Primary provider.
    primary: Arc<dyn LlmProvider>,
    /// Ordered list of fallback providers.
    fallbacks: Vec<Arc<dyn LlmProvider>>,
    /// State tracking which provider is active.
    state: Mutex<FallbackChainState>,
}

struct FallbackChainState {
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
            state: Mutex::new(FallbackChainState {
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
            tracing::warn!("Activated fallback provider (index {})", state.active_index);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smart_model_routing::ApiMode;

    fn sample_primary() -> PrimaryRuntime {
        PrimaryRuntime {
            model: "gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            base_url: Some("https://api.openai.com/v1".to_string()),
            api_mode: ApiMode::ChatCompletions,
            command: None,
            args: Vec::new(),
            credential_pool: None,
        }
    }

    #[test]
    fn restore_noop_when_fallback_not_active_resets_chain_index() {
        let primary = sample_primary();
        let mut active = PrimaryRuntime {
            model: "fallback-model".to_string(),
            ..primary.clone()
        };
        let mut state = TurnFallbackState::new();
        state.fallback_chain_index = 2;

        assert!(!state.restore_primary_runtime(&primary, &mut active));
        assert_eq!(active.model, "fallback-model");
        assert_eq!(state.fallback_chain_index, 0);
    }

    #[test]
    fn restore_primary_after_fallback_activation() {
        let primary = sample_primary();
        let mut active = PrimaryRuntime {
            model: "anthropic/claude-sonnet-4".to_string(),
            provider: Some("openrouter".to_string()),
            base_url: None,
            api_mode: ApiMode::ChatCompletions,
            command: None,
            args: Vec::new(),
            credential_pool: None,
        };
        let mut state = TurnFallbackState::new();
        state.mark_fallback_activated();

        assert!(state.restore_primary_runtime(&primary, &mut active));
        assert_eq!(active.model, primary.model);
        assert_eq!(active.provider, primary.provider);
        assert!(!state.is_fallback_activated());
    }

    #[test]
    fn restore_skipped_during_rate_limit_cooldown() {
        let primary = sample_primary();
        let mut active = PrimaryRuntime {
            model: "fallback-model".to_string(),
            ..primary.clone()
        };
        let mut state = TurnFallbackState::new();
        state.mark_fallback_activated();
        state.note_primary_rate_limited();

        assert!(!state.restore_primary_runtime(&primary, &mut active));
        assert_eq!(active.model, "fallback-model");
        assert!(state.is_fallback_activated());
    }
}
