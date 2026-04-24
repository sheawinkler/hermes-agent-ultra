//! MCP Transport Layer
//!
//! Provides transport abstraction for MCP communication:
//! - **StdioTransport**: Communicates via stdin/stdout with a child process (JSON-RPC)
//! - **HttpTransport**: Communicates via HTTP/SSE with a remote MCP server
//!
//! The transport layer handles:
//! - Message framing with Content-Length header + JSON body
//! - Connection lifecycle (start, send, receive, close)
//! - Error handling and connection lifecycle

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tokio::fs::OpenOptions;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tracing::{error, info, trace};

use crate::McpError;

const MCP_PROTOCOL_VERSION_HEADER_VALUE: &str = "2025-03-26";

fn hermes_home_dir() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_HOME") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let trimmed = home.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed).join(".hermes-agent-ultra");
        }
    }
    PathBuf::from(".hermes-agent-ultra")
}

async fn mcp_stderr_log_file() -> Option<tokio::fs::File> {
    let path = hermes_home_dir().join("logs").join("mcp-stderr.log");
    if let Some(parent) = path.parent() {
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return None;
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await
        .ok()
}

fn classify_http_status_error(status: reqwest::StatusCode, body: &str) -> McpError {
    let msg = format!("HTTP error {}: {}", status, body);
    match status.as_u16() {
        401 => McpError::Auth(msg),
        403 => McpError::Forbidden(msg),
        404 => McpError::ResourceNotFound(msg),
        400 | 422 => McpError::InvalidParams(msg),
        501 | 503 => McpError::NotConfigured(msg),
        _ => McpError::ConnectionError(msg),
    }
}

// ---------------------------------------------------------------------------
// McpTransport trait
// ---------------------------------------------------------------------------

/// Transport trait for MCP communication.
///
/// Implementations handle the low-level details of sending and receiving
/// JSON-RPC messages over different transport mechanisms.
#[async_trait]
pub trait McpTransport: Send + Sync {
    /// Start the transport connection.
    async fn start(&mut self) -> Result<(), McpError>;

    /// Send a JSON-RPC message.
    async fn send(&mut self, message: Value) -> Result<(), McpError>;

    /// Receive a JSON-RPC message.
    async fn receive(&mut self) -> Result<Value, McpError>;

    /// Close the transport connection.
    async fn close(&mut self) -> Result<(), McpError>;
}

// ---------------------------------------------------------------------------
// StdioTransport
// ---------------------------------------------------------------------------

/// Transport that communicates via stdin/stdout with a child process.
///
/// Uses the MCP message framing protocol:
/// - Each message is prefixed with a `Content-Length` header
/// - The body is a UTF-8 encoded JSON string
/// - Messages are separated by `\r\n\r\n` between header and body
///
/// This is the standard transport for local MCP server processes.
pub struct StdioTransport {
    /// The command to execute.
    command: String,
    /// Arguments for the command.
    args: Vec<String>,
    /// Environment variables for the child process.
    env: HashMap<String, String>,
    /// The child process.
    child: Option<Child>,
    /// Background task draining child stderr to an MCP log file.
    stderr_drain_task: Option<JoinHandle<()>>,
    /// Whether the transport has been started.
    started: bool,
}

impl StdioTransport {
    /// Create a new stdio transport for the given command.
    pub fn new(command: impl Into<String>, args: &[String], env: &HashMap<String, String>) -> Self {
        Self {
            command: command.into(),
            args: args.to_vec(),
            env: env.clone(),
            child: None,
            stderr_drain_task: None,
            started: false,
        }
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        info!(
            "Starting MCP stdio transport: {} {:?}",
            self.command, self.args
        );

        let mut cmd = Command::new(&self.command);
        cmd.args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set environment variables
        for (key, value) in &self.env {
            cmd.env(key, value);
        }

        let child = cmd.spawn().map_err(|e| {
            McpError::ConnectionError(format!("Failed to spawn process '{}': {}", self.command, e))
        })?;

        let mut child = child;
        if let Some(stderr) = child.stderr.take() {
            let command = self.command.clone();
            self.stderr_drain_task = Some(tokio::spawn(async move {
                let mut reader = tokio::io::BufReader::new(stderr);
                let mut line = String::new();
                let mut log = mcp_stderr_log_file().await;
                if let Some(file) = log.as_mut() {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let _ = file
                        .write_all(
                            format!(
                                "\n===== [ts={}] starting MCP stdio server '{}' =====\n",
                                ts, command
                            )
                            .as_bytes(),
                        )
                        .await;
                }
                loop {
                    line.clear();
                    let Ok(bytes) = reader.read_line(&mut line).await else {
                        break;
                    };
                    if bytes == 0 {
                        break;
                    }
                    if let Some(file) = log.as_mut() {
                        if file.write_all(line.as_bytes()).await.is_err() {
                            log = None;
                        }
                    }
                }
            }));
        }

        self.child = Some(child);
        self.started = true;

        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdin = child
            .stdin
            .as_mut()
            .ok_or(McpError::ConnectionError("stdin not available".to_string()))?;

        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        trace!("Sending MCP message: {} bytes", body.len());

        stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdin
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;

        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let child = self
            .child
            .as_mut()
            .ok_or(McpError::ConnectionError("Process not started".to_string()))?;

        let stdout = child.stdout.as_mut().ok_or(McpError::ConnectionError(
            "stdout not available".to_string(),
        ))?;

        // Read Content-Length header
        let mut reader = tokio::io::BufReader::new(stdout);

        // Read header lines until we get Content-Length or empty line
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;

            if line.is_empty() {
                return Err(McpError::ConnectionClosed);
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                // End of headers
                break;
            }

            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }

        // Read the JSON body
        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        let body_str = String::from_utf8(body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid UTF-8 in body: {}", e),
        })?;

        trace!("Received MCP message: {} bytes", body_str.len());

        let value: Value = serde_json::from_str(&body_str).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })?;

        Ok(value)
    }

    async fn close(&mut self) -> Result<(), McpError> {
        if let Some(task) = self.stderr_drain_task.take() {
            task.abort();
        }
        if let Some(mut child) = self.child.take() {
            info!("Shutting down MCP stdio process");
            // Try graceful shutdown first
            match child.kill().await {
                Ok(()) => {
                    // Wait for the process to exit
                    let _ = child.wait().await;
                }
                Err(e) => {
                    error!("Failed to kill MCP process: {}", e);
                }
            }
        }
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ServerStdioTransport — server-side stdio (reads own stdin, writes own stdout)
// ---------------------------------------------------------------------------

/// Server-side stdio transport that reads from the process's own stdin
/// and writes to stdout. Used when hermes itself acts as an MCP server
/// (e.g. `hermes mcp serve`).
pub struct ServerStdioTransport {
    started: bool,
}

impl ServerStdioTransport {
    pub fn new() -> Self {
        Self { started: false }
    }
}

#[async_trait]
impl McpTransport for ServerStdioTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }
        self.started = true;
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let body =
            serde_json::to_string(&message).map_err(|e| McpError::Serialization(e.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());

        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(header.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write header failed: {}", e)))?;
        stdout
            .write_all(body.as_bytes())
            .await
            .map_err(|e| McpError::ConnectionError(format!("Write body failed: {}", e)))?;
        stdout
            .flush()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Flush failed: {}", e)))?;
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        let stdin = tokio::io::stdin();
        let mut reader = tokio::io::BufReader::new(stdin);

        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .await
                .map_err(|e| McpError::ConnectionError(format!("Read header failed: {}", e)))?;
            if n == 0 {
                return Err(McpError::ConnectionClosed);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
                content_length =
                    len_str
                        .trim()
                        .parse::<usize>()
                        .map_err(|e| McpError::Protocol {
                            code: -32700,
                            message: format!("Invalid Content-Length: {}", e),
                        })?;
            }
        }

        if content_length == 0 {
            return Err(McpError::Protocol {
                code: -32700,
                message: "Missing Content-Length header".to_string(),
            });
        }

        let mut body_buf = vec![0u8; content_length];
        reader
            .read_exact(&mut body_buf)
            .await
            .map_err(|e| McpError::ConnectionError(format!("Read body failed: {}", e)))?;

        let body_str = String::from_utf8(body_buf).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid UTF-8 in body: {}", e),
        })?;

        serde_json::from_str(&body_str).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("JSON parse error: {}", e),
        })
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self.started = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HttpTransport
// ---------------------------------------------------------------------------

