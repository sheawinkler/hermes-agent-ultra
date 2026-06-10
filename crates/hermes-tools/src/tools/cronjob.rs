//! Cron job management tool

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

use std::sync::Arc;

const CRONJOB_DESCRIPTION: &str = "\
Manage scheduled cron jobs with a single compressed tool.\n\n\
REQUIRED for user reminders: when the user asks to be reminded later or wants a scheduled \
task, call action='create' in the same turn. Do not promise a reminder without creating \
the job — chat memory alone never fires reminders.\n\n\
Use action='create' to schedule a new job from a prompt/task or one or more skills.\n\
Use action='list' to inspect jobs.\n\
Use action='update', 'pause', 'resume', 'remove', or 'run' to manage an existing job.\n\n\
To stop a job the user no longer wants: first action='list' to find the job id (or job_id), then action='remove' with that id. Never guess job IDs — always list first.\n\n\
Schedule DSL (required on create): '2m' or '30m' = one-shot in N minutes; 'every 30m' or 'every 2h' = recurring; '0 9 * * *' = daily cron; ISO timestamp = one-shot at time. For 'remind me in 2 minutes' / '2分钟后提醒我' use '2m', NOT '2 minutes from now' or cron expressions like '*/2 * * * *'.\n\
On create success, the response includes `next_run` (RFC3339 UTC) and `next_run_display` (Hermes wall-clock with seconds, e.g. June 09, 2026 at 05:55:55 PM). When telling the user when a reminder will fire, quote `next_run_display` exactly — do not guess from conversation context or session start time.\n\n\
Jobs run in a fresh session with no current-chat context, so prompts/tasks must be self-contained.\n\
If skills are provided on create, the future cron run loads those skills in order, then follows the prompt/task as the task instruction.\n\
On update, passing skills=[] clears attached skills.\n\n\
NOTE: The agent's final response is auto-delivered to the target. Put the primary user-facing content in the final response. Cron jobs run autonomously with no user present — they cannot ask questions or request clarification.\n\n\
Important safety rule: cron-run sessions should not recursively schedule more cron jobs.";

const NO_AGENT_DESCRIPTION: &str = "\
Default: False (LLM-driven job — the agent runs the prompt each tick). \
Set True to skip the LLM entirely: the scheduler just runs `script` on schedule and delivers its stdout verbatim. No tokens, no agent loop, no model override honoured.\n\n\
REQUIREMENTS when True: `script` MUST be set (`prompt`/`task` and `skills` are ignored).\n\n\
DELIVERY SEMANTICS when True: \
(a) non-empty stdout is sent verbatim as the message; \
(b) EMPTY stdout means SILENT — nothing is sent to the user and they won't see anything happened, so design your script to stay quiet when there's nothing to report (the watchdog pattern); \
(c) non-zero exit / timeout sends an error alert so a broken watchdog can't fail silently.\n\n\
WHEN TO USE True: recurring script-only pings where the script itself produces the exact message text (memory/disk/GPU watchdogs, threshold alerts, heartbeats, CI notifications, API pollers with a fixed output shape). \
WHEN TO USE False (default): anything that needs reasoning — summarize a feed, draft a daily briefing, pick interesting items, rephrase data for a human, follow conditional logic based on content.";

// ---------------------------------------------------------------------------
// CronjobBackend trait
// ---------------------------------------------------------------------------

/// Backend for cron job management operations.
#[async_trait]
pub trait CronjobBackend: Send + Sync {
    /// Create a new cron job.
    async fn create(
        &self,
        name: Option<&str>,
        schedule: &str,
        task: &str,
        skills: Option<&[String]>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        context_from: Option<&Value>,
        enabled_toolsets: Option<&[String]>,
        workdir: Option<&str>,
        profile: Option<&str>,
        script: Option<&str>,
        no_agent: Option<bool>,
        deliver: Option<&str>,
        repeat: Option<u32>,
    ) -> Result<String, ToolError>;
    /// List all cron jobs.
    async fn list(&self, include_disabled: bool) -> Result<String, ToolError>;
    /// Update a cron job.
    async fn update(
        &self,
        id: &str,
        schedule: Option<&str>,
        task: Option<&str>,
        enabled: Option<bool>,
        context_from: Option<&Value>,
        enabled_toolsets: Option<&Value>,
        script: Option<&str>,
        no_agent: Option<bool>,
        skills: Option<&Value>,
        model: Option<&str>,
        provider: Option<&str>,
        base_url: Option<&str>,
        workdir: Option<&str>,
        profile: Option<&str>,
        deliver: Option<&str>,
        repeat: Option<u32>,
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

fn param_str<'a>(params: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| params.get(*key).and_then(|v| v.as_str()))
}

fn parse_skills_param(params: &Value) -> Option<Vec<String>> {
    if let Some(skills) = params.get("skills") {
        if skills.is_array() {
            let mut out = Vec::new();
            for item in skills.as_array().into_iter().flatten() {
                let s = item.as_str()?.trim();
                if !s.is_empty() {
                    out.push(s.to_string());
                }
            }
            return Some(out);
        }
        if let Some(s) = skills.as_str() {
            let trimmed = s.trim();
            if !trimmed.is_empty() {
                return Some(vec![trimmed.to_string()]);
            }
            return Some(Vec::new());
        }
    }
    if let Some(s) = param_str(params, &["skill", "toolset"]) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(vec![trimmed.to_string()]);
        }
    }
    None
}

