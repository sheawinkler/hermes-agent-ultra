//! Bounded invariant coverage: invalid JSON tool call returns error
//! **Validates: Requirement 20.4**
//!
//! For malformed JSON embedded in ```tool_call``` tags, parse_tool_calls
//! returns InvalidToolCall error rather than panicking.

use hermes_core::{parse_tool_calls, AgentError};

fn invalid_json_cases() -> &'static [&'static str] {
    &[
        "plain text invalid",
        "{bad json!!!}",
        r#"{"key": }"#,
        r#"{"unclosed"#,
        "not json at all",
        "{,,,}",
        "[[[",
        "abc123!@#",
        r#"{"arguments": {"unterminated": true"#,
    ]
}

#[test]
fn invalid_json_in_tool_call_returns_error() {
    for invalid in invalid_json_cases() {
        let content = format!("Some text\n```tool_call\n{invalid}\n```\n");
        let result = parse_tool_calls(&content);

        assert!(
            result.is_err(),
            "Expected error for invalid JSON '{}', got Ok({:?})",
            invalid,
            result.unwrap()
        );

        match result.unwrap_err() {
            AgentError::InvalidToolCall(msg) => {
                assert!(
                    msg.contains("Invalid JSON") || msg.contains("Missing"),
                    "Error message should mention invalid JSON, got: {}",
                    msg
                );
            }
            other => panic!("Expected InvalidToolCall error, got: {other:?}"),
        }
    }
}
