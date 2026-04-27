//! Cron job management tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use std::sync::Arc;

// ---------------------------------------------------------------------------
// CronjobBackend trait
// ---------------------------------------------------------------------------

/// Backend for cron job management operations.
#[async_trait]
pub trait CronjobBackend: Send + Sync {
    /// Create a new cron job.
    async fn create(
        &self,
        name: &str,
        schedule: &str,
        task: &str,
        toolset: Option<&str>,
        context_from: Option<&Value>,
    ) -> Result<String, ToolError>;
    /// List all cron jobs.
    async fn list(&self) -> Result<String, ToolError>;
    /// Update a cron job.
    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
        context_from: Option<&Value>,
    ) -> Result<String, ToolError>;
    /// Pause a cron job.
    async fn pause(&self, id: &str) -> Result<String, ToolError>;
    /// Resume a cron job.
    async fn resume(&self, id: &str) -> Result<String, ToolError>;
    /// Remove a cron job.
    async fn remove(&self, id: &str) -> Result<String, ToolError>;
    /// Run a cron job immediately.
    async fn run(&self, id: &str) -> Result<String, ToolError>;
}

// ---------------------------------------------------------------------------
// CronjobHandler
// ---------------------------------------------------------------------------

/// Tool for managing cron jobs: create, list, update, pause, resume, remove, run.
pub struct CronjobHandler {
    backend: Arc<dyn CronjobBackend>,
}

impl CronjobHandler {
    pub fn new(backend: Arc<dyn CronjobBackend>) -> Self {
        Self { backend }
    }
}

#[async_trait]
impl ToolHandler for CronjobHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let schedule =
                    params
                        .get("schedule")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| {
                            ToolError::InvalidParams("Missing 'schedule' parameter".into())
                        })?;
                let task = params
                    .get("task")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'task' parameter".into()))?;
                let toolset = params.get("toolset").and_then(|v| v.as_str());
                let context_from = params.get("context_from");
                self.backend
                    .create(name, schedule, task, toolset, context_from)
                    .await
            }
            "list" => self.backend.list().await,
            "update" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                let schedule = params.get("schedule").and_then(|v| v.as_str());
                let task = params.get("task").and_then(|v| v.as_str());
                let enabled = params.get("enabled").and_then(|v| v.as_bool());
                let context_from = params.get("context_from");
                self.backend
                    .update(id, schedule, task, enabled, context_from)
                    .await
            }
            "pause" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.pause(id).await
            }
            "resume" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.resume(id).await
            }
            "remove" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.remove(id).await
            }
            "run" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'id' parameter".into()))?;
                self.backend.run(id).await
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown action: '{}'. Use create, list, update, pause, resume, remove, or run.",
                other
            ))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "description": "Action to perform",
                "enum": ["create", "list", "update", "pause", "resume", "remove", "run"]
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Cron job ID (for update, pause, resume, remove, run)"
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Cron job name (for create)"
            }),
        );
        props.insert(
            "schedule".into(),
            json!({
                "type": "string",
                "description": "Cron schedule expression (e.g. '0 9 * * *' for 9am daily)"
            }),
        );
        props.insert(
            "task".into(),
            json!({
                "type": "string",
                "description": "Task description for the cron job"
            }),
        );
        props.insert(
            "toolset".into(),
            json!({
                "type": "string",
                "description": "Toolset to assign to the cron job's agent"
            }),
        );
        props.insert(
            "enabled".into(),
            json!({
                "type": "boolean",
                "description": "Whether the cron job is enabled (for update)"
            }),
        );
        props.insert(
            "context_from".into(),
            json!({
                "oneOf": [
                    {
                        "type": "string",
                        "description": "A single job ID whose most recent output should be injected into this job's prompt."
                    },
                    {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "A list of job IDs to use as context in order."
                    },
                    {
                        "type": "null",
                        "description": "On update, clears any existing context sources."
                    }
                ]
            }),
        );

        tool_schema(
            "cronjob",
            "Manage cron jobs: create, list, update, pause, resume, remove, or run scheduled tasks.",
            JsonSchema::object(props, vec!["action".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MockCronBackend;
    #[async_trait]
    impl CronjobBackend for MockCronBackend {
        async fn create(
            &self,
            name: &str,
            _schedule: &str,
            _task: &str,
            _toolset: Option<&str>,
            _context_from: Option<&Value>,
        ) -> Result<String, ToolError> {
            Ok(format!("Created cronjob: {}", name))
        }
        async fn list(&self) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn update(
            &self,
            id: &str,
            _schedule: Option<&str>,
            _task: Option<&str>,
            _enabled: Option<bool>,
            _context_from: Option<&Value>,
        ) -> Result<String, ToolError> {
            Ok(format!("Updated cronjob: {}", id))
        }
        async fn pause(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Paused: {}", id))
        }
        async fn resume(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Resumed: {}", id))
        }
        async fn remove(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Removed: {}", id))
        }
        async fn run(&self, id: &str) -> Result<String, ToolError> {
            Ok(format!("Ran: {}", id))
        }
    }

    #[tokio::test]
    async fn test_cronjob_create() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        let result = handler.execute(json!({"action": "create", "name": "test", "schedule": "0 9 * * *", "task": "Say hello"})).await.unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_cronjob_list() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        let result = handler.execute(json!({"action": "list"})).await.unwrap();
        assert_eq!(result, "[]");
    }

    #[tokio::test]
    async fn test_cronjob_schema() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        assert_eq!(handler.schema().name, "cronjob");
    }
}