/// Transport that communicates via HTTP with a remote MCP server.
///
/// Uses HTTP POST for sending JSON-RPC messages and receives responses
/// directly. For servers supporting SSE (Server-Sent Events), this
/// transport can also handle streaming responses.
pub struct HttpTransport {
    /// The base URL of the MCP server.
    url: String,
    /// Optional bearer token for authentication.
    auth_token: Option<String>,
    /// HTTP client.
    client: reqwest::Client,
    /// Whether the transport has been started.
    started: bool,
    /// Pending response from the server.
    pending_response: Option<Value>,
}

impl HttpTransport {
    /// Create a new HTTP transport for the given URL.
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        Self {
            url: url.to_string(),
            auth_token,
            client: reqwest::Client::new(),
            started: false,
            pending_response: None,
        }
    }

    /// Build the full URL for an MCP endpoint.
    fn endpoint_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// Build a request with optional authentication.
    fn build_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        builder = builder.header("Content-Type", "application/json");
        builder = builder.header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION_HEADER_VALUE);
        if let Some(ref token) = self.auth_token {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }
        builder
    }
}

#[async_trait]
impl McpTransport for HttpTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        // For HTTP transport, "starting" means verifying the server is reachable
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        // Verify the server is reachable
        let url = self.endpoint_url("/health");
        match self.client.get(&url).send().await {
            Ok(_) => {
                info!("MCP HTTP transport connected to: {}", self.url);
                self.started = true;
                Ok(())
            }
            Err(_) => {
                // Health endpoint may not exist; try the base URL
                let base_url = &self.url;
                match self.client.get(base_url).send().await {
                    Ok(_) => {
                        info!("MCP HTTP transport connected to: {}", self.url);
                        self.started = true;
                        Ok(())
                    }
                    Err(e) => Err(McpError::ConnectionError(format!(
                        "Failed to connect to MCP server at {}: {}",
                        self.url, e
                    ))),
                }
            }
        }
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let url = self.endpoint_url("/message");
        let builder = self.build_request(reqwest::Method::POST, &url);
        let response = builder
            .json(&message)
            .send()
            .await
            .map_err(|e| McpError::ConnectionError(format!("HTTP send failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(classify_http_status_error(status, &body));
        }

        // Store the response for receive()
        let body = response
            .text()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Failed to read response: {}", e)))?;

        let value: Value = serde_json::from_str(&body).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid JSON response: {}", e),
        })?;

        self.pending_response = Some(value);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        // For HTTP transport, the response is already stored from send()
        match self.pending_response.take() {
            Some(value) => Ok(value),
            None => Err(McpError::ConnectionError(
                "No pending response. Call send() first.".to_string(),
            )),
        }
    }

    async fn close(&mut self) -> Result<(), McpError> {
        info!("Closing MCP HTTP transport");
        self.started = false;
        self.pending_response = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// HttpSseTransport
// ---------------------------------------------------------------------------

/// Transport that sends JSON-RPC requests via HTTP POST and receives
/// responses through a Server-Sent Events (SSE) stream.
///
/// This is the standard transport for remote MCP servers that support
/// the SSE protocol. The flow is:
/// 1. Connect to the SSE endpoint to start receiving events
/// 2. Send JSON-RPC requests via HTTP POST to the message endpoint
/// 3. Receive JSON-RPC responses as SSE `message` events
///
/// Falls back to reading the HTTP response body directly when the server
/// does not use SSE for the response.
pub struct HttpSseTransport {
    /// The base URL of the MCP server.
    url: String,
    /// Optional bearer token for authentication.
    auth_token: Option<String>,
    /// HTTP client.
    client: reqwest::Client,
    /// Whether the transport has been started.
    started: bool,
    /// Pending response from the last POST (non-SSE fallback).
    pending_response: Option<Value>,
    /// The message endpoint URL discovered from the SSE stream, or default.
    message_endpoint: Option<String>,
}

impl HttpSseTransport {
    /// Create a new HTTP+SSE transport for the given URL.
    pub fn new(url: &str, auth_token: Option<String>) -> Self {
        Self {
            url: url.to_string(),
            auth_token,
            client: reqwest::Client::new(),
            started: false,
            pending_response: None,
            message_endpoint: None,
        }
    }

    /// Build the full URL for an MCP endpoint.
    fn endpoint_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    /// The URL used for POSTing JSON-RPC messages.
    fn post_url(&self) -> String {
        if let Some(ref ep) = self.message_endpoint {
            ep.clone()
        } else {
            self.endpoint_url("/message")
        }
    }

    /// Build a request with optional authentication.
    fn build_request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut builder = self.client.request(method, url);
        builder = builder.header("Content-Type", "application/json");
        builder = builder.header("MCP-Protocol-Version", MCP_PROTOCOL_VERSION_HEADER_VALUE);
        if let Some(ref token) = self.auth_token {
            builder = builder.header("Authorization", format!("Bearer {}", token));
        }
        builder
    }
}

