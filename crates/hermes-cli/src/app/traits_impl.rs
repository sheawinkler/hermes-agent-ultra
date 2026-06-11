use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_agent::{AgentLoop, InterruptController};
use hermes_config::GatewayConfig;
use hermes_core::{AgentError, ToolSchema};
use hermes_tools::ToolRegistry;

use hermes_acp_server::server::AcpPipeServer;

use super::traits::{
    AcpServerRuntime, AgentCoordinator, AgentDriver, AuthRuntime, ModelRuntime, SessionRuntime,
    SessionRuntimeAsync, SessionSnapshotRuntime, TranscriptRuntime, UiChromeRuntime,
};
use super::{App, PetSettings, UiTranscriptMessage};

impl SessionRuntime for App {
    fn state_root(&self) -> &Path {
        &self.state_root
    }

    fn session_id(&self) -> &str {
        &self.session.session_id
    }

    fn session_id_mut(&mut self) -> &mut String {
        &mut self.session.session_id
    }

    fn messages(&self) -> &[hermes_core::Message] {
        &self.session.messages
    }

    fn messages_mut(&mut self) -> &mut Vec<hermes_core::Message> {
        &mut self.session.messages
    }

    fn ui_messages(&self) -> &[UiTranscriptMessage] {
        &self.session.ui_messages
    }

    fn ui_messages_mut(&mut self) -> &mut Vec<UiTranscriptMessage> {
        &mut self.session.ui_messages
    }

    fn session_objective(&self) -> Option<&str> {
        self.session.session_objective.as_deref()
    }

    fn set_session_objective(&mut self, objective: Option<String>) {
        App::set_session_objective(self, objective);
    }

    fn input_history(&self) -> &[String] {
        &self.session.input_history
    }

    fn input_history_mut(&mut self) -> &mut Vec<String> {
        &mut self.session.input_history
    }

    fn history_index_mut(&mut self) -> &mut usize {
        &mut self.session.history_index
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

    fn sync_agent_runtime_session_id(&self, session_id: &str) {
        self.core.agent.set_runtime_session_id(session_id);
    }

    fn history_prev(&mut self) -> Option<&str> {
        App::history_prev(self)
    }

    fn history_next(&mut self) -> Option<&str> {
        App::history_next(self)
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
        &self.core.config
    }

    fn set_config(&mut self, config: Arc<GatewayConfig>) {
        self.core.config = config;
    }

    fn current_model(&self) -> &str {
        &self.model.current_model
    }

    fn current_model_mut(&mut self) -> &mut String {
        &mut self.model.current_model
    }

    fn current_personality(&self) -> Option<&str> {
        self.model.current_personality.as_deref()
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
        self.stream.stream_attached()
    }

    fn stream_handle(&self) -> Option<&crate::tui::StreamHandle> {
        self.stream.stream_handle.as_ref()
    }

    fn set_stream_handle(&mut self, handle: Option<crate::tui::StreamHandle>) {
        App::set_stream_handle(self, handle);
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
        &self.core.agent
    }

    fn tool_registry(&self) -> &Arc<ToolRegistry> {
        &self.core.tool_registry
    }

    fn tool_schemas(&self) -> &[ToolSchema] {
        &self.core.tool_schemas
    }

    fn interrupt_controller(&self) -> &InterruptController {
        &self.core.interrupt_controller
    }

    fn interrupt_controller_mut(&mut self) -> &mut InterruptController {
        &mut self.core.interrupt_controller
    }

    fn running(&self) -> bool {
        self.runtime.running
    }

    fn set_running(&mut self, running: bool) {
        self.runtime.running = running;
    }

    fn quorum_armed_once(&self) -> bool {
        self.runtime.quorum_armed_once
    }

    fn set_quorum_armed_once(&mut self, armed: bool) {
        self.runtime.quorum_armed_once = armed;
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

impl SessionSnapshotRuntime for App {
    fn session_info(&self) -> super::SessionInfo {
        App::session_info(self)
    }

    fn persist_session_snapshot(
        &mut self,
        name: Option<&str>,
    ) -> Result<std::path::PathBuf, AgentError> {
        App::persist_session_snapshot(self, name)
    }

    fn apply_agent_result_and_persist(
        &mut self,
        result: hermes_core::AgentResult,
    ) -> Result<(), AgentError> {
        App::apply_agent_result_and_persist(self, result)
    }

    fn flush_session_teardown(&self, interrupted: bool) {
        App::flush_session_teardown(self, interrupted);
    }

    fn running_background_job_count(&self) -> usize {
        App::running_background_job_count(self)
    }
}

impl UiChromeRuntime for App {
    fn mouse_enabled(&self) -> bool {
        App::mouse_enabled(self)
    }

    fn set_mouse_enabled(&mut self, enabled: bool) {
        App::set_mouse_enabled(self, enabled);
    }

    fn request_theme_change(&mut self, skin: &str) {
        App::request_theme_change(self, skin);
    }

    fn take_pending_theme_change(&mut self) -> Option<String> {
        App::take_pending_theme_change(self)
    }

    fn take_pending_input_prefill(&mut self) -> Option<String> {
        App::take_pending_input_prefill(self)
    }

    fn set_pending_image_hint(&mut self, path: String) {
        App::set_pending_image_hint(self, path);
    }

    fn pending_image_hint(&self) -> Option<&str> {
        App::pending_image_hint(self)
    }

    fn clear_pending_image_hint(&mut self) {
        App::clear_pending_image_hint(self);
    }

    fn pet_settings(&self) -> &PetSettings {
        App::pet_settings(self)
    }

    fn set_pet_settings(&mut self, settings: PetSettings) -> Result<(), AgentError> {
        App::set_pet_settings(self, settings)
    }
}

impl AcpServerRuntime for App {
    fn acp_server(&self) -> Option<&Arc<AcpPipeServer>> {
        self.acp.server.as_ref()
    }

    fn acp_server_mut(&mut self) -> &mut Option<Arc<AcpPipeServer>> {
        &mut self.acp.server
    }

    fn acp_event_buffer(&self) -> Option<&Arc<std::sync::Mutex<Vec<String>>>> {
        self.acp.event_buffer.as_ref()
    }

    fn acp_event_buffer_mut(&mut self) -> &mut Option<Arc<std::sync::Mutex<Vec<String>>>> {
        &mut self.acp.event_buffer
    }
}
