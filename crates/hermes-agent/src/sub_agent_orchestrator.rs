//! In-process sub-agent orchestrator.
//!
//! Python v2026.4.13 parity: when the primary agent calls `delegate_task`, we
//! actually spawn a bounded child [`AgentLoop`] rather than just returning a
//! signal envelope. The orchestrator is responsible for:
//!
//! - building a restricted child [`AgentConfig`] (depth+1, half-remaining turn
//!   budget, isolated session id)
//! - running the child with a wall-clock timeout and cooperative cancellation
//!   tied to the parent's [`InterruptController`]
//! - persisting a lineage record per sub-agent under
//!   `$HERMES_HOME/subagents/<sub_agent_id>.json` with
//!   started/ended timestamps, status, turn count, usage and cost
//! - returning a JSON string that the agent loop feeds back to the LLM as the
//!   tool result (must include `sub_agent_id` so
//!   [`AgentLoop::delegation_event_from_tool_result`] still fires).
//!
//! Registration: consumers build an [`AgentLoop`] via
//! [`crate::AgentLoop::with_sub_agent_orchestrator`] to swap the default
//! signal-only behavior with real execution.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use hermes_core::{AgentError, LlmProvider, Message, ToolSchema};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::timeout;

use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::run_ledger::RunId;
use crate::run_runtime::RunRuntime;

/// Boxed `Send` future type alias used to short-circuit async-recursion
/// between parent and child [`AgentLoop::execute_tool_calls`] futures.
type BoxSendFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Default wall-clock cap for a single child agent turn-chain.
pub const DEFAULT_SUB_AGENT_TIMEOUT_SECS: u64 = 600;
/// Grace period for cooperative child work cancellation.
const SUB_AGENT_CANCEL_GRACE: Duration = Duration::from_millis(250);
/// Separate bounded grace for the child's governed run finalizer.
const SUB_AGENT_FINALIZATION_GRACE: Duration = Duration::from_secs(2);
/// Floor on child max_turns regardless of parent budget.
pub const SUB_AGENT_MIN_TURNS: u32 = 2;
/// Ceiling on child max_turns to keep sub-tasks bounded.
pub const SUB_AGENT_MAX_TURNS_CAP: u32 = 12;

/// Tools that a delegated child must never advertise. `execute_code` is
/// intentionally absent: child agents may use it to batch mechanical work.
const CHILD_BLOCKED_TOOL_NAMES: &[&str] = &[
    "delegate_task",
    "clarify",
    "memory",
    "send_message",
    "cronjob",
];

fn filter_child_tool_schemas(tool_schemas: Vec<ToolSchema>) -> Vec<ToolSchema> {
    tool_schemas
        .into_iter()
        .filter(|schema| !CHILD_BLOCKED_TOOL_NAMES.contains(&schema.name.as_str()))
        .collect()
}


#[derive(Debug)]
enum CancelAndJoinError {
    Join(tokio::task::JoinError),
    FinalizationProofUnavailable(tokio::task::JoinError),
}

impl std::fmt::Display for CancelAndJoinError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Join(error) => write!(formatter, "child task join failed: {error}"),
            Self::FinalizationProofUnavailable(error) => write!(
                formatter,
                "child cancellation exceeded its finalization grace; durable completion proof is unavailable: {error}"
            ),
        }
    }
}

struct AbortOnDropTask<T: Send + 'static> {
    handle: Option<tokio::task::JoinHandle<T>>,
    interrupt: Option<InterruptController>,
}

impl<T: Send + 'static> AbortOnDropTask<T> {
    fn new(
        handle: tokio::task::JoinHandle<T>,
        interrupt: Option<InterruptController>,
    ) -> Self {
        Self {
            handle: Some(handle),
            interrupt,
        }
    }

    fn handle_mut(&mut self) -> &mut tokio::task::JoinHandle<T> {
        self.handle.as_mut().expect("supervised task handle")
    }

    async fn cancel_and_join(
        &mut self,
        reason: &str,
        timed_out: bool,
    ) -> Result<T, CancelAndJoinError> {
        if let Some(interrupt) = self.interrupt.as_ref() {
            if timed_out {
                interrupt.interrupt_timeout(Some(reason.to_string()));
            } else {
                interrupt.interrupt(Some(reason.to_string()));
            }
        }
        let handle = self.handle_mut();
        match timeout(SUB_AGENT_CANCEL_GRACE, &mut *handle).await {
            Ok(result) => result.map_err(CancelAndJoinError::Join),
            Err(_) => match timeout(SUB_AGENT_FINALIZATION_GRACE, &mut *handle).await {
                Ok(result) => result.map_err(CancelAndJoinError::Join),
                Err(_) => {
                    handle.abort();
                    match (&mut *handle).await {
                        Ok(result) => Ok(result),
                        Err(error) => Err(CancelAndJoinError::FinalizationProofUnavailable(error)),
                    }
                }
            },
        }
    }
}

