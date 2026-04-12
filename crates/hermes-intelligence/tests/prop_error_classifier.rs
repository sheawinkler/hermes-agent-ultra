//! Property 10: Error classifier consistency
//! **Validates: Requirement 16.2**
//!
//! For any AgentError, classify produces a valid ErrorCategory, and
//! recommend_strategy returns a valid RetryStrategy.
//! Specifically: AuthFailed always maps to NoRetry.

use proptest::prelude::*;

use hermes_core::AgentError;
use hermes_intelligence::{ErrorCategory, ErrorClassifier, RetryStrategy};

// ---------------------------------------------------------------------------
// Strategy for AgentError
// ---------------------------------------------------------------------------

fn arb_agent_error() -> impl Strategy<Value = AgentError> {
    let short_text = ".{1,64}";
    prop_oneof![
        short_text.clone().prop_map(AgentError::LlmApi),
        short_text.clone().prop_map(AgentError::ToolExecution),
        short_text.clone().prop_map(AgentError::Config),
        short_text.clone().prop_map(AgentError::Gateway),
        short_text.clone().prop_map(AgentError::Timeout),
        Just(AgentError::MaxTurnsExceeded),
        short_text.clone().prop_map(AgentError::InvalidToolCall),
        Just(AgentError::ContextTooLong),
        proptest::option::of(0u64..3600)
            .prop_map(|retry_after_secs| AgentError::RateLimited { retry_after_secs }),
        short_text.clone().prop_map(AgentError::AuthFailed),
        short_text.clone().prop_map(AgentError::Io),
        proptest::option::of(".{1,64}").prop_map(|message| AgentError::Interrupted { message }),
    ]
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_classify_produces_valid_category_and_strategy(error in arb_agent_error()) {
        let classifier = ErrorClassifier::new();
        let category = classifier.classify(&error);
        let strategy = classifier.recommend_strategy(&category);

        // The category and strategy should be well-formed (no panic = pass)
        // Additionally verify the AuthFailed invariant
        if matches!(error, AgentError::AuthFailed(_)) {
            prop_assert_eq!(
                &category,
                &ErrorCategory::AuthFailed,
                "AuthFailed error should classify as AuthFailed"
            );
        }

        if category == ErrorCategory::AuthFailed {
            prop_assert_eq!(
                strategy,
                RetryStrategy::NoRetry,
                "AuthFailed should always map to NoRetry"
            );
        }
    }

    #[test]
    fn prop_context_too_long_uses_fallback(
        _dummy in 0..1u8
    ) {
        let classifier = ErrorClassifier::new();
        let error = AgentError::ContextTooLong;
        let category = classifier.classify(&error);
        let strategy = classifier.recommend_strategy(&category);

        prop_assert_eq!(category, ErrorCategory::ContextTooLong);
        prop_assert_eq!(strategy, RetryStrategy::UseFallbackModel);
    }
}
