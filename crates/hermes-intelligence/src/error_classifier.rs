//! Error classifier — maps LLM API errors to categories and recommends retry strategies.
//!
//! Requirement 16.2

use serde::{Deserialize, Serialize};

use hermes_core::AgentError;

// ---------------------------------------------------------------------------
// ErrorCategory
// ---------------------------------------------------------------------------

/// Classified error categories for LLM API failures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCategory {
    /// Rate limit exceeded; `retry_after_secs` is a hint from the API.
    RateLimit { retry_after_secs: Option<u64> },

    /// Authentication failed (bad API key, etc.).
    AuthFailed,

    /// The request context exceeds the model's context window.
    ContextTooLong,

    /// Server-side error with an HTTP status code.
    ServerError { status_code: u16 },

    /// Network connectivity issue.
    NetworkError,

    /// The request was malformed or invalid.
    InvalidRequest,

    /// The model is temporarily overloaded.
    ModelOverloaded,

    /// The request timed out.
    Timeout,

    /// An uncategorised error.
    Unknown,
}

// ---------------------------------------------------------------------------
// RetryStrategy
// ---------------------------------------------------------------------------

/// Recommended retry strategy for a given error category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RetryStrategy {
    /// Retry with exponential back-off.
    RetryWithBackoff {
        max_retries: u32,
        base_delay_secs: u64,
    },
    /// Retry once immediately.
    RetryOnce,
    /// Do not retry.
    NoRetry,
    /// Switch to a fallback model and retry.
    UseFallbackModel,
}

// ---------------------------------------------------------------------------
// ErrorClassifier
// ---------------------------------------------------------------------------

/// Classifies LLM API errors into categories and recommends retry strategies.
#[derive(Debug, Clone, Default)]
pub struct ErrorClassifier;

impl ErrorClassifier {
    pub fn new() -> Self {
        Self
    }

    /// Classify an `AgentError` into an `ErrorCategory`.
    pub fn classify(&self, error: &AgentError) -> ErrorCategory {
        match error {
            AgentError::LlmApi(msg) => classify_llm_api_message(msg),
            AgentError::RateLimited { retry_after_secs } => ErrorCategory::RateLimit {
                retry_after_secs: *retry_after_secs,
            },
            AgentError::ContextTooLong => ErrorCategory::ContextTooLong,
            AgentError::AuthFailed(_) => ErrorCategory::AuthFailed,
            AgentError::Timeout(_) => ErrorCategory::Timeout,
            AgentError::Gateway(msg) => classify_gateway_message(msg),
            AgentError::ToolExecution(_) => ErrorCategory::Unknown,
            AgentError::Config(_) => ErrorCategory::InvalidRequest,
            AgentError::MaxTurnsExceeded => ErrorCategory::Unknown,
            AgentError::Io(_) => ErrorCategory::NetworkError,
            AgentError::InvalidToolCall(_) => ErrorCategory::Unknown,
            AgentError::Interrupted { .. } => ErrorCategory::Unknown,
        }
    }