impl<T: Send + 'static> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        let Some(mut handle) = self.handle.take() else {
            return;
        };
        if handle.is_finished() {
            return;
        }
        if let Some(interrupt) = self.interrupt.take() {
            interrupt.interrupt(Some("orchestration owner dropped".to_string()));
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if timeout(SUB_AGENT_CANCEL_GRACE, &mut handle).await.is_err()
                    && timeout(SUB_AGENT_FINALIZATION_GRACE, &mut handle)
                        .await
                        .is_err()
                {
                    handle.abort();
                    let _ = handle.await;
                }
            });
        } else {
            handle.abort();
        }
    }
}

/// Lineage record persisted to disk for each spawned sub-agent.
///
/// One record is written at `started` and rewritten at terminal state. The file
/// layout stays intentionally flat so it can also be inspected / tailed by
/// other tooling (Python baseline uses a similar per-sub-agent JSON file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubAgentLineage {
    pub sub_agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    pub task: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub depth: u32,
    pub max_depth: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_budget_remaining_usd: Option<f64>,
    pub status: SubAgentStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub total_turns: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_cost_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Terminal state vocabulary for sub-agent lineage records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubAgentStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
    Timeout,
}

/// Parameters forwarded by the `delegate_task` tool invocation.
#[derive(Debug, Clone, Default)]
pub struct SubAgentRequest {
    pub task: String,
    pub context: Option<String>,
    pub toolset: Option<String>,
    /// Parent-advertised tools to inherit when no explicit child toolset is requested.
    pub inherited_tool_schemas: Vec<ToolSchema>,
    pub model: Option<String>,
    /// Child depth injected by parent agent loop (parent_depth + 1).
    pub child_depth: u32,
    /// Propagated max depth for lineage / bookkeeping.
    pub max_depth: u32,
    /// Parent budget remaining (USD) — exposed to the child for cost awareness.
    pub parent_budget_remaining_usd: Option<f64>,
    /// Authoritative parent run for child provenance.
    pub parent_run_id: Option<RunId>,
    /// Run the child detached and deliver completion through the background
    /// delegation callback instead of blocking the parent tool turn.
    pub background: bool,
}

/// Configuration for [`SubAgentOrchestrator`].
#[derive(Clone)]
pub struct SubAgentOrchestratorConfig {
    pub parent_config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub parent_credential_pool: Option<Arc<CredentialPool>>,
    pub parent_interrupt: InterruptController,
    pub hermes_home: PathBuf,
    pub parent_session_id: Option<String>,
    pub timeout: Duration,
}

/// In-process executor for `delegate_task` tool calls.
pub struct SubAgentOrchestrator {
    cfg: SubAgentOrchestratorConfig,
    run_runtime: Arc<RunRuntime>,
}

impl SubAgentOrchestrator {
    pub fn new(cfg: SubAgentOrchestratorConfig, run_runtime: Arc<RunRuntime>) -> Self {
        Self { cfg, run_runtime }
    }

    /// Default orchestrator that mirrors the current `AgentLoop`'s wiring.
    ///
    /// The returned orchestrator inherits the parent's tool registry, provider
    /// and interrupt handle so cancellation propagates from the parent to any
    /// actively running child.
    pub fn from_parent(parent: &AgentLoop, hermes_home: PathBuf) -> Result<Self, AgentError> {
        let run_runtime = parent.run_runtime.clone().ok_or_else(|| {
            AgentError::Config(
                "sub-agent orchestration requires an attached run-record runtime".to_string(),
            )
        })?;
        Ok(Self {
            cfg: SubAgentOrchestratorConfig {
                parent_config: parent.config.clone(),
                tool_registry: parent.tool_registry.clone(),
                llm_provider: parent.llm_provider.clone(),
                parent_credential_pool: parent.primary_credential_pool.clone(),
                parent_interrupt: parent.interrupt.clone(),
                hermes_home,
                parent_session_id: parent.config.session_id.clone(),
                timeout: Duration::from_secs(DEFAULT_SUB_AGENT_TIMEOUT_SECS),
            },
            run_runtime,
        })
    }

    /// Execute a `delegate_task` request synchronously from the perspective of
    /// the caller, returning a JSON string suitable for use as the tool result.
    ///
    /// The returned JSON always contains `sub_agent_id` at the top level so
    /// existing delegation-event wiring in [`AgentLoop`] continues to work.
    ///
    /// Returning `BoxSendFuture` (rather than an `async fn`) is load-bearing:
    /// the parent `AgentLoop::execute_tool_calls` awaits this future, and the
    /// child may eventually call back into an orchestrator of its own. A
    /// concrete `impl Future` type would force the compiler to recursively
    /// prove `Send` through that cycle, which it cannot. Boxing into
    /// `dyn Future + Send` erases the child's future type and breaks the
    /// cycle.
    pub fn execute(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
        let this = self.clone();
        Box::pin(async move {
            if req.background {
                json!({
                    "sub_agent_id": null,
                    "status": "rejected",
                    "background": true,
                    "task": req.task,
                    "error": "background delegation requires durable scheduling and governed completion delivery",
                })
                .to_string()
            } else {
                let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
                this.execute_inner(req, sub_agent_id).await
            }
        })
    }


