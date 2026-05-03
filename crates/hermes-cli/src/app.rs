//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

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
use hermes_agent::{
    AgentCallbacks, AgentConfig, AgentLoop, InterruptController, SessionPersistence,
};
use hermes_config::{cron_dir, hermes_home as hermes_home_dir, load_config, GatewayConfig};
use hermes_core::ToolSchema;
use hermes_core::{AgentError, LlmProvider};
use hermes_cron::cron_scheduler_for_data_dir;
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;

use crate::cli::Cli;
use crate::commands::recover_queued_background_jobs;
use crate::model_switch::provider_model_ids;
use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
use crate::terminal_backend::build_terminal_backend;
use crate::tui::StreamHandle;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PetDock {
    Left,
    #[default]
    Right,
}

impl PetDock {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PetSettings {
    pub enabled: bool,
    pub species: String,
    pub mood: String,
    pub dock: PetDock,
    pub tick_ms: u64,
}

impl Default for PetSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            species: "boba".to_string(),
            mood: "ready".to_string(),
            dock: PetDock::Right,
            tick_ms: 420,
        }
    }
}

impl PetSettings {
    const SPECIES: [&'static str; 6] = ["boba", "bytecat", "otter", "fox", "owl", "capy"];
    const MOODS: [&'static str; 5] = ["ready", "working", "sleepy", "hyped", "chill"];
    const MIN_TICK_MS: u64 = 120;
    const MAX_TICK_MS: u64 = 2000;

    pub fn normalized(mut self) -> Self {
        let species = self.species.trim().to_ascii_lowercase();
        if Self::SPECIES.iter().any(|candidate| *candidate == species) {
            self.species = species;
        } else {
            self.species = Self::default().species;
        }

        let mood = self.mood.trim().to_ascii_lowercase();
        if Self::MOODS.iter().any(|candidate| *candidate == mood) {
            self.mood = mood;
        } else {
            self.mood = Self::default().mood;
        }

        self.tick_ms = self.tick_ms.clamp(Self::MIN_TICK_MS, Self::MAX_TICK_MS);
        self
    }

    pub fn species_catalog() -> &'static [&'static str] {
        &Self::SPECIES
    }

    pub fn mood_catalog() -> &'static [&'static str] {
        &Self::MOODS
    }
}

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

    /// UI-only transcript messages (slash commands, local notices), anchored
    /// to a conversation index so they do not pollute model context.
    pub ui_messages: Vec<UiTranscriptMessage>,

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
    /// Shared streaming sink used by agent callbacks for progress events.
    stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>>,
    /// Whether TUI mouse events are enabled.
    pub mouse_enabled: bool,
    /// Optional durable objective for the current interactive session.
    pub session_objective: Option<String>,
    /// Animated companion pet settings.
    pub pet_settings: PetSettings,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("session_id", &self.session_id)
            .field("running", &self.running)
            .field("current_model", &self.current_model)
            .field("current_personality", &self.current_personality)
            .field("history_index", &self.history_index)
            .field("mouse_enabled", &self.mouse_enabled)
            .field("session_objective", &self.session_objective)
            .field("pet_settings", &self.pet_settings)
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

/// A TUI-local transcript message anchored to a conversation position.
#[derive(Debug, Clone)]
pub struct UiTranscriptMessage {
    /// Conversation message count at insertion time.
    pub insert_at: usize,
    /// Rendered message payload.
    pub message: hermes_core::Message,
}

// ---------------------------------------------------------------------------
// App implementation
// ---------------------------------------------------------------------------

impl App {
    const SESSION_OBJECTIVE_PREFIX: &'static str = "[SESSION_OBJECTIVE] ";

    fn push_stream_extra_event(
        shared: &Arc<StdMutex<Option<StreamHandle>>>,
        payload: serde_json::Value,
    ) {
        if let Ok(guard) = shared.lock() {
            if let Some(handle) = guard.clone() {
                handle.send_chunk(hermes_core::StreamChunk {
                    delta: Some(hermes_core::StreamDelta {
                        content: None,
                        tool_calls: None,
                        extra: Some(payload),
                    }),
                    finish_reason: None,
                    usage: None,
                });
            }
        }
    }

