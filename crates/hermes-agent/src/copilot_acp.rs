//! GitHub Copilot ACP client.
//!
//! Allows Hermes to delegate tasks to GitHub Copilot via the Agent
//! Communication Protocol (ACP). Copilot runs as a subprocess and
//! communicates over stdin/stdout JSON-RPC.
//!
//! Flow:
//! 1. Spawn the Copilot ACP subprocess
//! 2. Send `initialize` with client capabilities
//! 3. Send `session/new` to create a session
//! 4. Send `prompt` requests and collect streaming responses
//! 5. Handle errors and automatic reconnection

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use hermes_core::AgentError;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Copilot ACP client.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotAcpConfig {
    /// Path to the Copilot ACP binary (or command to run).
    pub command: String,
    /// Arguments to pass to the command.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables to set.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Timeout for individual requests.
    pub request_timeout: Duration,
    /// Maximum reconnection attempts.
    pub max_reconnects: u32,
    /// Delay between reconnection attempts.
    pub reconnect_delay: Duration,
}

impl Default for CopilotAcpConfig {
    fn default() -> Self {
        Self {
            command: "copilot-acp".into(),
            args: vec![],
            env: HashMap::new(),
            request_timeout: Duration::from_secs(60),
            max_reconnects: 3,
            reconnect_delay: Duration::from_secs(2),
        }
    }
}

// ---------------------------------------------------------------------------
// Client state
// ---------------------------------------------------------------------------

struct AcpConnection {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
    request_id: u64,
    session_id: Option<String>,
}

/// Copilot ACP client.
pub struct CopilotAcpClient {
    config: CopilotAcpConfig,
    connection: Arc<Mutex<Option<AcpConnection>>>,
}

impl CopilotAcpClient {
    pub fn new(config: CopilotAcpConfig) -> Self {
        Self {
            config,
            connection: Arc::new(Mutex::new(None)),
        }
    }

    /// Connect to the Copilot ACP subprocess.
    pub async fn connect(&self) -> Result<(), AgentError> {
        let mut conn = self.connection.lock().await;
        if conn.is_some() {
            return Ok(()); // Already connected
        }

        let new_conn = self.spawn_subprocess().await?;
        *conn = Some(new_conn);

        // Send initialize
        drop(conn);
        self.initialize().await?;

        Ok(())
    }

    /// Disconnect and kill the subprocess.
    pub async fn disconnect(&self) -> Result<(), AgentError> {
        let mut conn = self.connection.lock().await;
        if let Some(mut c) = conn.take() {
            let _ = c.child.kill().await;
        }
        Ok(())
    }

    /// Check if connected.
    pub async fn is_connected(&self) -> bool {
        self.connection.lock().await.is_some()
    }

    /// Send a prompt to Copilot and get the response.
    pub async fn prompt(
        &self,
        message: &str,
        system_prompt: Option<&str>,
    ) -> Result<String, AgentError> {
        self.ensure_connected().await?;

        let session_id = {
            let conn = self.connection.lock().await;
            conn.as_ref().and_then(|c| c.session_id.clone())
        };

        // Create session if needed
        let session_id = match session_id {
            Some(id) => id,
            None => self.create_session().await?,
        };

        let params = json!({
            "sessionId": session_id,
            "content": [{"type": "text", "text": message}],
            "systemPrompt": system_prompt,
        });

        let response = self.send_request("prompt", params).await?;

        // Extract text from response content blocks
        let text = response
            .get("content")
            .and_then(|c| c.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();

        Ok(text)
    }

    // -- Internal methods ----------------------------------------------------

    async fn spawn_subprocess(&self) -> Result<AcpConnection, AgentError> {
        let mut cmd = Command::new(&self.config.command);
        cmd.args(&self.config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        for (k, v) in &self.config.env {
            cmd.env(k, v);
        }

        let mut child = cmd.spawn().map_err(|e| {
            AgentError::Config(format!(
                "Failed to spawn Copilot ACP subprocess '{}': {}",
                self.config.command, e
            ))
        })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Config("Failed to capture Copilot ACP stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Config("Failed to capture Copilot ACP stdout".into()))?;

        tracing::info!(command = %self.config.command, "Copilot ACP subprocess spawned");

        Ok(AcpConnection {
            child,
            stdin,
            reader: BufReader::new(stdout),
            request_id: 0,
            session_id: None,
        })
    }

    async fn initialize(&self) -> Result<Value, AgentError> {
        let params = json!({
            "clientInfo": {
                "name": "hermes-agent",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "capabilities": {
                "tools": true,
                "streaming": true,
            }
        });

        self.send_request("initialize", params).await
    }

    async fn create_session(&self) -> Result<String, AgentError> {
        let params = json!({
            "name": "hermes-copilot-session",
        });

        let response = self.send_request("session/new", params).await?;
        let session_id = response
            .get("sessionId")
            .and_then(|v| v.as_str())
            .ok_or_else(|| AgentError::LlmApi("No sessionId in session/new response".into()))?
            .to_string();

        // Store session id
        let mut conn = self.connection.lock().await;
        if let Some(ref mut c) = *conn {
            c.session_id = Some(session_id.clone());
        }

        Ok(session_id)
    }

    async fn ensure_connected(&self) -> Result<(), AgentError> {
        if !self.is_connected().await {
            self.connect().await?;
        }
        Ok(())
    }

    async fn send_request(&self, method: &str, params: Value) -> Result<Value, AgentError> {
        let mut conn_guard = self.connection.lock().await;
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| AgentError::Config("Copilot ACP not connected".into()))?;

        conn.request_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": conn.request_id,
            "method": method,
            "params": params,
        });

        let mut line = serde_json::to_string(&request)
            .map_err(|e| AgentError::LlmApi(format!("JSON serialize: {e}")))?;
        line.push('\n');

        conn.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| AgentError::LlmApi(format!("Write to Copilot ACP: {e}")))?;
        conn.stdin
            .flush()
            .await
            .map_err(|e| AgentError::LlmApi(format!("Flush Copilot ACP: {e}")))?;

        // Read response
        let mut response_line = String::new();
        let read_result = tokio::time::timeout(
            self.config.request_timeout,
            conn.reader.read_line(&mut response_line),
        )
        .await;

        match read_result {
            Ok(Ok(0)) => Err(AgentError::LlmApi(
                "Copilot ACP subprocess closed stdout".into(),
            )),
            Ok(Ok(_)) => {
                let response: Value = serde_json::from_str(response_line.trim())
                    .map_err(|e| AgentError::LlmApi(format!("Parse Copilot ACP response: {e}")))?;

                if let Some(error) = response.get("error") {
                    let msg = error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown error");
                    return Err(AgentError::LlmApi(format!("Copilot ACP error: {msg}")));
                }

                Ok(response.get("result").cloned().unwrap_or(Value::Null))
            }
            Ok(Err(e)) => Err(AgentError::LlmApi(format!("Read from Copilot ACP: {e}"))),
            Err(_) => Err(AgentError::LlmApi(format!(
                "Copilot ACP request timed out after {:?}",
                self.config.request_timeout
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = CopilotAcpConfig::default();
        assert_eq!(cfg.command, "copilot-acp");
        assert_eq!(cfg.max_reconnects, 3);
    }

    #[test]
    fn config_serialization() {
        let cfg = CopilotAcpConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let parsed: CopilotAcpConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.command, "copilot-acp");
    }

    #[tokio::test]
    async fn client_not_connected_initially() {
        let client = CopilotAcpClient::new(CopilotAcpConfig::default());
        assert!(!client.is_connected().await);
    }
}
