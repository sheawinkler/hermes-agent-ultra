//! ACP server (JSON-RPC over stdin/stdout).
//!
//! Full implementation with:
//! - Agent capabilities declaration
//! - Session state machine (create/resume/complete/cancel)
//! - Message streaming support
//! - Tool approval callback integration
//! - MCP server config block generation
//! - Progress events (thinking, tool_call, tool_result steps)

use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::events::EventSink;
use crate::handler::AcpHandler;
use crate::permissions::PermissionStore;
use crate::protocol::{AcpRequest, AcpResponse, SessionUpdate};
use crate::session::SessionManager;

/// ACP JSON-RPC server with full protocol support.
pub struct AcpServer {
    handler: Arc<dyn AcpHandler>,
    event_sink: Arc<EventSink>,
    session_manager: Arc<SessionManager>,
    permission_store: Arc<PermissionStore>,
}

impl AcpServer {
    pub fn new(handler: Arc<dyn AcpHandler>) -> Self {
        Self {
            handler,
            event_sink: Arc::new(EventSink::default()),
            session_manager: Arc::new(SessionManager::new()),
            permission_store: Arc::new(PermissionStore::new()),
        }
    }

    /// Create a server with shared session manager and event sink.
    pub fn with_components(
        handler: Arc<dyn AcpHandler>,
        session_manager: Arc<SessionManager>,
        event_sink: Arc<EventSink>,
        permission_store: Arc<PermissionStore>,
    ) -> Self {
        Self {
            handler,
            event_sink,
            session_manager,
            permission_store,
        }
    }

    /// Get a reference to the event sink.
    pub fn event_sink(&self) -> &Arc<EventSink> {
        &self.event_sink
    }

    /// Get a reference to the session manager.
    pub fn session_manager(&self) -> &Arc<SessionManager> {
        &self.session_manager
    }

    /// Get a reference to the permission store.
    pub fn permission_store(&self) -> &Arc<PermissionStore> {
        &self.permission_store
    }

    /// Run the server, reading JSON-RPC requests from stdin and writing
    /// responses to stdout.
    pub async fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let reader = BufReader::new(stdin);
        let mut lines = reader.lines();

        tracing::info!("ACP server started, listening on stdin");

        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let request: AcpRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let error_resp = AcpResponse::error(
                        None,
                        -32700,
                        format!("Parse error: {}", e),
                    );
                    let json = serde_json::to_string(&error_resp)?;
                    stdout.write_all(json.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            let response = self.handler.handle_request(request).await;
            let json = serde_json::to_string(&response)?;
            stdout.write_all(json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;

            // Flush any pending session events after the response
            self.flush_pending_events(&mut stdout).await?;
        }

        Ok(())
    }

    /// Run the server on a custom pair of reader/writer (for testing or
    /// non-stdio transports).
    pub async fn run_on<R, W>(
        &self,
        reader: R,
        mut writer: W,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        R: tokio::io::AsyncRead + Unpin,
        W: tokio::io::AsyncWrite + Unpin,
    {
        let reader = BufReader::new(reader);
        let mut lines = reader.lines();

        while let Some(line) = lines.next_line().await? {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }

            let request: AcpRequest = match serde_json::from_str(&line) {
                Ok(r) => r,
                Err(e) => {
                    let error_resp = AcpResponse::error(
                        None,
                        -32700,
                        format!("Parse error: {}", e),
                    );
                    let json = serde_json::to_string(&error_resp)?;
                    writer.write_all(json.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
                }
            };

            let response = self.handler.handle_request(request).await;
            let json = serde_json::to_string(&response)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }

        Ok(())
    }

    /// Write any pending events as JSON-RPC notifications to the writer.
    async fn flush_pending_events(
        &self,
        writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    ) -> Result<(), Box<dyn std::error::Error>> {
        let events = self.event_sink.drain_all();
        let has_events = !events.is_empty();
        for event in events {
            let notification = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": event,
            });
            let json = serde_json::to_string(&notification)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
        }
        if has_events {
            writer.flush().await?;
        }
        Ok(())
    }
}

/// Generate an MCP server config block for embedding in an ACP session.
///
/// This creates a stdio-based MCP server entry that points to the hermes
/// binary, suitable for inclusion in the ACP session's `mcp_servers` list.
pub fn generate_mcp_server_config(hermes_binary: &str) -> serde_json::Value {
    serde_json::json!({
        "hermes": {
            "command": hermes_binary,
            "args": ["mcp", "serve"],
            "env": {}
        }
    })
}
