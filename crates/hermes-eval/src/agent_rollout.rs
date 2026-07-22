//! [`TaskRollout`](crate::runner::TaskRollout) implementation backed by [`hermes_agent::AgentLoop`].
//!
//! Enable with `--features agent-loop`. Callers construct [`AgentLoop`](hermes_agent::AgentLoop)
//! the same way as `hermes-cli` (config, tool registry, provider), wrap in [`std::sync::Arc`],
//! then pass [`AgentLoopRollout`] to [`Runner::run`](crate::runner::Runner::run).
//!
//! ```ignore
//! use hermes_eval::runner::{Runner, RunnerConfig};
//! use hermes_eval::agent_rollout::AgentLoopRollout;
//! use std::sync::Arc;
//!
//! let agent: Arc<hermes_agent::AgentLoop> = /* from CLI-style setup */;
//! let rollout = Arc::new(AgentLoopRollout::new(agent));
//! let record = Runner::new(RunnerConfig::default())
//!     .run(Arc::new(adapter), rollout)
//!     .await?;
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::AgentLoop;
use hermes_core::Message;
use serde_json::{json, Value};

use crate::adapter::TaskSpec;
use crate::error::{EvalError, EvalResult};
use crate::result::{CostUsd, PricingProvenance, TaskTelemetry, Telemetry};
use crate::runner::{TaskRollout, TaskRolloutResult};

/// Drives [`AgentLoop::run`](hermes_agent::AgentLoop::run) once per eval task (user message = task instruction).
#[derive(Clone)]
pub struct AgentLoopRollout {
    agent: Arc<AgentLoop>,
    full_state: bool,
}

impl AgentLoopRollout {
    pub fn new(agent: Arc<AgentLoop>) -> Self {
        Self {
            agent,
            full_state: false,
        }
    }

    pub fn with_full_state(mut self, full_state: bool) -> Self {
        self.full_state = full_state;
        self
    }

    /// Build the JSON blob passed to the benchmark [`Verifier`](crate::verifier::Verifier).
    ///
    /// Default: compact summary to keep artifacts small; set `full` to embed the whole
    /// [`hermes_core::AgentResult`] (can be very large).
    pub fn summarize_result(
        result: &hermes_core::AgentResult,
        task_id: &str,
        full: bool,
    ) -> EvalResult<Value> {
        if full {
            return serde_json::to_value(result).map_err(EvalError::Serde);
        }
        let last_assistant = result
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let preview: String = last_assistant.chars().take(2000).collect();
        Ok(json!({
            "task_id": task_id,
            "finished_naturally": result.finished_naturally,
            "total_turns": result.total_turns,
            "message_count": result.messages.len(),
            "tool_error_count": result.tool_errors.len(),
            "usage": result.usage,
            "last_assistant_preview": preview,
        }))
    }

    /// Extract only telemetry carried by the returned [`hermes_core::AgentResult`].
    ///
    /// The result does not expose retry or run/proof identifiers, so those
    /// remain typed unavailable rather than being inferred from verifier JSON
    /// or configuration. Invalid cost is rejected while retaining any other
    /// telemetry already observed.
    fn telemetry_from_result(
        result: &hermes_core::AgentResult,
    ) -> Result<TaskTelemetry, (TaskTelemetry, EvalError)> {
        let mut telemetry = TaskTelemetry {
            total_turns: Telemetry::Available(result.total_turns),
            ..TaskTelemetry::default()
        };

        if let Some(usage) = &result.usage {
            telemetry.tokens_input = Telemetry::Available(usage.prompt_tokens);
            telemetry.tokens_output = Telemetry::Available(usage.completion_tokens);
            if let Some(cost) = usage.estimated_cost {
                match CostUsd::new(cost, PricingProvenance::agent_result_usage_estimate()) {
                    Ok(cost) => telemetry.cost_usd = Telemetry::Available(cost),
                    Err(error) => return Err((telemetry, error)),
                }
            }
        }

        Ok(telemetry)
    }
}

