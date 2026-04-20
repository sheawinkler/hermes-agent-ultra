//! Errors and error classification for the auxiliary client.
//!
//! Mirrors the Python `_is_payment_error` / `_is_connection_error` helpers so
//! the Rust router can decide when to fall back to another provider vs.
//! propagate the failure to the caller.

use std::time::Duration;

use hermes_core::AgentError;
use thiserror::Error;

/// Result alias used throughout the auxiliary subsystem.
pub type AuxiliaryResult<T> = std::result::Result<T, AuxiliaryError>;

/// Top-level errors returned by [`crate::auxiliary::AuxiliaryClient`].
#[derive(Debug, Error)]
pub enum AuxiliaryError {
    /// No provider could be resolved for the requested task.
    #[error("no auxiliary provider available (tried: {tried:?})")]
    NoProviderAvailable { tried: Vec<String> },

    /// The whole fallback chain ran out without producing a successful response.
    #[error("all auxiliary providers failed: {summary}")]
    AllProvidersFailed {
        errors: Vec<(String, String)>,
        summary: String,
    },

    /// The selected provider exhausted its credits (HTTP 402 or message hint).
    /// Returned only when the chain has been disabled (e.g. a single explicit
    /// provider was requested).
    #[error("payment / credit exhaustion on provider {provider}: {reason}")]
    PaymentRequired { provider: String, reason: String },

    /// Network failure (DNS, refused, TLS, timeout) on the selected provider.
    #[error("connection failure on provider {provider}: {reason}")]
    ConnectionFailed { provider: String, reason: String },

    /// The request itself was malformed (empty messages, unknown task name, ...).
    #[error("invalid auxiliary request: {0}")]
    InvalidRequest(String),

    /// Hard timeout enforced by the client (independent of the underlying
    /// HTTP timeout — used when the caller wants a strict wall-clock budget).
    #[error("auxiliary call exceeded the {0:?} wall-clock budget")]
    Timeout(Duration),

    /// LLM provider returned an error the auxiliary layer doesn't recognise.
    #[error("LLM error on provider {provider}: {source}")]
    Llm {
        provider: String,
        #[source]
        source: AgentError,
    },
}

impl AuxiliaryError {
    pub fn all_providers_failed(errors: Vec<(String, String)>) -> Self {
        let summary = errors
            .iter()
            .map(|(label, msg)| format!("{label}: {msg}"))
            .collect::<Vec<_>>()
            .join("; ");
        AuxiliaryError::AllProvidersFailed { errors, summary }
    }
}

// ---------------------------------------------------------------------------
// Classification helpers — pure functions on [`AgentError`] strings
// ---------------------------------------------------------------------------

/// Substrings that strongly indicate a payment / credit exhaustion problem
/// regardless of HTTP status code. Mirrors Python `_is_payment_error`.
const PAYMENT_KEYWORDS: &[&str] = &[
    "credits",
    "insufficient funds",
    "can only afford",
    "billing",
    "payment required",
    "402",
    "quota exhausted",
    "exceeded your monthly",
];

/// Substrings that indicate a connection / DNS / TLS failure.
const CONNECTION_KEYWORDS: &[&str] = &[
    "connection refused",
    "name or service not known",
    "no route to host",
    "network is unreachable",
    "connection reset",
    "tls handshake",
    "dns",
    "timed out",
    "deadline exceeded",
    "broken pipe",
    "eof while",
];

/// Returns `true` if the error looks like a payment / credit exhaustion
/// failure that warrants trying the next provider in the chain.
pub fn is_payment_error(err: &AgentError) -> bool {
    let msg = err.to_string().to_lowercase();
    PAYMENT_KEYWORDS.iter().any(|kw| msg.contains(kw))
}

/// Returns `true` if the error looks like a transient connection problem
/// (DNS, refused, TLS, timeout) that warrants trying the next provider.
pub fn is_connection_error(err: &AgentError) -> bool {
    let msg = err.to_string().to_lowercase();
    CONNECTION_KEYWORDS.iter().any(|kw| msg.contains(kw))
}

/// Convenience: returns `true` iff [`is_payment_error`] OR [`is_connection_error`].
pub fn should_fallback(err: &AgentError) -> bool {
    is_payment_error(err) || is_connection_error(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::AgentError;

    fn err(msg: &str) -> AgentError {
        AgentError::LlmApi(msg.to_string())
    }

    #[test]
    fn detects_payment_402() {
        assert!(is_payment_error(&err("HTTP 402: payment required")));
        assert!(is_payment_error(&err("you can only afford 12 credits")));
        assert!(is_payment_error(&err("Quota exhausted on this org")));
    }

    #[test]
    fn detects_connection_failures() {
        assert!(is_connection_error(&err("connection refused")));
        assert!(is_connection_error(&err("DNS lookup failed")));
        assert!(is_connection_error(&err("request timed out after 30s")));
    }

    #[test]
    fn benign_errors_dont_trigger_fallback() {
        assert!(!should_fallback(&err("invalid api key")));
        assert!(!should_fallback(&err("model not found")));
        assert!(!should_fallback(&err("HTTP 400 bad request")));
    }
}
