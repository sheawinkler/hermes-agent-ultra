//! Composed runtime state shards owned by [`super::App`].

use std::sync::{Arc, Mutex as StdMutex};

use hermes_agent::{AgentLoop, InterruptController};
use hermes_config::GatewayConfig;
use hermes_core::ToolSchema;
use hermes_tools::ToolRegistry;

use hermes_acp_server::server::AcpPipeServer;

use super::{PetSettings, UiTranscriptMessage};
use crate::tui::StreamHandle;

/// LLM agent loop, tools, and gateway configuration.
#[derive(Clone)]
pub struct AgentCore {
    pub config: Arc<GatewayConfig>,
    pub agent: Arc<AgentLoop>,
    pub tool_registry: Arc<ToolRegistry>,
    pub tool_schemas: Vec<ToolSchema>,
    pub interrupt_controller: InterruptController,
}

/// Conversation session identity and transcript.
#[derive(Clone)]
pub struct SessionState {
    pub session_id: String,
    pub messages: Vec<hermes_core::Message>,
    pub ui_messages: Vec<UiTranscriptMessage>,
    pub session_objective: Option<String>,
    pub input_history: Vec<String>,
    pub history_index: usize,
}

impl SessionState {
    pub fn new(session_id: String) -> Self {
        Self {
            session_id,
            messages: Vec::new(),
            ui_messages: Vec::new(),
            session_objective: None,
            input_history: Vec::new(),
            history_index: 0,
        }
    }

    pub fn push_input_line(&mut self, line: &str) {
        self.input_history.push(line.to_string());
        self.history_index = self.input_history.len();
    }

    pub fn clear_input_history(&mut self) {
        self.input_history.clear();
        self.history_index = 0;
    }

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

/// Active model and personality selection.
#[derive(Clone)]
pub struct ModelState {
    pub current_model: String,
    pub current_personality: Option<String>,
}

/// TUI streaming sink and composer chrome.
#[derive(Clone)]
pub struct StreamState {
    pub stream_handle: Option<StreamHandle>,
    pub stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>>,
    pub mouse_enabled: bool,
    pub pending_theme: Option<String>,
    pub pending_image_hint: Option<String>,
    pub pending_input_prefill: Option<String>,
}

impl StreamState {
    pub fn new(
        stream_handle_shared: Arc<StdMutex<Option<StreamHandle>>>,
        mouse_enabled: bool,
    ) -> Self {
        Self {
            stream_handle: None,
            stream_handle_shared,
            mouse_enabled,
            pending_theme: None,
            pending_image_hint: None,
            pending_input_prefill: None,
        }
    }

    pub fn stream_attached(&self) -> bool {
        self.stream_handle.is_some()
    }
}

/// Interactive loop control flags.
#[derive(Clone, Debug)]
pub struct RuntimeFlags {
    pub running: bool,
    pub quorum_armed_once: bool,
}

impl RuntimeFlags {
    pub const fn new() -> Self {
        Self {
            running: true,
            quorum_armed_once: false,
        }
    }
}

/// Background ACP pipe server handles.
#[derive(Clone)]
pub struct AcpState {
    pub server: Option<Arc<AcpPipeServer>>,
    pub event_buffer: Option<Arc<StdMutex<Vec<String>>>>,
}

impl AcpState {
    pub const fn new() -> Self {
        Self {
            server: None,
            event_buffer: None,
        }
    }
}

/// Companion pet settings (persisted separately).
#[derive(Clone)]
pub struct ChromeState {
    pub pet_settings: PetSettings,
}

impl ChromeState {
    pub fn new(pet_settings: PetSettings) -> Self {
        Self { pet_settings }
    }
}
