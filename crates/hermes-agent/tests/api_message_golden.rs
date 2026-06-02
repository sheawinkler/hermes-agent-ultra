//! Golden oracle for `AgentLoop::messages_for_api_call` (legacy baseline).
//!
//! When migrating to zero-copy API views, every case here must pass via
//! `hermes_agent::assert_dual_run_eq(legacy, candidate)`.

use std::sync::Arc;

use futures::stream::BoxStream;
use hermes_agent::memory_manager::build_memory_context_block;
use hermes_agent::prompt_caching::build_cache_marker;
use hermes_agent::{
    assert_dual_run_eq, canonical_messages_json, AgentConfig, AgentLoop, ApiMode, ContextManager,
    GenericProvider, ToolRegistry,
};
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmProvider, LlmResponse, Message, MessageRole,
    StreamChunk, ToolCall, ToolSchema,
};

struct NoopProvider;

#[async_trait::async_trait]
impl LlmProvider for NoopProvider {
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

fn noop_agent(config: AgentConfig) -> AgentLoop {
    AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(NoopProvider))
}

fn oracle(agent: &AgentLoop, ctx: &mut ContextManager) -> Vec<Message> {
    agent.oracle_messages_for_api_call(ctx)
}

#[test]
fn golden_basic_system_and_user() {
    let agent = noop_agent(AgentConfig::default());
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("You are helpful."));
    ctx.add_message(Message::user("hello"));

    let out = oracle(&agent, &mut ctx);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].role, MessageRole::System);
    assert_eq!(out[1].role, MessageRole::User);
    assert_eq!(out[1].content.as_deref(), Some("hello"));
    assert!(out.iter().all(|m| m.cache_control.is_none()));
}

#[test]
fn golden_tool_call_pair_preserved() {
    let agent = noop_agent(AgentConfig::default());
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("run"));
    ctx.add_message(Message::assistant_with_tool_calls(
        Some("ok".into()),
        vec![ToolCall {
            id: "tc1".into(),
            function: FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            },
            extra_content: None,
        }],
    ));
    ctx.add_message(Message {
        role: MessageRole::Tool,
        content: Some("file contents".into()),
        tool_calls: None,
        tool_call_id: Some("tc1".into()),
        name: None,
        reasoning_content: None,
        cache_control: None,
    });

    let out = oracle(&agent, &mut ctx);
    assert_eq!(out.len(), 4);
    assert_eq!(out[2].tool_calls.as_ref().unwrap()[0].id, "tc1");
    assert_eq!(out[3].tool_call_id.as_deref(), Some("tc1"));
}

#[test]
fn golden_prefetch_merges_into_last_user() {
    let agent = noop_agent(AgentConfig::default());
    agent.oracle_set_turn_ext_prefetch_cache("User prefers Rust.");
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("first"));
    ctx.add_message(Message::assistant("ack"));
    ctx.add_message(Message::user("second question"));

    let out = oracle(&agent, &mut ctx);
    let fenced = build_memory_context_block("User prefers Rust.");
    let last_user = out
        .iter()
        .rfind(|m| m.role == MessageRole::User)
        .expect("user");
    let content = last_user.content.as_deref().unwrap_or("");
    assert!(content.contains("second question"));
    assert!(content.contains(&fenced));
    assert_eq!(
        out.iter().filter(|m| m.role == MessageRole::User).count(),
        2
    );
}

#[test]
fn golden_ephemeral_system_appended() {
    let agent = noop_agent(AgentConfig {
        ephemeral_system_prompt: Some("Ephemeral hint for this turn.".into()),
        ..AgentConfig::default()
    });
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("persistent sys"));
    ctx.add_message(Message::user("hi"));

    let out = oracle(&agent, &mut ctx);
    assert_eq!(out.len(), 3);
    assert_eq!(out.last().unwrap().role, MessageRole::System);
    assert_eq!(
        out.last().unwrap().content.as_deref(),
        Some("Ephemeral hint for this turn.")
    );
}

#[test]
fn golden_anthropic_prompt_cache_markers() {
    let agent = noop_agent(AgentConfig {
        provider: Some("anthropic".into()),
        model: "claude-sonnet-4-20250514".into(),
        api_mode: ApiMode::AnthropicMessages,
        cache_ttl: "1h".into(),
        use_prompt_caching: true,
        use_native_cache_layout: true,
        ..AgentConfig::default()
    });
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("cached sys"));
    ctx.add_message(Message::user("u1"));
    ctx.add_message(Message::assistant("a1"));
    ctx.add_message(Message::user("u2"));

    let out = oracle(&agent, &mut ctx);
    assert!(
        out.first()
            .and_then(|m| m.cache_control.as_ref())
            .is_some(),
        "system should receive cache_control under anthropic policy"
    );
    let marker = build_cache_marker("1h");
    assert_eq!(out[0].cache_control.as_ref(), Some(&marker));
}