#[async_trait]
impl TaskRollout for AgentLoopRollout {
    async fn execute(&self, task: &TaskSpec) -> EvalResult<TaskRolloutResult> {
        let messages = vec![Message::user(task.instruction.clone())];
        let result = self
            .agent
            .run(messages, None)
            .await
            .map_err(|error| EvalError::TaskExecution(error.to_string()))?;
        let verifier_state = Self::summarize_result(&result, &task.task_id, self.full_state)?;
        match Self::telemetry_from_result(&result) {
            Ok(telemetry) => Ok(TaskRolloutResult::completed(verifier_state, telemetry)),
            Err((telemetry, error)) => Ok(TaskRolloutResult::execution_failure(
                error.to_string(),
                telemetry,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use futures::stream::BoxStream;
    use hermes_agent::agent_loop::ToolRegistry;
    use hermes_agent::AgentConfig;
    use hermes_core::{
        AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema, UsageStats,
    };

    use crate::runner::{Runner, RunnerConfig};
    use crate::tblite::TbliteSmokeAdapter;

    struct UsageProvider;

    #[async_trait]
    impl LlmProvider for UsageProvider {
        async fn chat_completion(
            &self,
            _: &[Message],
            _: &[ToolSchema],
            _: Option<u32>,
            _: Option<f64>,
            _: Option<&str>,
            _: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant("stub completion"),
                usage: Some(UsageStats {
                    prompt_tokens: 13,
                    completion_tokens: 7,
                    total_tokens: 20,
                    estimated_cost: Some(0.25),
                }),
                model: "stub/model".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _: &[Message],
            _: &[ToolSchema],
            _: Option<u32>,
            _: Option<f64>,
            _: Option<&str>,
            _: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }

    #[test]
    fn absent_agent_usage_is_explicitly_unavailable() {
        let result = hermes_core::AgentResult {
            total_turns: 2,
            ..hermes_core::AgentResult::default()
        };
        let telemetry =
            AgentLoopRollout::telemetry_from_result(&result).expect("absent usage is valid");

        assert_eq!(telemetry.total_turns, Telemetry::Available(2));
        assert!(matches!(telemetry.tokens_input, Telemetry::Unavailable));
        assert!(matches!(telemetry.tokens_output, Telemetry::Unavailable));
        assert!(matches!(telemetry.cost_usd.as_ref(), Telemetry::Unavailable));
        assert!(matches!(telemetry.retry_count, Telemetry::Unavailable));
        assert_eq!(
            telemetry.provenance,
            crate::result::RolloutProvenance::unavailable()
        );
    }

    #[test]
    fn invalid_agent_usage_cost_is_rejected_without_erasing_observed_usage() {
        let result = hermes_core::AgentResult {
            total_turns: 2,
            usage: Some(hermes_core::UsageStats {
                prompt_tokens: 13,
                completion_tokens: 7,
                total_tokens: 20,
                estimated_cost: Some(-0.25),
            }),
            ..hermes_core::AgentResult::default()
        };
        let (telemetry, error) =
            AgentLoopRollout::telemetry_from_result(&result).expect_err("negative cost rejected");

        assert!(matches!(error, EvalError::TaskExecution(_)));
        assert_eq!(telemetry.tokens_input, Telemetry::Available(13));
        assert_eq!(telemetry.tokens_output, Telemetry::Available(7));
        assert_eq!(telemetry.total_turns, Telemetry::Available(2));
        assert!(matches!(telemetry.cost_usd.as_ref(), Telemetry::Unavailable));
    }

    #[tokio::test]
    async fn agent_loop_usage_reaches_task_results_and_aggregate() {
        let agent = Arc::new(AgentLoop::new(
            AgentConfig {
                max_turns: 1,
                model: "stub/model".into(),
                provider: Some("stub-provider".into()),
                skip_memory: true,
                skip_context_files: true,
                ..AgentConfig::default()
            },
            Arc::new(ToolRegistry::new()),
            Arc::new(UsageProvider),
        ));
        let record = Runner::new(RunnerConfig {
            concurrency: 1,
            ..RunnerConfig::default()
        })
        .run(
            Arc::new(TbliteSmokeAdapter),
            Arc::new(AgentLoopRollout::new(agent)),
        )
        .await
        .expect("agent rollout run");

        assert_eq!(record.tasks.len(), 2);
        for task in &record.tasks {
            assert_eq!(task.tokens_input, Telemetry::Available(13));
            assert_eq!(task.tokens_output, Telemetry::Available(7));
            assert_eq!(task.agent_turns, Telemetry::Available(1));
            assert!(matches!(
                task.cost_usd.as_ref(),
                Telemetry::Available(cost) if (cost.usd() - 0.25).abs() < f64::EPSILON
            ));
        }
        assert_eq!(record.metrics.total_tokens_input, Telemetry::Available(26));
        assert_eq!(record.metrics.total_tokens_output, Telemetry::Available(14));
        assert_eq!(record.metrics.total_agent_turns, Telemetry::Available(2));
        assert!(matches!(
            record.metrics.total_cost_usd,
            Telemetry::Available(cost) if (cost - 0.5).abs() < f64::EPSILON
        ));
    }
}
