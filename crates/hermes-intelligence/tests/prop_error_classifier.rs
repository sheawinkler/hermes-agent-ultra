//! Bounded invariant coverage: error classifier consistency
//! **Validates: Requirement 16.2**
//!
//! For every AgentError variant, classify produces a valid ErrorCategory and
//! recommend_strategy returns a valid RetryStrategy.

use hermes_core::AgentError;
use hermes_intelligence::{ErrorCategory, ErrorClassifier, RetryStrategy};

fn agent_error_cases() -> Vec<AgentError> {
    vec![
        AgentError::LlmApi("api unavailable".to_string()),
        AgentError::ToolExecution("tool failed".to_string()),
        AgentError::Config("bad config".to_string()),
        AgentError::Gateway("gateway failed".to_string()),
        AgentError::Timeout("deadline exceeded".to_string()),
        AgentError::MaxTurnsExceeded,
        AgentError::InvalidToolCall("missing arguments".to_string()),
        AgentError::ContextTooLong,
        AgentError::RateLimited {
            retry_after_secs: None,
        },
        AgentError::RateLimited {
            retry_after_secs: Some(30),
        },
        AgentError::AuthFailed("bad token".to_string()),
        AgentError::Io("disk error".to_string()),
        AgentError::Interrupted { message: None },
        AgentError::Interrupted {
            message: Some("user cancelled".to_string()),
        },
    ]
}

#[test]
fn classify_produces_valid_category_and_strategy() {
    let classifier = ErrorClassifier::new();

    for error in agent_error_cases() {
        let category = classifier.classify(&error);
        let strategy = classifier.recommend_strategy(&category);

        if matches!(error, AgentError::AuthFailed(_)) {
            assert_eq!(
                &category,
                &ErrorCategory::AuthFailed,
                "AuthFailed error should classify as AuthFailed"
            );
        }

        if category == ErrorCategory::AuthFailed {
            assert_eq!(
                strategy,
                RetryStrategy::NoRetry,
                "AuthFailed should always map to NoRetry"
            );
        }
    }
}

#[test]
fn context_too_long_uses_fallback() {
    let classifier = ErrorClassifier::new();
    let error = AgentError::ContextTooLong;
    let category = classifier.classify(&error);
    let strategy = classifier.recommend_strategy(&category);

    assert_eq!(category, ErrorCategory::ContextTooLong);
    assert_eq!(strategy, RetryStrategy::UseFallbackModel);
}
