//! Job runner for the cron scheduler.
//!
//! The `CronRunner` is responsible for executing a cron job by creating a fresh
//! agent loop context, loading the job's skills, and delivering results to
//! the configured target platform.
//!
//! Safety: cron jobs **cannot** recursively schedule more cron jobs. The runner
//! runs the agent with a restricted tool set that excludes the cronjob tool.

use std::sync::Arc;

use hermes_agent::{AgentConfig, AgentLoop};
use hermes_agent::agent_loop::ToolRegistry;
use hermes_core::{AgentResult, LlmProvider, Message, ToolSchema};

use crate::job::{CronJob, DeliverConfig, DeliverTarget};
use crate::scheduler::CronError;

// ---------------------------------------------------------------------------
// CronRunner
// ---------------------------------------------------------------------------

/// Executes cron jobs by spinning up a fresh agent loop for each invocation.
pub struct CronRunner {
    /// LLM provider for agent completions.
    llm_provider: Arc<dyn LlmProvider>,
    /// Tool registry providing available tools.
    tool_registry: Arc<ToolRegistry>,
}

impl CronRunner {
    /// Create a new cron runner.
    pub fn new(
        llm_provider: Arc<dyn LlmProvider>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self {
            llm_provider,
            tool_registry,
        }
    }

    /// Run a cron job.
    ///
    /// Creates a fresh agent loop context, loads skills per the job's config,
    /// and delivers results to the configured target platform.
    ///
    /// The agent is run with a restricted tool set that excludes the
    /// `cronjob` tool to prevent recursive scheduling.
    pub async fn run_job(&self, job: &CronJob) -> Result<AgentResult, CronError> {
        tracing::info!("Running cron job '{}' ({})", job.name.as_deref().unwrap_or(&job.id), job.id);

        // Build agent config from job settings
        let mut config = AgentConfig::default();
        if let Some(ref model_cfg) = job.model {
            if let Some(ref model) = model_cfg.model {
                config.model = model.clone();
            }
        }
        // System prompt includes safety notice that cron tools are unavailable
        config.system_prompt = Some(format!(
            "You are executing a scheduled cron job. \
             You cannot schedule or manage other cron jobs from within a cron job execution. \
             Focus on completing the assigned task.\n\nTask: {}",
            job.prompt
        ));

        // Build tool list, excluding the cronjob tool to prevent recursive scheduling
        let tools = self.filtered_tool_schemas();

        // Create a fresh agent loop
        let agent_loop = AgentLoop::new(config, self.tool_registry.clone(), self.llm_provider.clone());

        // Build initial messages
        let messages = self.build_messages(job);

        // Run the agent loop
        let result = agent_loop.run(messages, Some(tools)).await.map_err(CronError::Agent)?;

        // Deliver results if configured
        if let Some(ref deliver) = job.deliver {
            if let Err(e) = self.deliver_result(&result, deliver).await {
                tracing::warn!(
                    "Failed to deliver result for job '{}': {}",
                    job.id,
                    e
                );
            }
        }

        Ok(result)
    }

    /// Build the initial messages for the agent from the job definition.
    fn build_messages(&self, job: &CronJob) -> Vec<Message> {
        let mut messages = Vec::new();

        // If a script is provided, use it as the user message; otherwise use the prompt
        let user_content = if let Some(ref script) = job.script {
            script.clone()
        } else {
            job.prompt.clone()
        };

        // Include skill context if skills are configured
        if let Some(ref skills) = job.skills {
            if !skills.is_empty() {
                let skill_context = format!(
                    "Available skills for this task: {}",
                    skills.join(", ")
                );
                messages.push(Message::user(skill_context));
            }
        }

        messages.push(Message::user(user_content));
        messages
    }

    /// Filter out the `cronjob` tool from the registry to prevent recursive scheduling.
    fn filtered_tool_schemas(&self) -> Vec<ToolSchema> {
        self.tool_registry
            .schemas()
            .into_iter()
            .filter(|schema| schema.name != "cronjob")
            .collect()
    }

