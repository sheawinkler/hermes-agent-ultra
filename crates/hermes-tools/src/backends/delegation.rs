//! Delegation backends: in-process signal vs HTTP RPC.
//!
//! [`SignalDelegationBackend`] returns a JSON envelope for the agent loop to
//! interpret (spawn / route locally). [`RpcDelegationBackend`] POSTs JSON to
//! a user-provided HTTP endpoint and returns the response body as the tool
//! result — use when a remote worker implements delegation.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::delegation::DelegationBackend;
use hermes_core::ToolError;

/// Delegation backend that returns a signal for the agent loop to spawn a sub-agent.
/// The actual spawning is handled by the orchestration layer (hermes-agent).
pub struct SignalDelegationBackend {
    current_depth: u32,
    max_depth: u32,
    parent_budget_remaining_usd: Option<f64>,
}

impl SignalDelegationBackend {
    pub fn new() -> Self {
        Self {
            current_depth: 0,
            max_depth: 4,
            parent_budget_remaining_usd: None,
        }
    }

    pub fn with_depth(mut self, current: u32, max: u32) -> Self {
        self.current_depth = current;
        self.max_depth = max;
        self
    }

    pub fn with_parent_budget(mut self, remaining_usd: f64) -> Self {
        self.parent_budget_remaining_usd = Some(remaining_usd);
        self
    }
}

impl Default for SignalDelegationBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DelegationBackend for SignalDelegationBackend {
    async fn delegate(
        &self,
        task: &str,
        context: Option<&str>,
        toolset: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, ToolError> {
        if self.current_depth >= self.max_depth {
            return Err(ToolError::ExecutionFailed(format!(
                "Delegation depth limit reached ({}/{}); cannot spawn further sub-agents",
                self.current_depth, self.max_depth
            )));
        }
        Ok(json!({
            "type": "delegation_request",
            "task": task,
            "context": context,
            "toolset": toolset,
            "model": model,
            "child_depth": self.current_depth + 1,
            "max_depth": self.max_depth,
            "parent_budget_remaining_usd": self.parent_budget_remaining_usd,
            "status": "pending",
        })
        .to_string())
    }
}

/// Delegation backend that forwards requests to an RPC endpoint.
pub struct RpcDelegationBackend {
    endpoint: String,
    client: reqwest::Client,
}

impl RpcDelegationBackend {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl DelegationBackend for RpcDelegationBackend {
    async fn delegate(
        &self,
        task: &str,
        context: Option<&str>,
        toolset: Option<&str>,
        model: Option<&str>,
    ) -> Result<String, ToolError> {
        let payload = json!({
            "task": task,
            "context": context,
            "toolset": toolset,
            "model": model,
        });
        let resp = self
            .client
            .post(&self.endpoint)
            .json(&payload)
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("RPC delegation failed: {}", e)))?;
        let text = resp.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed reading RPC response: {}", e))
        })?;
        Ok(text)
    }
}
