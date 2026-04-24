//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use hermes_agent::agent_loop::ToolRegistry as AgentToolRegistry;
use hermes_agent::provider::{
    AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
};
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator;
use hermes_agent::{AgentConfig, AgentLoop, InterruptController, SessionPersistence};
use hermes_config::{cron_dir, hermes_home as hermes_home_dir, load_config, GatewayConfig};
use hermes_core::ToolSchema;
use hermes_core::{AgentError, LlmProvider};
use hermes_cron::cron_scheduler_for_data_dir;
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

use crate::cli::Cli;
use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
use crate::terminal_backend::build_terminal_backend;
use crate::tui::StreamHandle;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

/// Top-level application state for an interactive Hermes session.
pub struct App {
    /// Loaded gateway configuration.
    pub config: Arc<GatewayConfig>,

    /// The agent loop engine.
    pub agent: Arc<AgentLoop>,

    /// The tool registry (shared with the agent).
    pub tool_registry: Arc<ToolRegistry>,

    /// Active tool schemas exposed to the model for this runtime.
    pub tool_schemas: Vec<ToolSchema>,

    /// Conversation messages for the current session.
    pub messages: Vec<hermes_core::Message>,

    /// Unique identifier for the current session.
    pub session_id: String,

    /// Whether the application loop is still running.
    pub running: bool,

    /// Currently active model identifier (e.g. "openai:gpt-4o").
    pub current_model: String,

    /// Currently active personality name.
    pub current_personality: Option<String>,

    /// History of user inputs for recall.
    pub input_history: Vec<String>,

    /// Index into input_history for up/down arrow navigation.
    pub history_index: usize,

    /// Interrupt controller for stopping agent execution.
    pub interrupt_controller: InterruptController,

    /// Optional TUI streaming sink for incremental chunks.
    pub stream_handle: Option<StreamHandle>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("session_id", &self.session_id)
            .field("running", &self.running)
            .field("current_model", &self.current_model)
            .field("current_personality", &self.current_personality)
            .field("history_index", &self.history_index)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// SessionInfo (for serialization)
// ---------------------------------------------------------------------------

/// Serializable snapshot of a session (for save/restore).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub model: String,
    pub personality: Option<String>,
    pub message_count: usize,
    pub created_at: String,
}

// ---------------------------------------------------------------------------
// App implementation
// ---------------------------------------------------------------------------

impl App {
    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        if let Some(ref model) = cli.model {
            config.model = Some(model.clone());
        }
        if let Some(ref personality) = cli.personality {
            config.personality = Some(personality.clone());
        }

        if config.sessions.auto_prune {
            let resolved_home = config
                .home_dir
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var("HERMES_HOME")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(hermes_home_dir);
            let sp = SessionPersistence::new(&resolved_home);
            let maintenance = sp.maybe_auto_prune_and_vacuum(
                config.sessions.retention_days,
                config.sessions.min_interval_hours,
                config.sessions.vacuum_after_prune,
            );
            if let Some(err) = maintenance.error {
                tracing::debug!("sessions db auto-maintenance skipped: {}", err);
            } else if !maintenance.skipped && maintenance.pruned > 0 {
                tracing::info!(
                    "sessions db auto-maintenance pruned {} session(s){}",
                    maintenance.pruned,
                    if maintenance.vacuumed {
                        " + vacuum"
                    } else {
                        ""
                    }
                );
            }
        }

        let current_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
        let current_personality = config.personality.clone();

