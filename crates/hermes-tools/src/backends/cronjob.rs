//! Real cronjob backend: delegates to hermes-cron scheduler.
//!
//! This backend provides the interface for cron job management.
//! The actual scheduling is handled by the hermes-cron crate.

use async_trait::async_trait;
use serde_json::json;

use crate::tools::cronjob::CronjobBackend;
use hermes_core::ToolError;

/// Cronjob backend that signals the cron scheduler for CRUD operations.
/// The actual scheduling integration happens at the binary level.
pub struct SignalCronjobBackend;

impl SignalCronjobBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SignalCronjobBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CronjobBackend for SignalCronjobBackend {
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        toolset: Option<&str>,
        context_from: Option<&serde_json::Value>,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "create",
            "name": name,
            "schedule": schedule,
            "task": task,
            "toolset": toolset,
            "context_from": context_from.cloned(),
        })
        .to_string())
    }

    async fn list(&self) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "list",
        })
        .to_string())
    }

    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
        context_from: Option<&serde_json::Value>,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "update",
            "id": id,
            "schedule": schedule,
            "task": task,
            "enabled": enabled,
            "context_from": context_from.cloned(),
        })
        .to_string())
    }

    async fn pause(&self, id: &str) -> Result<String, ToolError> {
        Ok(json!({"type": "cronjob_request", "action": "pause", "id": id}).to_string())
    }

    async fn resume(&self, id: &str) -> Result<String, ToolError> {
        Ok(json!({"type": "cronjob_request", "action": "resume", "id": id}).to_string())
    }

    async fn remove(&self, id: &str) -> Result<String, ToolError> {
        Ok(json!({"type": "cronjob_request", "action": "remove", "id": id}).to_string())
    }

    async fn run(&self, id: &str) -> Result<String, ToolError> {
        Ok(json!({"type": "cronjob_request", "action": "run", "id": id}).to_string())
    }
}
