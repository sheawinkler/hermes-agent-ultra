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

use serde_json::{Number, Value};

pub(crate) fn coerce_mcp_tool_arguments_for_schema(
    arguments: Value,
    schema: Option<&Value>,
) -> Value {
    let mut arguments = coerce_whole_mcp_arguments(arguments);
    let Some(schema) = schema else {
        return arguments;
    };
    let Some(args) = arguments.as_object_mut() else {
        return arguments;
    };
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return arguments;
    };

    for (key, value) in args {
        let Some(raw) = value.as_str().map(str::to_string) else {
            continue;
        };
        let Some(property_schema) = properties.get(key) else {
            continue;
        };
        if let Some(coerced) = coerce_string_for_schema(&raw, property_schema) {
            *value = coerced;
        }
    }

    arguments
}

fn coerce_whole_mcp_arguments(arguments: Value) -> Value {
    let Value::String(raw) = arguments else {
        return arguments;
    };
    match serde_json::from_str::<Value>(raw.trim()) {
        Ok(parsed @ (Value::Object(_) | Value::Array(_))) => parsed,
        _ => Value::String(raw),
    }
}

fn coerce_string_for_schema(raw: &str, schema: &Value) -> Option<Value> {
    let trimmed = raw.trim();
    if schema_allows_null(schema) && trimmed.eq_ignore_ascii_case("null") {
        return Some(Value::Null);
    }

    for expected in schema_types(schema) {
        if expected == "null" && trimmed.eq_ignore_ascii_case("null") {
            return Some(Value::Null);
        }
        if let Some(coerced) = coerce_string_for_type(trimmed, expected) {
            return Some(coerced);
        }
    }

    None
}

fn coerce_string_for_type(raw: &str, expected: &str) -> Option<Value> {
    match expected {
        "integer" => coerce_number(raw, true),
        "number" => coerce_number(raw, false),
        "boolean" => coerce_boolean(raw),
        "array" => coerce_json_shape(raw, Value::is_array),
        "object" => coerce_json_shape(raw, Value::is_object),
        _ => None,
    }
}

fn coerce_number(raw: &str, integer_only: bool) -> Option<Value> {
    let parsed = raw.parse::<f64>().ok()?;
    if !parsed.is_finite() {
        return None;
    }
    if parsed.fract() == 0.0 {
        if parsed < i64::MIN as f64 || parsed > i64::MAX as f64 {
            return None;
        }
        return Some(Value::Number(Number::from(parsed as i64)));
    }
    if integer_only {
        return None;
    }
    Number::from_f64(parsed).map(Value::Number)
}

fn coerce_boolean(raw: &str) -> Option<Value> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" => Some(Value::Bool(true)),
        "false" => Some(Value::Bool(false)),
        _ => None,
    }
}

fn coerce_json_shape(raw: &str, predicate: fn(&Value) -> bool) -> Option<Value> {
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    predicate(&parsed).then_some(parsed)
}

fn schema_types(schema: &Value) -> Vec<&str> {
    let mut out = Vec::new();
    collect_schema_types(schema, &mut out);
    out
}

fn collect_schema_types<'a>(schema: &'a Value, out: &mut Vec<&'a str>) {
    match schema.get("type") {
        Some(Value::String(kind)) => out.push(kind),
        Some(Value::Array(kinds)) => {
            for kind in kinds {
                if let Some(kind) = kind.as_str() {
                    out.push(kind);
                }
            }
        }
        _ => {}
    }

    for key in ["anyOf", "oneOf"] {
        if let Some(variants) = schema.get(key).and_then(Value::as_array) {
            for variant in variants {
                collect_schema_types(variant, out);
            }
        }
    }
}

fn schema_allows_null(schema: &Value) -> bool {
    if schema.get("nullable").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    match schema.get("type") {
        Some(Value::String(kind)) if kind == "null" => return true,
        Some(Value::Array(kinds)) if kinds.iter().any(|kind| kind.as_str() == Some("null")) => {
            return true;
        }
        _ => {}
    }

    for key in ["anyOf", "oneOf"] {
        if schema
            .get(key)
            .and_then(Value::as_array)
            .is_some_and(|variants| variants.iter().any(schema_allows_null))
        {
            return true;
        }
    }

    false
}

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

    /// Operation is not configured on this MCP endpoint.
    #[error("Not configured: {0}")]
    NotConfigured(String),

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

    /// The operation is forbidden by capability policy.
    #[error("Forbidden: {0}")]
    Forbidden(String),

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
    sanitize_mcp_name_component, LlmCallback, McpClient, McpDiscoveryReport, McpManager,
    McpProbeResult, McpServerConfig, McpServerStatus, PromptArgument, PromptInfo, PromptMessage,
    PromptResult, ResourceInfo, SamplingConfig, SamplingMetrics,
};
pub use serve::{
    ApprovalStore as McpApprovalStore, BridgeEvent, EventBridge, HermesMcpServe,
    InMemorySessionStore, PendingApproval, SessionEntry, SessionMessage, SessionStore,
};
pub use server::McpServer;
pub use transport::{
    HttpSseTransport, HttpTransport, McpTransport, ServerStdioTransport, StdioTransport,
};