        let tool_registry = Arc::new(ToolRegistry::new());
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = cron_dir();
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_data_dir.display(), e)))?;
        let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let tool_schemas =
            crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry);

        let agent_config = build_agent_config(&config, &current_model);
        let provider = build_provider(&config, &current_model);

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider);
        let hermes_home = hermes_home_dir();
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(&agent_inner, hermes_home));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        Ok(Self {
            config: Arc::new(config),
            agent,
            tool_registry,
            tool_schemas,
            messages: Vec::new(),
            session_id: Uuid::new_v4().to_string(),
            running: true,
            current_model,
            current_personality,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
        })
    }

    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        self.stream_handle = handle;
    }

    /// Run the interactive REPL loop.
    ///
    /// This is the main entry point for interactive mode. It delegates
    /// to the TUI subsystem for rendering and event handling.
    pub async fn run_interactive(&mut self) -> Result<(), AgentError> {
        // The actual TUI loop is in crate::tui::run()
        // This method exists so non-TUI callers can drive the loop manually.
        if self.running {
            loop {
                if !self.running {
                    break;
                }
                // In a real implementation, the TUI event loop would drive this.
                // Here we just mark that we're ready.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        Ok(())
    }

    /// Handle a line of user input.
    ///
    /// If the input starts with `/` it is treated as a slash command.
    /// Otherwise it is sent as a user message to the agent.
    pub async fn handle_input(&mut self, input: &str) -> Result<(), AgentError> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        // Store in input history
        self.input_history.push(trimmed.to_string());
        self.history_index = self.input_history.len();

        if trimmed.starts_with('/') {
            // Parse the slash command and its arguments
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            let cmd = parts[0];
            let args: Vec<&str> = parts
                .get(1)
                .map(|s| s.split_whitespace().collect())
                .unwrap_or_default();

            let result = crate::commands::handle_slash_command(self, cmd, &args).await?;
            if result == crate::commands::CommandResult::Quit {
                self.running = false;
            }
        } else {
            // Regular user message
            self.messages.push(hermes_core::Message::user(trimmed));
            self.run_agent().await?;
        }

        Ok(())
    }

    /// Handle a slash command string (without the leading `/`).
    pub async fn handle_command(&mut self, cmd: &str) -> Result<(), AgentError> {
        let trimmed = cmd.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
        let slash_cmd = if parts[0].starts_with('/') {
            parts[0]
        } else {
            // Prepend / if not present
            return self.handle_input(&format!("/{}", trimmed)).await;
        };

        let args: Vec<&str> = parts
            .get(1)
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        let result = crate::commands::handle_slash_command(self, slash_cmd, &args).await?;
        if result == crate::commands::CommandResult::Quit {
            self.running = false;
        }
        Ok(())
    }

    /// Create a new session, clearing all messages.
    pub fn new_session(&mut self) {
        self.session_id = Uuid::new_v4().to_string();
        self.messages.clear();
        self.input_history.clear();
        self.history_index = 0;
    }

    /// Reset the current session (clear messages but keep session ID).
    pub fn reset_session(&mut self) {
        self.messages.clear();
        self.input_history.clear();
        self.history_index = 0;
    }

    /// Retry the last user message by re-sending it to the agent.
    ///
    /// Finds the last user message in history, removes all messages after it
    /// (including the assistant response), and re-runs the agent.
    pub async fn retry_last(&mut self) -> Result<(), AgentError> {
        // Find the last user message
        let last_user_idx = self
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User);

        if let Some(idx) = last_user_idx {
            let last_user_msg = self.messages[idx].clone();
            // Truncate messages to just before the last user message
            self.messages.truncate(idx);
            // Re-add the user message
            self.messages.push(last_user_msg);
            // Re-run the agent
            self.run_agent().await?;
        }

        Ok(())
    }

    /// Undo the last exchange (remove the last user message and its response).
    pub fn undo_last(&mut self) {
        // Find the last user message
        if let Some(idx) = self
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User)
        {
            // Remove everything from the last user message onward
            self.messages.truncate(idx);
        }
    }

    /// Switch the active model, rebuilding the provider and agent loop.
    pub fn switch_model(&mut self, provider_model: &str) {
        self.current_model = provider_model.to_string();

        let provider = build_provider(&self.config, &self.current_model);
        let agent_config = build_agent_config(&self.config, &self.current_model);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&self.tool_registry));

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider);
        let hermes_home = hermes_home_dir();
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(&agent_inner, hermes_home));
        self.agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        tracing::info!("Switched model to: {}", provider_model);
    }

    /// Switch the active personality.
    pub fn switch_personality(&mut self, name: &str) {
        self.current_personality = Some(name.to_string());
        tracing::info!("Switched personality to: {}", name);
    }

    /// Run the agent on the current message history.
    ///
    /// Sends all messages to the agent loop and appends the result.
    /// Checks the interrupt controller before running and clears it after.
    async fn run_agent(&mut self) -> Result<(), AgentError> {
        self.interrupt_controller.clear_interrupt();

        let messages = self.messages.clone();
        let result = if self.config.streaming.enabled {
            let stream_handle = self.stream_handle.clone();
            let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                stream_handle.map(|h| {
                    Box::new(move |chunk: hermes_core::StreamChunk| {
                        h.send_chunk(chunk);
                    }) as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
                });
            self.agent
                .run_stream(messages, Some(self.tool_schemas.clone()), stream_cb)
                .await
        } else {
            self.agent
                .run(messages, Some(self.tool_schemas.clone()))
                .await
        };

        match result {
            Ok(result) => {
                self.messages = result.messages;
                if let Some(handle) = &self.stream_handle {
                    handle.send_done();
                }
                if result.interrupted {
                    tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                    println!("[Agent execution interrupted]");
                } else if !result.finished_naturally {
                    tracing::warn!(
                        "Agent stopped after {} turns (did not finish naturally)",
                        result.total_turns
                    );
                }
            }
            Err(AgentError::Interrupted { message }) => {
                self.interrupt_controller.clear_interrupt();
                if let Some(handle) = &self.stream_handle {
                    handle.send_done();
                }
                if let Some(redirect) = message {
                    tracing::info!("Agent interrupted with redirect: {}", redirect);
                } else {
                    tracing::info!("Agent interrupted by user");
                }
                println!("[Agent execution interrupted]");
            }
            Err(e) => {
                if let Some(handle) = &self.stream_handle {
                    handle.send_done();
                }
                return Err(e);
            }
        }

        Ok(())
    }

    /// Get a serializable snapshot of the current session info.
    pub fn session_info(&self) -> SessionInfo {
        SessionInfo {
            session_id: self.session_id.clone(),
            model: self.current_model.clone(),
            personality: self.current_personality.clone(),
            message_count: self.messages.len(),
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }

    /// Navigate backward in input history.
    pub fn history_prev(&mut self) -> Option<&str> {
        if self.history_index > 0 {
            self.history_index -= 1;
            self.input_history
                .get(self.history_index)
                .map(|s| s.as_str())
        } else {
            None
        }
    }

    /// Navigate forward in input history.
    pub fn history_next(&mut self) -> Option<&str> {
        if self.history_index < self.input_history.len() {
            self.history_index += 1;
            if self.history_index < self.input_history.len() {
                self.input_history
                    .get(self.history_index)
                    .map(|s| s.as_str())
            } else {
                None
            }
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::LlmProviderConfig;
    use std::collections::HashMap;

    #[test]
    fn test_session_info_serialization() {
        let info = SessionInfo {
            session_id: "test-123".to_string(),
            model: "gpt-4o".to_string(),
            personality: Some("helpful".to_string()),
            message_count: 5,
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&info).unwrap();
        let back: SessionInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "test-123");
        assert_eq!(back.model, "gpt-4o");
    }

    #[test]
    fn test_build_agent_config_maps_runtime_provider_api_key_env() {
        let mut cfg = GatewayConfig::default();
        let mut providers = HashMap::new();
        providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                api_key: None,
                api_key_env: Some("MY_FALLBACK_KEY".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        cfg.llm_providers = providers;

        let agent_cfg = build_agent_config(&cfg, "custom:some-model");
        let runtime = agent_cfg
            .runtime_providers
            .get("custom")
            .expect("runtime provider should exist");
        assert_eq!(runtime.api_key_env.as_deref(), Some("MY_FALLBACK_KEY"));
    }

    #[test]
    fn test_build_agent_config_infers_provider_for_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("claude-opus-4-6".to_string());
        cfg.llm_providers.insert(
            "anthropic".to_string(),
            LlmProviderConfig {
                model: Some("claude-opus-4-6".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let agent_cfg = build_agent_config(&cfg, "claude-opus-4-6");
        assert_eq!(agent_cfg.provider.as_deref(), Some("anthropic"));
    }

    #[test]
    fn test_resolve_provider_and_model_uses_single_provider_fallback() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("stepfun".to_string(), LlmProviderConfig::default());
        let (provider, model) = resolve_provider_and_model(&cfg, "step-3.5-flash");
        assert_eq!(provider, "stepfun");
        assert_eq!(model, "step-3.5-flash");
    }

    #[test]
    fn test_provider_api_key_from_env_supports_stepfun() {
        let hermes_var = "HERMES_STEPFUN_API_KEY";
        let stepfun_var = "STEPFUN_API_KEY";
        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);

        std::env::set_var(stepfun_var, "stepfun-direct");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-direct")
        );

        std::env::set_var(hermes_var, "stepfun-hermes");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-hermes")
        );

        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_openai_codex() {
        let var = "HERMES_OPENAI_CODEX_API_KEY";
        std::env::remove_var(var);
        std::env::set_var(var, "codex-oauth-token");
        assert_eq!(
            provider_api_key_from_env("openai-codex").as_deref(),
            Some("codex-oauth-token")
        );
        std::env::remove_var(var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_qwen_oauth() {
        let oauth_var = "HERMES_QWEN_OAUTH_API_KEY";
        let fallback_var = "DASHSCOPE_API_KEY";
        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);

        std::env::set_var(fallback_var, "dashscope-fallback");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("dashscope-fallback")
        );

        std::env::set_var(oauth_var, "qwen-oauth-token");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("qwen-oauth-token")
        );

        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);
    }
}

