//! Phase A `run_agent.py` contract tests (see `python_alignment` module docs).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use hermes_agent::{
    agent_loop::ToolRegistry,
    plugins::{HookResult, HookType, Plugin, PluginContext, PluginManager, PluginMeta},
    AgentConfig, AgentLoop,
};
use hermes_agent::interrupt::InterruptController;
use tokio_stream::wrappers::ReceiverStream;
use hermes_core::{
    AgentError, FunctionCall, JsonSchema, LlmResponse, Message, MessageRole, StreamChunk,
    StreamDelta, ToolCall, ToolSchema, UsageStats,
};

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
        ctx.on(self.hook, Arc::new(move |_ctx_val: &serde_json::Value| {
            counter.push(label);
            HookResult::Ok
        }));
    }
}

fn register_hook(counter: &HookCounter, pm: &mut PluginManager, hook: HookType, label: &'static str) {
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
            })
        } else {
            Ok(LlmResponse {
                message: Message::assistant("final"),
                usage: None,
                model: "test".into(),
                finish_reason: Some("stop".into()),
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

// --- Phase A-1: new session -------------------------------------------------

#[tokio::test]
async fn phase_a1_new_session_fires_on_session_start() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let counter = HookCounter(events.clone());
    let mut pm = PluginManager::new();
    register_hook(&counter, &mut pm, HookType::OnSessionStart, "on_session_start");

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

// --- Phase A-2: continue session --------------------------------------------

#[tokio::test]
async fn phase_a2_continue_session_skips_on_session_start() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let counter = HookCounter(events.clone());
    let mut pm = PluginManager::new();
    register_hook(&counter, &mut pm, HookType::OnSessionStart, "on_session_start");

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
