//! Runtime wiring helpers for swapping signal backends to live implementations.

use std::io::IsTerminal;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::ToolHandler;
use hermes_cron::{CronScheduler, ScheduledCronjobBackend};
use hermes_gateway::tool_backends::{
    ChannelClarifyBackend, ClarifyDispatcher, GatewayMessagingBackend,
};
use hermes_gateway::Gateway;
use hermes_tools::tools::clarify::{ClarifyBackend, ClarifyHandler};
use hermes_tools::tools::cronjob::CronjobHandler;
use hermes_tools::tools::messaging::SendMessageHandler;
use hermes_tools::ToolRegistry;
use serde_json::json;

fn register_runtime_tool(
    tool_registry: &Arc<ToolRegistry>,
    name: &str,
    toolset: &str,
    handler: Arc<dyn ToolHandler>,
    description: &str,
    emoji: &str,
) {
    // Runtime backends intentionally replace baseline tool handlers.
    // Deregister first to avoid noisy overwrite warnings in interactive TUI sessions.
    let _ = tool_registry.deregister(name);
    let schema = handler.schema();
    tool_registry.register(
        name,
        toolset,
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        description,
        emoji,
        None,
    );
}

/// Replace `send_message` signal backend with a live gateway-backed sender.
pub fn wire_gateway_messaging_backend(tool_registry: &Arc<ToolRegistry>, gateway: Arc<Gateway>) {
    let backend = Arc::new(GatewayMessagingBackend::new(gateway));
    let handler: Arc<dyn ToolHandler> = Arc::new(SendMessageHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "send_message",
        "messaging",
        handler,
        "Send a message to a recipient on a specific platform.",
        "💬",
    );
}

/// Replace `cronjob` signal backend with a live scheduler-backed backend.
pub fn wire_cron_scheduler_backend(
    tool_registry: &Arc<ToolRegistry>,
    scheduler: Arc<CronScheduler>,
) {
    let backend = Arc::new(ScheduledCronjobBackend::new(scheduler));
    let handler: Arc<dyn ToolHandler> = Arc::new(CronjobHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "cronjob",
        "cronjob",
        handler,
        "Manage cron jobs: create, list, update, pause, resume, remove, or run scheduled tasks.",
        "⏰",
    );
}

/// Replace `clarify` signal backend with a channel-backed async clarify flow.
pub fn wire_gateway_clarify_backend(
    tool_registry: &Arc<ToolRegistry>,
    dispatcher: ClarifyDispatcher,
) {
    let backend = Arc::new(ChannelClarifyBackend::new(dispatcher));
    let handler: Arc<dyn ToolHandler> = Arc::new(ClarifyHandler::new(backend));
    register_runtime_tool(
        tool_registry,
        "clarify",
        "clarify",
        handler,
        "Ask the user a clarification question. Optionally provide choices for the user to select from.",
        "❓",
    );
}

/// Clarify backend for CLI/stdio runtimes.
///
/// Behavior:
/// - Interactive terminal: prompt for an answer.
/// - Non-interactive: use `HERMES_CLARIFY_AUTO_ANSWER` when set, otherwise first
///   choice when available, otherwise return an error.
pub struct StdioClarifyBackend;

impl StdioClarifyBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StdioClarifyBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ClarifyBackend for StdioClarifyBackend {
    async fn ask(
        &self,
        question: &str,
        choices: Option<&[String]>,
    ) -> Result<String, hermes_core::ToolError> {
        let choices_vec = choices.map(|c| c.to_vec()).unwrap_or_default();
        let auto_answer = std::env::var("HERMES_CLARIFY_AUTO_ANSWER")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if auto_answer.is_some() || !std::io::stdin().is_terminal() {
            let answer = auto_answer
                .or_else(|| choices_vec.first().cloned())
                .ok_or_else(|| {
                    hermes_core::ToolError::ExecutionFailed(
                        "clarify requires interactive terminal input or HERMES_CLARIFY_AUTO_ANSWER"
                            .to_string(),
                    )
                })?;
            return Ok(json!({
                "type": "clarify_response",
                "question": question,
                "answer": answer,
                "source": "non_interactive_fallback",
            })
            .to_string());
        }

        let question_owned = question.to_string();
        let task =
            tokio::task::spawn_blocking(move || -> Result<String, hermes_core::ToolError> {
                use std::io::{self, Write};

                eprintln!("\n[clarify] {}", question_owned);
                if !choices_vec.is_empty() {
                    eprintln!("[clarify] choices:");
                    for (idx, choice) in choices_vec.iter().enumerate() {
                        eprintln!("  {}. {}", idx + 1, choice);
                    }
                }
                eprint!("[clarify] answer> ");
                io::stderr().flush().map_err(|e| {
                    hermes_core::ToolError::ExecutionFailed(format!("clarify flush failed: {e}"))
                })?;

                let mut input = String::new();
                io::stdin().read_line(&mut input).map_err(|e| {
                    hermes_core::ToolError::ExecutionFailed(format!("clarify read failed: {e}"))
                })?;

                let trimmed = input.trim();
                if trimmed.is_empty() {
                    return choices_vec.first().cloned().ok_or_else(|| {
                        hermes_core::ToolError::ExecutionFailed(
                            "empty clarify response with no fallback choices".to_string(),
                        )
                    });
                }
                if let Ok(n) = trimmed.parse::<usize>() {
                    if n >= 1 && n <= choices_vec.len() {
                        return Ok(choices_vec[n - 1].clone());
                    }
                }
                Ok(trimmed.to_string())
            })
            .await
            .map_err(|e| {
                hermes_core::ToolError::ExecutionFailed(format!("clarify join failed: {e}"))
            })??;

        Ok(json!({
            "type": "clarify_response",
            "question": question,
            "answer": task,
            "source": "stdio",
        })
        .to_string())
    }
}