// ---------------------------------------------------------------------------
// Helper: build AgentConfig from GatewayConfig
// ---------------------------------------------------------------------------

pub fn build_agent_config(config: &GatewayConfig, model: &str) -> AgentConfig {
    let (resolved_provider, _) = resolve_provider_and_model(config, model);
    let skip_memory_env = std::env::var("HERMES_SKIP_MEMORY")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let skip_context_files_env = std::env::var("HERMES_SKIP_CONTEXT_FILES")
        .ok()
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    let hermes_home = config
        .home_dir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let skip_memory = skip_memory_env || hermes_home.join(".memory_disabled").exists();
    let skip_context_files = config.agent.skip_context_files || skip_context_files_env;

    AgentConfig {
        max_turns: config.max_turns,
        budget: config.budget.clone(),
        model: model.to_string(),
        system_prompt: config.system_prompt.clone(),
        personality: config.personality.clone(),
        hermes_home: config.home_dir.clone(),
        provider: Some(resolved_provider),
        stream: config.streaming.enabled,
        skip_memory,
        skip_context_files,
        platform: Some("cli".to_string()),
        pass_session_id: true,
        runtime_providers: config
            .llm_providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    hermes_agent::agent_loop::RuntimeProviderConfig {
                        api_key: cfg.api_key.clone(),
                        api_key_env: cfg.api_key_env.clone(),
                        base_url: cfg.base_url.clone(),
                        command: cfg.command.clone(),
                        args: cfg.args.clone(),
                        oauth_token_url: cfg.oauth_token_url.clone(),
                        oauth_client_id: cfg.oauth_client_id.clone(),
                    },
                )
            })
            .collect(),
        smart_model_routing: hermes_agent::agent_loop::SmartModelRoutingConfig {
            enabled: config.smart_model_routing.enabled,
            max_simple_chars: config.smart_model_routing.max_simple_chars,
            max_simple_words: config.smart_model_routing.max_simple_words,
            cheap_model: config.smart_model_routing.cheap_model.as_ref().map(|m| {
                hermes_agent::agent_loop::CheapModelRouteConfig {
                    provider: m.provider.clone(),
                    model: m.model.clone(),
                    base_url: m.base_url.clone(),
                    api_key_env: m.api_key_env.clone(),
                }
            }),
        },
        memory_nudge_interval: config.agent.memory_nudge_interval,
        skill_creation_nudge_interval: config.agent.skill_creation_nudge_interval,
        background_review_enabled: config.agent.background_review_enabled,
        ..AgentConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Helper: bridge hermes_tools::ToolRegistry → agent_loop::ToolRegistry
