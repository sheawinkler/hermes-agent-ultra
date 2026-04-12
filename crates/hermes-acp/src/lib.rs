//! Agent Communication Protocol (ACP) adapter.
//!
//! Implements the ACP JSON-RPC interface so that Hermes can be controlled
//! by external agent orchestrators. Full protocol surface includes:
//!
//! - **Lifecycle**: initialize, authenticate
//! - **Sessions**: new, load, resume, fork, list, cancel
//! - **Prompts**: prompt with streaming events
//! - **Configuration**: set model, mode, config options
//! - **Events**: thinking, tool progress, message streaming
//! - **Permissions**: approval callback bridging

pub mod events;
pub mod handler;
pub mod permissions;
pub mod protocol;
pub mod server;
pub mod session;

pub use server::AcpServer;
pub use protocol::{
    AcpError, AcpMethod, AcpRequest, AcpResponse,
    AgentCapabilities, AuthMethod, AvailableCommand, ClientCapabilities,
    ContentBlock, Implementation, InitializeResponse, McpServerConfig,
    PromptResponse, SessionCapabilities, SessionUpdate, StopReason, Usage,
};
pub use handler::{AcpHandler, DefaultAcpHandler, HermesAcpHandler};
pub use session::{SessionInfo, SessionManager, SessionPhase, SessionState};
pub use events::{AcpEvent, AcpEventKind, EventSink, ToolCallIdTracker};
pub use permissions::{
    ApprovalCallback, PermissionKind, PermissionOption, PermissionOutcome,
    PermissionRequest, PermissionStore,
};

use std::sync::Arc;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// CLI integration
// ---------------------------------------------------------------------------

/// Configuration for starting the ACP server from CLI context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpConfig {
    /// Model to use for the agent behind this ACP server.
    pub model: String,
    /// Optional personality / system prompt override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// List of tool names to enable.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Maximum conversation turns before the server auto-closes.
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
}

fn default_max_turns() -> usize {
    100
}

impl Default for AcpConfig {
    fn default() -> Self {
        Self {
            model: "default".to_string(),
            personality: None,
            tools: vec![],
            max_turns: default_max_turns(),
        }
    }
}

/// Start the ACP server from CLI context.
pub async fn start_acp_server(config: AcpConfig) -> Result<(), Box<dyn std::error::Error>> {
    let session_manager = Arc::new(SessionManager::new());
    let event_sink = Arc::new(EventSink::default());
    let permission_store = Arc::new(PermissionStore::new());

    let handler = Arc::new(HermesAcpHandler::new(
        session_manager.clone(),
        event_sink.clone(),
        permission_store.clone(),
    ));

    start_acp_server_with_handler(config, handler).await
}

/// Start the ACP server with a custom handler implementation.
pub async fn start_acp_server_with_handler(
    config: AcpConfig,
    handler: Arc<dyn AcpHandler>,
) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        "Starting ACP server: model={}, tools={:?}, max_turns={}",
        config.model,
        config.tools,
        config.max_turns,
    );

    let server = AcpServer::new(handler);
    server.run().await
}
