//! Property 12: Invalid JSON tool call returns error
//! **Validates: Requirement 20.4**
//!
//! For any non-JSON string embedded in ```tool_call``` tags,
//! parse_tool_calls returns InvalidToolCall error rather than panicking.

use proptest::prelude::*;

use hermes_core::{parse_tool_calls, AgentError};

// ---------------------------------------------------------------------------
// Strategy for invalid JSON strings
// ---------------------------------------------------------------------------

/// Generate strings that are definitely NOT valid JSON objects.
fn arb_invalid_json() -> impl Strategy<Value = String> {
    prop_oneof![
        // Random text that can't be JSON
        "[a-zA-Z ]{5,50}".prop_map(|s| s),
        // Broken JSON-like strings
        Just("{bad json!!!}".to_string()),
        Just("{\"key\": }".to_string()),
        Just("{\"unclosed".to_string()),
        Just("not json at all".to_string()),
        Just("{,,,}".to_string()),
        Just("[[[".to_string()),
        // Random alphanumeric
        "[a-z0-9!@#$%^&*()]{3,30}".prop_map(|s| s),
    ]
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_invalid_json_in_tool_call_returns_error(invalid in arb_invalid_json()) {
        let content = format!("Some text\n```tool_call\n{invalid}\n```\n");
        let result = parse_tool_calls(&content);

        prop_assert!(result.is_err(),
            "Expected error for invalid JSON '{}', got Ok({:?})",
            invalid, result.unwrap());

        match result.unwrap_err() {
            AgentError::InvalidToolCall(msg) => {
                prop_assert!(msg.contains("Invalid JSON") || msg.contains("Missing"),
                    "Error message should mention invalid JSON, got: {}", msg);
            }
            other => {
                prop_assert!(false,
                    "Expected InvalidToolCall error, got: {:?}", other);
            }
        }
    }
}