    fn preview_for_status(raw: &str, max_chars: usize) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
        if collapsed.chars().count() <= max_chars {
            collapsed
        } else {
            let mut out: String = collapsed
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect();
            out.push('…');
            out
        }
    }

    fn stream_callbacks(shared: Arc<StdMutex<Option<StreamHandle>>>) -> AgentCallbacks {
        let thinking_shared = shared.clone();
        let tool_start_shared = shared.clone();
        let tool_done_shared = shared.clone();
        let status_shared = shared;
        AgentCallbacks {
            on_thinking: Some(Box::new(move |thinking: &str| {
                let preview = App::preview_for_status(thinking, 220);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &thinking_shared,
                    serde_json::json!({
                        "ui_event": "thinking",
                        "text": preview,
                    }),
                );
            })),
            on_tool_start: Some(Box::new(move |tool: &str, args: &Value| {
                let arg_preview = App::preview_for_status(&args.to_string(), 140);
                App::push_stream_extra_event(
                    &tool_start_shared,
                    serde_json::json!({
                        "ui_event": "tool_start",
                        "tool": tool,
                        "args_preview": arg_preview,
                    }),
                );
            })),
            on_tool_complete: Some(Box::new(move |tool: &str, content: &str| {
                let preview = App::preview_for_status(content, 160);
                App::push_stream_extra_event(
                    &tool_done_shared,
                    serde_json::json!({
                        "ui_event": "tool_complete",
                        "tool": tool,
                        "result_preview": preview,
                    }),
                );
            })),
            status_callback: Some(Arc::new(move |event_type: &str, message: &str| {
                let preview = App::preview_for_status(message, 200);
                if preview.is_empty() {
                    return;
                }
                App::push_stream_extra_event(
                    &status_shared,
                    serde_json::json!({
                        "ui_event": "status",
                        "event_type": event_type,
                        "message": preview,
                    }),
                );
            })),
            ..AgentCallbacks::default()
        }
    }

    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        apply_cli_runtime_overrides(&mut config, &cli);

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

        let configured_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
        let current_model = resolve_startup_model(&config, &configured_model);
        let current_personality = config.personality.clone();

        sync_runtime_model_env(&config, &current_model);

        let tool_registry = Arc::new(ToolRegistry::new());
        if default_rtk_raw_mode() {
            tool_registry.set_raw_mode(true);
        }
        let stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>> =
            Arc::new(StdMutex::new(None));
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

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(Self::stream_callbacks(stream_handle_shared.clone()));
        let hermes_home = hermes_home_dir();
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(&agent_inner, hermes_home));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        let recovered_background_jobs = recover_queued_background_jobs(8);
        if recovered_background_jobs > 0 {
            tracing::info!(
                "Recovered {} queued background job(s) from durable status queue",
                recovered_background_jobs
            );
        }

        Ok(Self {
            config: Arc::new(config),
            agent,
            tool_registry,
            tool_schemas,
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id: Uuid::new_v4().to_string(),
            running: true,
            current_model,
            current_personality,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared,
            mouse_enabled: default_mouse_enabled(),
            session_objective: None,
            pet_settings: load_pet_settings(),
        })
    }

    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        if let Ok(mut guard) = self.stream_handle_shared.lock() {
            *guard = handle.clone();
        }
        self.stream_handle = handle;
    }

    /// Enable/disable TUI mouse handling.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.mouse_enabled = enabled;
    }

    /// Current TUI mouse handling state.
    pub fn mouse_enabled(&self) -> bool {
        self.mouse_enabled
    }

    /// Retrieve current companion pet settings.
    pub fn pet_settings(&self) -> &PetSettings {
        &self.pet_settings
    }

    /// Update and persist companion pet settings.
    pub fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        let normalized = settings.normalized();
        persist_pet_settings(&normalized)?;
        self.pet_settings = normalized;
        Ok(())
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
            if self.stream_handle.is_some() {
                self.push_ui_user(trimmed);
            }
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

        if self.stream_handle.is_some() {
            self.push_ui_user(trimmed);
        }

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
        self.ui_messages.clear();
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
    }

    /// Reset the current session (clear messages but keep session ID).
    pub fn reset_session(&mut self) {
        self.messages.clear();
        self.ui_messages.clear();
        self.session_objective = None;
        self.input_history.clear();
        self.history_index = 0;
    }

    /// Set or clear a durable session objective.
    ///
    /// The objective is represented as a synthetic system message so it is
    /// applied consistently on every turn without requiring user re-entry.
    pub fn set_session_objective(&mut self, objective: Option<String>) {
        self.messages.retain(|m| {
            if m.role != hermes_core::MessageRole::System {
                return true;
            }
            !m.content
                .as_deref()
                .unwrap_or_default()
                .starts_with(Self::SESSION_OBJECTIVE_PREFIX)
        });

        self.session_objective = objective
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        if let Some(obj) = &self.session_objective {
            let system =
                hermes_core::Message::system(format!("{}{}", Self::SESSION_OBJECTIVE_PREFIX, obj));
            self.messages.insert(0, system);
        }
        self.prune_ui_after_current_messages();
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
            self.prune_ui_after_current_messages();
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
            self.prune_ui_after_current_messages();
        }
    }

    /// Switch the active model, rebuilding the provider and agent loop.
    pub fn switch_model(&mut self, provider_model: &str) {
        self.current_model = provider_model.to_string();
        sync_runtime_model_env(&self.config, &self.current_model);

        let provider = build_provider(&self.config, &self.current_model);
        let agent_config = build_agent_config(&self.config, &self.current_model);
        let agent_tool_registry = Arc::new(bridge_tool_registry(&self.tool_registry));

        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(Self::stream_callbacks(self.stream_handle_shared.clone()));
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
        let mut remediation_attempted = false;
        loop {
            let messages = self.messages.clone();
            let result = if self.config.streaming.enabled {
                let stream_handle = self.stream_handle.clone();
                let stream_cb: Option<Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>> =
                    stream_handle.map(|h| {
                        Box::new(move |chunk: hermes_core::StreamChunk| {
                            h.send_chunk(chunk);
                        })
                            as Box<dyn Fn(hermes_core::StreamChunk) + Send + Sync>
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
                    self.prune_ui_after_current_messages();
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if result.interrupted {
                        tracing::info!("Agent loop returned interrupted=true (graceful stop)");
                        if self.stream_handle.is_some() {
                            self.push_ui_assistant("[Agent execution interrupted]");
                        } else {
                            println!("[Agent execution interrupted]");
                        }
                    } else if !result.finished_naturally {
                        tracing::warn!(
                            "Agent stopped after {} turns (did not finish naturally)",
                            result.total_turns
                        );
                    }
                    break;
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
                    if self.stream_handle.is_some() {
                        self.push_ui_assistant("[Agent execution interrupted]");
                    } else {
                        println!("[Agent execution interrupted]");
                    }
                    break;
                }
                Err(e) => {
                    if let Some(handle) = &self.stream_handle {
                        handle.send_done();
                    }
                    if !remediation_attempted {
                        if let Some((next_model, notice)) =
                            self.model_auto_remediation_target(&e).await
                        {
                            tracing::warn!(
                                "Model auto-remediation triggered: {} -> {}",
                                self.current_model,
                                next_model
                            );
                            if self.stream_handle.is_some() {
                                self.push_ui_assistant(notice.clone());
                            } else {
                                println!("{notice}");
                            }
                            self.switch_model(&next_model);
                            remediation_attempted = true;
                            continue;
                        }
                    }
                    return Err(e);
                }
            }
        }

        Ok(())
    }

    /// Append a UI-only message anchored to the current conversation size.
    pub fn push_ui_message(&mut self, message: hermes_core::Message) {
        self.ui_messages.push(UiTranscriptMessage {
            insert_at: self.messages.len(),
            message,
        });
    }

    /// Append a UI-only user transcript line.
    pub fn push_ui_user(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::user(text.into()));
    }

    /// Append a UI-only assistant transcript line.
    pub fn push_ui_assistant(&mut self, text: impl Into<String>) {
        self.push_ui_message(hermes_core::Message::assistant(text.into()));
    }

    /// Build the merged transcript for TUI rendering.
    ///
    /// This includes durable conversation history and UI-only events in
    /// chronological order, while preserving model-facing context purity.
    pub fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        let mut merged = Vec::with_capacity(self.messages.len() + self.ui_messages.len());
        for idx in 0..=self.messages.len() {
            for ui in self.ui_messages.iter().filter(|m| m.insert_at == idx) {
                merged.push(ui.message.clone());
            }
            if idx < self.messages.len() {
                merged.push(self.messages[idx].clone());
            }
        }
        merged
    }

    fn prune_ui_after_current_messages(&mut self) {
        let cap = self.messages.len();
        self.ui_messages.retain(|m| m.insert_at <= cap);
    }

    /// Count background jobs currently queued/running.
    pub fn running_background_job_count(&self) -> usize {
        let jobs_dir = hermes_config::hermes_home().join("background_jobs");
        let mut active = 0usize;
        let entries = match std::fs::read_dir(jobs_dir) {
            Ok(entries) => entries,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
                continue;
            };
            let status = value
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if matches!(status, "queued" | "running") {
                active += 1;
            }
        }
        active
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

    fn model_auto_remediation_enabled() -> bool {
        !matches!(
            std::env::var("HERMES_MODEL_AUTO_REMEDIATE")
                .ok()
                .as_deref()
                .map(|v| v.trim().to_ascii_lowercase()),
            Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
        )
    }

    fn is_model_not_found_error(err: &AgentError) -> bool {
        let message = match err {
            AgentError::LlmApi(msg)
            | AgentError::Config(msg)
            | AgentError::ToolExecution(msg)
            | AgentError::Gateway(msg) => msg.to_ascii_lowercase(),
            _ => return false,
        };
        let model_not_found = message.contains("model not found")
            || message.contains("requested model does not exist")
            || message.contains("404 not found")
            || message.contains("openrouter catalog");
        model_not_found && message.contains("model")
    }

    async fn model_auto_remediation_target(&self, err: &AgentError) -> Option<(String, String)> {
        if !Self::model_auto_remediation_enabled() || !Self::is_model_not_found_error(err) {
            return None;
        }

        let (provider, current_model_id) = self
            .current_model
            .split_once(':')
            .unwrap_or(("openai", self.current_model.as_str()));
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return None;
        }

        let catalog = provider_model_ids(&provider).await;
        if catalog.is_empty() {
            return None;
        }

        let current_trimmed = current_model_id.trim().to_ascii_lowercase();
        let slash_suffix = format!("/{}", current_trimmed);
        let selected = catalog
            .iter()
            .find(|m| {
                let lower = m.trim().to_ascii_lowercase();
                lower == current_trimmed || lower.ends_with(&slash_suffix)
            })
            .cloned()
            .or_else(|| catalog.first().cloned())?;

        let next_model = format!("{}:{}", provider, selected.trim());
        if next_model.eq_ignore_ascii_case(&self.current_model) {
            return None;
        }
        let notice = format!(
            "Model catalog remediation: `{}` failed with not-found; switching to `{}` and retrying once.",
            self.current_model, next_model
        );
        Some((next_model, notice))
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

