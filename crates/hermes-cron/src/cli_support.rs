//! Helpers for CLI / tooling: build a [`CronScheduler`] backed by disk + a minimal LLM stub.
//!
//! The stub provider is only suitable for **manual `run`** smoke tests and job CRUD;
//! production gateways should inject a real [`hermes_core::LlmProvider`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use hermes_agent::agent_loop::ToolRegistry;
use hermes_core::{
    AgentError, LlmProvider, LlmResponse, Message, StreamChunk, StreamDelta, ToolSchema,
};

use crate::persistence::FileJobPersistence;
use crate::runner::CronRunner;
use crate::scheduler::CronScheduler;

/// Minimal LLM used when the CLI constructs a scheduler without a real provider.
#[derive(Debug)]
pub struct MinimalCronLlm;

#[async_trait]
impl LlmProvider for MinimalCronLlm {
    async fn chat_completion(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> Result<LlmResponse, AgentError> {
        Ok(LlmResponse {
            message: Message::assistant(
                "CLI cron manual run (minimal stub LLM — configure gateway for real execution).",
            ),
            usage: None,
            model: model.unwrap_or("stub").to_string(),
            finish_reason: Some("stop".into()),
        })
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&serde_json::Value>,
    ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
        stream::iter(vec![
            Ok(StreamChunk {
                delta: Some(StreamDelta {
                    content: Some("ok".into()),
                    tool_calls: None,
                }),
                finish_reason: None,
                usage: None,
            }),
            Ok(StreamChunk {
                delta: None,
                finish_reason: Some("stop".into()),
                usage: None,
            }),
        ])
        .boxed()
    }
}

/// Build a scheduler that persists jobs under `data_dir` (e.g. `$HERMES_HOME/cron`).
pub fn cron_scheduler_for_data_dir(data_dir: PathBuf) -> CronScheduler {
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir));
    let llm: Arc<dyn LlmProvider> = Arc::new(MinimalCronLlm);
    let runner = Arc::new(CronRunner::new(llm, Arc::new(ToolRegistry::new())));
    CronScheduler::new(persistence, runner)
}
