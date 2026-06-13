//! Bounded invariant coverage: tool call parser roundtrip consistency
//! **Validates: Requirement 20.3**
//!
//! For representative valid Vec<ToolCall>, format_tool_calls ->
//! parse_tool_calls should produce a semantically equivalent list.

use hermes_core::{format_tool_calls, parse_tool_calls, FunctionCall, ToolCall};

fn tool_call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: format!("call_{name}"),
        function: FunctionCall {
            name: name.to_string(),
            arguments: arguments.to_string(),
        },
        extra_content: None,
    }
}

fn tool_call_cases() -> Vec<Vec<ToolCall>> {
    vec![
        vec![tool_call("echo", "{}")],
        vec![tool_call("lookup_1", r#"{"key":"value"}"#)],
        vec![tool_call("score", r#"{"score":42}"#)],
        vec![tool_call("toggle", r#"{"enabled":true}"#)],
        vec![
            tool_call("first", r#"{"a":1}"#),
            tool_call("second", r#"{"b":"two"}"#),
            tool_call("third", r#"{"c":false}"#),
        ],
    ]
}

#[test]
fn tool_call_roundtrip() {
    for calls in tool_call_cases() {
        let formatted = format_tool_calls(&calls);
        let parsed = parse_tool_calls(&formatted).unwrap();

        assert_eq!(
            calls.len(),
            parsed.len(),
            "Length mismatch: expected {}, got {}",
            calls.len(),
            parsed.len()
        );

        for (orig, reparsed) in calls.iter().zip(parsed.iter()) {
            assert_eq!(
                &orig.function.name, &reparsed.function.name,
                "Name mismatch"
            );

            let orig_args: serde_json::Value =
                serde_json::from_str(&orig.function.arguments).unwrap();
            let reparsed_args: serde_json::Value =
                serde_json::from_str(&reparsed.function.arguments).unwrap();
            assert_eq!(
                &orig_args, &reparsed_args,
                "Arguments mismatch for tool '{}'",
                orig.function.name
            );
        }
    }
}

#[test]
fn empty_calls_format_is_empty() {
    let formatted = format_tool_calls(&[]);
    assert!(formatted.is_empty());
}
