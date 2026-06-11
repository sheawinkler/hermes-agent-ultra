//! Bounded capability traits for the interactive CLI runtime.
//!
//! `App` implements these traits so slash handlers and the TUI can depend on
//! narrow interfaces instead of the full god-object surface.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::{AgentLoop, InterruptController};
use hermes_config::GatewayConfig;
use hermes_core::{AgentError, ToolSchema};
use hermes_tools::ToolRegistry;

use super::{App, UiTranscriptMessage};

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

/// Marker supertrait: everything slash dispatch needs from the CLI host.
pub trait SlashCommandHost:
    SessionRuntime
    + SessionRuntimeAsync
    + ModelRuntime
    + TranscriptRuntime
    + AgentCoordinator
    + AgentDriver
{
}

impl SlashCommandHost for App {}
