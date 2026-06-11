//! Bounded capability traits for the interactive CLI runtime.
//!
//! `App` implements these traits so slash handlers and the TUI can depend on
//! narrow interfaces instead of the full god-object surface.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_acp_server::server::AcpPipeServer;
use hermes_agent::{AgentLoop, InterruptController};
use hermes_config::GatewayConfig;
use hermes_core::{AgentError, AgentResult, ToolSchema};
use hermes_tools::ToolRegistry;

use super::{App, PetSettings, UiTranscriptMessage};

/// Session identity, conversation transcript, and session lifecycle hooks.
pub trait SessionRuntime {
    fn state_root(&self) -> &Path;
    fn session_id(&self) -> &str;
    fn session_id_mut(&mut self) -> &mut String;
    fn messages(&self) -> &[hermes_core::Message];
    fn messages_mut(&mut self) -> &mut Vec<hermes_core::Message>;
    fn ui_messages(&self) -> &[UiTranscriptMessage];
    fn ui_messages_mut(&mut self) -> &mut Vec<UiTranscriptMessage>;
    fn session_objective(&self) -> Option<&str>;
    fn set_session_objective(&mut self, objective: Option<String>);
    fn input_history(&self) -> &[String];
    fn input_history_mut(&mut self) -> &mut Vec<String>;
    fn history_index_mut(&mut self) -> &mut usize;
    fn notify_memory_session_switch(
        &self,
        new_session_id: &str,
        parent_session_id: &str,
        reset: bool,
        reason: &str,
    );
    fn new_session(&mut self);
    fn reset_session(&mut self);
    fn undo_last(&mut self) -> Option<String>;
    fn undo_last_n(&mut self, user_turns: usize) -> Option<String>;
    fn sync_agent_runtime_session_id(&self, session_id: &str);
    fn history_prev(&mut self) -> Option<&str>;
    fn history_next(&mut self) -> Option<&str>;
}

/// Async session operations (compression, retry).
#[async_trait]
pub trait SessionRuntimeAsync: SessionRuntime {
    async fn retry_last(&mut self) -> Result<(), AgentError>;
    async fn compress_conversation_context(&mut self) -> Result<(usize, usize, bool), AgentError>;
}

/// Active model / personality selection.
pub trait ModelRuntime {
    fn config(&self) -> &Arc<GatewayConfig>;
    fn set_config(&mut self, config: Arc<GatewayConfig>);
    fn current_model(&self) -> &str;
    fn current_model_mut(&mut self) -> &mut String;
    fn current_personality(&self) -> Option<&str>;
    fn switch_model(&mut self, provider_model: &str);
    fn switch_personality(&mut self, name: &str);
    fn current_runtime_provider(&self) -> String;
}

/// UI transcript overlay and composer helpers (TUI vs stdout).
pub trait TranscriptRuntime {
    fn stream_attached(&self) -> bool;
    fn stream_handle(&self) -> Option<&crate::tui::StreamHandle>;
    fn set_stream_handle(&mut self, handle: Option<crate::tui::StreamHandle>);
    fn push_ui_message(&mut self, message: hermes_core::Message);
    fn push_ui_user(&mut self, text: String);
    fn push_ui_assistant(&mut self, text: String);
    fn transcript_messages(&self) -> Vec<hermes_core::Message>;
    fn prepare_user_message(&mut self, raw: &str) -> String;
}

/// Shared agent-loop, tools, and run-loop control.
pub trait AgentCoordinator {
    fn agent(&self) -> &Arc<AgentLoop>;
    fn tool_registry(&self) -> &Arc<ToolRegistry>;
    fn tool_schemas(&self) -> &[ToolSchema];
    fn interrupt_controller(&self) -> &InterruptController;
    fn interrupt_controller_mut(&mut self) -> &mut InterruptController;
    fn running(&self) -> bool;
    fn set_running(&mut self, running: bool);
    fn quorum_armed_once(&self) -> bool;
    fn set_quorum_armed_once(&mut self, armed: bool);
}

/// Drive a full agent turn from the current message history.
#[async_trait]
pub trait AgentDriver: AgentCoordinator {
    async fn run_agent_turn(&mut self) -> Result<(), AgentError>;
}

/// OAuth / API-key hydration for the active provider.
#[async_trait]
pub trait AuthRuntime {
    async fn verify_runtime_auth(&mut self, force_refresh: bool) -> Result<String, AgentError>;
}

/// Session snapshot persistence (JSON files under state root).
pub trait SessionSnapshotRuntime: SessionRuntime {
    fn session_info(&self) -> super::SessionInfo;
    fn persist_session_snapshot(
        &mut self,
        name: Option<&str>,
    ) -> Result<std::path::PathBuf, AgentError>;
    fn apply_agent_result_and_persist(&mut self, result: AgentResult) -> Result<(), AgentError>;
    fn flush_session_teardown(&self, interrupted: bool);
    fn running_background_job_count(&self) -> usize;
}

/// TUI chrome: mouse, theme, image hints, companion pet.
pub trait UiChromeRuntime {
    fn mouse_enabled(&self) -> bool;
    fn set_mouse_enabled(&mut self, enabled: bool);
    fn request_theme_change(&mut self, skin: &str);
    fn take_pending_theme_change(&mut self) -> Option<String>;
    fn take_pending_input_prefill(&mut self) -> Option<String>;
    fn set_pending_image_hint(&mut self, path: String);
    fn pending_image_hint(&self) -> Option<&str>;
    fn clear_pending_image_hint(&mut self);
    fn pet_settings(&self) -> &PetSettings;
    fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError>;
}

/// Background ACP pipe server state.
pub trait AcpServerRuntime: AgentCoordinator {
    fn acp_server(&self) -> Option<&Arc<AcpPipeServer>>;
    fn acp_server_mut(&mut self) -> &mut Option<Arc<AcpPipeServer>>;
    fn acp_event_buffer(&self) -> Option<&Arc<std::sync::Mutex<Vec<String>>>>;
    fn acp_event_buffer_mut(&mut self) -> &mut Option<Arc<std::sync::Mutex<Vec<String>>>>;
}

/// Marker supertrait: everything slash dispatch needs from the CLI host.
pub trait SlashCommandHost:
    SessionRuntime
    + SessionRuntimeAsync
    + ModelRuntime
    + TranscriptRuntime
    + AgentCoordinator
    + AgentDriver
    + AuthRuntime
    + UiChromeRuntime
    + SessionSnapshotRuntime
{
}

impl SlashCommandHost for App {}
