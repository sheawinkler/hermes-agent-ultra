//! Parity with Python `tests/run_agent/test_session_reset_fix.py` and
//! `tests/agent/test_context_engine_host_contract.py` (reset_session_state paths).

use std::sync::Arc;

use futures::stream::BoxStream;
use futures::StreamExt;
use hermes_agent::{AgentConfig, AgentLoop, ToolRegistry};
use hermes_core::{AgentError, LlmProvider, Message, StreamChunk, ToolSchema};

struct DummyProvider;

#[async_trait::async_trait]
impl LlmProvider for DummyProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Ok(hermes_core::LlmResponse::default())
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

fn test_agent() -> AgentLoop {
    let config = AgentConfig::default();
    AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
}

#[test]
fn reset_clears_session_usage_metrics() {
    let agent = test_agent();
    {
        let mut m = agent.session_usage.lock().expect("lock");
        m.total_tokens = 999;
        m.input_tokens = 100;
        m.api_calls = 5;
        m.estimated_cost_usd = 0.42;
        m.cost_status = "estimated".into();
        m.cost_source = "openrouter".into();
    }
    agent.reset_session_state(None, None, false);
    let m = agent.session_usage_metrics();
    assert_eq!(m.total_tokens, 0);
    assert_eq!(m.input_tokens, 0);
    assert_eq!(m.api_calls, 0);
    assert_eq!(m.estimated_cost_usd, 0.0);
    assert_eq!(m.cost_status, "unknown");
    assert_eq!(m.cost_source, "none");
}

#[test]
fn reset_clears_user_turn_count() {
    let agent = test_agent();
    {
        let mut c = agent.evolution_counters.lock().expect("lock");
        c.user_turn_count = 7;
    }
    agent.reset_session_state(None, None, false);
    assert_eq!(
        agent.evolution_counters.lock().expect("lock").user_turn_count,
        0
    );
}

#[test]
fn accumulate_api_call_updates_session_metrics() {
    let agent = test_agent();
    let usage = hermes_core::UsageStats {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
        estimated_cost: None,
    };
    agent.record_api_usage(&usage);
    let m = agent.session_usage_metrics();
    assert_eq!(m.api_calls, 1);
    assert_eq!(m.prompt_tokens, 100);
    assert_eq!(m.completion_tokens, 50);
    assert_eq!(m.total_tokens, 150);
}

#[test]
fn reset_with_session_metadata_clears_counters() {
    let agent = test_agent();
    agent.set_runtime_session_id("new-sid");
    agent.reset_session_state(
        Some(&[Message::user("hi")]),
        Some("old-sid"),
        false,
    );
    let m = agent.session_usage_metrics();
    assert_eq!(m.total_tokens, 0);
}
