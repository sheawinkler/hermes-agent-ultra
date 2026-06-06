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
use crate::protocol::{AcpRequest, AcpResponse};
use crate::session::SessionManager;

fn flush_events_before_response(method: &str) -> bool {
    matches!(
        method,
        "session/load" | "load_session" | "session/resume" | "resume_session"
    )
}

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
                    let error_resp =
                        AcpResponse::error(None, -32700, format!("Parse error: {}", e));
                    let json = serde_json::to_string(&error_resp)?;
                    stdout.write_all(json.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            let flush_before_response = flush_events_before_response(&request.method);
            let response = self.handler.handle_request(request).await;
            if flush_before_response {
                self.flush_pending_events(&mut stdout).await?;
            }
            let json = serde_json::to_string(&response)?;
            stdout.write_all(json.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;

            if !flush_before_response {
                self.flush_pending_events(&mut stdout).await?;
            }
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
                    let error_resp =
                        AcpResponse::error(None, -32700, format!("Parse error: {}", e));
                    let json = serde_json::to_string(&error_resp)?;
                    writer.write_all(json.as_bytes()).await?;
                    writer.write_all(b"\n").await?;
                    writer.flush().await?;
                    continue;
                }
            };

            let flush_before_response = flush_events_before_response(&request.method);
            let response = self.handler.handle_request(request).await;
            if flush_before_response {
                self.flush_pending_events(&mut writer).await?;
            }
            let json = serde_json::to_string(&response)?;
            writer.write_all(json.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;

            if !flush_before_response {
                // Match stdio mode: most responses are immediately followed by any
                // pending session/update notifications generated by the handler.
                self.flush_pending_events(&mut writer).await?;
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::AcpEvent;
    use crate::handler::HermesAcpHandler;
    use crate::permissions::{build_permission_request, PermissionOutcome};

    #[tokio::test]
    async fn run_on_flushes_pending_events_after_each_response() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        event_sink.push(AcpEvent::tool_call_start(
            "session-1",
            "tc-read",
            "read_file",
            Some(serde_json::json!({"path": "/tmp/a.txt"})),
        ));

        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = br#"{"jsonrpc":"2.0","id":1,"method":"status.get"}"#;
        let mut output = Vec::new();
        server.run_on(&input[..], &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["result"]["status"], "ready");
        assert_eq!(lines[1]["method"], "session/update");
        assert_eq!(lines[1]["params"]["kind"], "tool_call_start");
        assert_eq!(lines[1]["params"]["tool_call_id"], "tc-read");
        assert_eq!(lines[1]["params"]["tool_name"], "read_file");
        assert_eq!(lines[1]["params"]["arguments"]["path"], "/tmp/a.txt");
    }

    #[tokio::test]
    async fn run_on_advertises_available_commands_after_new_session() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = br#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp"}}"#;
        let mut output = Vec::new();
        server.run_on(&input[..], &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 3);
        let session_id = lines[0]["result"]["sessionId"].as_str().unwrap();
        assert_eq!(lines[1]["method"], "session/update");
        assert_eq!(lines[1]["params"]["kind"], "available_commands_update");
        assert_eq!(
            lines[1]["params"]["sessionUpdate"],
            "available_commands_update"
        );
        assert_eq!(lines[1]["params"]["session_id"], session_id);
        let commands = lines[1]["params"]["availableCommands"]
            .as_array()
            .expect("available commands");
        assert!(commands.iter().any(|command| command["name"] == "help"));
        let model = commands
            .iter()
            .find(|command| command["name"] == "model")
            .expect("model command");
        assert_eq!(model["inputHint"], "model name to switch to");
        assert_eq!(lines[2]["method"], "session/update");
        assert_eq!(lines[2]["params"]["kind"], "usage_update");
        assert_eq!(lines[2]["params"]["sessionUpdate"], "usage_update");
        assert_eq!(lines[2]["params"]["session_id"], session_id);
        assert!(lines[2]["params"]["size"].as_u64().unwrap() > 0);
        assert!(lines[2]["params"]["used"].as_u64().is_some());
    }

    #[tokio::test]
    async fn run_on_replays_load_history_before_response() {
        let session_manager = Arc::new(SessionManager::new());
        let event_sink = Arc::new(EventSink::default());
        let permission_store = Arc::new(PermissionStore::new());
        let state = session_manager.create_session("/tmp");
        let session_id = state.session_id;
        session_manager.set_history(
            &session_id,
            vec![
                serde_json::json!({"role": "user", "content": "restore this thread"}),
                serde_json::json!({"role": "assistant", "content": "thread restored"}),
            ],
        );
        let handler = Arc::new(HermesAcpHandler::new(
            session_manager.clone(),
            event_sink.clone(),
            permission_store.clone(),
        ));
        let server =
            AcpServer::with_components(handler, session_manager, event_sink, permission_store);

        let input = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"session/load","params":{{"sessionId":"{session_id}","cwd":"/tmp"}}}}"#
        );
        let mut output = Vec::new();
        server.run_on(input.as_bytes(), &mut output).await.unwrap();

        let text = String::from_utf8(output).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0]["method"], "session/update");
        assert_eq!(lines[0]["params"]["kind"], "user_message_chunk");
        assert_eq!(lines[0]["params"]["sessionUpdate"], "user_message_chunk");
        assert_eq!(lines[0]["params"]["content"]["text"], "restore this thread");
        assert_eq!(lines[1]["method"], "session/update");
        assert_eq!(lines[1]["params"]["kind"], "agent_message_chunk");
        assert_eq!(lines[2]["method"], "session/update");
        assert_eq!(lines[2]["params"]["kind"], "available_commands_update");
        assert_eq!(lines[3]["method"], "session/update");
        assert_eq!(lines[3]["params"]["kind"], "usage_update");
        assert_eq!(lines[3]["params"]["sessionUpdate"], "usage_update");
        assert!(lines[3]["params"]["size"].as_u64().unwrap() > 0);
        assert!(lines[3]["params"]["used"].as_u64().unwrap() > 0);
        assert_eq!(lines[4]["id"], 1);
        assert_eq!(lines[4]["result"], serde_json::json!({}));
    }

    #[test]
    fn server_permission_stores_are_instance_scoped() {
        let handler_a = Arc::new(HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        ));
        let handler_b = Arc::new(HermesAcpHandler::new(
            Arc::new(SessionManager::new()),
            Arc::new(EventSink::default()),
            Arc::new(PermissionStore::new()),
        ));
        let server_a = AcpServer::new(handler_a);
        let server_b = AcpServer::new(handler_b);

        let mut request = build_permission_request(
            "acp-session-A",
            "rm -rf /tmp/a",
            "dangerous command",
            true,
            0,
        );
        request.id = "req-a".to_string();
        server_a.permission_store().add_pending(request);

        assert_eq!(server_a.permission_store().list_pending().len(), 1);
        assert!(server_b.permission_store().list_pending().is_empty());
        assert!(server_a
            .permission_store()
            .resolve("req-a", PermissionOutcome::Denied));
        assert!(!server_b
            .permission_store()
            .resolve("req-a", PermissionOutcome::Denied));
    }
}