fn apply_cli_runtime_overrides(config: &mut GatewayConfig, cli: &Cli) {
    if let Some(ref model) = cli.model {
        config.model = Some(model.clone());
    }
    if let Some(ref personality) = cli.personality {
        config.personality = Some(personality.clone());
    }
    if let Some(provider) = cli
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let provider = normalize_runtime_provider_name(provider);
        let existing_model = config.model.as_deref().unwrap_or("gpt-4o").trim();
        let model_name = existing_model
            .split_once(':')
            .map(|(_, name)| name.trim())
            .unwrap_or(existing_model);
        config.model = Some(format!("{provider}:{model_name}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::LlmProviderConfig;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env test lock")
    }

    fn build_minimal_test_app() -> App {
        let config = Arc::new(GatewayConfig::default());
        let tool_registry = Arc::new(ToolRegistry::new());
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let agent_config = build_agent_config(config.as_ref(), "openai:gpt-4o");
        let provider: Arc<dyn LlmProvider> = Arc::new(NoBackendProvider {
            model: "openai:gpt-4o".to_string(),
        });
        let agent_inner = AgentLoop::new(agent_config, agent_tool_registry, provider)
            .with_callbacks(App::stream_callbacks(Arc::new(StdMutex::new(None))));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            hermes_home_dir(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        App {
            config,
            agent,
            tool_registry,
            tool_schemas: Vec::new(),
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_id: "test-session".to_string(),
            running: true,
            current_model: "openai:gpt-4o".to_string(),
            current_personality: None,
            input_history: Vec::new(),
            history_index: 0,
            interrupt_controller: InterruptController::new(),
            stream_handle: None,
            stream_handle_shared: Arc::new(StdMutex::new(None)),
            mouse_enabled: true,
            session_objective: None,
            pet_settings: PetSettings::default(),
        }
    }

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
    fn test_apply_cli_runtime_overrides_applies_provider_to_prefixed_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("openai:gpt-4o".to_string());
        let cli = Cli {
            command: None,
            verbose: false,
            config_dir: None,
            model: None,
            provider: Some("nous".to_string()),
            oneshot: None,
            allow_tools: false,
            personality: None,
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("nous:gpt-4o"));
    }

    #[test]
    fn test_apply_cli_runtime_overrides_applies_provider_to_bare_model() {
        let mut cfg = GatewayConfig::default();
        cfg.model = Some("moonshotai/kimi-k2.6".to_string());
        let cli = Cli {
            command: None,
            verbose: false,
            config_dir: None,
            model: None,
            provider: Some("anthropic".to_string()),
            oneshot: None,
            allow_tools: false,
            personality: None,
        };

        apply_cli_runtime_overrides(&mut cfg, &cli);
        assert_eq!(cfg.model.as_deref(), Some("anthropic:moonshotai/kimi-k2.6"));
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
    fn test_resolve_startup_model_prefers_provider_runtime_model_for_provider_slug() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "nous".to_string(),
            LlmProviderConfig {
                model: Some("moonshotai/kimi-k2.6".to_string()),
                ..LlmProviderConfig::default()
            },
        );
        let startup = resolve_startup_model(&cfg, "nous");
        assert_eq!(startup, "nous:moonshotai/kimi-k2.6");
    }

    #[test]
    fn test_sync_runtime_model_env_sets_model_and_provider_values() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("anthropic".to_string(), LlmProviderConfig::default());

        let keys = [
            "HERMES_MODEL",
            "HERMES_INFERENCE_MODEL",
            "HERMES_INFERENCE_PROVIDER",
            "HERMES_TUI_PROVIDER",
        ];
        for key in keys {
            std::env::remove_var(key);
        }
        std::env::set_var("HERMES_TUI_PROVIDER", "openai");

        sync_runtime_model_env(&cfg, "anthropic:claude-sonnet-4-6");

        assert_eq!(
            std::env::var("HERMES_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_MODEL").ok().as_deref(),
            Some("anthropic:claude-sonnet-4-6")
        );
        assert_eq!(
            std::env::var("HERMES_INFERENCE_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            std::env::var("HERMES_TUI_PROVIDER").ok().as_deref(),
            Some("anthropic")
        );

        for key in keys {
            std::env::remove_var(key);
        }
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
    fn test_provider_api_key_from_env_supports_anthropic_aliases() {
        let primary = "ANTHROPIC_API_KEY";
        let secondary = "ANTHROPIC_TOKEN";
        let tertiary = "CLAUDE_CODE_OAUTH_TOKEN";
        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);

        std::env::set_var(tertiary, "claude-oauth-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("claude-oauth-token")
        );

        std::env::set_var(secondary, "anthropic-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-token")
        );

        std::env::set_var(primary, "anthropic-api-key");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-api-key")
        );

        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);
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

    #[test]
    fn test_provider_api_key_from_env_supports_google_gemini_cli() {
        let var = "HERMES_GEMINI_OAUTH_API_KEY";
        std::env::remove_var(var);
        std::env::set_var(var, "google-gemini-oauth-token");
        assert_eq!(
            provider_api_key_from_env("google-gemini-cli").as_deref(),
            Some("google-gemini-oauth-token")
        );
        std::env::remove_var(var);
    }

    #[test]
    fn test_provider_api_key_from_env_supports_extended_registry() {
        let checks = [
            ("AI_GATEWAY_API_KEY", "ai-gateway"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("HF_TOKEN", "huggingface"),
            ("KILOCODE_API_KEY", "kilocode"),
            ("NVIDIA_API_KEY", "nvidia"),
            ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
            ("LLAMA_CPP_API_KEY", "llama-cpp"),
            ("VLLM_API_KEY", "vllm"),
            ("MLX_API_KEY", "mlx"),
            ("APPLE_ANE_API_KEY", "apple-ane"),
            ("SGLANG_API_KEY", "sglang"),
            ("TGI_API_KEY", "tgi"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
            ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
            ("XAI_API_KEY", "xai"),
            ("XIAOMI_API_KEY", "xiaomi"),
            ("GLM_API_KEY", "zai"),
        ];
        for (env_var, provider) in checks {
            std::env::remove_var(env_var);
            let expected = format!("token-for-{provider}");
            std::env::set_var(env_var, expected.clone());
            assert_eq!(
                provider_api_key_from_env(provider).as_deref(),
                Some(expected.as_str())
            );
            std::env::remove_var(env_var);
        }
    }

    #[test]
    fn test_normalize_runtime_provider_name_covers_aliases() {
        assert_eq!(
            normalize_runtime_provider_name("gemini-cli"),
            "google-gemini-cli"
        );
        assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
        assert_eq!(
            normalize_runtime_provider_name("alibaba-coding-plan"),
            "qwen"
        );
        assert_eq!(normalize_runtime_provider_name("opencode"), "opencode-zen");
        assert_eq!(normalize_runtime_provider_name("ollama"), "ollama-local");
        assert_eq!(normalize_runtime_provider_name("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("ollvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("llvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("mlx-lm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("ane"), "apple-ane");
    }

    #[test]
    fn test_allow_no_api_key_for_local_backends_and_private_base_urls() {
        assert!(allow_no_api_key("ollama-local", "ollama-local", None));
        assert!(allow_no_api_key(
            "openai",
            "openai",
            Some("http://127.0.0.1:11434/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://192.168.1.20:8000/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://[::1]:11434/v1")
        ));
        assert!(!allow_no_api_key(
            "openai",
            "openai",
            Some("https://api.openai.com/v1")
        ));
    }

    #[test]
    fn test_default_mouse_enabled_respects_env_override() {
        std::env::remove_var("HERMES_TUI_MOUSE");
        assert!(default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "off");
        assert!(!default_mouse_enabled());

        std::env::set_var("HERMES_TUI_MOUSE", "1");
        assert!(default_mouse_enabled());

        std::env::remove_var("HERMES_TUI_MOUSE");
    }

    #[test]
    fn test_pet_settings_normalization_clamps_and_rewrites_invalid_values() {
        let input = PetSettings {
            enabled: true,
            species: "unknown".to_string(),
            mood: "invalid".to_string(),
            dock: PetDock::Left,
            tick_ms: 10,
        };
        let normalized = input.normalized();
        assert!(normalized.enabled);
        assert_eq!(normalized.species, "boba");
        assert_eq!(normalized.mood, "ready");
        assert_eq!(normalized.dock, PetDock::Left);
        assert_eq!(normalized.tick_ms, 120);
    }

    #[test]
    fn test_load_pet_settings_uses_persisted_file_if_present() {
        let _lock = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        std::fs::write(
            tmp.path().join("pet.json"),
            r#"{"enabled":true,"species":"fox","mood":"hyped","dock":"left","tick_ms":180}"#,
        )
        .expect("write pet settings");
        let loaded = load_pet_settings();
        assert!(loaded.enabled);
        assert_eq!(loaded.species, "fox");
        assert_eq!(loaded.mood, "hyped");
        assert_eq!(loaded.dock, PetDock::Left);
        assert_eq!(loaded.tick_ms, 180);
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn test_default_rtk_raw_mode_respects_env_override() {
        std::env::remove_var("HERMES_RTK_RAW");
        assert!(!default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "on");
        assert!(default_rtk_raw_mode());

        std::env::set_var("HERMES_RTK_RAW", "0");
        assert!(!default_rtk_raw_mode());

        std::env::remove_var("HERMES_RTK_RAW");
    }

    #[test]
    fn test_is_model_not_found_error_detects_provider_404_shape() {
        let err = AgentError::LlmApi(
            "API error 404 Not Found: model foo/bar not found in OpenRouter catalog".to_string(),
        );
        assert!(App::is_model_not_found_error(&err));
    }

    #[test]
    fn test_is_model_not_found_error_ignores_non_catalog_errors() {
        let err = AgentError::LlmApi("Rate limit exceeded".to_string());
        assert!(!App::is_model_not_found_error(&err));
    }

    #[test]
    fn test_set_session_objective_injects_replaces_and_clears_system_message() {
        let mut app = build_minimal_test_app();
        app.messages
            .push(hermes_core::Message::user("hello before objective"));

        app.set_session_objective(Some(
            "Ship parity with upstream plus stronger UX".to_string(),
        ));
        assert_eq!(
            app.session_objective.as_deref(),
            Some("Ship parity with upstream plus stronger UX")
        );
        assert_eq!(app.messages.len(), 2);
        assert_eq!(app.messages[0].role, hermes_core::MessageRole::System);
        let system = app.messages[0].content.clone().unwrap_or_default();
        assert!(system.starts_with("[SESSION_OBJECTIVE] "));
        assert!(system.contains("Ship parity with upstream plus stronger UX"));

        app.set_session_objective(Some("Minimize latency regressions".to_string()));
        let system_count = app
            .messages
            .iter()
            .filter(|m| {
                m.role == hermes_core::MessageRole::System
                    && m.content
                        .as_deref()
                        .unwrap_or_default()
                        .starts_with("[SESSION_OBJECTIVE] ")
            })
            .count();
        assert_eq!(system_count, 1);
        assert_eq!(
            app.session_objective.as_deref(),
            Some("Minimize latency regressions")
        );

        app.set_session_objective(None);
        assert!(app.session_objective.is_none());
        assert!(app.messages.iter().all(|m| {
            !m.content
                .as_deref()
                .unwrap_or_default()
                .starts_with("[SESSION_OBJECTIVE] ")
        }));
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
        enabled_skills: config.skills.enabled.clone(),
        disabled_skills: config.skills.disabled.clone(),
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
        code_index_enabled: config.agent.code_index_enabled,
        code_index_max_files: config.agent.code_index_max_files,
        code_index_max_symbols: config.agent.code_index_max_symbols,
        lsp_context_enabled: config.agent.lsp_context_enabled,
        lsp_context_max_chars: config.agent.lsp_context_max_chars,
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
const QWEN_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
const ALIBABA_CODING_PLAN_BASE_URL: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
const GOOGLE_GEMINI_CLI_BASE_URL: &str = "cloudcode-pa://google";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
const KIMI_CODING_BASE_URL: &str = "https://api.moonshot.ai/v1";
const KIMI_CODING_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";
const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
const XAI_BASE_URL: &str = "https://api.x.ai/v1";
const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const KILOCODE_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
const HUGGINGFACE_BASE_URL: &str = "https://router.huggingface.co/v1";
const XIAOMI_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const ARCEE_BASE_URL: &str = "https://api.arcee.ai/api/v1";
const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com/v1";
const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
const OLLAMA_LOCAL_BASE_URL: &str = "http://127.0.0.1:11434/v1";
const LLAMA_CPP_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const VLLM_BASE_URL: &str = "http://127.0.0.1:8000/v1";
const MLX_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const APPLE_ANE_BASE_URL: &str = "http://127.0.0.1:8081/v1";
const SGLANG_BASE_URL: &str = "http://127.0.0.1:30000/v1";
const TGI_BASE_URL: &str = "http://127.0.0.1:8082/v1";

fn normalize_runtime_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        _ => normalized,
    }
}

fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-codex" | "codex" => Some(OPENAI_CODEX_BASE_URL),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => Some(GOOGLE_GEMINI_CLI_BASE_URL),
        "gemini" | "google" => Some(GEMINI_BASE_URL),
        "qwen" | "alibaba" => Some(QWEN_BASE_URL),
        "alibaba-coding-plan" => Some(ALIBABA_CODING_PLAN_BASE_URL),
        "stepfun" | "step" | "step-plan" => Some(STEPFUN_BASE_URL),
        "ai-gateway" => Some(AI_GATEWAY_BASE_URL),
        "kimi-coding" => Some(KIMI_CODING_BASE_URL),
        "kimi-coding-cn" | "moonshot" | "kimi" => Some(KIMI_CODING_CN_BASE_URL),
        "minimax-cn" => Some(MINIMAX_CN_BASE_URL),
        "xai" => Some(XAI_BASE_URL),
        "nvidia" => Some(NVIDIA_BASE_URL),
        "opencode-go" => Some(OPENCODE_GO_BASE_URL),
        "opencode-zen" | "opencode" => Some(OPENCODE_ZEN_BASE_URL),
        "kilocode" | "kilo" => Some(KILOCODE_BASE_URL),
        "huggingface" => Some(HUGGINGFACE_BASE_URL),
        "xiaomi" => Some(XIAOMI_BASE_URL),
        "zai" => Some(ZAI_BASE_URL),
        "arcee" => Some(ARCEE_BASE_URL),
        "ollama-cloud" => Some(OLLAMA_CLOUD_BASE_URL),
        "ollama-local" | "ollama" => Some(OLLAMA_LOCAL_BASE_URL),
        "llama-cpp" | "llama.cpp" | "llamacpp" => Some(LLAMA_CPP_BASE_URL),
        "vllm" | "ollvm" | "llvm" => Some(VLLM_BASE_URL),
        "mlx" | "mlx-lm" | "apple-mlx" => Some(MLX_BASE_URL),
        "apple-ane" | "ane" | "apple-neural-engine" => Some(APPLE_ANE_BASE_URL),
        "sglang" => Some(SGLANG_BASE_URL),
        "tgi" | "text-generation-inference" => Some(TGI_BASE_URL),
        "deepseek" => Some(DEEPSEEK_BASE_URL),
        _ => None,
    }
}

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

fn resolve_startup_model(config: &GatewayConfig, configured_model: &str) -> String {
    let raw = configured_model.trim();
    if raw.is_empty() {
        return "gpt-4o".to_string();
    }
    if raw.contains(':') {
        return raw.to_string();
    }

    // If config.model is a provider slug (e.g. "nous"), prefer that provider's
    // configured runtime model instead of sending the bare slug as a model id.
    if let Some((provider, provider_cfg)) = config
        .llm_providers
        .iter()
        .find(|(provider, _)| provider.eq_ignore_ascii_case(raw))
    {
        if let Some(runtime_model) = provider_cfg
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .filter(|m| !m.eq_ignore_ascii_case(provider))
        {
            if runtime_model.contains(':') {
                return runtime_model.to_string();
            }
            return format!("{provider}:{runtime_model}");
        }
    }

    raw.to_string()
}

fn sync_runtime_model_env(config: &GatewayConfig, provider_model: &str) {
    let model = provider_model.trim();
    if model.is_empty() {
        return;
    }
    let (provider, _) = resolve_provider_and_model(config, model);
    std::env::set_var("HERMES_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_MODEL", model);
    std::env::set_var("HERMES_INFERENCE_PROVIDER", provider.as_str());
    if std::env::var_os("HERMES_TUI_PROVIDER").is_some() {
        std::env::set_var("HERMES_TUI_PROVIDER", provider.as_str());
    }
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

fn default_mouse_enabled() -> bool {
    match std::env::var("HERMES_TUI_MOUSE") {
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        Err(_) => true,
    }
}

fn pet_settings_path() -> PathBuf {
    hermes_home_dir().join("pet.json")
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_pet_settings() -> PetSettings {
    let mut settings = PetSettings::default();
    if let Ok(raw) = std::env::var("HERMES_PET") {
        if let Some(enabled) = parse_bool_env(&raw) {
            settings.enabled = enabled;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_PET_SPECIES") {
        settings.species = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_MOOD") {
        settings.mood = raw;
    }
    if let Ok(raw) = std::env::var("HERMES_PET_DOCK") {
        settings.dock = if raw.trim().eq_ignore_ascii_case("left") {
            PetDock::Left
        } else {
            PetDock::Right
        };
    }
    if let Ok(raw) = std::env::var("HERMES_PET_TICK_MS") {
        if let Ok(value) = raw.trim().parse::<u64>() {
            settings.tick_ms = value;
        }
    }
    settings.normalized()
}

fn load_pet_settings() -> PetSettings {
    let path = pet_settings_path();
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<PetSettings>(&raw).ok())
        .map(PetSettings::normalized);
    from_file.unwrap_or_else(default_pet_settings)
}

fn persist_pet_settings(settings: &PetSettings) -> Result<(), AgentError> {
    let path = pet_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            AgentError::Io(format!(
                "Failed to create pet settings directory '{}': {}",
                parent.display(),
                e
            ))
        })?;
    }
    let body = serde_json::to_string_pretty(settings)
        .map_err(|e| AgentError::Config(format!("pet settings serialization failed: {e}")))?;
    std::fs::write(&path, format!("{body}\n")).map_err(|e| {
        AgentError::Io(format!(
            "Failed to persist pet settings '{}': {}",
            path.display(),
            e
        ))
    })
}

fn default_rtk_raw_mode() -> bool {
    match std::env::var("HERMES_RTK_RAW") {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "on" | "yes"
        ),
        Err(_) => false,
    }
}

