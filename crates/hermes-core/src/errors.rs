/// Primary error type for agent operations.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AgentError {
    #[error("LLM API error: {0}")]
    LlmApi(String),

    #[error("Tool execution error: {0}")]
    ToolExecution(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Policy blocked: {0}")]
    PolicyBlocked(String),

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
    /// Handler invocation began, but no trustworthy completion outcome exists.
    #[error("Tool execution outcome unknown: {0}")]
    OutcomeUnknown(String),

    #[error("Schema violation: {0}")]
    SchemaViolation(String),
}

/// Platform-neutral category for gateway send failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendErrorKind {
    /// Content exceeded a platform message-size cap.
    TooLong,
    /// Markup/entities could not be parsed by the platform.
    BadFormat,
    /// The bot is blocked, kicked, unauthenticated, or lacks post rights.
    Forbidden,
    /// The target chat, thread, topic, or message no longer exists.
    NotFound,
    /// Platform flood control or rate limiting throttled the send.
    RateLimited,
    /// Connection-level failure that is safe to retry.
    Transient,
    /// No known provider error shape matched.
    Unknown,
}

impl SendErrorKind {
    pub const ALL: [Self; 7] = [
        Self::TooLong,
        Self::BadFormat,
        Self::Forbidden,
        Self::NotFound,
        Self::RateLimited,
        Self::Transient,
        Self::Unknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TooLong => "too_long",
            Self::BadFormat => "bad_format",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not_found",
            Self::RateLimited => "rate_limited",
            Self::Transient => "transient",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for SendErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

pub const SEND_ERROR_KINDS: &[SendErrorKind] = &SendErrorKind::ALL;

/// Classify a provider send failure without tying consumers to raw text shapes.
pub fn classify_send_error_text(error_text: &str) -> SendErrorKind {
    let blob = error_text.to_lowercase();
    if blob.trim().is_empty() {
        return SendErrorKind::Unknown;
    }

    if blob.contains("message_too_long")
        || blob.contains("too long")
        || blob.contains("message is too long")
    {
        return SendErrorKind::TooLong;
    }

    if blob.contains("can't parse entities")
        || blob.contains("cant parse entities")
        || blob.contains("can't find end")
        || blob.contains("unsupported start tag")
        || (blob.contains("entity") && blob.contains("parse"))
        || (blob.contains("bad request") && blob.contains("entit"))
    {
        return SendErrorKind::BadFormat;
    }

    if blob.contains("forbidden")
        || blob.contains("bot was blocked")
        || blob.contains("blocked by the user")
        || blob.contains("user is deactivated")
        || blob.contains("not enough rights")
        || blob.contains("have no rights")
        || blob.contains("not a member")
    {
        return SendErrorKind::Forbidden;
    }

    if blob.contains("chat not found")
        || blob.contains("message to edit not found")
        || blob.contains("message to reply not found")
        || blob.contains("thread not found")
        || blob.contains("topic_deleted")
        || blob.contains("message_id_invalid")
    {
        return SendErrorKind::NotFound;
    }

    if blob.contains("flood")
        || blob.contains("too many requests")
        || blob.contains("retry after")
        || blob.contains("rate limit")
    {
        return SendErrorKind::RateLimited;
    }

    if blob.contains("timeout")
        || blob.contains("timed out")
        || blob.contains("connection reset")
        || blob.contains("connection aborted")
        || blob.contains("connection refused")
        || blob.contains("connection closed")
        || blob.contains("broken pipe")
        || blob.contains("temporarily unavailable")
        || blob.contains("dns")
        || blob.contains("tls")
        || blob.contains("ssl")
        || blob.contains("econnreset")
        || blob.contains("econnrefused")
        || blob.contains("connecttimeout")
        || blob.contains("network")
        || blob.contains("transport")
        || blob.contains("http 502")
        || blob.contains("http 503")
        || blob.contains("http 504")
    {
        return SendErrorKind::Transient;
    }

    SendErrorKind::Unknown
}

/// Durable information needed to account for a response whose prefix was
/// accepted by a provider. `remaining_text` is replayable only when
/// `retryable` is true; transport ambiguity must remain operator-visible until
/// the provider offers idempotency.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PartialDelivery {
    pub platform: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    pub reply_to_message_id: Option<String>,
    pub reply_to_mode: String,
    pub non_conversational: bool,
    /// Index in the original split response of the first unsent chunk.
    pub next_chunk_index: usize,
    pub remaining_text: String,
    pub accepted_message_ids: Vec<String>,
    pub accepted_chunks: usize,
    /// True only when the provider proved that `remaining_text` was not
    /// accepted (for example, a 429 before the request could be accepted).
    pub retryable: bool,
    /// Provider-authoritative pacing for the retryable suffix. This remains
    /// structured so durable replay never has to parse a display string or
    /// round a fractional server hint to whole seconds.
    #[serde(default)]
    pub retry_after: Option<std::time::Duration>,
    pub failure: String,
    /// Local state repair required before a retained suffix may be replayed.
    /// This is separate from the provider failure so durable spool consumers
    /// cannot accidentally treat an accepted prefix as an ordinary retry.
    #[serde(default)]
    pub repair_required: Option<String>,
}

impl std::fmt::Display for PartialDelivery {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} accepted chunk(s) on {} before delivery stopped: {}",
            self.accepted_chunks, self.platform, self.failure
        )
    }
}

