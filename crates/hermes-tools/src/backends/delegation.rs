//! Real delegation backend: placeholder that signals sub-agent spawning.
//!
//! The actual sub-agent spawning requires access to the full agent loop,
//! which lives in hermes-agent. This backend provides the interface;
//! the real wiring happens at the binary/CLI level.

use async_trait::async_trait;
use serde_json::json;

use hermes_core::ToolError;
use crate::tools::delegation::DelegationBackend;

/// Delegation backend that returns a signal for the agent loop to spawn a sub-agent.
/// The actual spawning is handled by the orchestration layer (hermes-agent).
pub struct SignalDelegationBackend;

impl SignalDelegationBackend {
    pub fn new() -> Self {
        Self
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
        // Return a structured signal for the agent loop to handle
        Ok(json!({
            "type": "delegation_request",
            "task": task,
            "context": context,
            "toolset": toolset,
            "model": model,
            "status": "pending",
        }).to_string())
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
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed reading RPC response: {}", e)))?;
        Ok(text)
    }
}