fn provider_base_url_from_env(provider: &str) -> Option<String> {
    let env_var = match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" | "ollama" => "OLLAMA_BASE_URL",
        "llama-cpp" | "llama.cpp" | "llamacpp" => "LLAMA_CPP_BASE_URL",
        "vllm" | "ollvm" | "llvm" => "VLLM_BASE_URL",
        "mlx" | "mlx-lm" | "apple-mlx" => "MLX_BASE_URL",
        "apple-ane" | "ane" | "apple-neural-engine" => "APPLE_ANE_BASE_URL",
        "sglang" => "SGLANG_BASE_URL",
        "tgi" | "text-generation-inference" => "TGI_BASE_URL",
        _ => return None,
    };
    std::env::var(env_var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn provider_is_local_backend(provider: &str) -> bool {
    matches!(
        provider.trim().to_ascii_lowercase().as_str(),
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    )
}

fn allow_no_api_key(provider_name: &str, runtime_provider: &str, base_url: Option<&str>) -> bool {
    provider_is_local_backend(runtime_provider)
        || provider_is_local_backend(provider_name)
        || base_url.is_some_and(url_is_local_or_private)
}

fn url_is_local_or_private(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let no_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = no_scheme.split('/').next().unwrap_or(no_scheme).trim();
    let host = if authority.starts_with('[') {
        authority
            .find(']')
            .map(|idx| authority[1..idx].to_string())
            .unwrap_or_else(|| authority.trim_matches(&['[', ']'][..]).to_string())
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .trim()
            .to_string()
    }
    .to_ascii_lowercase();

    if host == "localhost" {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
        };
    }
    false
}