    /// Deliver the agent result to the configured target.
    ///
    /// This is a best-effort delivery; errors are logged but do not fail the job.
    async fn deliver_result(
        &self,
        result: &AgentResult,
        deliver: &DeliverConfig,
    ) -> Result<(), CronError> {
        // Extract the final text from the agent result
        let text = result
            .messages
            .iter()
            .rev()
            .find_map(|msg| {
                if msg.role == hermes_core::MessageRole::Assistant {
                    msg.content.clone()
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "(no output)".to_string());

        match deliver.target {
            DeliverTarget::Origin => {
                // Result is returned directly to caller; nothing extra to do
                tracing::debug!("Delivering result to origin");
            }
            DeliverTarget::Local => {
                // Log locally
                tracing::info!("Cron job result (local delivery):\n{}", text);
            }
            DeliverTarget::Telegram
            | DeliverTarget::Discord
            | DeliverTarget::Slack
            | DeliverTarget::Email
            | DeliverTarget::WhatsApp
            | DeliverTarget::Signal
            | DeliverTarget::Matrix
            | DeliverTarget::Mattermost
            | DeliverTarget::DingTalk
            | DeliverTarget::Feishu
            | DeliverTarget::WeCom
            | DeliverTarget::Weixin
            | DeliverTarget::BlueBubbles
            | DeliverTarget::Sms
            | DeliverTarget::HomeAssistant => {
                // Platform delivery requires a platform adapter, which is not
                // directly available in the runner. This would be wired up
                // through the gateway crate. For now, log the intended delivery.
                tracing::info!(
                    "Cron job result delivery to {:?} (platform: {:?}):\n{}",
                    deliver.target,
                    deliver.platform,
                    text
                );
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::CronJob;
    use hermes_core::ToolError;

    #[test]
    fn test_filtered_tool_schemas_excludes_cronjob() {
        // Create a minimal tool registry with a cronjob tool
        let mut registry = ToolRegistry::new();
        registry.register(
            "cronjob",
            hermes_core::tool_schema(
                "cronjob",
                "Manage cron jobs",
                hermes_core::JsonSchema::new("object"),
            ),
            Arc::new(|_params: serde_json::Value| -> Result<String, ToolError> { Ok("ok".to_string()) }),
        );
        registry.register(
            "terminal",
            hermes_core::tool_schema(
                "terminal",
                "Run commands",
                hermes_core::JsonSchema::new("object"),
            ),
            Arc::new(|_params: serde_json::Value| -> Result<String, ToolError> { Ok("ok".to_string()) }),
        );

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(registry),
        };

        let schemas = runner.filtered_tool_schemas();
        let names: Vec<&str> = schemas.iter().map(|s| s.name.as_str()).collect();
        assert!(!names.contains(&"cronjob"));
        assert!(names.contains(&"terminal"));
    }

    #[test]
    fn test_build_messages_with_skills() {
        let mut job = CronJob::new("0 9 * * *", "Say hello");
        job.skills = Some(vec!["web_search".to_string(), "terminal".to_string()]);

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(ToolRegistry::new()),
        };

        let messages = runner.build_messages(&job);
        // Should have skill context + prompt message
        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.as_ref().unwrap().contains("web_search"));
        assert_eq!(messages[1].content.as_deref(), Some("Say hello"));
    }

    #[test]
    fn test_build_messages_with_script() {
        let mut job = CronJob::new("0 9 * * *", "Say hello");
        job.script = Some("echo hello world".to_string());

        let runner = CronRunner {
            llm_provider: Arc::new(MockLlmProvider),
            tool_registry: Arc::new(ToolRegistry::new()),
        };

        let messages = runner.build_messages(&job);
        // Script overrides prompt as user message
        assert_eq!(messages[0].content.as_deref(), Some("echo hello world"));
    }

    // Minimal mock LLM provider for testing
    struct MockLlmProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockLlmProvider {
        async fn chat_completion(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[hermes_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<hermes_core::LlmResponse, hermes_core::AgentError> {
            Ok(hermes_core::LlmResponse {
                message: hermes_core::Message::assistant("done"),
                usage: None,
                model: "mock".to_string(),
                finish_reason: Some("stop".to_string()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[hermes_core::Message],
            _tools: &[hermes_core::ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, hermes_core::AgentError>> {
            Box::pin(futures::stream::empty())
        }
    }
}