#[test]
fn golden_non_vision_model_strips_image_placeholder() {
    let agent = noop_agent(AgentConfig {
        model: "llama-3.1-8b".into(),
        ..AgentConfig::default()
    });
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user(
        "see this data:image/png;base64,AAAA and describe",
    ));

    let out = oracle(&agent, &mut ctx);
    let user = out
        .iter()
        .find(|m| m.role == MessageRole::User)
        .unwrap();
    let content = user.content.as_deref().unwrap_or("");
    assert!(!content.contains("data:image"));
    assert!(content.contains("does not support vision"));
}

#[test]
fn golden_steer_drains_into_last_tool_result() {
    let agent = noop_agent(AgentConfig::default());
    agent.steer("Focus on tests only.");
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("go"));
    ctx.add_message(Message::assistant_with_tool_calls(
        None,
        vec![ToolCall {
            id: "t1".into(),
            function: FunctionCall {
                name: "terminal".into(),
                arguments: r#"{"command":"ls"}"#.into(),
            },
            extra_content: None,
        }],
    ));
    ctx.add_message(Message {
        role: MessageRole::Tool,
        content: Some("output".into()),
        tool_calls: None,
        tool_call_id: Some("t1".into()),
        name: None,
        reasoning_content: None,
        cache_control: None,
    });

    let out = oracle(&agent, &mut ctx);
    let tool = out
        .iter()
        .find(|m| m.role == MessageRole::Tool)
        .unwrap();
    assert!(
        tool
            .content
            .as_deref()
            .unwrap_or("")
            .contains("User guidance:")
    );
    assert!(tool.content.as_deref().unwrap_or("").contains("Focus on tests only."));
}

#[test]
fn golden_retry_idempotent_on_unchanged_ctx() {
    let agent = noop_agent(AgentConfig::default());
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("stable"));

    let first = oracle(&agent, &mut ctx);
    let second = oracle(&agent, &mut ctx);
    assert_eq!(
        canonical_messages_json(&first),
        canonical_messages_json(&second),
        "unchanged ctx should yield identical API messages across retries"
    );
}

#[test]
fn golden_ctx_transcript_unchanged_except_steer_drain() {
    let agent = noop_agent(AgentConfig::default());
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("hi"));
    let before: Vec<Message> = ctx.get_messages().to_vec();

    let _ = oracle(&agent, &mut ctx);
    let after = ctx.get_messages();
    assert_eq!(before.len(), after.len());
    assert_eq!(before[0].content, after[0].content);
    assert_eq!(before[1].content, after[1].content);
}

#[test]
fn golden_provider_body_from_oracle_messages() {
    let agent = noop_agent(AgentConfig::default());
    let mut ctx = ContextManager::new(200_000);
    ctx.add_message(Message::system("sys"));
    ctx.add_message(Message::user("ping"));

    let api_messages = oracle(&agent, &mut ctx);
    let tools = vec![ToolSchema {
        name: "echo".into(),
        description: "echo".into(),
        parameters: JsonSchema::new("object"),
    }];
    let body = GenericProvider::oracle_chat_completions_body(&api_messages, &tools, "gpt-4o");
    assert_eq!(body["model"], "gpt-4o");
    assert!(body["messages"].is_array());
    assert!(body["tools"].is_array());
    assert_eq!(body["messages"].as_array().unwrap().len(), 2);
}

#[test]
fn golden_dual_run_placeholder_for_migration() {
    let agent = noop_agent(AgentConfig::default());

    let legacy = {
        let mut ctx = ContextManager::new(200_000);
        ctx.add_message(Message::system("sys"));
        ctx.add_message(Message::user("dual-run"));
        oracle(&agent, &mut ctx)
    };
    let candidate = {
        let mut ctx = ContextManager::new(200_000);
        ctx.add_message(Message::system("sys"));
        ctx.add_message(Message::user("dual-run"));
        oracle(&agent, &mut ctx)
    };
    assert_dual_run_eq(&legacy, &candidate);
}
