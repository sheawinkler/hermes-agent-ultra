//! Application state management for the interactive CLI.
//!
//! The `App` struct owns the configuration, agent loop, tool registry,
//! and conversation message history. It coordinates input handling,
//! slash commands, and session management.

use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use hermes_agent::sub_agent_orchestrator::SubAgentOrchestrator;
use hermes_agent::{AgentLoop, InterruptController, SessionPersistence};
use hermes_config::{GatewayConfig, hermes_home as hermes_home_dir, load_config, state_dir};
use hermes_core::AgentError;
use hermes_cron::cron_scheduler_for_data_dir;
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;
use hermes_tools::tools::messaging::MessagingSessionContext;

use crate::cli::Cli;
use crate::commands::recover_queued_background_jobs;
use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
use crate::terminal_backend::build_terminal_backend;
use crate::tui::StreamHandle;

pub mod actors;
mod agent_run;
mod auth_refresh;
mod inference;
mod model_switch;
mod objective;
mod pet;
mod provider;
mod quorum;
mod runtime_auth;
mod runtime_env;
mod session_lifecycle;
mod session_snapshot;
mod snapshot_policy;
mod state;
mod stream_events;
pub mod traits;
mod traits_impl;
mod ui_transcript;

#[cfg(test)]
mod tests;

pub use pet::{PetDock, PetSettings};
pub use provider::{
    async_tool_dispatch_for, bridge_tool_registry, build_agent_config, build_provider,
    provider_api_key_from_env,
};
pub use state::{
    AcpState, AgentCore, ChromeState, ModelState, RuntimeFlags, SessionState, StreamState,
};
pub use traits::{
    AcpServerRuntime, AgentCoordinator, AgentDriver, AuthRuntime, ModelRuntime, SessionRuntime,
    SessionRuntimeAsync, SessionSnapshotRuntime, SlashCommandHost, TranscriptRuntime,
    UiChromeRuntime,
};

use actors::{AuthLane, PersistLane};
use pet::{load_pet_settings, persist_pet_settings};
use provider::{
    apply_cli_runtime_overrides, default_mouse_enabled, default_rtk_raw_mode,
    normalize_runtime_provider_name, resolve_provider_and_model, resolve_startup_model,
    sync_runtime_model_env,
};
use snapshot_policy::SnapshotPersistGate;

/// Top-level application state for an interactive Hermes session.
pub struct App {
    pub state_root: PathBuf,
    pub core: AgentCore,
    pub session: SessionState,
    pub model: ModelState,
    pub stream: StreamState,
    pub runtime: RuntimeFlags,
    pub chrome: ChromeState,
    pub acp: AcpState,
    pub(super) snapshot_gate: SnapshotPersistGate,
    pub(super) persist_lane: PersistLane,
    pub(super) auth_lane: AuthLane,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("state_root", &self.state_root)
            .field("session_id", &self.session.session_id)
            .field("running", &self.runtime.running)
            .field("current_model", &self.model.current_model)
            .field("current_personality", &self.model.current_personality)
            .field("history_index", &self.session.history_index)
            .field("mouse_enabled", &self.stream.mouse_enabled)
            .field("pending_theme", &self.stream.pending_theme)
            .field("pending_image_hint", &self.stream.pending_image_hint)
            .field("session_objective", &self.session.session_objective)
            .field("pending_input_prefill", &self.stream.pending_input_prefill)
            .field("quorum_armed_once", &self.runtime.quorum_armed_once)
            .field("pet_settings", &self.chrome.pet_settings)
            .finish_non_exhaustive()
    }
}