#[async_trait]
impl McpTransport for HttpSseTransport {
    async fn start(&mut self) -> Result<(), McpError> {
        if self.started {
            return Err(McpError::ConnectionError(
                "Transport already started".to_string(),
            ));
        }

        // Try to connect to the SSE endpoint first for streaming support.
        // If it fails, fall back to the base URL health check.
        let sse_url = self.endpoint_url("/sse");
        let mut sse_builder = self.client.get(&sse_url);
        if let Some(ref token) = self.auth_token {
            sse_builder = sse_builder.header("Authorization", format!("Bearer {}", token));
        }

        match sse_builder.send().await {
            Ok(resp) if resp.status().is_success() => {
                // If the SSE endpoint returns an `endpoint` field in the
                // first event, use it for POSTing messages.
                let body = resp.text().await.unwrap_or_default();
                for line in body.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        if let Ok(val) = serde_json::from_str::<Value>(data) {
                            if let Some(ep) = val.get("endpoint").and_then(|v| v.as_str()) {
                                self.message_endpoint = Some(ep.to_string());
                            }
                        }
                    }
                }
                info!("MCP HTTP+SSE transport connected to: {}", self.url);
            }
            _ => {
                // SSE endpoint not available — verify the base URL is reachable.
                let base_url = &self.url;
                self.client.get(base_url).send().await.map_err(|e| {
                    McpError::ConnectionError(format!(
                        "Failed to connect to MCP server at {}: {}",
                        self.url, e
                    ))
                })?;
                info!("MCP HTTP+SSE transport connected (no SSE) to: {}", self.url);
            }
        }

        self.started = true;
        Ok(())
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let url = self.post_url();
        let builder = self.build_request(reqwest::Method::POST, &url);
        let response = builder
            .json(&message)
            .send()
            .await
            .map_err(|e| McpError::ConnectionError(format!("HTTP send failed: {}", e)))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            return Err(classify_http_status_error(status, &body));
        }

        let body = response
            .text()
            .await
            .map_err(|e| McpError::ConnectionError(format!("Failed to read response: {}", e)))?;

        // Parse response — may be SSE or plain JSON.
        let json_body = if body.contains("data: ") {
            // SSE-formatted response: extract the last `data:` line.
            body.lines()
                .filter_map(|line| line.strip_prefix("data: "))
                .last()
                .unwrap_or(&body)
                .to_string()
        } else {
            body
        };

        let value: Value = serde_json::from_str(&json_body).map_err(|e| McpError::Protocol {
            code: -32700,
            message: format!("Invalid JSON response: {}", e),
        })?;

        self.pending_response = Some(value);
        Ok(())
    }

    async fn receive(&mut self) -> Result<Value, McpError> {
        match self.pending_response.take() {
            Some(value) => Ok(value),
            None => Err(McpError::ConnectionError(
                "No pending response. Call send() first.".to_string(),
            )),
        }
    }

    async fn close(&mut self) -> Result<(), McpError> {
        info!("Closing MCP HTTP+SSE transport");
        self.started = false;
        self.pending_response = None;
        self.message_endpoint = None;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stdio_transport_new() {
        let env = HashMap::new();
        let transport = StdioTransport::new("echo", &[], &env);
        assert_eq!(transport.command, "echo");
        assert!(transport.args.is_empty());
        assert!(!transport.started);
    }

    #[test]
    fn test_http_transport_new() {
        let transport = HttpTransport::new("http://localhost:8080/mcp", None);
        assert_eq!(transport.url, "http://localhost:8080/mcp");
        assert!(transport.auth_token.is_none());
        assert!(!transport.started);
    }

    #[test]
    fn test_http_transport_with_auth() {
        let transport = HttpTransport::new(
            "http://localhost:8080/mcp",
            Some("secret-token".to_string()),
        );
        assert_eq!(transport.auth_token, Some("secret-token".to_string()));
    }

    #[test]
    fn test_http_transport_seeds_protocol_header() {
        let transport = HttpTransport::new("http://localhost:8080/mcp", None);
        let req = transport
            .build_request(reqwest::Method::POST, "http://localhost:8080/mcp/message")
            .build()
            .expect("build request");
        assert_eq!(
            req.headers()
                .get("MCP-Protocol-Version")
                .and_then(|v| v.to_str().ok()),
            Some(MCP_PROTOCOL_VERSION_HEADER_VALUE)
        );
    }

    #[test]
    fn test_http_sse_transport_seeds_protocol_header() {
        let transport = HttpSseTransport::new("http://localhost:8080/mcp", None);
        let req = transport
            .build_request(reqwest::Method::POST, "http://localhost:8080/mcp/message")
            .build()
            .expect("build request");
        assert_eq!(
            req.headers()
                .get("MCP-Protocol-Version")
                .and_then(|v| v.to_str().ok()),
            Some(MCP_PROTOCOL_VERSION_HEADER_VALUE)
        );
    }

    #[test]
    fn test_endpoint_url() {
        let transport = HttpTransport::new("http://localhost:8080/mcp/", None);
        assert_eq!(
            transport.endpoint_url("/message"),
            "http://localhost:8080/mcp/message"
        );
        assert_eq!(
            transport.endpoint_url("message"),
            "http://localhost:8080/mcp/message"
        );
    }

    #[test]
    fn test_classify_http_status_error_forbidden() {
        let e = classify_http_status_error(reqwest::StatusCode::FORBIDDEN, "no capability");
        assert!(matches!(e, McpError::Forbidden(_)));
    }

    #[test]
    fn test_classify_http_status_error_not_configured() {
        let e =
            classify_http_status_error(reqwest::StatusCode::SERVICE_UNAVAILABLE, "not configured");
        assert!(matches!(e, McpError::NotConfigured(_)));
    }
}