    async fn execute_inner(self: Arc<Self>, req: SubAgentRequest, sub_agent_id: String) -> String {
        let started_at = Utc::now();

        let mut lineage = SubAgentLineage {
            sub_agent_id: sub_agent_id.clone(),
            parent_session_id: self.cfg.parent_session_id.clone(),
            task: req.task.clone(),
            context: req.context.clone(),
            toolset: req.toolset.clone(),
            model: clean_config_string(req.model.as_deref())
                .or_else(|| clean_config_string(self.cfg.parent_config.delegation_model.as_deref()))
                .map(str::to_string),
            provider: self.effective_delegation_provider(),
            depth: req.child_depth,
            max_depth: req.max_depth,
            parent_budget_remaining_usd: req.parent_budget_remaining_usd,
            status: SubAgentStatus::Started,
            started_at,
            ended_at: None,
            total_turns: 0,
            prompt_tokens: None,
            completion_tokens: None,
            estimated_cost_usd: None,
            error: None,
        };

        // Best-effort lineage persistence — failures must never break delegation.
        self.persist_lineage(&lineage).await;

        // Build and run the child.
        let outcome = self.run_child(&req, &sub_agent_id).await;

        match outcome {
            Ok(result) => {
                lineage.status = SubAgentStatus::Completed;
                lineage.total_turns = result.total_turns;
                if let Some(usage) = result.usage.as_ref() {
                    lineage.prompt_tokens = Some(usage.prompt_tokens);
                    lineage.completion_tokens = Some(usage.completion_tokens);
                    lineage.estimated_cost_usd = usage.estimated_cost;
                }
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

                let final_text = extract_final_assistant_text(&result.messages);
                json!({
                    "sub_agent_id": sub_agent_id,
                    "status": "completed",
                    "background": req.background,
                    "task": req.task,
                    "total_turns": result.total_turns,
                    "finished_naturally": result.finished_naturally,
                    "result": final_text,
                    "usage": result.usage.as_ref().map(|u| json!({
                        "prompt_tokens": u.prompt_tokens,
                        "completion_tokens": u.completion_tokens,
                        "total_tokens": u.total_tokens,
                        "estimated_cost": u.estimated_cost,
                    })),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
            Err(e) => {
                let status = match &e {
                    SubAgentError::Timeout => SubAgentStatus::Timeout,
                    SubAgentError::Cancelled => SubAgentStatus::Cancelled,
                    SubAgentError::Agent(_) => SubAgentStatus::Failed,
                };
                lineage.status = status;
                lineage.error = Some(e.to_string());
                lineage.ended_at = Some(Utc::now());
                self.persist_lineage(&lineage).await;

                json!({
                    "sub_agent_id": sub_agent_id,
                    "background": req.background,
                    "status": match status {
                        SubAgentStatus::Timeout => "timeout",
                        SubAgentStatus::Cancelled => "cancelled",
                        SubAgentStatus::Failed => "failed",
                        _ => "failed",
                    },
                    "task": req.task,
                    "error": e.to_string(),
                    "depth": req.child_depth,
                    "max_depth": req.max_depth,
                })
                .to_string()
            }
        }
    }

    async fn run_child(
        &self,
        req: &SubAgentRequest,
        sub_agent_id: &str,
    ) -> Result<hermes_core::AgentResult, SubAgentError> {
        let child_config = self.build_child_config(req, sub_agent_id);
        let child_provider = self.child_llm_provider(&child_config)?;
        let tool_registry = self.cfg.tool_registry.clone();
        let child_depth = req.child_depth;
        let run_runtime = self.run_runtime.clone();
        let parent_run_id = req.parent_run_id.ok_or_else(|| {
            SubAgentError::Agent(AgentError::Config(
                "sub-agent execution requires parent run lineage".to_string(),
            ))
        })?;
        // `None` means no child override: preserve the parent's exact advertised
        // set, including an intentionally empty set. Passing `None` to
        // `AgentLoop::run` would instead re-advertise the whole child registry.
        // Apply the child capability boundary after the parent has selected its
        // schemas so a mixed parent bundle cannot re-expose sensitive tools.
        let inherited_tools = req
            .toolset
            .is_none()
            .then(|| filter_child_tool_schemas(req.inherited_tool_schemas.clone()));
        let initial = initial_messages(&req.task, req.context.as_deref());

        let child_interrupt = InterruptController::new();
        let child_interrupt_for_spawn = child_interrupt.clone();



        // Run the child on its own tokio task so that its `impl Future` type is
        // fully erased behind a `JoinHandle`, breaking async recursion with the
        // parent's `execute_tool_calls` future (which hosts the orchestrator
        // call). `tokio::spawn` requires `Send + 'static`, which forces child
        // `AgentLoop::run` to already be Send — this is also what the existing
        // `spawn_background_review` relies on.
        let mut child_task = AbortOnDropTask::new(
            tokio::spawn(async move {
                let child_agent = AgentLoop::with_interrupt(
                    child_config,
                    tool_registry,
                    child_provider,
                    child_interrupt_for_spawn,
                )
                .with_run_runtime(run_runtime)
                .with_parent_run_id(parent_run_id)
                .with_delegate_depth(child_depth);
                child_agent.run(initial, inherited_tools).await
            }),
            Some(child_interrupt.clone()),
        );

        tokio::select! {
            biased;
            () = self.cfg.parent_interrupt.fatal_cancelled() => {
                let child_result = child_task
                    .cancel_and_join("parent cancelled", false)
                    .await
                    .map_err(cancellation_join_error)?;
                cancelled_child_outcome(child_result, false)
            }
            timed = timeout(self.cfg.timeout, child_task.handle_mut()) => {
                match timed {
                    Ok(Ok(Ok(result))) if result.interrupted => Err(SubAgentError::Cancelled),
                    Ok(Ok(Ok(result))) => Ok(result),
                    Ok(Ok(Err(AgentError::Interrupted { .. }))) => Err(SubAgentError::Cancelled),
                    Ok(Ok(Err(e))) => Err(SubAgentError::Agent(e)),
                    Ok(Err(join_err)) => Err(SubAgentError::Agent(
                        AgentError::ToolExecution(format!("sub-agent join error: {join_err}")),
                    )),
                    Err(_) => {
                        let child_result = child_task
                            .cancel_and_join("timeout", true)
                            .await
                            .map_err(cancellation_join_error)?;
                        cancelled_child_outcome(child_result, true)
                    }
                }
            }
        }
    }

    fn build_child_config(&self, req: &SubAgentRequest, sub_agent_id: &str) -> AgentConfig {
        let parent = &self.cfg.parent_config;

        let remaining_parent_turns = parent
            .max_turns
            .saturating_sub(req.child_depth.saturating_mul(2));
        let child_turns = remaining_parent_turns
            .saturating_div(2)
            .clamp(SUB_AGENT_MIN_TURNS, SUB_AGENT_MAX_TURNS_CAP);

        let child_session_id = match parent.session_id.as_deref() {
            Some(s) if !s.is_empty() => format!("{}::{}", s, sub_agent_id),
            _ => sub_agent_id.to_string(),
        };

        let mut child = parent.clone();
        child.session_id = Some(child_session_id);
        child.max_turns = child_turns;
        child.stream = false;
        // Keep child sessions silent/contained (Python child quiet_mode semantics).
        child.background_review_enabled = false;
        child.background_review_metrics_enabled = false;
        child.memory_nudge_interval = 0;
        child.skill_creation_nudge_interval = 0;
        child.quiet_mode = true;
        // Children should not re-spawn grandchildren beyond the contract;
        // depth checks inside AgentLoop::execute_tool_calls still apply.
        child.skip_memory = true;

        let requested_model = clean_config_string(req.model.as_deref())
            .map(str::to_string)
            .or_else(|| {
                clean_config_string(parent.delegation_model.as_deref()).map(str::to_string)
            });
        let runtime_provider = self.effective_delegation_provider();

        if let Some(provider) = runtime_provider {
            let model = requested_model.unwrap_or_else(|| model_part(parent.model.as_str()));
            child.provider = Some(provider.clone());
            child.model = format!("{provider}:{}", strip_provider_prefix(model.as_str()));
        } else if let Some(model) = requested_model {
            // Model-only override inherits the parent provider and credentials.
            child.model = model;
        }

        // Budget: hard-cap child by parent remaining budget when known.
        if let Some(remaining) = req.parent_budget_remaining_usd {
            let cap = remaining.max(0.0);
            child.max_cost_usd = Some(match child.max_cost_usd {
                Some(existing) => existing.min(cap),
                None => cap,
            });
        }
        child
    }

    fn effective_delegation_provider(&self) -> Option<String> {
        let parent = &self.cfg.parent_config;
        clean_config_string(parent.delegation_provider.as_deref())
            .map(str::to_string)
            .or_else(|| {
                let has_direct_endpoint =
                    clean_config_string(parent.delegation_base_url.as_deref()).is_some()
                        || clean_config_string(parent.delegation_api_key.as_deref()).is_some();
                has_direct_endpoint.then(|| "custom".to_string())
            })
    }

    fn child_llm_provider(
        &self,
        child_config: &AgentConfig,
    ) -> Result<Arc<dyn LlmProvider>, SubAgentError> {
        let Some(provider) = self.effective_delegation_provider() else {
            return Ok(self.cfg.llm_provider.clone());
        };

        let model_name = strip_provider_prefix(child_config.model.as_str()).to_string();
        let mut resolver = AgentLoop::with_interrupt(
            child_config.clone(),
            self.cfg.tool_registry.clone(),
            self.cfg.llm_provider.clone(),
            InterruptController::new(),
        );
        if let Some(pool) = self.cfg.parent_credential_pool.as_ref() {
            resolver = resolver.with_primary_credential_pool(pool.clone());
        }

        resolver
            .build_delegation_runtime_provider(
                provider.as_str(),
                model_name.as_str(),
                clean_config_string(child_config.delegation_base_url.as_deref()),
                clean_config_string(child_config.delegation_api_key.as_deref()),
            )
            .map_err(SubAgentError::Agent)
    }

    async fn persist_lineage(&self, lineage: &SubAgentLineage) {
        let dir = self.cfg.hermes_home.join("subagents");
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            tracing::debug!(error = %e, "sub-agent lineage dir create failed");
            return;
        }
        let path = dir.join(format!("{}.json", lineage.sub_agent_id));
        let body = match serde_json::to_string_pretty(lineage) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(error = %e, "sub-agent lineage serialize failed");
                return;
            }
        };
        if let Err(e) = tokio::fs::write(&path, body).await {
            tracing::debug!(error = %e, path = %path.display(), "sub-agent lineage write failed");
        }
    }
}