/// Replace `clarify` signal backend with a stdio-backed clarify flow.
pub fn wire_stdio_clarify_backend(tool_registry: &Arc<ToolRegistry>) {
    let handler: Arc<dyn ToolHandler> =
        Arc::new(ClarifyHandler::new(Arc::new(StdioClarifyBackend::new())));
    register_runtime_tool(
        tool_registry,
        "clarify",
        "clarify",
        handler,
        "Ask the user a clarification question. Optionally provide choices for the user to select from.",
        "❓",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Mutex;

    use hermes_core::{GatewayError, ParseMode, PlatformAdapter};
    use hermes_cron::{CronRunner, FileJobPersistence, MinimalCronLlm};
    use hermes_gateway::{gateway::GatewayConfig, DmManager, SessionManager};
    use tempfile::TempDir;

    struct RecordingAdapter {
        sent: Arc<Mutex<Vec<(String, String)>>>,
        running: bool,
        platform: String,
    }

    impl RecordingAdapter {
        fn new(platform: &str) -> Self {
            Self {
                sent: Arc::new(Mutex::new(Vec::new())),
                running: true,
                platform: platform.to_string(),
            }
        }
    }

    #[async_trait]
    impl PlatformAdapter for RecordingAdapter {
        async fn start(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_message(
            &self,
            chat_id: &str,
            text: &str,
            _parse_mode: Option<ParseMode>,
        ) -> Result<(), GatewayError> {
            self.sent
                .lock()
                .expect("recording adapter lock poisoned")
                .push((chat_id.to_string(), text.to_string()));
            Ok(())
        }

        async fn edit_message(
            &self,
            _chat_id: &str,
            _message_id: &str,
            _text: &str,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        async fn send_file(
            &self,
            _chat_id: &str,
            _file_path: &str,
            _caption: Option<&str>,
        ) -> Result<(), GatewayError> {
            Ok(())
        }

        fn is_running(&self) -> bool {
            self.running
        }

        fn platform_name(&self) -> &str {
            &self.platform
        }
    }

    #[tokio::test]
    async fn gateway_messaging_backend_sends_real_message() {
        let session_manager = Arc::new(SessionManager::new(
            hermes_config::session::SessionConfig::default(),
        ));
        let dm = DmManager::with_pair_behavior();
        let gateway = Arc::new(Gateway::new(session_manager, dm, GatewayConfig::default()));
        let adapter = Arc::new(RecordingAdapter::new("telegram"));
        let recorder = adapter.sent.clone();
        gateway.register_adapter("telegram", adapter).await;

        let registry = Arc::new(ToolRegistry::new());
        wire_gateway_messaging_backend(&registry, gateway.clone());

        let out = registry
            .dispatch_async(
                "send_message",
                json!({
                    "platform": "telegram",
                    "recipient": "12345",
                    "message": "hello"
                }),
            )
            .await;
        let parsed: serde_json::Value =
            serde_json::from_str(&out).expect("send_message output should be json");
        assert_eq!(parsed["status"], "sent");

        let sent = recorder.lock().expect("recording lock poisoned");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "12345");
        assert_eq!(sent[0].1, "hello");
    }

    #[tokio::test]
    async fn cron_scheduler_backend_handles_create_and_list() {
        let tmp = TempDir::new().expect("tempdir");
        let persistence = Arc::new(FileJobPersistence::with_dir(tmp.path().to_path_buf()));

        let empty_tools = Arc::new(crate::app::bridge_tool_registry(&ToolRegistry::new()));
        let runner = Arc::new(CronRunner::new(Arc::new(MinimalCronLlm), empty_tools));
        let scheduler = Arc::new(CronScheduler::new(persistence, runner));

        let registry = Arc::new(ToolRegistry::new());
        wire_cron_scheduler_backend(&registry, scheduler);

        let create = registry
            .dispatch_async(
                "cronjob",
                json!({
                    "action": "create",
                    "name": "daily-test",
                    "schedule": "0 9 * * *",
                    "task": "ping",
                }),
            )
            .await;
        let created: serde_json::Value =
            serde_json::from_str(&create).expect("create output should be json");
        assert_eq!(created["action"], "created");

        let list = registry
            .dispatch_async("cronjob", json!({"action": "list"}))
            .await;
        let listed: serde_json::Value =
            serde_json::from_str(&list).expect("list output should be json");
        assert_eq!(listed["action"], "list");
        assert_eq!(listed["count"], 1);
    }

    #[tokio::test]
    async fn stdio_clarify_backend_uses_auto_answer_in_non_interactive_mode() {
        std::env::set_var("HERMES_CLARIFY_AUTO_ANSWER", "from-env");
        let backend = StdioClarifyBackend::new();
        let out = backend
            .ask("pick?", Some(&["a".to_string(), "b".to_string()]))
            .await
            .expect("clarify should succeed with env override");
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("json output");
        assert_eq!(parsed["answer"], "from-env");
        std::env::remove_var("HERMES_CLARIFY_AUTO_ANSWER");
    }
}