fn validate_create_params(
    schedule: &str,
    task: &str,
    skills: Option<&[String]>,
    script: Option<&str>,
    no_agent: bool,
) -> Result<(), ToolError> {
    if schedule.trim().is_empty() {
        return Err(ToolError::InvalidParams(
            "schedule is required for create".into(),
        ));
    }
    if no_agent {
        if script.map(str::trim).filter(|s| !s.is_empty()).is_none() {
            return Err(ToolError::InvalidParams(
                "create with no_agent=True requires a script — the script is the job.".into(),
            ));
        }
    } else if task.trim().is_empty() && skills.map(|s| s.is_empty()).unwrap_or(true) {
        return Err(ToolError::InvalidParams(
            "create requires either prompt/task or at least one skill".into(),
        ));
    }
    Ok(())
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
                let schedule = param_str(&params, &["schedule"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'schedule' parameter".into())
                })?;
                let task = param_str(&params, &["task", "prompt"]).unwrap_or("");
                let name = param_str(&params, &["name"]);
                let skills_vec = parse_skills_param(&params);
                let skills = skills_vec.as_deref();
                let model = param_str(&params, &["model"]);
                let provider = param_str(&params, &["provider"]);
                let base_url = param_str(&params, &["base_url"]);
                let context_from = params.get("context_from");
                let enabled_toolsets = params
                    .get("enabled_toolsets")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                    });
                let enabled_toolsets = enabled_toolsets.as_deref();
                let workdir = param_str(&params, &["workdir"]);
                let profile = param_str(&params, &["profile"]);
                let script = param_str(&params, &["script"]);
                let no_agent = params.get("no_agent").and_then(|v| v.as_bool()).unwrap_or(false);
                let deliver = param_str(&params, &["deliver"]);
                let repeat = params
                    .get("repeat")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                validate_create_params(schedule, task, skills, script, no_agent)?;
                self.backend
                    .create(
                        name,
                        schedule,
                        task,
                        skills,
                        model,
                        provider,
                        base_url,
                        context_from,
                        enabled_toolsets,
                        workdir,
                        profile,
                        script,
                        Some(no_agent),
                        deliver,
                        repeat,
                    )
                    .await
            }
            "list" => {
                let include_disabled = params
                    .get("include_disabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                self.backend.list(include_disabled).await
            }
            "update" => {
                let id = param_str(&params, &["id", "job_id"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'id' or 'job_id' parameter".into())
                })?;
                let schedule = param_str(&params, &["schedule"]);
                let task = param_str(&params, &["task", "prompt"]);
                let enabled = params.get("enabled").and_then(|v| v.as_bool());
                let context_from = params.get("context_from");
                let enabled_toolsets = params.get("enabled_toolsets");
                let script = param_str(&params, &["script"]);
                let no_agent = params.get("no_agent").and_then(|v| v.as_bool());
                let skills = params.get("skills").or_else(|| params.get("skill").or(params.get("toolset")));
                let model = param_str(&params, &["model"]);
                let provider = param_str(&params, &["provider"]);
                let base_url = param_str(&params, &["base_url"]);
                let workdir = param_str(&params, &["workdir"]);
                let profile = param_str(&params, &["profile"]);
                let deliver = param_str(&params, &["deliver"]);
                let repeat = params
                    .get("repeat")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);
                self.backend
                    .update(
                        id,
                        schedule,
                        task,
                        enabled,
                        context_from,
                        enabled_toolsets,
                        script,
                        no_agent,
                        skills,
                        model,
                        provider,
                        base_url,
                        workdir,
                        profile,
                        deliver,
                        repeat,
                    )
                    .await
            }
            "pause" => {
                let id = param_str(&params, &["id", "job_id"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'id' or 'job_id' parameter".into())
                })?;
                self.backend.pause(id).await
            }
            "resume" => {
                let id = param_str(&params, &["id", "job_id"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'id' or 'job_id' parameter".into())
                })?;
                self.backend.resume(id).await
            }
            "remove" => {
                let id = param_str(&params, &["id", "job_id"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'id' or 'job_id' parameter".into())
                })?;
                self.backend.remove(id).await
            }
            "run" => {
                let id = param_str(&params, &["id", "job_id"]).ok_or_else(|| {
                    ToolError::InvalidParams("Missing 'id' or 'job_id' parameter".into())
                })?;
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
                "description": "One of: create, list, update, pause, resume, remove, run. When action=create, the 'schedule' and 'prompt' (or 'task') fields are REQUIRED.",
                "enum": ["create", "list", "update", "pause", "resume", "remove", "run"]
            }),
        );
        props.insert(
            "id".into(),
            json!({
                "type": "string",
                "description": "Cron job ID for update/pause/resume/remove/run. Alias: job_id."
            }),
        );
        props.insert(
            "job_id".into(),
            json!({
                "type": "string",
                "description": "Alias for id. Required for update/pause/resume/remove/run."
            }),
        );
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Optional human-friendly name"
            }),
        );
        props.insert(
            "prompt".into(),
            json!({
                "type": "string",
                "description": "For create: the full self-contained prompt. Alias for task. If skills are also provided, this becomes the task instruction paired with those skills."
            }),
        );
        props.insert(
            "task".into(),
            json!({
                "type": "string",
                "description": "For create: the full self-contained task/prompt. Alias for prompt. If skills are also provided, this becomes the task instruction paired with those skills."
            }),
        );
        props.insert(
            "schedule".into(),
            json!({
                "type": "string",
                "description": "REQUIRED for action=create. Schedule DSL (not natural language): '2m' or '30m' = one-shot in N minutes; 'every 30m' or 'every 2h' = recurring; '0 9 * * *' = daily cron; '2026-06-01T09:00:00' = one-shot at time. Examples: '2m' (once in 2 minutes), 'every 2h' (every 2 hours), '0 9 * * *' (daily at 9am). For 'remind me in 2 minutes' / '2分钟后提醒我' use '2m', NOT '2 minutes from now' or cron expressions like '*/2 * * * *'. You MUST include this field when action=create."
            }),
        );
        props.insert(
            "repeat".into(),
            json!({
                "type": "integer",
                "description": "Optional repeat count. Omit for defaults (once for one-shot, forever for recurring)."
            }),
        );
        props.insert(
            "deliver".into(),
            json!({
                "type": "string",
                "description": "Omit this parameter to auto-deliver back to the current chat and topic (recommended). Auto-detection preserves thread/topic context. Only set explicitly when the user asks to deliver somewhere OTHER than the current conversation. Values: 'origin' (same as omitting), 'local' (no delivery, save only), 'all' (fan out to every connected home channel), or platform:chat_id:thread_id for a specific destination. Combine with comma: 'origin,all' delivers to the origin plus every other connected channel. Examples: 'telegram:-1001234567890:17585', 'discord:#engineering', 'sms:+15551234567', 'all'. WARNING: 'platform:chat_id' without :thread_id loses topic targeting. 'all' resolves at fire time, so a job created before a channel was wired up will pick it up automatically once connected."
            }),
        );
        props.insert(
            "model".into(),
            json!({
                "type": "string",
                "description": "Optional per-job model override (e.g. 'anthropic/claude-sonnet-4')."
            }),
        );
        props.insert(
            "provider".into(),
            json!({
                "type": "string",
                "description": "Optional provider override (e.g. 'openrouter', 'anthropic', or custom provider name)."
            }),
        );
        props.insert(
            "base_url".into(),
            json!({
                "type": "string",
                "description": "Optional per-job base URL override. Useful for custom provider gateways."
            }),
        );
        props.insert(
            "skills".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional ordered list of skill names to load before executing the cron prompt. On update, pass an empty array to clear attached skills."
            }),
        );
        props.insert(
            "skill".into(),
            json!({
                "type": "string",
                "description": "Optional single skill name (legacy alias). Prefer skills=[...] for multiple skills."
            }),
        );
        props.insert(
            "toolset".into(),
            json!({
                "type": "string",
                "description": "Legacy alias for a single skill/toolset name."
            }),
        );
        props.insert(
            "script".into(),
            json!({
                "type": "string",
                "description": "Optional path to a script that runs each tick. In the default mode its stdout is injected into the agent's prompt as context (data-collection / change-detection pattern). With no_agent=True, the script IS the job and its stdout is delivered verbatim (classic watchdog pattern). Relative paths resolve under ~/.hermes/scripts/. .sh/.bash extensions run via bash, everything else via Python. On update, pass empty string to clear."
            }),
        );
        props.insert(
            "no_agent".into(),
            json!({
                "type": "boolean",
                "default": false,
                "description": NO_AGENT_DESCRIPTION
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
            "include_disabled".into(),
            json!({
                "type": "boolean",
                "description": "For action=list: include paused/disabled jobs. Default false."
            }),
        );
        props.insert(
            "context_from".into(),
            json!({
                "oneOf": [
                    {
                        "type": "string",
                        "description": "Optional job ID whose most recent completed output is injected into the prompt as context before each run."
                    },
                    {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Optional list of job IDs whose most recent completed output is injected into the prompt as context before each run. Use this to chain cron jobs: job A collects data, job B processes it. Each entry must be a valid job ID (from cronjob action='list'). Note: injects the most recent completed output — does not wait for upstream jobs running in the same tick. On update, pass an empty array to clear."
                    },
                    {
                        "type": "null",
                        "description": "On update, clears any existing context sources."
                    }
                ]
            }),
        );
        props.insert(
            "enabled_toolsets".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Optional list of toolset names to restrict the job's agent to. On update, pass an empty array to clear."
            }),
        );
        props.insert(
            "workdir".into(),
            json!({
                "type": "string",
                "description": "Optional absolute path to run the job from. On update, pass empty string to clear."
            }),
        );
        props.insert(
            "profile".into(),
            json!({
                "type": "string",
                "description": "Optional Hermes profile name to run the job under. On update, pass empty string to clear."
            }),
        );

        tool_schema(
            "cronjob",
            CRONJOB_DESCRIPTION,
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
            name: Option<&str>,
            _schedule: &str,
            _task: &str,
            _skills: Option<&[String]>,
            _model: Option<&str>,
            _provider: Option<&str>,
            _base_url: Option<&str>,
            _context_from: Option<&Value>,
            _enabled_toolsets: Option<&[String]>,
            _workdir: Option<&str>,
            _profile: Option<&str>,
            _script: Option<&str>,
            _no_agent: Option<bool>,
            _deliver: Option<&str>,
            _repeat: Option<u32>,
        ) -> Result<String, ToolError> {
            Ok(format!(
                "Created cronjob: {}",
                name.unwrap_or("unnamed")
            ))
        }
        async fn list(&self, _include_disabled: bool) -> Result<String, ToolError> {
            Ok("[]".to_string())
        }
        async fn update(
            &self,
            id: &str,
            _schedule: Option<&str>,
            _task: Option<&str>,
            _enabled: Option<bool>,
            _context_from: Option<&Value>,
            _enabled_toolsets: Option<&Value>,
            _script: Option<&str>,
            _no_agent: Option<bool>,
            _skills: Option<&Value>,
            _model: Option<&str>,
            _provider: Option<&str>,
            _base_url: Option<&str>,
            _workdir: Option<&str>,
            _profile: Option<&str>,
            _deliver: Option<&str>,
            _repeat: Option<u32>,
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
        let result = handler
            .execute(json!({
                "action": "create",
                "name": "test",
                "schedule": "0 9 * * *",
                "task": "Say hello"
            }))
            .await
            .unwrap();
        assert!(result.contains("Created"));
    }

    #[tokio::test]
    async fn test_cronjob_create_accepts_prompt_alias() {
        let handler = CronjobHandler::new(Arc::new(MockCronBackend));
        let result = handler
            .execute(json!({
                "action": "create",
                "schedule": "2m",
                "prompt": "Remind me to drink water"
            }))
            .await
            .unwrap();
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
        let schema = handler.schema();
        assert_eq!(schema.name, "cronjob");
        assert!(schema.description.contains("Schedule DSL"));
        assert!(schema.description.contains("skills=[]"));
    }
}