#[derive(Debug)]
enum SubAgentError {
    Timeout,
    Cancelled,
    Agent(AgentError),
}

impl std::fmt::Display for SubAgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SubAgentError::Timeout => write!(f, "sub-agent timed out"),
            SubAgentError::Cancelled => write!(f, "sub-agent cancelled"),
            SubAgentError::Agent(e) => write!(f, "sub-agent error: {}", e),
        }
    }
}

fn cancellation_join_error(error: CancelAndJoinError) -> SubAgentError {
    SubAgentError::Agent(AgentError::ToolExecution(format!(
        "sub-agent cancellation finalization failed: {error}"
    )))
}

fn cancelled_child_outcome(
    child_result: Result<hermes_core::AgentResult, AgentError>,
    requested_timeout: bool,
) -> Result<hermes_core::AgentResult, SubAgentError> {
    match child_result {
        // A concurrently completed child has supplied valid proof, but the
        // parent cancellation/timeout still wins this delegation outcome.
        Ok(_) => Err(if requested_timeout {
            SubAgentError::Timeout
        } else {
            SubAgentError::Cancelled
        }),
        Err(AgentError::Timeout(_)) => Err(SubAgentError::Timeout),
        Err(AgentError::Interrupted { .. }) => Err(SubAgentError::Cancelled),
        Err(error) => Err(SubAgentError::Agent(error)),
    }
}

