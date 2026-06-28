//! Full ACP request handler that implements the ACP protocol methods.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use hermes_intelligence::model_metadata::{
    estimate_request_tokens_rough, get_model_context_length,
};
use hermes_mcp::{
    sanitize_mcp_name_component, BearerTokenAuth, McpManager,
    McpServerConfig as HermesMcpServerConfig,
};
use hermes_tools::ToolRegistry;
use serde_json::{json, Value};
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::auth::{
    build_auth_methods_for_provider, detect_provider, TERMINAL_SETUP_AUTH_METHOD_ID,
};
use crate::events::{plan_entries_from_todo_result, AcpEvent, AcpEventKind, EventSink};
use crate::permissions::PermissionStore;
use crate::protocol::*;
use crate::session::{SessionManager, SessionMetaUpdate, SessionPhase, SessionState};

/// Trait for handling ACP requests.
#[async_trait::async_trait]
pub trait AcpHandler: Send + Sync {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse;
}

/// Output returned by a concrete ACP prompt executor.
#[derive(Debug, Clone, Default)]
pub struct PromptExecutionOutput {
    pub response_text: String,
    pub usage: Option<Usage>,
    pub total_turns: Option<u32>,
    pub events: Vec<AcpEvent>,
}

/// Pluggable ACP prompt executor.
#[async_trait::async_trait]
pub trait AcpPromptExecutor: Send + Sync {
    async fn execute_prompt(
        &self,
        session: &SessionState,
        user_text: &str,
        history: &[Value],
    ) -> Result<PromptExecutionOutput, String>;

    fn steer_prompt(&self, _session: &SessionState, _guidance: &str) -> Result<bool, String> {
        Ok(false)
    }
}

const MAX_ACP_RESOURCE_BYTES: usize = 512 * 1024;
const IMAGE_EXT_MIME: &[(&str, &str)] = &[
    (".png", "image/png"),
    (".jpg", "image/jpeg"),
    (".jpeg", "image/jpeg"),
    (".gif", "image/gif"),
    (".webp", "image/webp"),
    (".bmp", "image/bmp"),
    (".svg", "image/svg+xml"),
];

const TEXT_RESOURCE_MIME_PREFIXES: &[&str] = &["text/"];
const TEXT_RESOURCE_MIME_TYPES: &[&str] = &[
    "application/json",
    "application/javascript",
    "application/typescript",
    "application/xml",
    "application/x-yaml",
    "application/yaml",
    "application/toml",
    "application/sql",
];

#[derive(Debug, Clone)]
struct PromptExtraction {
    user_text: String,
    user_content: Value,
    text_only_prompt: bool,
    has_content: bool,
}

include!("handler/prompt_parts.rs");

include!("handler/mcp_config.rs");

include!("handler/hermes_handler_impl.rs");

include!("handler/session_helpers.rs");
include!("handler/request_dispatch.rs");

// ---------------------------------------------------------------------------
// Default handler (backward compat)
// ---------------------------------------------------------------------------

/// Minimal default ACP handler for backward compatibility.
pub struct DefaultAcpHandler {
    inner: HermesAcpHandler,
}

impl DefaultAcpHandler {
    pub fn new() -> Self {
        Self {
            inner: HermesAcpHandler::new(
                Arc::new(SessionManager::new()),
                Arc::new(EventSink::default()),
                Arc::new(PermissionStore::new()),
            ),
        }
    }
}

impl Default for DefaultAcpHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl AcpHandler for DefaultAcpHandler {
    async fn handle_request(&self, request: AcpRequest) -> AcpResponse {
        self.inner.handle_request(request).await
    }
}

#[cfg(test)]
mod tests;
