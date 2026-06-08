//! Phase A `run_agent.py` contract tests (see `message_sanitization` module docs).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use hermes_agent::interrupt::InterruptController;
use hermes_agent::{
    AgentConfig, AgentLoop,
    agent_loop::ToolRegistry,
    plugins::{HookResult, HookType, Plugin, PluginContext, PluginManager, PluginMeta},
};
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmResponse, Message, MessageRole, StreamChunk,
    StreamDelta, ToolCall, ToolSchema, UsageStats,
};
use tokio_stream::wrappers::ReceiverStream;

// --- shared test helpers ----------------------------------------------------

#[derive(Clone)]
struct HookCounter(Arc<Mutex<Vec<String>>>);

impl HookCounter {
    fn push(&self, label: &str) {
        self.0
            .lock()
            .expect("hook counter lock")
            .push(label.to_string());
    }
}

struct CountingHookPlugin {
    hook: HookType,
    counter: HookCounter,
    label: &'static str,
}

#[async_trait]
impl Plugin for CountingHookPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: format!("counting_{}", self.hook.as_str()),
            version: "0.0.0".into(),
            description: "phase-a hook counter".into(),
            author: None,
        }
    }

    async fn initialize(&self) -> Result<(), AgentError> {
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), AgentError> {
        Ok(())
    }

    fn register(&self, ctx: &mut PluginContext) {
        let counter = self.counter.clone();
        let label = self.label;
        ctx.on(
            self.hook,
            Arc::new(move |_ctx_val: &serde_json::Value| {
                counter.push(label);
                HookResult::Ok
            }),
        );
    }
}

fn register_hook(
    counter: &HookCounter,
    pm: &mut PluginManager,
    hook: HookType,
    label: &'static str,
) {
    pm.register(Arc::new(CountingHookPlugin {
        hook,
        counter: counter.clone(),
        label,
    }));
}

struct StopAssistantProvider;

#[async_trait]
impl hermes_core::LlmProvider for StopAssistantProvider {
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
            message: Message::assistant("done"),
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::empty().boxed()
    }
}

fn echo_tool_registry() -> ToolRegistry {
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
                .unwrap_or("ok")
                .to_string())
        }),
    );
    tools
}

/// Returns tool_calls for the first `tool_turns` LLM calls, then a final stop.
struct ToolThenStopProvider {
    calls: AtomicUsize,
    tool_turns: usize,
}

impl ToolThenStopProvider {
    fn new(tool_turns: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            tool_turns,
        }
    }
}

#[async_trait]
impl hermes_core::LlmProvider for ToolThenStopProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n < self.tool_turns {
            Ok(LlmResponse {
                message: Message::assistant_with_tool_calls(
                    None,
                    vec![ToolCall {
                        id: format!("tc-{n}"),
                        function: FunctionCall {
                            name: "echo_tool".to_string(),
                            arguments: r#"{"msg":"x"}"#.to_string(),
                        },
                        extra_content: None,
                    }],
                ),
                usage: None,
                model: "test".into(),
                finish_reason: Some("tool_calls".into()),

                ..Default::default()
            })
        } else {
            Ok(LlmResponse {
                message: Message::assistant("final"),
                usage: None,
                model: "test".into(),
                finish_reason: Some("stop".into()),
                ..Default::default()
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::empty().boxed()
    }
}

fn last_tool_budget_text(messages: &[Message]) -> Option<String> {
    let content = messages
        .iter()
        .rev()
        .find(|m| m.role == MessageRole::Tool)?
        .content
        .as_ref()?;
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
        if let Some(w) = v.get("_budget_warning").and_then(|w| w.as_str()) {
            return Some(w.to_string());
        }
    }
    if content.contains("[BUDGET") {
        return Some(content.clone());
    }
    None
}

