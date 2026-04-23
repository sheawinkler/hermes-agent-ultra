//! Property 7: Agent loop respects max_turns limit
//! **Validates: Requirement 3.2**
//!
//! For any max_turns value and an LLM that always returns tool_calls,
//! the agent loop stops at exactly max_turns, with finished_naturally == false.

use proptest::prelude::*;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use hermes_core::{
    AgentError, FunctionCall, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall, ToolError,
    ToolSchema, UsageStats,
};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Mock LLM that always returns a tool call
// ---------------------------------------------------------------------------

struct AlwaysToolCallLlm;

#[async_trait]
impl LlmProvider for AlwaysToolCallLlm {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<LlmResponse, AgentError> {
        Ok(LlmResponse {
            message: Message::assistant_with_tool_calls(
                Some("Let me use a tool.".to_string()),
                vec![ToolCall {
                    id: "call_1".to_string(),
                    function: FunctionCall {
                        name: "echo".to_string(),
                        arguments: r#"{"input":"test"}"#.to_string(),
                    },
                    extra_content: None,
                }],
            ),
            usage: Some(UsageStats {
                prompt_tokens: 10,
                completion_tokens: 5,
                total_tokens: 15,
                estimated_cost: None,
            }),
            model: "mock".to_string(),
            finish_reason: Some("tool_calls".to_string()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        Box::pin(futures::stream::empty())
    }
}

// ---------------------------------------------------------------------------
// Echo handler for the tool registry
// ---------------------------------------------------------------------------

fn echo_handler(params: Value) -> Result<String, ToolError> {
    Ok(params.to_string())
}

fn make_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(
        "echo",
        hermes_core::tool_schema("echo", "Echo input", hermes_core::JsonSchema::new("object")),
        Arc::new(echo_handler),
    );
    registry
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_agent_loop_respects_max_turns(max_turns in 1u32..20) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let config = AgentConfig {
                max_turns,
                ..AgentConfig::default()
            };
            let registry = Arc::new(make_registry());
            let llm = Arc::new(AlwaysToolCallLlm);
            let agent = AgentLoop::new(config, registry, llm);

            let result = agent
                .run(vec![Message::user("test")], None)
                .await
                .unwrap();

            assert_eq!(result.total_turns, max_turns,
                "Expected {} turns, got {}", max_turns, result.total_turns);
            assert!(!result.finished_naturally,
                "Should not finish naturally when LLM always returns tool calls");
        });
    }
}