/// Resolve API key / token for a named LLM provider from well-known environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    let provider = normalize_runtime_provider_name(provider);
    match provider.as_str() {
        "openai" => std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "openai-codex" | "codex" => std::env::var("HERMES_OPENAI_CODEX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
        }
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
        "kimi" | "moonshot" => std::env::var("KIMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("KIMI_CODING_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MOONSHOT_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("KIMI_CN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "minimax" => std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_CN_API_KEY").ok())
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
        "ai-gateway" => std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "arcee" => std::env::var("ARCEEAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ARCEE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "deepseek" => std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "huggingface" => std::env::var("HF_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "kilocode" => std::env::var("KILOCODE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nvidia" => std::env::var("NVIDIA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-cloud" => std::env::var("OLLAMA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-local" => std::env::var("OLLAMA_LOCAL_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OLLAMA_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "llama-cpp" => std::env::var("LLAMA_CPP_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "vllm" => std::env::var("VLLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "mlx" => std::env::var("MLX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "apple-ane" => std::env::var("APPLE_ANE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "sglang" => std::env::var("SGLANG_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tgi" => std::env::var("TGI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("HUGGINGFACE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "opencode-go" => std::env::var("OPENCODE_GO_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-zen" => std::env::var("OPENCODE_ZEN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xai" => std::env::var("XAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xiaomi" => std::env::var("XIAOMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "zai" => std::env::var("GLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ZAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("Z_AI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    let (provider_name, model_name) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(provider_name.as_str());

    let provider_config = config
        .llm_providers
        .get(provider_name.as_str())
        .or_else(|| config.llm_providers.get(runtime_provider.as_str()));
    let provider_config = provider_config.or_else(|| {
        config.llm_providers.iter().find_map(|(name, cfg)| {
            if name.eq_ignore_ascii_case(provider_name.as_str())
                || name.eq_ignore_ascii_case(runtime_provider.as_str())
            {
                Some(cfg)
            } else {
                None
            }
        })
    });

    let default_base_url = provider_default_base_url(provider_name.as_str())
        .or_else(|| provider_default_base_url(runtime_provider.as_str()));
    let base_url = provider_config
        .and_then(|c| c.base_url.clone())
        .or_else(|| provider_base_url_from_env(provider_name.as_str()))
        .or_else(|| provider_base_url_from_env(runtime_provider.as_str()))
        .or_else(|| default_base_url.map(ToString::to_string));

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
        .or_else(|| provider_api_key_from_env(provider_name.as_str()))
        .or_else(|| provider_api_key_from_env(runtime_provider.as_str()));

    let local_no_key_ok = allow_no_api_key(
        provider_name.as_str(),
        runtime_provider.as_str(),
        base_url.as_deref(),
    );

    let api_key = match api_key {
        Some(k) => k,
        None if local_no_key_ok => "local-no-key".to_string(),
        None => {
            tracing::warn!(
                "No API key for provider '{}'(runtime '{}'); using NoBackendProvider",
                provider_name,
                runtime_provider
            );
            return Arc::new(NoBackendProvider {
                model: model.to_string(),
            });
        }
    };

    match runtime_provider.as_str() {
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
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi" => {
            let url = base_url.unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
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