#[tokio::test]
async fn new_session_fires_on_session_start() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let counter = HookCounter(events.clone());
    let mut pm = PluginManager::new();
    register_hook(
        &counter,
        &mut pm,
        HookType::OnSessionStart,
        "on_session_start",
    );

    let cfg = AgentConfig {
        stored_system_prompt: None,
        session_id: Some("phase-a1-new".into()),
        max_turns: 1,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(StopAssistantProvider),
    )
    .with_plugins(Arc::new(Mutex::new(pm)));

    let result = agent.run(vec![Message::user("hi")], None).await;
    assert!(result.is_ok(), "{result:?}");
    let inner = result.unwrap();
    assert!(inner.session_started_hooks_fired);
    let fired = events.lock().expect("events lock");
    assert!(
        fired.iter().any(|e| e == "on_session_start"),
        "expected on_session_start, got {fired:?}"
    );
}

#[tokio::test]
async fn continue_session_skips_on_session_start() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let counter = HookCounter(events.clone());
    let mut pm = PluginManager::new();
    register_hook(
        &counter,
        &mut pm,
        HookType::OnSessionStart,
        "on_session_start",
    );

    const STORED: &str = "STORED_SYSTEM_PROMPT_FOR_PHASE_A2";
    let cfg = AgentConfig {
        stored_system_prompt: Some(STORED.into()),
        session_id: Some("phase-a2-continue".into()),
        max_turns: 1,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(StopAssistantProvider),
    )
    .with_plugins(Arc::new(Mutex::new(pm)));

    let result = agent.run(vec![Message::user("hi")], None).await;
    assert!(result.is_ok(), "{result:?}");
    let inner = result.unwrap();
    assert!(!inner.session_started_hooks_fired);
    let fired = events.lock().expect("events lock");
    assert!(
        !fired.iter().any(|e| e == "on_session_start"),
        "on_session_start must not fire when stored_system_prompt is set, got {fired:?}"
    );
    let system = inner
        .messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .and_then(|m| m.content.as_deref());
    assert_eq!(system, Some(STORED));
}

#[tokio::test]
async fn budget_caution_injected_at_seventy_percent() {
    let provider = Arc::new(ToolThenStopProvider::new(7));
    let cfg = AgentConfig {
        max_turns: 10,
        budget_pressure_enabled: true,
        budget_caution_threshold: 0.7,
        budget_warning_threshold: 0.9,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(echo_tool_registry()), provider);
    let result = agent.run(vec![Message::user("go")], None).await;
    assert!(result.is_ok(), "{result:?}");
    let w = last_tool_budget_text(&result.unwrap().messages)
        .expect("expected budget pressure on a tool result");
    assert!(w.contains("[BUDGET:"), "{w}");
    assert!(!w.contains("BUDGET WARNING"), "{w}");
}

#[tokio::test]
async fn budget_warning_injected_at_ninety_percent() {
    let provider = Arc::new(ToolThenStopProvider::new(9));
    let cfg = AgentConfig {
        max_turns: 10,
        budget_pressure_enabled: true,
        budget_caution_threshold: 0.7,
        budget_warning_threshold: 0.9,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(echo_tool_registry()), provider);
    let result = agent.run(vec![Message::user("go")], None).await;
    assert!(result.is_ok(), "{result:?}");
    let w = last_tool_budget_text(&result.unwrap().messages)
        .expect("expected budget pressure on a tool result");
    assert!(w.contains("BUDGET WARNING"), "{w}");
}

struct EmptyThenOkProvider {
    calls: AtomicUsize,
}

