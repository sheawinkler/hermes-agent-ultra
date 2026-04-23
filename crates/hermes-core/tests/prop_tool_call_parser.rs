//! Property 2: Tool call parser roundtrip consistency
//! **Validates: Requirement 20.3**
//!
//! For any valid Vec<ToolCall>, format_tool_calls -> parse_tool_calls should
//! produce a semantically equivalent list (same names and arguments, ids may differ).

use proptest::prelude::*;

use hermes_core::{format_tool_calls, parse_tool_calls, FunctionCall, ToolCall};

// ---------------------------------------------------------------------------
// Strategies (local, since test_generators is #[cfg(test)] internal)
// ---------------------------------------------------------------------------

/// Alphanumeric identifier safe for XML attribute values.
fn arb_identifier() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9_]{0,15}"
}

/// Generate a valid JSON object string with simple key-value pairs.
fn arb_json_object() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("{}".to_string()),
        arb_identifier().prop_map(|k| format!(r#"{{"{k}": "value"}}"#)),
        (arb_identifier(), 0i64..1000).prop_map(|(k, v)| format!(r#"{{"{k}": {v}}}"#)),
        (arb_identifier(), proptest::bool::ANY).prop_map(|(k, v)| format!(r#"{{"{k}": {v}}}"#)),
    ]
}

fn arb_tool_call() -> impl Strategy<Value = ToolCall> {
    (arb_identifier(), arb_json_object()).prop_map(|(name, arguments)| ToolCall {
        id: format!("call_{name}"),
        function: FunctionCall { name, arguments },
        extra_content: None,
    })
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_tool_call_roundtrip(calls in proptest::collection::vec(arb_tool_call(), 1..5)) {
        let formatted = format_tool_calls(&calls);
        let parsed = parse_tool_calls(&formatted).unwrap();

        prop_assert_eq!(calls.len(), parsed.len(),
            "Length mismatch: expected {}, got {}", calls.len(), parsed.len());

        for (orig, reparsed) in calls.iter().zip(parsed.iter()) {
            prop_assert_eq!(&orig.function.name, &reparsed.function.name,
                "Name mismatch");

            // Compare arguments as parsed JSON values (key order may differ)
            let orig_args: serde_json::Value =
                serde_json::from_str(&orig.function.arguments).unwrap();
            let reparsed_args: serde_json::Value =
                serde_json::from_str(&reparsed.function.arguments).unwrap();
            prop_assert_eq!(&orig_args, &reparsed_args,
                "Arguments mismatch for tool '{}'", orig.function.name);
        }
    }

    #[test]
    fn prop_empty_calls_format_is_empty(_dummy in 0..1u8) {
        let formatted = format_tool_calls(&[]);
        prop_assert!(formatted.is_empty());
    }
}
