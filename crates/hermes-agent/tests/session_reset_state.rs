//! Parity with Python `tests/run_agent/test_session_reset_fix.py` and
//! `tests/agent/test_context_engine_host_contract.py` (reset_session_state paths).

use std::sync::Arc;

use futures::StreamExt;
use futures::stream::BoxStream;
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
    AgentLoop::new(
        config,
        Arc::new(ToolRegistry::new()),
        Arc::new(DummyProvider),
    )
}

#[test]
fn reset_clears_session_usage_metrics() {
    let agent = test_agent();
    {
        let mut state = agent.state.lock().expect("lock");
        state.session_usage.total_tokens = 999;
        state.session_usage.input_tokens = 100;
        state.session_usage.api_calls = 5;
        state.session_usage.estimated_cost_usd = 0.42;
        state.session_usage.cost_status = "estimated".into();
        state.session_usage.cost_source = "openrouter".into();
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
        let mut state = agent.state.lock().expect("lock");
        state.evolution_counters.user_turn_count = 7;
    }
    agent.reset_session_state(None, None, false);
    assert_eq!(
        agent
            .state
            .lock()
            .expect("lock")
            .evolution_counters
            .user_turn_count,
        0
    );
}

#[test]
fn accumulate_api_call_updates_session_metrics() {
    let agent = test_agent();
    let usage = hermes_core::UsageStats {
        prompt_tokens: 1000,
        completion_tokens: 200,
        total_tokens: 1200,
        input_tokens: 200,
        output_tokens: 200,
        cache_read_tokens: 500,
        cache_write_tokens: 300,
        estimated_cost: None,
        ..Default::default()
    };
    agent.record_api_usage(&usage);
    let m = agent.session_usage_metrics();
    assert_eq!(m.api_calls, 1);
    assert_eq!(m.prompt_tokens, 1000);
    assert_eq!(m.completion_tokens, 200);
    assert_eq!(m.total_tokens, 1200);
    assert_eq!(m.input_tokens, 200);
    assert_eq!(m.cache_read_tokens, 500);
    assert_eq!(m.cache_write_tokens, 300);
}

#[test]
fn reset_with_session_metadata_clears_counters() {
    let agent = test_agent();
    agent.set_runtime_session_id("new-sid");
    agent.reset_session_state(Some(&[Message::user("hi")]), Some("old-sid"), false);
    let m = agent.session_usage_metrics();
    assert_eq!(m.total_tokens, 0);
}
