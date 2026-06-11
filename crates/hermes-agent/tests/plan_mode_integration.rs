//! Plan mode integration: planning pause, registry sync, tool executor gate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use hermes_agent::agent_loop::ToolRegistry;
use hermes_agent::{AgentConfig, AgentLoop, RunConversationParams};
use hermes_core::{
    AgentError, FunctionCall, LlmProvider, LlmResponse, Message, StreamChunk, ToolCall, ToolSchema,
};
use hermes_tools::{PlanPhase, ToolRegistry as HermesToolsRegistry};
use serde_json::json;

struct PlanTextProvider;

#[async_trait]
impl LlmProvider for PlanTextProvider {
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
            message: Message::assistant(
                "## Plan\n1. Read src/main.rs\n2. Apply patch\n3. Run cargo test",
            ),
            usage: None,
            model: "test".into(),
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

struct WriteToolProvider;

#[async_trait]
impl LlmProvider for WriteToolProvider {
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
            message: Message::assistant_with_tool_calls(
                None,
                vec![ToolCall {
                    id: "tc-write".into(),
                    function: FunctionCall {
                        name: "write_file".into(),
                        arguments: r#"{"path":"out.txt","content":"x"}"#.into(),
                    },
                    extra_content: None,
                }],
            ),
            usage: None,
            model: "test".into(),
            finish_reason: Some("tool_calls".into()),
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

#[tokio::test]
async fn planning_text_only_pauses_for_approval() {
    let agent = AgentLoop::new(
        AgentConfig {
            max_turns: 3,
            ..AgentConfig::default()
        },
        Arc::new(ToolRegistry::new()),
        Arc::new(PlanTextProvider),
    );
    agent.set_plan_phase(PlanPhase::Planning);

    let conv = agent
        .run_conversation(RunConversationParams {
            user_message: "Plan a refactor".into(),
            conversation_history: vec![],
            task_id: None,
            stream_callback: None,
            persist_user_message: None,
            tools: None,
            persist_session: false,
        })
        .await
        .expect("run_conversation");

    let result = conv.into_loop_result();
    assert_eq!(result.turn_exit_reason, "plan_awaiting_approval");
    assert_eq!(agent.plan_phase(), PlanPhase::AwaitingApproval);
    assert!(result.plan_pending.as_ref().is_some_and(|p| p.contains("Plan")));
    assert_eq!(result.plan_phase.as_deref(), Some("awaiting_approval"));
}

#[tokio::test]
async fn planning_blocks_write_tool_in_executor_fallback_path() {
    let handler_ran = Arc::new(AtomicBool::new(false));
    let handler_ran_clone = handler_ran.clone();
    let mut tools = ToolRegistry::new();
    tools.register(
        "write_file",
        ToolSchema::new("write_file", "write", hermes_core::JsonSchema::new("object")),
        Arc::new(move |_params| {
            handler_ran_clone.store(true, Ordering::SeqCst);
            Ok("written".into())
        }),
    );

    let agent = AgentLoop::new(
        AgentConfig {
            max_turns: 3,
            ..AgentConfig::default()
        },
        Arc::new(tools),
        Arc::new(WriteToolProvider),
    );
    agent.set_plan_phase(PlanPhase::Planning);

    let conv = agent
        .run_conversation(RunConversationParams {
            user_message: "write a file".into(),
            conversation_history: vec![],
            task_id: None,
            stream_callback: None,
            persist_user_message: None,
            tools: None,
            persist_session: false,
        })
        .await
        .expect("run_conversation");

    assert!(
        !handler_ran.load(Ordering::SeqCst),
        "write handler must not run during Planning"
    );
    let saw_plan_block = conv.messages().iter().any(|m| {
        m.role == hermes_core::MessageRole::Tool
            && m.content
                .as_deref()
                .is_some_and(|c| c.contains("plan_block") || c.contains("plan mode"))
    });
    assert!(saw_plan_block, "tool result should contain plan_block message");
}

#[test]
fn synced_registry_inherits_plan_phase() {
    let registry = Arc::new(HermesToolsRegistry::new());
    let agent = AgentLoop::new(
        AgentConfig::default(),
        Arc::new(ToolRegistry::new()),
        Arc::new(PlanTextProvider),
    )
    .with_synced_tools_registry(registry.clone());

    agent.set_plan_phase(PlanPhase::Planning);
    assert_eq!(registry.plan_phase(), PlanPhase::Planning);

    agent.set_plan_phase(PlanPhase::Executing);
    assert_eq!(registry.plan_phase(), PlanPhase::Executing);
}

#[tokio::test]
async fn hermes_tools_registry_blocks_write_in_planning() {
    use async_trait::async_trait;
    use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

    struct EchoHandler;

    #[async_trait]
    impl ToolHandler for EchoHandler {
        async fn execute(&self, params: serde_json::Value) -> Result<String, ToolError> {
            Ok(params.to_string())
        }
        fn schema(&self) -> ToolSchema {
            tool_schema("patch", "patch", JsonSchema::new("object"))
        }
    }

    let registry = HermesToolsRegistry::new();
    let handler = Arc::new(EchoHandler);
    registry.register(
        "patch",
        "test",
        handler.schema(),
        handler,
        Arc::new(|| true),
        vec![],
        false,
        "Patch",
        "🩹",
        None,
    );
    registry.set_plan_phase(PlanPhase::Planning);
    let out = registry.dispatch_async("patch", json!({"path": "a.rs"})).await;
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["plan"]["decision"], "plan_block");
}