/// A local media check that failed before any provider request was started.
///
/// This deliberately contains only a display-safe basename; callers must log
/// the original path locally rather than returning it to a chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPreflightFailure {
    NotFound,
    Unreadable,
    TooLarge,
}

impl MediaPreflightFailure {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Unreadable => "unreadable",
            Self::TooLarge => "too_large",
        }
    }
}

impl std::fmt::Display for MediaPreflightFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error type for gateway / platform communication failures.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Media attachment unavailable ({code}): {file_name}")]
    MediaPreflightFailed {
        file_name: String,
        code: MediaPreflightFailure,
    },

    /// Discord (or another native media provider) returned a definitive
    /// client-error response before accepting the attachment/embed. Callers
    /// may safely fall back to a plain-text representation.
    #[error("Remote media delivery rejected (HTTP {status})")]
    MediaRemoteRejected { status: u16 },

    #[error("Platform error: {0}")]
    Platform(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Rate limited{0}", retry_after.map(|duration| format!(" (retry after {duration:?})")).unwrap_or_default())]
    RateLimited {
        /// Provider pacing remains a duration rather than a rounded display
        /// value so callers can preserve fractional retry hints.
        retry_after: Option<std::time::Duration>,
    },

    #[error("Session expired: {0}")]
    SessionExpired(String),

    #[error("Partial delivery: {0}")]
    PartialDelivery(PartialDelivery),

    /// A provider accepted a remote side effect, but the corresponding local
    /// state could not be durably committed. Callers must repair local state,
    /// never replay the provider operation.
    #[error(
        "Accepted remote state is not durable; repair required ({repair}): {accepted_message_ids:?}"
    )]
    AcceptedStateUndurable {
        /// Non-secret provider identifiers whose accepted state needs repair.
        accepted_message_ids: Vec<String>,
        /// Safe operator-facing repair action; never contains a local path or
        /// provider credential.
        repair: String,
    },
}

impl GatewayError {
    pub fn send_error_kind(&self) -> SendErrorKind {
        match self {
            GatewayError::RateLimited { .. } => SendErrorKind::RateLimited,
            GatewayError::Auth(message) => match classify_send_error_text(message) {
                SendErrorKind::Unknown => SendErrorKind::Forbidden,
                kind => kind,
            },
            GatewayError::ConnectionFailed(message) => match classify_send_error_text(message) {
                SendErrorKind::Unknown => SendErrorKind::Transient,
                kind => kind,
            },
            GatewayError::PartialDelivery(partial) => {
                let kind = classify_send_error_text(&partial.failure);
                if partial.retryable && kind == SendErrorKind::Unknown {
                    SendErrorKind::Transient
                } else {
                    kind
                }
            }
            GatewayError::MediaPreflightFailed { code, .. } => match code {
                MediaPreflightFailure::NotFound => SendErrorKind::NotFound,
                MediaPreflightFailure::Unreadable => SendErrorKind::Transient,
                MediaPreflightFailure::TooLarge => SendErrorKind::TooLong,
            },
            GatewayError::MediaRemoteRejected { status } => match *status {
                401 | 403 => SendErrorKind::Forbidden,
                404 => SendErrorKind::NotFound,
                413 => SendErrorKind::TooLong,
                _ => SendErrorKind::Unknown,
            },
            GatewayError::AcceptedStateUndurable { .. } => SendErrorKind::Unknown,
            GatewayError::SendFailed(message)
            | GatewayError::Platform(message)
            | GatewayError::SessionExpired(message) => classify_send_error_text(message),
        }
    }

