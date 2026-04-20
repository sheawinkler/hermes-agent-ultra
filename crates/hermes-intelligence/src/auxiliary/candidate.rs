//! Provider candidates and chain construction.
//!
//! A [`ProviderCandidate`] is a *recipe* for instantiating an
//! [`hermes_core::LlmProvider`] for an auxiliary task — base URL, default
//! model, capability flags, vision support — together with the resolved
//! [`Arc<dyn LlmProvider>`] itself.
//!
//! The intelligence crate cannot import the concrete provider structs from
//! `hermes-agent` (cycle), so the binary layer wires up
//! [`ProviderCandidate::with_provider`] using a closure that returns the
//! correct `Arc<dyn LlmProvider>` for a `(base_url, api_key)` pair.

use std::sync::Arc;

use hermes_core::LlmProvider;

/// Where the candidate sources its credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AuxiliarySource {
    /// `https://openrouter.ai/api/v1` keyed by `OPENROUTER_API_KEY`.
    OpenRouter,
    /// `https://inference-api.nousresearch.com/v1` keyed by Nous auth.json /
    /// `NOUS_API_KEY` env var.
    Nous,
    /// Custom OpenAI-compatible endpoint (`OPENAI_API_KEY` + `OPENAI_BASE_URL`,
    /// or per-task overrides).
    Custom,
    /// `https://api.anthropic.com` keyed by `ANTHROPIC_API_KEY`.
    Anthropic,
    /// Direct API-key provider — z.ai / GLM, Kimi, MiniMax, Gemini, ...
    /// The string is the canonical provider name (`zai`, `kimi`, `gemini`,
    /// `minimax`, `minimax-cn`).
    DirectKey(String),
}

impl AuxiliarySource {
    /// Stable label used in logs and error messages.
    pub fn label(&self) -> String {
        match self {
            AuxiliarySource::OpenRouter => "openrouter".into(),
            AuxiliarySource::Nous => "nous".into(),
            AuxiliarySource::Custom => "custom".into(),
            AuxiliarySource::Anthropic => "anthropic".into(),
            AuxiliarySource::DirectKey(name) => name.clone(),
        }
    }
}

/// A fully-resolved provider entry the chain can call into.
#[derive(Clone)]
pub struct ProviderCandidate {
    pub source: AuxiliarySource,
    pub default_model: String,
    pub provider: Arc<dyn LlmProvider>,
    pub supports_vision: bool,
}

impl std::fmt::Debug for ProviderCandidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderCandidate")
            .field("source", &self.source)
            .field("default_model", &self.default_model)
            .field("supports_vision", &self.supports_vision)
            .finish()
    }
}

impl ProviderCandidate {
    pub fn new(
        source: AuxiliarySource,
        default_model: impl Into<String>,
        provider: Arc<dyn LlmProvider>,
    ) -> Self {
        let supports_vision = matches!(
            source,
            AuxiliarySource::OpenRouter
                | AuxiliarySource::Nous
                | AuxiliarySource::Anthropic
                | AuxiliarySource::Custom
                | AuxiliarySource::DirectKey(_)
        );
        Self {
            source,
            default_model: default_model.into(),
            provider,
            supports_vision,
        }
    }

    pub fn with_supports_vision(mut self, value: bool) -> Self {
        self.supports_vision = value;
        self
    }

    pub fn label(&self) -> String {
        self.source.label()
    }
}

/// An ordered, deduplicated chain of provider candidates.
///
/// The chain is intentionally a value type (not a singleton) so callers can
/// build per-task chains — e.g. a vision task drops candidates whose
/// `supports_vision == false`.
#[derive(Default, Clone, Debug)]
pub struct ProviderChain {
    candidates: Vec<ProviderCandidate>,
}

impl ProviderChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, candidate: ProviderCandidate) {
        // Dedup by label so users don't double-pay if the same provider was
        // registered twice (e.g. once explicitly + once via auto-detect).
        let label = candidate.label();
        if !self.candidates.iter().any(|c| c.label() == label) {
            self.candidates.push(candidate);
        }
    }

    pub fn extend<I: IntoIterator<Item = ProviderCandidate>>(&mut self, iter: I) {
        for c in iter {
            self.push(c);
        }
    }

    /// Filter the chain to only providers that support vision.
    pub fn vision_only(&self) -> Self {
        Self {
            candidates: self
                .candidates
                .iter()
                .filter(|c| c.supports_vision)
                .cloned()
                .collect(),
        }
    }

    /// Move the candidate with the given label to the front. No-op if absent.
    pub fn promote(&mut self, label: &str) {
        if let Some(idx) = self.candidates.iter().position(|c| c.label() == label) {
            let cand = self.candidates.remove(idx);
            self.candidates.insert(0, cand);
        }
    }

    /// Drop the first candidate whose label matches.
    pub fn drop_label(&mut self, label: &str) {
        if let Some(idx) = self.candidates.iter().position(|c| c.label() == label) {
            self.candidates.remove(idx);
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProviderCandidate> {
        self.candidates.iter()
    }

    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    pub fn labels(&self) -> Vec<String> {
        self.candidates.iter().map(|c| c.label()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use hermes_core::{AgentError, LlmResponse, Message, StreamChunk, ToolSchema};

    struct DummyProvider;

    #[async_trait]
    impl LlmProvider for DummyProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Err(AgentError::LlmApi("dummy".into()))
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    fn cand(source: AuxiliarySource, model: &str) -> ProviderCandidate {
        ProviderCandidate::new(source, model, Arc::new(DummyProvider))
    }

    #[test]
    fn chain_dedups_by_label() {
        let mut chain = ProviderChain::new();
        chain.push(cand(AuxiliarySource::OpenRouter, "x"));
        chain.push(cand(AuxiliarySource::OpenRouter, "y")); // dropped
        chain.push(cand(AuxiliarySource::Anthropic, "z"));
        assert_eq!(chain.len(), 2);
        assert_eq!(chain.labels(), vec!["openrouter", "anthropic"]);
    }

    #[test]
    fn promote_moves_label_to_front() {
        let mut chain = ProviderChain::new();
        chain.push(cand(AuxiliarySource::OpenRouter, "a"));
        chain.push(cand(AuxiliarySource::Anthropic, "b"));
        chain.push(cand(AuxiliarySource::Custom, "c"));
        chain.promote("custom");
        assert_eq!(chain.labels(), vec!["custom", "openrouter", "anthropic"]);
    }

    #[test]
    fn vision_only_filters() {
        let mut chain = ProviderChain::new();
        chain.push(cand(AuxiliarySource::OpenRouter, "or"));
        chain
            .push(cand(AuxiliarySource::DirectKey("kimi".into()), "k").with_supports_vision(false));
        let v = chain.vision_only();
        assert_eq!(v.labels(), vec!["openrouter"]);
    }
}
