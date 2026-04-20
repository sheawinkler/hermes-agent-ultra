//! Wires the abstract [`AuxiliaryClient`] (defined in `hermes-intelligence`)
//! to concrete [`LlmProvider`] implementations from this crate.
//!
//! The intelligence crate cannot depend on `hermes-agent` (would create a
//! cycle), so the binary layer owns the provider construction. This module
//! reads standard environment variables and assembles the auto-detect chain
//! that mirrors the Python `_get_provider_chain` ordering.

use std::sync::Arc;

use hermes_core::LlmProvider;
use hermes_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryConfig, AuxiliarySource, ProviderCandidate,
};

use crate::provider::{AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider};

/// Default auxiliary models per source. Mirrors the Python
/// `_API_KEY_PROVIDER_AUX_MODELS` table — chosen to be cheap and fast.
mod default_models {
    pub const OPENROUTER: &str = "google/gemini-3-flash-preview";
    pub const ANTHROPIC: &str = "claude-haiku-4-5-20251001";
    pub const OPENAI: &str = "gpt-4o-mini";
    pub const ZAI: &str = "glm-4.5-flash";
    pub const KIMI: &str = "kimi-k2-turbo-preview";
    pub const MINIMAX: &str = "MiniMax-M2.7";
    pub const GEMINI: &str = "gemini-3-flash-preview";
}

/// Returned by [`build_default_auxiliary_client`] alongside the client so
/// callers can introspect what was wired (e.g. for `hermes status`).
#[derive(Debug, Clone)]
pub struct AuxiliaryWiringSummary {
    pub registered: Vec<String>,
    pub skipped: Vec<String>,
}

/// Build an [`AuxiliaryClient`] from environment variables.
///
/// Resolution rules:
///
/// * `OPENROUTER_API_KEY` → registers an `openrouter` candidate
/// * `ANTHROPIC_API_KEY` → registers an `anthropic` candidate
/// * `OPENAI_API_KEY` (+ optional `OPENAI_BASE_URL`) → registers a `custom`
///   candidate
/// * `ZAI_API_KEY`, `KIMI_API_KEY`, `MINIMAX_API_KEY`, `GEMINI_API_KEY` →
///   register direct-key candidates (OpenAI-compatible base URLs)
///
/// Order matches Python: OpenRouter > Custom > Anthropic > direct keys.
pub fn build_default_auxiliary_client(
    config: AuxiliaryConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    let mut summary = AuxiliaryWiringSummary {
        registered: Vec::new(),
        skipped: Vec::new(),
    };
    let mut builder = AuxiliaryClient::builder().config(config);

    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> = Arc::new(
                OpenRouterProvider::new(key.trim())
                    .with_model(default_models::OPENROUTER)
                    .with_http_referer("https://hermes-agent.nousresearch.com")
                    .with_x_title("Hermes Agent"),
            );
            builder = builder.add_candidate(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                default_models::OPENROUTER,
                provider,
            ));
            summary.registered.push("openrouter".into());
        } else {
            summary.skipped.push("openrouter (empty)".into());
        }
    } else {
        summary.skipped.push("openrouter (no key)".into());
    }

    // Custom OpenAI-compatible endpoint (covers OPENAI_API_KEY + custom base
    // URLs). We mark it `Custom` rather than the OpenAI source so that the
    // chain dedup logic doesn't collide with explicitly-named providers.
    if let Ok(key) = std::env::var("OPENAI_API_KEY") {
        if !key.trim().is_empty() {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let model = std::env::var("OPENAI_AUXILIARY_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| default_models::OPENAI.into());
            let provider: Arc<dyn LlmProvider> =
                Arc::new(GenericProvider::new(base_url, key.trim(), model.clone()));
            builder = builder.add_candidate(ProviderCandidate::new(
                AuxiliarySource::Custom,
                model,
                provider,
            ));
            summary.registered.push("custom".into());
        } else {
            summary.skipped.push("custom (empty key)".into());
        }
    } else {
        summary.skipped.push("custom (no key)".into());
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> =
                Arc::new(AnthropicProvider::new(key.trim()).with_model(default_models::ANTHROPIC));
            builder = builder.add_candidate(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                default_models::ANTHROPIC,
                provider,
            ));
            summary.registered.push("anthropic".into());
        }
    } else {
        summary.skipped.push("anthropic (no key)".into());
    }

    register_direct_key(
        &mut builder,
        &mut summary,
        "ZAI_API_KEY",
        "zai",
        "https://api.z.ai/api/coding/paas/v4",
        default_models::ZAI,
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "KIMI_API_KEY",
        "kimi",
        "https://api.moonshot.ai/v1",
        default_models::KIMI,
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "MINIMAX_API_KEY",
        "minimax",
        "https://api.minimax.io/v1",
        default_models::MINIMAX,
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "GEMINI_API_KEY",
        "gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        default_models::GEMINI,
    );

    let client = builder.build();
    (client, summary)
}

fn register_direct_key(
    builder: &mut hermes_intelligence::auxiliary::AuxiliaryClientBuilder,
    summary: &mut AuxiliaryWiringSummary,
    env_var: &str,
    label: &str,
    base_url: &str,
    default_model: &str,
) {
    let owned;
    let key = match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => {
            owned = v;
            owned.trim()
        }
        _ => {
            summary.skipped.push(format!("{label} (no key)"));
            return;
        }
    };
    let provider: Arc<dyn LlmProvider> =
        Arc::new(GenericProvider::new(base_url, key, default_model));
    let temp_builder = std::mem::take(builder);
    *builder = temp_builder.add_candidate(ProviderCandidate::new(
        AuxiliarySource::DirectKey(label.to_string()),
        default_model,
        provider,
    ));
    summary.registered.push(label.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_AUXILIARY_MODEL",
        "ANTHROPIC_API_KEY",
        "ZAI_API_KEY",
        "KIMI_API_KEY",
        "MINIMAX_API_KEY",
        "GEMINI_API_KEY",
    ];

    /// Save current env, then clear; restored when the guard drops.
    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let mut previous = Vec::new();
            for k in KEYS {
                previous.push((*k, std::env::var(k).ok()));
                std::env::remove_var(k);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.previous.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Run all env-mutating cases serially in a single test to avoid races
    /// with parallel cargo test workers (env is process-global).
    #[test]
    fn build_default_auxiliary_client_scenarios() {
        let _g = EnvGuard::clear();

        // Scenario 1: no keys → empty chain.
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 0, "empty env produced non-empty chain");
            assert!(summary.registered.is_empty());
            assert!(!summary.skipped.is_empty());
        }

        // Scenario 2: only OpenRouter.
        std::env::set_var("OPENROUTER_API_KEY", "sk-test");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 1);
            assert_eq!(client.chain_labels(), vec!["openrouter"]);
            assert_eq!(summary.registered, vec!["openrouter"]);
        }
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 3: full chain, deterministic order.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or");
        std::env::set_var("OPENAI_API_KEY", "sk-oa");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-an");
        std::env::set_var("ZAI_API_KEY", "z");
        {
            let (client, _) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(
                client.chain_labels(),
                vec!["openrouter", "custom", "anthropic", "zai"]
            );
        }
    }
}