    pub fn is_send_error_kind(&self, kind: SendErrorKind) -> bool {
        self.send_error_kind() == kind
    }
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
    fn gateway_error_rate_limit_display() {
        let err = GatewayError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(42)),
        };
        assert_eq!(err.to_string(), "Rate limited (retry after 42s)");
    }

    #[test]
    fn partial_delivery_roundtrips_fractional_retry_after() {
        let partial = PartialDelivery {
            platform: "discord".to_string(),
            chat_id: "channel-1".to_string(),
            thread_id: None,
            reply_to_message_id: None,
            reply_to_mode: "first".to_string(),
            non_conversational: false,
            next_chunk_index: 1,
            remaining_text: "retry this suffix".to_string(),
            accepted_message_ids: vec!["message-1".to_string()],
            accepted_chunks: 1,
            retryable: true,
            retry_after: Some(std::time::Duration::from_millis(125)),
            failure: "rate limited".to_string(),
            repair_required: None,
        };
        let encoded = serde_json::to_string(&partial).expect("partial delivery serializes");
        let decoded: PartialDelivery =
            serde_json::from_str(&encoded).expect("partial delivery deserializes");
        assert_eq!(decoded.retry_after, partial.retry_after);
        assert_eq!(decoded, partial);
    }

    #[test]
    fn send_error_kind_names_match_wire_contract() {
        let names: Vec<_> = SEND_ERROR_KINDS.iter().map(|kind| kind.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "too_long",
                "bad_format",
                "forbidden",
                "not_found",
                "rate_limited",
                "transient",
                "unknown",
            ]
        );
        assert_eq!(
            serde_json::to_string(&SendErrorKind::TooLong).unwrap(),
            "\"too_long\""
        );
    }

    #[test]
    fn classify_send_error_text_covers_provider_shapes() {
        let cases = [
            ("Bad Request: message_too_long", SendErrorKind::TooLong),
            (
                "Bad Request: can't parse entities: Can't find end of the entity",
                SendErrorKind::BadFormat,
            ),
            (
                "Forbidden: bot was blocked by the user",
                SendErrorKind::Forbidden,
            ),
            ("Bad Request: chat not found", SendErrorKind::NotFound),
            (
                "Too Many Requests: retry after 12",
                SendErrorKind::RateLimited,
            ),
            (
                "request failed: connection reset by peer",
                SendErrorKind::Transient,
            ),
            ("provider said no", SendErrorKind::Unknown),
        ];

        for (text, expected) in cases {
            assert_eq!(classify_send_error_text(text), expected, "{text}");
        }
    }
    #[test]
    fn gateway_error_exposes_send_error_kind() {
        assert_eq!(
            GatewayError::RateLimited {
                retry_after: Some(std::time::Duration::from_secs(5))
            }
            .send_error_kind(),
            SendErrorKind::RateLimited
        );
        assert_eq!(
            GatewayError::Auth("bad token".into()).send_error_kind(),
            SendErrorKind::Forbidden
        );
        assert_eq!(
            GatewayError::ConnectionFailed("dial failed".into()).send_error_kind(),
            SendErrorKind::Transient
        );
        assert!(GatewayError::SendFailed("message_id_invalid".into())
            .is_send_error_kind(SendErrorKind::NotFound));
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
