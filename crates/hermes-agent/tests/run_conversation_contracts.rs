//! `run_conversation` public API contracts.

use std::sync::Arc;

use async_trait::async_trait;
use futures::{stream::BoxStream, StreamExt};
use hermes_agent::{
    agent_loop::ToolRegistry,
    conversation_loop::extract_last_reasoning_current_turn,
    split_messages_for_run_conversation, AgentConfig, AgentLoop, RunConversationParams,
};
use hermes_core::{AgentError, LlmProvider, Message, MessageRole, StreamChunk, ToolSchema};

struct StopAssistantProvider;

#[async_trait]
impl LlmProvider for StopAssistantProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Ok(hermes_core::LlmResponse {
            message: Message::assistant("hello back"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
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

fn test_agent() -> AgentLoop {
    AgentLoop::new(
        AgentConfig {
            max_turns: 2,
            memory_nudge_interval: 4,
            ..AgentConfig::default()
        },
        Arc::new(ToolRegistry::new()),
        Arc::new(StopAssistantProvider),
    )
}

#[test]
fn split_and_reasoning_helpers_match_turn_boundary() {
    let messages = vec![
        Message::user("old"),
        Message::assistant("hi"),
        Message::user("new"),
    ];
    let (hist, user) = split_messages_for_run_conversation(messages).expect("split");
    assert_eq!(user, "new");
    assert_eq!(hist.len(), 2);

    let mut stale = Message::assistant("old");
    stale.reasoning_content = Some("stale".into());
    let mut fresh = Message::assistant("ok");
    fresh.reasoning_content = Some("fresh".into());
    let msgs = vec![Message::user("prior"), stale, Message::user("current"), fresh];
    assert_eq!(
        extract_last_reasoning_current_turn(&msgs).as_deref(),
        Some("fresh")
    );
}

#[tokio::test]
async fn run_conversation_sets_task_id_and_single_user_turn() {
    let agent = test_agent();
    let conv = agent
        .run_conversation(RunConversationParams {
            user_message: "ping".into(),
            conversation_history: vec![Message::user("earlier")],
            task_id: Some("turn-task".into()),
            stream_callback: None,
            persist_user_message: None,
            tools: None,
            persist_session: false,
        })
        .await
        .expect("run_conversation");

    assert_eq!(agent.current_task_id().as_deref(), Some("turn-task"));
    assert!(conv.completed);
    assert_eq!(conv.final_response.as_deref(), Some("hello back"));
    assert!(conv.completed);
    let user_msgs: Vec<_> = conv
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .collect();
    assert_eq!(user_msgs.len(), 2, "history + current user");
}

#[tokio::test]
async fn run_conversation_drains_pending_steer_into_result() {
    let agent = test_agent();
    assert!(agent.steer("focus on tests"));
    let conv = agent
        .run_conversation(RunConversationParams {
            user_message: "go".into(),
            conversation_history: vec![],
            task_id: None,
            stream_callback: None,
            persist_user_message: None,
            tools: None,
            persist_session: false,
        })
        .await
        .expect("run_conversation");
    assert_eq!(conv.pending_steer.as_deref(), Some("focus on tests"));
}