impl Clone for App {
    fn clone(&self) -> Self {
        Self {
            state_root: self.state_root.clone(),
            core: self.core.clone(),
            session: self.session.clone(),
            model: self.model.clone(),
            stream: self.stream.clone(),
            runtime: self.runtime.clone(),
            chrome: self.chrome.clone(),
            acp: self.acp.clone(),
            snapshot_gate: self.snapshot_gate.clone(),
            persist_lane: self.persist_lane.clone(),
            auth_lane: self.auth_lane.clone(),
        }
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
    /// Create a new `App` from the parsed CLI arguments.
    ///
    /// This loads (or creates) the gateway configuration, builds a tool
    /// registry with the configured tools, constructs an LLM provider,
    /// and initializes the agent loop.
    pub async fn new(cli: Cli) -> Result<Self, AgentError> {
        let state_root = state_dir(cli.config_dir.as_deref().map(std::path::Path::new));
        let config = load_config(cli.config_dir.as_deref())
            .map_err(|e| AgentError::Config(e.to_string()))?;

        let mut config = config;
        apply_cli_runtime_overrides(&mut config, &cli);
        Self::apply_explore_first_runtime_defaults();

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
        let live_count =
            crate::live_messaging::enable_live_messaging_tool(&config, &tool_registry).await;
        if live_count > 0 {
            tracing::info!(
                adapters = live_count,
                "send_message live delivery enabled via configured gateway adapters"
            );
        }
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = state_root.join("cron");
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_data_dir.display(), e)))?;
        let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(
            &tool_registry,
            cron_scheduler,
            MessagingSessionContext::new(),
        );
        let agent_tool_registry = Arc::new(bridge_tool_registry(&tool_registry));
        let tool_schemas =
            crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry);

        let agent_config = build_agent_config(&config, &current_model);
        let provider = build_provider(&config, &current_model);

        let agent_inner = hermes_agent::attach_agent_runtime(
            AgentLoop::new(agent_config, agent_tool_registry, provider)
                .with_async_tool_dispatch(async_tool_dispatch_for(tool_registry.clone()))
                .with_synced_tools_registry(tool_registry.clone()),
        )
        .with_callbacks(Self::stream_callbacks(stream_handle_shared.clone()));
        let orchestrator = Arc::new(SubAgentOrchestrator::from_parent(
            &agent_inner,
            state_root.clone(),
        ));
        let agent = Arc::new(agent_inner.with_sub_agent_orchestrator(orchestrator));

        let recovered_background_jobs = recover_queued_background_jobs(8);
        if recovered_background_jobs > 0 {
            tracing::info!(
                "Recovered {} queued background job(s) from durable status queue",
                recovered_background_jobs
            );
        }

        let app = Self {
            state_root,
            core: AgentCore {
                config: Arc::new(config),
                agent,
                tool_registry,
                tool_schemas,
                interrupt_controller: InterruptController::new(),
            },
            session: SessionState::new(Uuid::new_v4().to_string()),
            model: ModelState {
                current_model,
                current_personality,
            },
            stream: StreamState::new(stream_handle_shared, default_mouse_enabled()),
            runtime: RuntimeFlags::new(),
            chrome: ChromeState::new(load_pet_settings()),
            acp: AcpState::new(),
            snapshot_gate: SnapshotPersistGate::new(),
            persist_lane: PersistLane::spawn(),
            auth_lane: AuthLane::spawn(),
        };
        app.ensure_session_stub_snapshot();
        Ok(app)
    }
    pub fn stream_attached(&self) -> bool {
        self.stream.stream_attached()
    }

    pub fn set_running(&mut self, running: bool) {
        self.runtime.running = running;
    }

    /// Attach a streaming handle (used by TUI mode).
    pub fn set_stream_handle(&mut self, handle: Option<StreamHandle>) {
        if let Ok(mut guard) = self.stream.stream_handle_shared.lock() {
            *guard = handle.clone();
        }
        self.stream.stream_handle = handle;
    }

    /// Enable/disable TUI mouse handling.
    pub fn set_mouse_enabled(&mut self, enabled: bool) {
        self.stream.mouse_enabled = enabled;
    }

    /// Current TUI mouse handling state.
    pub fn mouse_enabled(&self) -> bool {
        self.stream.mouse_enabled
    }

    /// Queue a TUI skin/theme change request to be applied in the UI loop.
    pub fn request_theme_change(&mut self, skin: &str) {
        let value = skin.trim();
        if value.is_empty() {
            return;
        }
        self.stream.pending_theme = Some(value.to_string());
    }

    /// Queue an image hint for the next user prompt.
    pub fn set_pending_image_hint(&mut self, path: String) {
        let trimmed = path.trim();
        self.stream.pending_image_hint = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
    }

    /// Read queued image hint without consuming it.
    pub fn pending_image_hint(&self) -> Option<&str> {
        self.stream.pending_image_hint.as_deref()
    }

    /// Clear queued image hint.
    pub fn clear_pending_image_hint(&mut self) {
        self.stream.pending_image_hint = None;
    }

    /// Prepare outbound user text, consuming any queued image hint.
    pub fn prepare_user_message(&mut self, raw: &str) -> String {
        let base = raw.trim();
        if let Some(path) = self
            .stream
            .pending_image_hint
            .take()
            .filter(|value| !value.trim().is_empty())
        {
            format!("[IMAGE_HINT] path={}\n{}", path, base)
        } else {
            base.to_string()
        }
    }

    /// Drain any queued skin/theme change request.
    pub fn take_pending_theme_change(&mut self) -> Option<String> {
        self.stream.pending_theme.take()
    }

    /// Drain composer prefill staged by `/undo` or `/rewind`.
    pub fn take_pending_input_prefill(&mut self) -> Option<String> {
        self.stream.pending_input_prefill.take()
    }

    /// Retrieve current companion pet settings.
    pub fn pet_settings(&self) -> &PetSettings {
        &self.chrome.pet_settings
    }

    /// Update and persist companion pet settings.
    pub fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        let normalized = settings.normalized();
        persist_pet_settings(&normalized)?;
        self.chrome.pet_settings = normalized;
        Ok(())
    }

    /// Run the interactive REPL loop.
    ///
    /// This is the main entry point for interactive mode. It delegates
    /// to the TUI subsystem for rendering and event handling.
    pub async fn run_interactive(&mut self) -> Result<(), AgentError> {
        // The actual TUI loop is in crate::tui::run()
        // This method exists so non-TUI callers can drive the loop manually.
        if self.runtime.running {
            loop {
                if !self.runtime.running {
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

        self.session.push_input_line(trimmed);

        if trimmed.starts_with('/') {
            if self.stream_attached() {
                self.push_ui_user(trimmed.to_string());
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
                self.set_running(false);
            }
        } else {
            // Regular user message
            let user_message = self.prepare_user_message(trimmed);
            self.session
                .messages
                .push(hermes_core::Message::user(user_message));
            self.run_agent_turn().await?;
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

        if self.stream_attached() {
            self.push_ui_user(trimmed.to_string());
        }

        let args: Vec<&str> = parts
            .get(1)
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();

        let result = crate::commands::handle_slash_command(self, slash_cmd, &args).await?;
        if result == crate::commands::CommandResult::Quit {
            self.set_running(false);
        }
        Ok(())
    }

    /// Run agent-loop context compression on the current CLI transcript.
    pub async fn compress_conversation_context(
        &mut self,
    ) -> Result<(usize, usize, bool), AgentError> {
        let pre_len = self.session.messages.len();
        if pre_len <= 2 {
            return Ok((pre_len, pre_len, false));
        }
        let model = self.model.current_model.clone();
        let session_id = self.session.session_id.clone();
        let (compressed_messages, did_compress) = self
            .core
            .agent
            .compress_messages(self.session.messages.clone(), &session_id, &model)
            .await;
        let post_len = compressed_messages.len();
        self.session.messages = compressed_messages;
        self.session
            .ui_messages
            .retain(|msg| msg.insert_at <= self.session.messages.len());
        if let Some(new_sid) = self.core.agent.runtime_session_id() {
            let new_sid = new_sid.trim();
            if !new_sid.is_empty() && new_sid != self.session.session_id {
                self.session.session_id = new_sid.to_string();
            }
        }
        Ok((pre_len, post_len, did_compress))
    }

    /// Retry the last user message by re-sending it to the agent.
    ///
    /// Finds the last user message in history, removes all messages after it
    /// (including the assistant response), and re-runs the agent.
    pub async fn retry_last(&mut self) -> Result<(), AgentError> {
        // Find the last user message
        let last_user_idx = self
            .session
            .messages
            .iter()
            .rposition(|m| m.role == hermes_core::MessageRole::User);

        if let Some(idx) = last_user_idx {
            let last_user_msg = self.session.messages[idx].clone();
            // Truncate messages to just before the last user message
            self.session.messages.truncate(idx);
            // Re-add the user message
            self.session.messages.push(last_user_msg);
            // Re-run the agent
            self.run_agent().await?;
            self.prune_ui_after_current_messages();
        }

        Ok(())
    }

    /// Undo one or more user turns, returning the text staged for editing.
    pub fn undo_last(&mut self) -> Option<String> {
        self.undo_last_n(1)
    }

    pub fn undo_last_n(&mut self, user_turns: usize) -> Option<String> {
        let user_indices: Vec<usize> = self
            .session
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| (msg.role == hermes_core::MessageRole::User).then_some(idx))
            .collect();
        if user_indices.is_empty() {
            return None;
        }
        let count = user_turns.max(1);
        let target_pos = user_indices.len().saturating_sub(count);
        let target_idx = user_indices[target_pos];
        let prefill = self.session.messages[target_idx]
            .content
            .as_deref()
            .unwrap_or_default()
            .to_string();

        match SessionPersistence::new(&self.state_root)
            .rewind_active_user_turns(&self.session.session_id, count)
        {
            Ok(Some(outcome)) => tracing::debug!(
                "Soft-rewound session {} at message {} (inactive={}, active={})",
                self.session.session_id,
                outcome.target_message_id,
                outcome.inactive_count,
                outcome.active_message_count
            ),
            Ok(None) => tracing::debug!(
                "No persisted session row available for undo in session {}",
                self.session.session_id
            ),
            Err(err) => tracing::debug!("Failed to soft-rewind persisted session: {}", err),
        }

        self.session.messages.truncate(target_idx);
        self.prune_ui_after_current_messages();
        if prefill.trim().is_empty() {
            self.stream.pending_input_prefill = None;
        } else {
            self.stream.pending_input_prefill = Some(prefill.clone());
        }
        Some(prefill)
    }

    pub fn history_prev(&mut self) -> Option<&str> {
        self.session.history_prev()
    }

    pub fn history_next(&mut self) -> Option<&str> {
        self.session.history_next()
    }
}
