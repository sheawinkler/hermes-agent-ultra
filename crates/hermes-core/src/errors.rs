/// Primary error type for agent operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("LLM API error: {0}")]
    LlmApi(String),

    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Gateway error: {0}")]
    Gateway(String),

    #[error("Timeout error: {0}")]
    Timeout(String),

    #[error("Maximum number of turns exceeded")]
    MaxTurnsExceeded,

    #[error("Invalid tool call: {0}")]
    InvalidToolCall(String),

    #[error("Context too long")]
    ContextTooLong,

    #[error("Rate limited{0}", retry_after_secs.map(|s| format!(" (retry after {}s)", s)).unwrap_or_default())]
    RateLimited { retry_after_secs: Option<u64> },

    #[error("Authentication failed: {0}")]
    AuthFailed(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Interrupted{0}", message.as_ref().map(|m| format!(": {}", m)).unwrap_or_default())]
    Interrupted { message: Option<String> },
}

/// Error type for tool execution failures.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("Tool execution failed: {0}")]
    ExecutionFailed(String),

    #[error("Invalid tool parameters: {0}")]
    InvalidParams(String),

    #[error("Tool not found: {0}")]
    NotFound(String),

    #[error("Tool timed out: {0}")]
    Timeout(String),

    #[error("Schema violation: {0}")]
    SchemaViolation(String),
}

/// Error type for gateway / platform communication failures.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Platform error: {0}")]
    Platform(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Session expired: {0}")]
    SessionExpired(String),
}

/// Error type for configuration parsing and validation.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Parse error: {0}")]
    ParseError(String),

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Merge conflict: {0}")]
    MergeConflict(String),
}

// ---------------------------------------------------------------------------
// From conversions
// ---------------------------------------------------------------------------

impl From<ToolError> for AgentError {
    fn from(err: ToolError) -> Self {
        AgentError::ToolExecution(err.to_string())
    }
}

impl From<GatewayError> for AgentError {
    fn from(err: GatewayError) -> Self {
        AgentError::Gateway(err.to_string())
    }
}

impl From<ConfigError> for AgentError {
    fn from(err: ConfigError) -> Self {
        AgentError::Config(err.to_string())
    }
}

impl From<std::io::Error> for AgentError {
    fn from(err: std::io::Error) -> Self {
        AgentError::Io(err.to_string())
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(err: serde_json::Error) -> Self {
        AgentError::Config(err.to_string())
    }
}

impl From<ToolError> for GatewayError {
    fn from(err: ToolError) -> Self {
        GatewayError::Platform(err.to_string())
    }
}

impl From<std::io::Error> for ToolError {
    fn from(err: std::io::Error) -> Self {
        ToolError::ExecutionFailed(err.to_string())
    }
}

impl From<std::io::Error> for GatewayError {
    fn from(err: std::io::Error) -> Self {
        GatewayError::ConnectionFailed(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_error_display() {
        let err = AgentError::LlmApi("timeout".into());
        assert_eq!(err.to_string(), "LLM API error: timeout");

        let err = AgentError::RateLimited {
            retry_after_secs: Some(30),
        };
        assert!(err.to_string().contains("30"));

        let err = AgentError::MaxTurnsExceeded;
        assert_eq!(err.to_string(), "Maximum number of turns exceeded");
    }

    #[test]
    fn tool_error_display() {
        let err = ToolError::NotFound("my_tool".into());
        assert_eq!(err.to_string(), "Tool not found: my_tool");
    }

    #[test]
    fn from_conversion_tool_to_agent() {
        let tool_err = ToolError::Timeout("10s".into());
        let agent_err: AgentError = tool_err.into();
        match agent_err {
            AgentError::ToolExecution(msg) => assert!(msg.contains("10s")),
            _ => panic!("expected ToolExecution variant"),
        }
    }

    #[test]
    fn from_conversion_io_to_agent() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let agent_err: AgentError = io_err.into();
        match agent_err {
            AgentError::Io(msg) => assert!(msg.contains("file missing")),
            _ => panic!("expected Io variant"),
        }
    }
}