// ---------------------------------------------------------------------------

pub fn bridge_tool_registry(tools: &ToolRegistry) -> AgentToolRegistry {
    let mut agent_registry = AgentToolRegistry::new();
    for schema in tools.get_definitions() {
        let name = schema.name.clone();
        let tools_clone = tools.clone();
        agent_registry.register(
            name.clone(),
            schema,
            Arc::new(
                move |params: Value| -> Result<String, hermes_core::ToolError> {
                    Ok(tools_clone.dispatch(&name, params))
                },
            ),
        );
    }
    agent_registry
}

// ---------------------------------------------------------------------------
// Helper: build LLM provider from config + model string
// ---------------------------------------------------------------------------

const STEPFUN_BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";
const OPENAI_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

fn resolve_provider_and_model(config: &GatewayConfig, model: &str) -> (String, String) {
    let trimmed = model.trim();
    if let Some((provider, model_name)) = trimmed.split_once(':') {
        return (provider.trim().to_string(), model_name.trim().to_string());
    }

    if let Some((provider, _)) = config.llm_providers.iter().find(|(_, cfg)| {
        cfg.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .is_some_and(|m| m == trimmed)
    }) {
        return (provider.to_string(), trimmed.to_string());
    }

    if config.llm_providers.len() == 1 {
        if let Some((provider, _)) = config.llm_providers.iter().next() {
            return (provider.to_string(), trimmed.to_string());
        }
    }

    ("openai".to_string(), trimmed.to_string())
}

