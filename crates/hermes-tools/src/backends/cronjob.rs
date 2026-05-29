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
        name: Option<&str>,
        schedule: &str,
        task: &str,
        skills: Option<&[String]>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        context_from: Option<&serde_json::Value>,
        enabled_toolsets: Option<&[String]>,
        workdir: Option<&str>,
        profile: Option<&str>,
        script: Option<&str>,
        no_agent: Option<bool>,
        deliver: Option<&str>,
        repeat: Option<u32>,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "create",
            "name": name,
            "schedule": schedule,
            "task": task,
            "skills": skills,
            "model": model,
            "provider": provider,
            "base_url": base_url,
            "context_from": context_from.cloned(),
            "enabled_toolsets": enabled_toolsets,
            "workdir": workdir,
            "profile": profile,
            "script": script,
            "no_agent": no_agent,
            "deliver": deliver,
            "repeat": repeat,
        })
        .to_string())
    }

    async fn list(&self, include_disabled: bool) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "list",
            "include_disabled": include_disabled,
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
        enabled_toolsets: Option<&serde_json::Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        skills: Option<&serde_json::Value>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        workdir: Option<&str>,
        profile: Option<&str>,
        deliver: Option<&str>,
        repeat: Option<u32>,
    ) -> Result<String, ToolError> {
        Ok(json!({
            "type": "cronjob_request",
            "action": "update",
            "id": id,
            "schedule": schedule,
            "task": task,
            "enabled": enabled,
            "context_from": context_from.cloned(),
            "enabled_toolsets": enabled_toolsets.cloned(),
            "script": script,
            "no_agent": no_agent,
            "skills": skills.cloned(),
            "model": model,
            "provider": provider,
            "base_url": base_url,
            "workdir": workdir,
            "profile": profile,
            "deliver": deliver,
            "repeat": repeat,
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