#[async_trait]
impl hermes_core::LlmProvider for EmptyThenOkProvider {
    async fn chat_completion(
        &self,
        _messages: &[Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            Ok(LlmResponse {
                message: Message::assistant(""),
                usage: None,
                model: "test".into(),
                finish_reason: None,
                ..Default::default()
            })
        } else {
            Ok(LlmResponse {
                message: Message::assistant("ok"),
                usage: None,
                model: "test".into(),
                finish_reason: Some("stop".into()),
                ..Default::default()
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::empty().boxed()
    }
}

#[tokio::test]
async fn empty_llm_retry_without_appending_empty_assistant() {
    let provider = Arc::new(EmptyThenOkProvider {
        calls: AtomicUsize::new(0),
    });
    let cfg = AgentConfig {
        max_turns: 1,
        empty_content_max_retries: 2,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), provider.clone());
    let result = agent.run(vec![Message::user("hi")], None).await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let msgs = &result.unwrap().messages;
    let empty_assistants = msgs.iter().filter(|m| {
        m.role == MessageRole::Assistant && m.content.as_deref().is_some_and(|c| c.is_empty())
    });
    assert_eq!(empty_assistants.count(), 0);
}

struct SlowStreamProvider;

#[async_trait]
impl hermes_core::LlmProvider for SlowStreamProvider {
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
            message: Message::assistant("unused"),
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            for i in 0..8 {
                let _ = tx
                    .send(Ok(StreamChunk {
                        delta: Some(StreamDelta {
                            content: Some(format!("part{i} ")),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    }))
                    .await;
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
            let _ = tx
                .send(Ok(StreamChunk {
                    delta: None,
                    finish_reason: Some("stop".into()),
                    usage: None,
                }))
                .await;
        });
        ReceiverStream::new(rx).boxed()
    }
}

#[tokio::test]
async fn stream_interrupt_forwards_deltas_and_stops() {
    let interrupt = InterruptController::new();
    let interrupt_handle = interrupt.clone();
    let cfg = AgentConfig {
        max_turns: 2,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::with_interrupt(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(SlowStreamProvider),
        interrupt,
    );

    let deltas = Arc::new(Mutex::new(Vec::new()));
    let deltas_ref = deltas.clone();

    let run = tokio::spawn(async move {
        agent
            .run_stream(
                vec![Message::user("stream")],
                None,
                Some(Box::new(move |chunk| {
                    if let Some(delta) = chunk.delta {
                        if let Some(text) = delta.content {
                            deltas_ref.lock().expect("deltas lock").push(text);
                        }
                    }
                })),
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(60)).await;
    interrupt_handle.interrupt(None);

    let result = run.await.expect("join").expect("run_stream ok");
    assert!(result.interrupted);
    let parts = deltas.lock().expect("deltas lock");
    assert!(!parts.is_empty(), "expected stream deltas before interrupt");
}

struct CostedStopProvider;

#[async_trait]
impl hermes_core::LlmProvider for CostedStopProvider {
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
            message: Message::assistant("priced"),
            usage: Some(UsageStats {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                estimated_cost: Some(0.042),
                ..Default::default()
            }),
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
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        futures::stream::empty().boxed()
    }
}

#[tokio::test]
async fn agent_result_populates_session_cost_usd() {
    let cfg = AgentConfig {
        max_turns: 1,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(CostedStopProvider),
    );
    let result = agent.run(vec![Message::user("hi")], None).await.unwrap();
    assert_eq!(result.session_cost_usd, Some(0.042));
    assert!(!result.interrupted);
}

#[tokio::test]
async fn agent_result_sets_interrupted_on_graceful_interrupt() {
    let interrupt = InterruptController::new();
    interrupt.interrupt(None);
    let cfg = AgentConfig {
        max_turns: 5,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::with_interrupt(
        cfg,
        Arc::new(ToolRegistry::new()),
        Arc::new(StopAssistantProvider),
        interrupt,
    );
    let result = agent.run(vec![Message::user("hi")], None).await.unwrap();
    assert!(result.interrupted);
}

#[tokio::test]
async fn steer_pre_api_injects_into_last_tool_during_run() {
    let provider = Arc::new(ToolThenStopProvider::new(1));
    let cfg = AgentConfig {
        max_turns: 3,
        ..AgentConfig::default()
    };
    let agent = AgentLoop::new(cfg, Arc::new(echo_tool_registry()), provider);
    assert!(agent.steer("focus on error handling"));

    let result = agent.run(vec![Message::user("go")], None).await;
    assert!(result.is_ok(), "{result:?}");
    let msgs = &result.unwrap().messages;
    let tool_with_guidance = msgs.iter().find(|m| {
        m.role == MessageRole::Tool
            && m.content.as_deref().is_some_and(|c| {
                c.contains("User guidance:") && c.contains("focus on error handling")
            })
    });
    assert!(
        tool_with_guidance.is_some(),
        "steer should inject into last tool result during run"
    );
}