fn resolve_api_key_literal_or_env_ref(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(env_ref).ok().filter(|v| !v.trim().is_empty());
    }
    Some(trimmed.to_string())
}

/// Resolve API key / token for a named LLM provider from well-known environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    match provider {
        "openai" => std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "openai-codex" | "codex" => std::env::var("HERMES_OPENAI_CODEX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "anthropic" => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "openrouter" => std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen" => std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen-oauth" => std::env::var("HERMES_QWEN_OAUTH_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "kimi" | "moonshot" => std::env::var("MOONSHOT_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "minimax" => std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "stepfun" => std::env::var("HERMES_STEPFUN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "nous" => std::env::var("NOUS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "copilot" => std::env::var("GITHUB_COPILOT_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    let (provider_name, model_name) = resolve_provider_and_model(config, model);

    let provider_config = config.llm_providers.get(provider_name.as_str());

    let api_key = provider_config
        .and_then(|c| c.api_key.as_deref())
        .and_then(resolve_api_key_literal_or_env_ref)
        .or_else(|| {
            provider_config
                .and_then(|c| c.api_key_env.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .and_then(|name| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| provider_api_key_from_env(provider_name.as_str()));

    let api_key = match api_key {
        Some(k) => k,
        None => {
            tracing::warn!("No API key for provider '{provider_name}'; using NoBackendProvider");
            return Arc::new(NoBackendProvider {
                model: model.to_string(),
            });
        }
    };

    let base_url = provider_config.and_then(|c| c.base_url.clone());

    match provider_name.as_str() {
        "openai" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openai-codex" | "codex" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            p = p.with_base_url(base_url.unwrap_or_else(|| OPENAI_CODEX_BASE_URL.to_string()));
            Arc::new(p)
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => {
            let p = OpenRouterProvider::new(&api_key).with_model(model_name.as_str());
            Arc::new(p)
        }
        "qwen" | "qwen-oauth" => {
            let mut p = QwenProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "kimi" | "moonshot" => {
            let mut p = KimiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "minimax" => {
            let mut p = MiniMaxProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "stepfun" => {
            let url = base_url.unwrap_or_else(|| STEPFUN_BASE_URL.to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
        }
        "nous" => {
            let mut p = NousProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "copilot" => {
            let p = CopilotProvider::new(
                base_url.unwrap_or_else(|| "https://api.github.com/copilot".to_string()),
                &api_key,
            )
            .with_model(model_name.as_str());
            Arc::new(p)
        }
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
        }
    }
}

// ---------------------------------------------------------------------------
// NoBackendProvider — explicit fallback when no API key is configured
// ---------------------------------------------------------------------------

struct NoBackendProvider {
    model: String,
}

#[async_trait::async_trait]
impl LlmProvider for NoBackendProvider {
    async fn chat_completion(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Err(AgentError::LlmApi(format!(
            "NoBackendProvider: no LLM backend configured for model '{}'. \
             Configure an API key and provider in the config file.",
            self.model
        )))
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
        futures::stream::once(async move {
            Err(AgentError::LlmApi(
                "NoBackendProvider: no LLM backend configured for streaming.".to_string(),
            ))
        })
        .boxed()
    }
}
