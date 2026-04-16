use std::sync::Arc;

use hermes_agent::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use hermes_agent::AgentError;
use hermes_core::{JsonSchema, LlmProvider, Message, StreamChunk, ToolSchema};
use serde::Deserialize;

use futures::stream::BoxStream;
use futures::StreamExt;

#[derive(Debug, Deserialize)]
struct FixtureCase {
    name: String,
    runs: u32,
    memory_nudge_interval: u32,
    skill_creation_nudge_interval: u32,
    register_memory_tool: bool,
    register_skill_tool: bool,
    expected_turns_since_memory: u32,
    expected_iters_since_skill: u32,
}

#[derive(Clone)]
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
        Ok(hermes_core::LlmResponse {
            message: Message::assistant("done"),
            usage: None,
            model: "dummy".into(),
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

#[test]
fn parity_python_v2026_4_13_self_evolution_fixtures() {
    let raw = include_str!("fixtures/python_v2026_4_13_self_evolution.json");
    let cases: Vec<FixtureCase> = serde_json::from_str(raw).expect("valid fixture json");
    let rt = tokio::runtime::Runtime::new().expect("runtime");

    for case in cases {
        let mut registry = ToolRegistry::new();
        if case.register_memory_tool {
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );
        }
        if case.register_skill_tool {
            registry.register(
                "skill_manage",
                ToolSchema::new("skill_manage", "Skill tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );
        }

        let config = AgentConfig {
            memory_nudge_interval: case.memory_nudge_interval,
            skill_creation_nudge_interval: case.skill_creation_nudge_interval,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));

        for _ in 0..case.runs {
            let _ = rt
                .block_on(agent.run(vec![Message::user("fixture run")], None))
                .expect("agent run should succeed");
        }

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(
            counters.turns_since_memory, case.expected_turns_since_memory,
            "fixture={} turns_since_memory mismatch",
            case.name
        );
        assert_eq!(
            counters.iters_since_skill, case.expected_iters_since_skill,
            "fixture={} iters_since_skill mismatch",
            case.name
        );
    }
}
