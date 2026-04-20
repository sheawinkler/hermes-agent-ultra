//! Hermes provider ID ↔ models.dev provider ID mapping.
//!
//! Verbatim port of `PROVIDER_TO_MODELS_DEV` in `agent/models_dev.py`.
//! Keep in sync when Python adds new providers.

use std::collections::HashMap;
use std::sync::OnceLock;

/// `(hermes_id, models_dev_id)` pairs.
const PAIRS: &[(&str, &str)] = &[
    ("openrouter", "openrouter"),
    ("anthropic", "anthropic"),
    ("zai", "zai"),
    ("kimi-coding", "kimi-for-coding"),
    ("minimax", "minimax"),
    ("minimax-cn", "minimax-cn"),
    ("deepseek", "deepseek"),
    ("alibaba", "alibaba"),
    ("qwen-oauth", "alibaba"),
    ("copilot", "github-copilot"),
    ("ai-gateway", "vercel"),
    ("opencode-zen", "opencode"),
    ("opencode-go", "opencode-go"),
    ("kilocode", "kilo"),
    ("fireworks", "fireworks-ai"),
    ("huggingface", "huggingface"),
    ("gemini", "google"),
    ("google", "google"),
    ("xai", "xai"),
    ("nvidia", "nvidia"),
    ("groq", "groq"),
    ("mistral", "mistral"),
    ("togetherai", "togetherai"),
    ("perplexity", "perplexity"),
    ("cohere", "cohere"),
];

/// Forward map: Hermes provider ID → models.dev provider ID.
pub fn forward_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| PAIRS.iter().copied().collect())
}

/// Reverse map: models.dev provider ID → Hermes provider ID.
///
/// When two Hermes IDs share one models.dev ID (e.g. both `gemini` and
/// `google` map to `google`), the **last entry in [`PAIRS`] wins** — this
/// matches Python's `{v: k for k, v in PROVIDER_TO_MODELS_DEV.items()}`
/// semantics where later iterations overwrite earlier ones.
pub fn reverse_map() -> &'static HashMap<&'static str, &'static str> {
    static MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (hermes, mdev) in PAIRS {
            m.insert(*mdev, *hermes);
        }
        m
    })
}

/// Look up the models.dev provider ID for a Hermes provider ID.
pub fn to_models_dev(hermes_id: &str) -> Option<&'static str> {
    forward_map().get(hermes_id).copied()
}

/// Look up the Hermes provider ID for a models.dev provider ID.
pub fn to_hermes(mdev_id: &str) -> Option<&'static str> {
    reverse_map().get(mdev_id).copied()
}

/// Resolve to a models.dev ID, falling back to the input when unmapped.
///
/// Mirrors `PROVIDER_TO_MODELS_DEV.get(provider_id, provider_id)` in the
/// Python `get_provider_info` / `get_model_info` helpers.
pub fn resolve_models_dev_id(provider_id: &str) -> &str {
    to_models_dev(provider_id).unwrap_or(provider_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forward_map_resolves_known_providers() {
        assert_eq!(to_models_dev("anthropic"), Some("anthropic"));
        assert_eq!(to_models_dev("kimi-coding"), Some("kimi-for-coding"));
        assert_eq!(to_models_dev("gemini"), Some("google"));
        assert_eq!(to_models_dev("nonexistent"), None);
    }

    #[test]
    fn reverse_map_picks_last_when_collision() {
        // Both "gemini" and "google" map to "google" — Python's dict comp
        // keeps the last assignment, which here is "google".
        assert_eq!(to_hermes("google"), Some("google"));
    }

    #[test]
    fn resolve_models_dev_id_falls_back_to_input() {
        assert_eq!(resolve_models_dev_id("anthropic"), "anthropic");
        assert_eq!(resolve_models_dev_id("custom-thing"), "custom-thing");
    }
}