    /// Recommend a retry strategy for a given error category.
    pub fn recommend_strategy(&self, category: &ErrorCategory) -> RetryStrategy {
        match category {
            ErrorCategory::RateLimit { retry_after_secs } => RetryStrategy::RetryWithBackoff {
                max_retries: 3,
                base_delay_secs: retry_after_secs.unwrap_or(5),
            },
            ErrorCategory::AuthFailed => RetryStrategy::NoRetry,
            ErrorCategory::ContextTooLong => RetryStrategy::UseFallbackModel,
            ErrorCategory::ServerError { status_code } => match status_code {
                500 | 502 | 503 => RetryStrategy::RetryWithBackoff {
                    max_retries: 2,
                    base_delay_secs: 2,
                },
                _ => RetryStrategy::NoRetry,
            },
            ErrorCategory::NetworkError => RetryStrategy::RetryWithBackoff {
                max_retries: 3,
                base_delay_secs: 1,
            },
            ErrorCategory::InvalidRequest => RetryStrategy::NoRetry,
            ErrorCategory::ModelOverloaded => RetryStrategy::UseFallbackModel,
            ErrorCategory::Timeout => RetryStrategy::RetryOnce,
            ErrorCategory::Unknown => RetryStrategy::RetryOnce,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Heuristically classify an LLM API error message string.
fn classify_llm_api_message(msg: &str) -> ErrorCategory {
    let lower = msg.to_lowercase();
    let has_ssl_transient = lower.contains("bad record mac")
        || lower.contains("ssl alert")
        || lower.contains("tls alert")
        || lower.contains("ssl handshake failure")
        || lower.contains("tlsv1 alert")
        || lower.contains("sslv3 alert")
        || lower.contains("bad_record_mac")
        || lower.contains("ssl_alert")
        || lower.contains("tls_alert")
        || lower.contains("tls_alert_internal_error")
        || lower.contains("[ssl:");

    // Common patterns from OpenAI, Anthropic, and Google APIs
    if lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
    {
        ErrorCategory::RateLimit {
            retry_after_secs: None,
        }
    } else if lower.contains("authentication")
        || lower.contains("invalid api key")
        || lower.contains("unauthorized")
        || lower.contains("401")
    {
        ErrorCategory::AuthFailed
    } else if lower.contains("context_length_exceeded")
        || lower.contains("context length")
        || lower.contains("token limit")
        || lower.contains("maximum context")
    {
        ErrorCategory::ContextTooLong
    } else if lower.contains("overloaded")
        || lower.contains("capacity")
        || lower.contains("503")
        || lower.contains("service unavailable")
    {
        ErrorCategory::ModelOverloaded
    } else if has_ssl_transient {
        ErrorCategory::Timeout
    } else if lower.contains("timeout") || lower.contains("timed out") {
        ErrorCategory::Timeout
    } else if lower.contains("invalid") || lower.contains("bad request") || lower.contains("400") {
        ErrorCategory::InvalidRequest
    } else if lower.contains("500") || lower.contains("internal server") {
        ErrorCategory::ServerError { status_code: 500 }
    } else if lower.contains("502") || lower.contains("bad gateway") {
        ErrorCategory::ServerError { status_code: 502 }
    } else {
        ErrorCategory::Unknown
    }
}

/// Heuristically classify a gateway error message string.
fn classify_gateway_message(msg: &str) -> ErrorCategory {
    let lower = msg.to_lowercase();
    if lower.contains("connection") || lower.contains("network") || lower.contains("dns") {
        ErrorCategory::NetworkError
    } else if lower.contains("timeout") || lower.contains("timed out") {
        ErrorCategory::Timeout
    } else {
        ErrorCategory::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_rate_limited() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::LlmApi("rate_limit exceeded".into());
        let cat = classifier.classify(&err);
        assert_eq!(
            cat,
            ErrorCategory::RateLimit {
                retry_after_secs: None
            }
        );
    }

    #[test]
    fn test_classify_auth_failed() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::AuthFailed("bad key".into());
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::AuthFailed);
    }

    #[test]
    fn test_classify_context_too_long() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::ContextTooLong;
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::ContextTooLong);
    }

    #[test]
    fn test_classify_timeout() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::Timeout("request timed out".into());
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::Timeout);
    }

    #[test]
    fn test_retry_strategy_rate_limit() {
        let classifier = ErrorClassifier::new();
        let cat = ErrorCategory::RateLimit {
            retry_after_secs: Some(10),
        };
        let strat = classifier.recommend_strategy(&cat);
        assert_eq!(
            strat,
            RetryStrategy::RetryWithBackoff {
                max_retries: 3,
                base_delay_secs: 10,
            }
        );
    }

    #[test]
    fn test_retry_strategy_auth() {
        let classifier = ErrorClassifier::new();
        let cat = ErrorCategory::AuthFailed;
        assert_eq!(classifier.recommend_strategy(&cat), RetryStrategy::NoRetry);
    }

    #[test]
    fn test_retry_strategy_context_too_long() {
        let classifier = ErrorClassifier::new();
        let cat = ErrorCategory::ContextTooLong;
        assert_eq!(
            classifier.recommend_strategy(&cat),
            RetryStrategy::UseFallbackModel
        );
    }

    #[test]
    fn test_classify_llm_api_overloaded() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::LlmApi("The model is overloaded".into());
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::ModelOverloaded);
    }

    #[test]
    fn test_classify_llm_api_500() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::LlmApi("500 internal server error".into());
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::ServerError { status_code: 500 });
    }

    #[test]
    fn test_classify_llm_api_ssl_bad_record_mac_as_timeout() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::LlmApi(
            "[SSL: BAD_RECORD_MAC] sslv3 alert bad record mac (_ssl.c:2580)".into(),
        );
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::Timeout);
    }

    #[test]
    fn test_classify_llm_api_ssl_token_form_as_timeout() {
        let classifier = ErrorClassifier::new();
        let err = AgentError::LlmApi("ERR_SSL_SSL/TLS_ALERT_BAD_RECORD_MAC during stream".into());
        let cat = classifier.classify(&err);
        assert_eq!(cat, ErrorCategory::Timeout);
    }
}
