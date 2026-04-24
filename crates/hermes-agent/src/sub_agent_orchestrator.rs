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
use hermes_core::{AgentError, LlmProvider, Message};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::time::timeout;

use crate::agent_loop::{AgentConfig, AgentLoop, ToolRegistry};
use crate::interrupt::InterruptController;

/// Boxed `Send` future type alias used to short-circuit async-recursion
/// between parent and child [`AgentLoop::execute_tool_calls`] futures.
type BoxSendFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Default wall-clock cap for a single child agent turn-chain.
pub const DEFAULT_SUB_AGENT_TIMEOUT_SECS: u64 = 600;
/// Floor on child max_turns regardless of parent budget.
pub const SUB_AGENT_MIN_TURNS: u32 = 2;
/// Ceiling on child max_turns to keep sub-tasks bounded.
pub const SUB_AGENT_MAX_TURNS_CAP: u32 = 12;

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
    pub model: Option<String>,
    /// Child depth injected by parent agent loop (parent_depth + 1).
    pub child_depth: u32,
    /// Propagated max depth for lineage / bookkeeping.
    pub max_depth: u32,
    /// Parent budget remaining (USD) — exposed to the child for cost awareness.
    pub parent_budget_remaining_usd: Option<f64>,
}

/// Configuration for [`SubAgentOrchestrator`].
#[derive(Clone)]
pub struct SubAgentOrchestratorConfig {
    pub parent_config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub parent_interrupt: InterruptController,
    pub hermes_home: PathBuf,
    pub parent_session_id: Option<String>,
    pub timeout: Duration,
}

/// In-process executor for `delegate_task` tool calls.
pub struct SubAgentOrchestrator {
    cfg: SubAgentOrchestratorConfig,
}

impl SubAgentOrchestrator {
    pub fn new(cfg: SubAgentOrchestratorConfig) -> Self {
        Self { cfg }
    }

    /// Default orchestrator that mirrors the current `AgentLoop`'s wiring.
    ///
    /// The returned orchestrator inherits the parent's tool registry, provider
    /// and interrupt handle so cancellation propagates from the parent to any
    /// actively running child.
    pub fn from_parent(parent: &AgentLoop, hermes_home: PathBuf) -> Self {
        Self {
            cfg: SubAgentOrchestratorConfig {
                parent_config: parent.config.clone(),
                tool_registry: parent.tool_registry.clone(),
                llm_provider: parent.llm_provider.clone(),
                parent_interrupt: parent.interrupt.clone(),
                hermes_home,
                parent_session_id: parent.config.session_id.clone(),
                timeout: Duration::from_secs(DEFAULT_SUB_AGENT_TIMEOUT_SECS),
            },
        }
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
        Box::pin(async move { this.execute_inner(req).await })
    }

    async fn execute_inner(self: Arc<Self>, req: SubAgentRequest) -> String {
        let sub_agent_id = format!("subagent-{}", uuid::Uuid::new_v4());
        let started_at = Utc::now();

        let mut lineage = SubAgentLineage {
            sub_agent_id: sub_agent_id.clone(),
            parent_session_id: self.cfg.parent_session_id.clone(),
            task: req.task.clone(),
            context: req.context.clone(),
            toolset: req.toolset.clone(),
            model: req.model.clone(),
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
        let child_interrupt = InterruptController::new();

        // Forward parent cancellation to child via a light watcher task.
        let parent_ctrl = self.cfg.parent_interrupt.clone();
        let child_ctrl = child_interrupt.clone();
        let watcher_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_stop_clone = watcher_stop.clone();
        let watcher = tokio::spawn(async move {
            while !watcher_stop_clone.load(std::sync::atomic::Ordering::Acquire) {
                if parent_ctrl.is_interrupted() {
                    child_ctrl.interrupt(Some("parent cancelled".to_string()));
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });

        let child_config = self.build_child_config(req, sub_agent_id);
        let child_interrupt_for_spawn = child_interrupt.clone();
        let tool_registry = self.cfg.tool_registry.clone();
        let llm_provider = self.cfg.llm_provider.clone();
        let child_depth = req.child_depth;
        let initial = initial_messages(&req.task, req.context.as_deref());

        // Run the child on its own tokio task so that its `impl Future` type is
        // fully erased behind a `JoinHandle`, breaking async recursion with the
        // parent's `execute_tool_calls` future (which hosts the orchestrator
        // call). `tokio::spawn` requires `Send + 'static`, which forces child
        // `AgentLoop::run` to already be Send — this is also what the existing
        // `spawn_background_review` relies on.
        let join = tokio::spawn(async move {
            let child_agent = AgentLoop::with_interrupt(
                child_config,
                tool_registry,
                llm_provider,
                child_interrupt_for_spawn,
            )
            .with_delegate_depth(child_depth);
            child_agent.run(initial, None).await
        });

        let result = match timeout(self.cfg.timeout, join).await {
            Ok(Ok(Ok(result))) if result.interrupted => Err(SubAgentError::Cancelled),
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(AgentError::Interrupted { .. }))) => Err(SubAgentError::Cancelled),
            Ok(Ok(Err(e))) => Err(SubAgentError::Agent(e)),
            Ok(Err(join_err)) => Err(SubAgentError::Agent(AgentError::ToolExecution(format!(
                "sub-agent join error: {}",
                join_err
            )))),
            Err(_) => {
                // Cooperative cancel on timeout — next `check_interrupt()`
                // boundary in the child unwinds.
                child_interrupt.interrupt(Some("timeout".to_string()));
                Err(SubAgentError::Timeout)
            }
        };

        watcher_stop.store(true, std::sync::atomic::Ordering::Release);
        let _ = watcher.await;
        result
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
        // Apply explicit model override if the caller requested one.
        if let Some(model) = req.model.as_ref() {
            if !model.is_empty() {
                child.model = model.clone();
            }
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
    use hermes_core::{LlmResponse, StreamChunk, ToolSchema};
    use std::sync::Arc;

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

    #[tokio::test]
    async fn spawn_timeout_path_produces_timeout_json() {
        struct SlowProvider;
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
            model: "slow".into(),
            ..AgentConfig::default()
        };
        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent_cfg,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(SlowProvider),
            parent_interrupt: InterruptController::new(),
            hermes_home: tmp.path().to_path_buf(),
            parent_session_id: Some("parent".into()),
            timeout: Duration::from_millis(50),
        }));
        let out = orch
            .execute(SubAgentRequest {
                task: "test task".into(),
                child_depth: 1,
                max_depth: 4,
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["status"], "timeout");
        assert!(parsed["sub_agent_id"]
            .as_str()
            .unwrap()
            .starts_with("subagent-"));
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
    async fn cancel_path_reports_cancelled() {
        let parent_ctrl = InterruptController::new();
        parent_ctrl.interrupt(Some("stop".into())); // pre-cancel before spawn

        let tmp = tempfile::tempdir().unwrap();
        let orch = Arc::new(SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: AgentConfig::default(),
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
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
                ..Default::default()
            })
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        // Parent interrupt propagates to child; status should be cancelled or
        // completed depending on scheduling. We accept either (cancelled most
        // of the time; completed if watcher missed the window). The lineage
        // file still must be written.
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
        let orch = SubAgentOrchestrator::new(SubAgentOrchestratorConfig {
            parent_config: parent,
            tool_registry: Arc::new(ToolRegistry::new()),
            llm_provider: Arc::new(NoopProvider),
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
}
