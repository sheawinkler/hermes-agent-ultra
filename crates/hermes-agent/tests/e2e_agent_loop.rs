use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use futures::stream::BoxStream;
use hermes_agent::agent_loop::ToolRegistry;
use hermes_agent::{AgentConfig, AgentLoop};
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall,
    ToolSchema,
};

struct MockProvider {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl LlmProvider for MockProvider {
    async fn chat_completion(
        &self,
        messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    Some("need tool".to_string()),
                    vec![ToolCall {
                        id: "tool-1".to_string(),
                        function: FunctionCall {
                            name: "echo_tool".to_string(),
                            arguments: "{\"msg\":\"hello\"}".to_string(),
                        },
                        extra_content: None,
                    }],
                ),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("tool_calls".to_string()),
            })
        } else {
            let saw_tool = messages
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("tool-1"));
            Ok(LlmResponse {
                message: Message::assistant(if saw_tool {
                    "final-answer"
                } else {
                    "missing-tool-result"
                }),
                usage: None,
                model: "mock:model".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }
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

#[tokio::test]
async fn e2e_agent_loop_tool_call_then_final_reply() {
    let mut tools = ToolRegistry::new();
    tools.register(
        "echo_tool",
        ToolSchema {
            name: "echo_tool".to_string(),
            description: "echo".to_string(),
            parameters: JsonSchema::new("object"),
        },
        Arc::new(|params| {
            Ok(params
                .get("msg")
                .and_then(|v| v.as_str())
                .unwrap_or("none")
                .to_string())
        }),
    );
    let provider = Arc::new(MockProvider {
        calls: AtomicUsize::new(0),
    });
    let loop_engine = AgentLoop::new(
        AgentConfig {
            max_turns: 5,
            ..AgentConfig::default()
        },
        Arc::new(tools),
        provider,
    );

    let result = loop_engine
        .run(vec![Message::user("hi")], None)
        .await
        .expect("agent loop should succeed");
    let final_reply = result.messages.iter().rev().find_map(|m| m.content.clone());
    assert_eq!(final_reply.as_deref(), Some("final-answer"));
}
