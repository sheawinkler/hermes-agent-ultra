//! # hermes-mcp
//!
//! MCP (Model Context Protocol) integration for Hermes Agent.
//!
//! This crate provides:
//! - **McpClient**: Connect to external MCP servers, discover and call their tools
//! - **McpServer**: Expose hermes-agent tools as MCP tools to external clients
//! - **McpTransport**: Transport layer abstraction (stdio, HTTP/SSE)
//! - **McpAuthProvider**: OAuth and bearer token authentication for remote MCP servers

pub mod auth;
pub mod client;
pub mod serve;
pub mod server;
pub mod transport;

// ---------------------------------------------------------------------------
// McpError
// ---------------------------------------------------------------------------

/// Error type for MCP operations.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// Error connecting to an MCP server.
    #[error("Connection error: {0}")]
    ConnectionError(String),

    /// Error in the MCP protocol (JSON-RPC error codes).
    #[error("Protocol error (code {code}): {message}")]
    Protocol { code: i64, message: String },

    /// Serialization or deserialization error.
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Configuration error.
    #[error("Configuration error: {0}")]
    Config(String),

    /// Authentication error.
    #[error("Authentication error: {0}")]
    Auth(String),

    /// The requested server was not found.
    #[error("Server not found: {0}")]
    ServerNotFound(String),

    /// The requested method was not found.
    #[error("Method not found: {0}")]
    MethodNotFound(String),

    /// Invalid parameters for a method call.
    #[error("Invalid parameters: {0}")]
    InvalidParams(String),

    /// The requested resource was not found.
    #[error("Resource not found: {0}")]
    ResourceNotFound(String),

    /// The connection was closed by the remote end.
    #[error("Connection closed")]
    ConnectionClosed,

    /// I/O error.
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for McpError {
    fn from(err: std::io::Error) -> Self {
        McpError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for McpError {
    fn from(err: serde_json::Error) -> Self {
        McpError::Serialization(err.to_string())
    }
}

// Re-export primary types
pub use auth::{BearerTokenAuth, McpAuthProvider, OAuthConfig};
pub use client::{
    McpClient, McpManager, McpProbeResult, McpServerConfig, McpServerStatus, PromptArgument,
    PromptInfo, PromptMessage, PromptResult, ResourceInfo, SamplingConfig,
};
pub use serve::{
    ApprovalStore as McpApprovalStore, BridgeEvent, EventBridge, HermesMcpServe,
    InMemorySessionStore, PendingApproval, SessionEntry, SessionMessage, SessionStore,
};
pub use server::McpServer;
pub use transport::{
    HttpSseTransport, HttpTransport, McpTransport, ServerStdioTransport, StdioTransport,
};