fn clean_config_string(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn strip_provider_prefix(model: &str) -> &str {
    model
        .split_once(':')
        .map(|(_, model)| model.trim())
        .filter(|model| !model.is_empty())
        .unwrap_or_else(|| model.trim())
}

fn model_part(model: &str) -> String {
    strip_provider_prefix(model).to_string()
}

fn initial_messages(task: &str, context: Option<&str>) -> Vec<Message> {
    let mut content = task.to_string();
    if let Some(ctx) = context {
        if !ctx.trim().is_empty() {
            content = format!("{task}\n\n[Context]\n{ctx}");
        }
    }
    vec![Message::user(&content)]
}

fn extract_final_assistant_text(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default()
}


#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream::{BoxStream, StreamExt};
    use hermes_core::{JsonSchema, LlmResponse, StreamChunk, ToolSchema};
    use std::sync::{Arc, Mutex};

    // Minimal test provider that returns a deterministic assistant response
    // so the child loop finishes in a single turn. Behavioural tests live
    // in agent_loop::tests.
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
            Ok(LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "noop".into(),
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
    struct RecordingProvider {
        seen_tools: Arc<Mutex<Vec<Vec<String>>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RecordingProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            self.seen_tools
                .lock()
                .expect("seen tools lock")
                .push(tools.iter().map(|tool| tool.name.clone()).collect());
            Ok(LlmResponse {
                message: Message::assistant("done"),
                usage: None,
                model: "recording".into(),
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


    struct TestOrchestrator {
        inner: Arc<SubAgentOrchestrator>,
        parent_run_id: RunId,
        _parent_run: Arc<crate::run_runtime::ActiveRun>,
    }

    impl std::ops::Deref for TestOrchestrator {
        type Target = SubAgentOrchestrator;

        fn deref(&self) -> &Self::Target {
            &self.inner
        }
    }

    impl TestOrchestrator {
        fn execute(self: &Arc<Self>, req: SubAgentRequest) -> BoxSendFuture<String> {
            self.inner.execute(req)
        }
    }

    fn test_config_orchestrator(cfg: SubAgentOrchestratorConfig) -> SubAgentOrchestrator {
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(cfg.hermes_home.join("run-runtime"))
                .expect("ephemeral run runtime"),
        );
        SubAgentOrchestrator::new(cfg, runtime)
    }

    fn test_orchestrator(cfg: SubAgentOrchestratorConfig) -> TestOrchestrator {
        let parent_tools = cfg.tool_registry.schemas();
        test_orchestrator_with_parent_tools(cfg, parent_tools)
    }

    fn test_orchestrator_with_parent_tools(
        cfg: SubAgentOrchestratorConfig,
        parent_tools: Vec<ToolSchema>,
    ) -> TestOrchestrator {
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(cfg.hermes_home.join("run-runtime"))
                .expect("ephemeral run runtime"),
        );
        let parent_messages = vec![Message::user("parent")];
        let parent_start_evidence =
            crate::run_runtime::PreparedRunStartEvidence::new(
                &parent_messages,
                &parent_tools,
                crate::run_runtime::PreparedSystemPromptEvidence::Fresh {
                    content: "sub-agent orchestrator parent test system prompt",
                    injected_skills_block: None,
                },
            );
        let containment =
            hermes_environments::ExecutionContainmentContext::manual(
                cfg.parent_config.execution_mode,
            )
            .expect("manual parent run test context");
        let parent_run = runtime
            .start_run(
                &cfg.parent_config,
                &containment,
                &cfg.tool_registry,
                parent_start_evidence,
                &crate::run_runtime::RunInvocationContext::default(),
                None,
            )
            .expect("parent run record");
        let parent_run_id = parent_run.run_id();
        TestOrchestrator {
            inner: Arc::new(SubAgentOrchestrator::new(cfg, runtime)),
            parent_run_id,
            _parent_run: parent_run,
        }
    }

    #[tokio::test]
    async fn omitted_toolset_inherits_parent_advertised_tools() {
        let terminal = ToolSchema::new(
            "terminal",
            "Terminal tool",
            JsonSchema::new("object"),
        );
        let mut registry = ToolRegistry::new();
        registry.register_test_observation(
            "terminal",
            terminal.clone(),
            Arc::new(|_| Ok("unused".to_string())),
        );
        let registry = Arc::new(registry);

        let seen_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            tool_registry: registry,
            llm_provider: Arc::new(RecordingProvider {
                seen_tools: seen_tools.clone(),
            }),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "child task".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                inherited_tool_schemas: vec![terminal],
                ..Default::default()
            })
            .await;

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "completed");
        let seen = seen_tools.lock().expect("seen tools lock");
        assert_eq!(seen.as_slice(), &[vec!["terminal".to_string()]]);
    }

    #[tokio::test]
    async fn omitted_toolset_blocks_child_only_tools_but_keeps_execute_code() {
        let schemas = [
            "terminal",
            "execute_code",
            "delegate_task",
            "clarify",
            "memory",
            "send_message",
            "cronjob",
        ]
        .into_iter()
        .map(|name| ToolSchema::new(name, format!("{name} tool"), JsonSchema::new("object")))
        .collect::<Vec<_>>();
        let mut registry = ToolRegistry::new();
        for schema in &schemas {
            registry.register_test_observation(
                &schema.name,
                schema.clone(),
                Arc::new(|_| Ok("unused".to_string())),
            );
        }
        let seen_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(registry),
            llm_provider: Arc::new(RecordingProvider {
                seen_tools: seen_tools.clone(),
            }),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "child task".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                inherited_tool_schemas: schemas,
                ..Default::default()
            })
            .await;

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "completed");
        let seen = seen_tools.lock().expect("seen tools lock");
        assert_eq!(
            seen.as_slice(),
            &[vec!["terminal".to_string(), "execute_code".to_string()]]
        );
    }

    #[tokio::test]
    async fn omitted_toolset_preserves_an_empty_parent_advertisement() {
        let seen_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
        let mut registry = ToolRegistry::new();
        registry.register_test_observation(
            "registry_only",
            ToolSchema::new(
                "registry_only",
                "A registered tool the parent did not advertise",
                JsonSchema::new("object"),
            ),
            Arc::new(|_| Ok("unused".to_string())),
        );
        let registry = Arc::new(registry);
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator_with_parent_tools(
            SubAgentOrchestratorConfig {
                parent_config: AgentConfig {
                    max_turns: 2,
                    ..AgentConfig::default()
                },
                tool_registry: registry,
                llm_provider: Arc::new(RecordingProvider {
                    seen_tools: seen_tools.clone(),
                }),
                parent_credential_pool: None,
                parent_interrupt: InterruptController::new(),
                hermes_home: tmp.path().to_path_buf(),
                parent_session_id: Some("parent".into()),
                timeout: Duration::from_secs(2),
            },
            Vec::new(),
        ));

        let out = orch
            .execute(SubAgentRequest {
                task: "child task".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                ..Default::default()
            })
            .await;

        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "completed");
        let seen = seen_tools.lock().expect("seen tools lock");
        assert_eq!(seen.as_slice(), &[Vec::<String>::new()]);
    }

    #[tokio::test]
    async fn spawn_timeout_path_produces_timeout_json() {
        struct SlowProvider {
            active: Arc<std::sync::atomic::AtomicBool>,
            entered: Arc<tokio::sync::Notify>,
        }
        struct ActiveGuard(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for ActiveGuard {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::Release);
            }
        }
        #[async_trait::async_trait]
        impl LlmProvider for SlowProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                self.active
                    .store(true, std::sync::atomic::Ordering::Release);
                self.entered.notify_one();
                let _active_guard = ActiveGuard(self.active.clone());
                tokio::time::sleep(Duration::from_secs(5)).await;
                Ok(LlmResponse {
                    message: Message::assistant("late"),
                    usage: None,
                    model: "slow".into(),
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

        let parent_cfg = AgentConfig {
            max_turns: 2,
            session_id: Some("parent".into()),
            model: "slow".into(),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let child_active = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_entered = Arc::new(tokio::sync::Notify::new());
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent_cfg,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(SlowProvider {
                active: child_active.clone(),
                entered: child_entered.clone(),
            }),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
        }));
        let execution_orchestrator = orch.clone();
        let execution = tokio::spawn(async move {
            execution_orchestrator
                .execute(SubAgentRequest {
                    task: "test task".into(),
                    child_depth: 1,
                    max_depth: 4,
                    parent_run_id: Some(execution_orchestrator.parent_run_id),
                    ..Default::default()
                })
                .await
        });
        timeout(Duration::from_secs(3), child_entered.notified())
            .await
            .expect("slow provider was never entered");
        let out = timeout(Duration::from_secs(5), execution)
            .await
            .expect("timed-out orchestration did not return")
            .expect("orchestration task panicked");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "timeout");
        assert!(
            !child_active.load(std::sync::atomic::Ordering::Acquire),
            "timed-out child remained active after orchestration returned"
        );
        assert!(parsed["sub_agent_id"]
            .as_str()
            .unwrap()
            .starts_with("subagent-"));
        let child_session = format!(
            "parent::{}",
            parsed["sub_agent_id"].as_str().expect("sub-agent id")
        );
        let child_runs = orch
            .inner
            .run_runtime
            .session_runs(
                &child_session,
                crate::run_runtime::SessionRunSelection::All,
            )
            .expect("child run inspection");
        assert_eq!(child_runs.len(), 1);
        // The deadline cancelled an already-dispatched provider request, so the
        // ledger must preserve its unknown one-shot outcome rather than claim
        // that timeout proved the remote request had no effect.
        assert_eq!(
            child_runs[0].status.exit_terminal_reason,
            Some(crate::run_ledger::TerminalReason::EffectNonResumableUnknown)
        );
        // Lineage file should exist under $HERMES_HOME/subagents/<id>.json
        let dir = tmp.path().join("subagents");
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        let entry = entries.next_entry().await.unwrap().unwrap();
        assert!(entry
            .path()
            .extension()
            .map(|s| s == "json")
            .unwrap_or(false));
    }


    #[tokio::test]
    async fn provider_panic_is_terminalized_without_leaving_a_running_child() {
        struct PanicProvider;

        #[async_trait::async_trait]
        impl LlmProvider for PanicProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<LlmResponse, AgentError> {
                panic!("provider panic fixture");
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

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                session_id: Some("parent".into()),
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(PanicProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "panic".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "failed");
        assert!(parsed["error"]
            .as_str()
            .expect("panic error")
            .contains("agent run panicked"));

        let child_session = format!(
            "parent::{}",
            parsed["sub_agent_id"].as_str().expect("sub-agent id")
        );
        let child_runs = orch
            .inner
            .run_runtime
            .session_runs(
                &child_session,
                crate::run_runtime::SessionRunSelection::All,
            )
            .expect("panic run inspection");
        assert_eq!(child_runs.len(), 1);
        assert_eq!(
            child_runs[0].status.exit_terminal_reason,
            Some(crate::run_ledger::TerminalReason::EffectNonResumableUnknown)
        );
    }
    #[tokio::test]
    async fn background_request_is_rejected_without_durable_scheduler() {
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: AgentConfig {
                max_turns: 2,
                ..AgentConfig::default()
            },
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_secs(2),
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "background task".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                background: true,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "rejected");
        assert_eq!(parsed["background"], true);
        assert!(parsed["sub_agent_id"].is_null());
        assert!(parsed["error"]
            .as_str()
            .unwrap()
            .contains("durable scheduling"));
        assert!(!tmp.path().join("subagents").exists());
    }

    #[tokio::test]
    async fn cancel_path_reports_cancelled() {
        let parent_ctrl = InterruptController::new();
        parent_ctrl.interrupt(Some("stop".into())); // pre-cancel before spawn

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: AgentConfig::default(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: parent_ctrl,
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: None,
            timeout: Duration::from_secs(2),
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "noop".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "cancelled");
        assert!(parsed["sub_agent_id"].as_str().is_some());
    }

    #[test]
    fn build_child_config_clamps_turns() {
        let parent = AgentConfig {
            max_turns: 20,
            model: "m".into(),
            session_id: Some("root".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = test_config_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
        });
        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                parent_budget_remaining_usd: Some(1.25),
                ..Default::default()
            },
            "subagent-test",
        );
        assert!(child.max_turns >= SUB_AGENT_MIN_TURNS);
        assert!(child.max_turns <= SUB_AGENT_MAX_TURNS_CAP);
        assert_eq!(child.session_id.as_deref(), Some("root::subagent-test"));
        assert_eq!(child.max_cost_usd, Some(1.25));
        assert!(child.skip_memory);
        assert!(!child.background_review_enabled);
        assert!(!child.background_review_metrics_enabled);
        assert_eq!(child.memory_nudge_interval, 0);
        assert_eq!(child.skill_creation_nudge_interval, 0);
        assert!(child.quiet_mode);
    }

    #[test]
    fn build_child_config_model_only_override_inherits_parent_provider() {
        let inherited_provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("google/gemini-3-flash-preview".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = test_config_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: inherited_provider.clone(),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-model-only",
        );
        let child_provider = orch
            .child_llm_provider(&child)
            .expect("model-only delegation inherits provider");

        assert_eq!(child.model, "google/gemini-3-flash-preview");
        assert_eq!(child.provider.as_deref(), Some("nous"));
        assert!(Arc::ptr_eq(&child_provider, &inherited_provider));
    }

    #[test]
    fn build_child_config_provider_override_prefixes_child_model() {
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("google/gemini-3-flash-preview".into()),
            delegation_provider: Some("openrouter".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = test_config_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-provider",
        );

        assert_eq!(child.provider.as_deref(), Some("openrouter"));
        assert_eq!(child.model, "openrouter:google/gemini-3-flash-preview");
    }

    #[test]
    fn direct_endpoint_override_builds_distinct_child_provider() {
        let inherited_provider: Arc<dyn LlmProvider> = Arc::new(NoopProvider);
        let parent = AgentConfig {
            max_turns: 20,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("qwen2.5-coder".into()),
            delegation_base_url: Some("http://127.0.0.1:1234/v1".into()),
            delegation_api_key: Some("local-key".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = test_config_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: inherited_provider.clone(),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
        });

        let child = orch.build_child_config(
            &SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            },
            "subagent-direct",
        );
        let child_provider = orch
            .child_llm_provider(&child)
            .expect("direct endpoint delegation builds provider");

        assert_eq!(child.provider.as_deref(), Some("custom"));
        assert_eq!(child.model, "custom:qwen2.5-coder");
        assert!(!Arc::ptr_eq(&child_provider, &inherited_provider));
    }

    #[tokio::test]
    async fn unresolved_delegation_provider_returns_structured_error() {
        let parent = AgentConfig {
            max_turns: 2,
            model: "nous:hermes-3".into(),
            provider: Some("nous".into()),
            delegation_model: Some("some-model".into()),
            delegation_provider: Some("definitely-missing-provider".into()),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(test_orchestrator(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
            parent_credential_pool: None,
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("root".into()),
            timeout: Duration::from_secs(1),
        }));

        let out = orch
            .execute(SubAgentRequest {
                task: "t".into(),
                child_depth: 1,
                max_depth: 4,
                parent_run_id: Some(orch.parent_run_id),
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(parsed["status"], "failed");
        assert!(parsed["error"]
            .as_str()
            .unwrap()
            .contains("definitely-missing-provider"));
    }
    #[tokio::test]
    async fn cancel_and_join_allows_a_separate_finalizer_grace() {
        let interrupt = InterruptController::new();
        let child_interrupt = interrupt.clone();
        let task = tokio::spawn(async move {
            child_interrupt.fatal_cancelled().await;
            tokio::time::sleep(SUB_AGENT_CANCEL_GRACE + Duration::from_millis(75)).await;
            "verified finalization"
        });
        let mut supervised = AbortOnDropTask::new(task, Some(interrupt));

        let joined = supervised
            .cancel_and_join("test cancellation", false)
            .await
            .expect("child finalizer should receive its separate grace");
        assert_eq!(joined, "verified finalization");
    }

    #[tokio::test]
    async fn child_finalizer_errors_are_propagated_and_never_report_completion() {
        let interrupt = InterruptController::new();
        let child_interrupt = interrupt.clone();
        let task = tokio::spawn(async move {
            child_interrupt.fatal_cancelled().await;
            Err::<hermes_core::AgentResult, _>(AgentError::Io(
                "child finalizer failed".to_string(),
            ))
        });
        let mut supervised = AbortOnDropTask::new(task, Some(interrupt));

        let child_result = supervised
            .cancel_and_join("test cancellation", false)
            .await
            .expect("child join should complete");
        assert!(matches!(
            &child_result,
            Err(AgentError::Io(message)) if message == "child finalizer failed"
        ));
        assert!(matches!(
            cancelled_child_outcome(child_result, false),
            Err(SubAgentError::Agent(AgentError::Io(message)))
                if message == "child finalizer failed"
        ));
        assert!(matches!(
            cancelled_child_outcome(Ok(hermes_core::AgentResult::default()), false),
            Err(SubAgentError::Cancelled)
        ));
    }
}
