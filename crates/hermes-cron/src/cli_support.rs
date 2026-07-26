//! Helpers for CLI / tooling: build a [`CronScheduler`] backed by disk + a minimal LLM.
//!
//! This fallback scheduler is suitable for CRUD/status operations and tests.
//! Real execution paths should inject a live [`hermes_core::LlmProvider`].

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use hermes_agent::{agent_loop::ToolRegistry, RunRuntime};
use hermes_config::GatewayConfig;
use hermes_core::{
    AgentError, LlmProvider, LlmResponse, Message, ResolvedExecutionMode, StreamChunk, StreamDelta,
    ToolSchema,
};
use hermes_environments::ExecutionContainmentContext;

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
                "CLI cron manual run (fallback LLM path — configure gateway for real execution).",
            ),
            usage: None,
            model: model.unwrap_or("fallback").to_string(),
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
                    extra: None,
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

/// Build a CRUD/test scheduler that persists jobs under `data_dir`.
///
/// The caller must supply the process-resolved mode and the exact containment
/// context already bound to its agent tool registry. This helper never creates
/// a registry or execution backend.
pub fn cron_scheduler_for_data_dir(
    data_dir: PathBuf,
    tool_registry: Arc<ToolRegistry>,
    run_runtime: Arc<RunRuntime>,
    execution_mode: ResolvedExecutionMode,
    execution_containment: Arc<ExecutionContainmentContext>,
) -> Result<CronScheduler, AgentError> {
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir));
    let llm: Arc<dyn LlmProvider> = Arc::new(MinimalCronLlm);
    let config = GatewayConfig::default();
    let runner = Arc::new(CronRunner::new(
        llm,
        tool_registry,
        run_runtime,
        execution_mode,
        execution_containment,
        &config,
    )?);
    Ok(CronScheduler::new(persistence, runner))
}
