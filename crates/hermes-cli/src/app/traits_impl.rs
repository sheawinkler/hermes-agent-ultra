use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::{AgentLoop, InterruptController};
use hermes_config::GatewayConfig;
use hermes_core::{AgentError, ToolSchema};
use hermes_tools::ToolRegistry;

use super::traits::{
    AgentCoordinator, AgentDriver, AuthRuntime, ModelRuntime, SessionRuntime, SessionRuntimeAsync,
    TranscriptRuntime,
};
use super::{App, UiTranscriptMessage};

impl SessionRuntime for App {
    fn state_root(&self) -> &Path {
        &self.state_root
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn session_id_mut(&mut self) -> &mut String {
        &mut self.session_id
    }

    fn messages(&self) -> &[hermes_core::Message] {
        &self.messages
    }

    fn messages_mut(&mut self) -> &mut Vec<hermes_core::Message> {
        &mut self.messages
    }

    fn ui_messages(&self) -> &[UiTranscriptMessage] {
        &self.ui_messages
    }

    fn ui_messages_mut(&mut self) -> &mut Vec<UiTranscriptMessage> {
        &mut self.ui_messages
    }

    fn session_objective(&self) -> Option<&str> {
        self.session_objective.as_deref()
    }

    fn set_session_objective(&mut self, objective: Option<String>) {
        App::set_session_objective(self, objective);
    }

    fn input_history_mut(&mut self) -> &mut Vec<String> {
        &mut self.input_history
    }

    fn history_index_mut(&mut self) -> &mut usize {
        &mut self.history_index
    }

    fn notify_memory_session_switch(
        &self,
        new_session_id: &str,
        parent_session_id: &str,
        reset: bool,
        reason: &str,
    ) {
        App::notify_memory_session_switch(self, new_session_id, parent_session_id, reset, reason);
    }

    fn new_session(&mut self) {
        App::new_session(self);
    }

    fn reset_session(&mut self) {
        App::reset_session(self);
    }

    fn undo_last(&mut self) -> Option<String> {
        App::undo_last(self)
    }

    fn undo_last_n(&mut self, user_turns: usize) -> Option<String> {
        App::undo_last_n(self, user_turns)
    }
}

#[async_trait]
impl SessionRuntimeAsync for App {
    async fn retry_last(&mut self) -> Result<(), AgentError> {
        App::retry_last(self).await
    }

    async fn compress_conversation_context(&mut self) -> Result<(usize, usize, bool), AgentError> {
        App::compress_conversation_context(self).await
    }
}

impl ModelRuntime for App {
    fn config(&self) -> &Arc<GatewayConfig> {
        &self.config
    }

    fn current_model(&self) -> &str {
        &self.current_model
    }

    fn current_model_mut(&mut self) -> &mut String {
        &mut self.current_model
    }

    fn current_personality(&self) -> Option<&str> {
        self.current_personality.as_deref()
    }

    fn switch_model(&mut self, provider_model: &str) {
        App::switch_model(self, provider_model);
    }

    fn switch_personality(&mut self, name: &str) {
        App::switch_personality(self, name);
    }

    fn current_runtime_provider(&self) -> String {
        App::current_runtime_provider(self)
    }
}

impl TranscriptRuntime for App {
    fn stream_attached(&self) -> bool {
        self.stream_handle.is_some()
    }

    fn push_ui_message(&mut self, message: hermes_core::Message) {
        App::push_ui_message(self, message);
    }

    fn push_ui_user(&mut self, text: String) {
        App::push_ui_user(self, text);
    }

    fn push_ui_assistant(&mut self, text: String) {
        App::push_ui_assistant(self, text);
    }

    fn transcript_messages(&self) -> Vec<hermes_core::Message> {
        App::transcript_messages(self)
    }

    fn prepare_user_message(&mut self, raw: &str) -> String {
        App::prepare_user_message(self, raw)
    }
}

impl AgentCoordinator for App {
    fn agent(&self) -> &Arc<AgentLoop> {
        &self.agent
    }

    fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.tool_registry
    }

    fn tool_schemas(&self) -> &[ToolSchema] {
        &self.tool_schemas
    }

    fn interrupt_controller(&self) -> &InterruptController {
        &self.interrupt_controller
    }

    fn interrupt_controller_mut(&mut self) -> &mut InterruptController {
        &mut self.interrupt_controller
    }

    fn running(&self) -> bool {
        self.running
    }

    fn set_running(&mut self, running: bool) {
        self.running = running;
    }

    fn quorum_armed_once(&self) -> bool {
        self.quorum_armed_once
    }

    fn set_quorum_armed_once(&mut self, armed: bool) {
        self.quorum_armed_once = armed;
    }
}

#[async_trait]
impl AgentDriver for App {
    async fn run_agent_turn(&mut self) -> Result<(), AgentError> {
        App::run_agent(self).await
    }
}

#[async_trait]
impl AuthRuntime for App {
    async fn verify_runtime_auth(&mut self, force_refresh: bool) -> Result<String, AgentError> {
        App::verify_runtime_auth(self, force_refresh).await
    }
}
