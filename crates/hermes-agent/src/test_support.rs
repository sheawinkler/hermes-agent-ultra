//! Shared test helpers for hermes-agent.
//!
//! Eliminates ~1300 lines of repeated `LlmProvider` boilerplate across the test suite.

use std::sync::Arc;

use futures::StreamExt as _;
use futures::stream::BoxStream;
use hermes_core::{AgentError, LlmResponse, Message, StreamChunk, ToolSchema};

use crate::agent_config::AgentConfig;
use crate::agent_loop::AgentLoop;
use crate::tool_registry::ToolRegistry;

// ---------------------------------------------------------------------------
// FixedAssistantProvider — always returns the same text with finish_reason=stop
// ---------------------------------------------------------------------------

/// An `LlmProvider` that always returns a fixed assistant message.
///
/// Replaces the ~41 identical `DummyProvider` struct definitions spread across
/// `agent_loop` tests.
pub struct FixedAssistantProvider {
    pub text: &'static str,
    pub model: &'static str,
}

impl FixedAssistantProvider {
    pub const fn new(text: &'static str) -> Self {
        Self {
            text,
            model: "dummy",
        }
    }

    pub const fn with_model(text: &'static str, model: &'static str) -> Self {
        Self { text, model }
    }
}

impl Default for FixedAssistantProvider {
    fn default() -> Self {
        Self::new("dummy")
    }
}

#[async_trait::async_trait]
impl hermes_core::LlmProvider for FixedAssistantProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Ok(LlmResponse {
            message: Message::assistant(self.text),
            usage: None,
            model: self.model.into(),
            finish_reason: Some("stop".into()),
            ..Default::default()
        })
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
        futures::stream::empty().boxed()
    }
}

// ---------------------------------------------------------------------------
// ErrNoopProvider — always returns LlmApi error (simulates missing/invalid key)
// ---------------------------------------------------------------------------

/// An `LlmProvider` that always returns an error.
///
/// Replaces the 3 identical error `NoopProvider` struct definitions.
pub struct ErrNoopProvider;

#[async_trait::async_trait]
impl hermes_core::LlmProvider for ErrNoopProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Err(AgentError::LlmApi("noop".into()))
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

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Build a minimal `AgentLoop` backed by `FixedAssistantProvider("dummy")`.
pub fn dummy_agent() -> AgentLoop {
    AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(FixedAssistantProvider::default()),
    )
}

/// Build a minimal `AgentLoop` backed by `FixedAssistantProvider` with custom text.
pub fn fixed_agent(text: &'static str) -> AgentLoop {
    AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(FixedAssistantProvider::new(text)),
    )
}

/// Build a minimal `AgentLoop` backed by `ErrNoopProvider`.
pub fn err_agent() -> AgentLoop {
    AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(ErrNoopProvider),
    )
}
