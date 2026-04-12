//! Property 4: Tool dispatch always returns valid JSON
//! **Validates: Requirements 4.4, 4.5, 4.6, 3.6, 3.7**
//!
//! For any tool name and parameters (whether the tool exists or not),
//! ToolRegistry::dispatch returns a parseable JSON string.

use proptest::prelude::*;

use hermes_tools::ToolRegistry;

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Dispatching a non-existent tool should still return valid JSON.
    #[test]
    fn prop_dispatch_nonexistent_returns_valid_json(
        name in "[a-z]{1,20}",
        params_str in prop_oneof![
            Just("null".to_string()),
            Just("{}".to_string()),
            Just(r#"{"key":"val"}"#.to_string()),
            Just("42".to_string()),
            Just(r#"[1,2,3]"#.to_string()),
        ]
    ) {
        let registry = ToolRegistry::new();
        let params: serde_json::Value = serde_json::from_str(&params_str).unwrap();
        let result = registry.dispatch(&name, params);

        // The result must always be valid JSON
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
        prop_assert!(parsed.is_ok(),
            "dispatch returned invalid JSON: {}", result);
    }

    /// The static tool_error helper always returns valid JSON.
    #[test]
    fn prop_tool_error_is_valid_json(msg in ".{0,200}") {
        let result = ToolRegistry::tool_error(&msg);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
        prop_assert!(parsed.is_ok(),
            "tool_error returned invalid JSON for msg '{}': {}", msg, result);
    }

    /// The static tool_result helper always returns valid JSON.
    #[test]
    fn prop_tool_result_is_valid_json(data in "[a-zA-Z0-9 ]{0,200}") {
        let result = ToolRegistry::tool_result(&data);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
        prop_assert!(parsed.is_ok(),
            "tool_result returned invalid JSON for data '{}': {}", data, result);
    }
}
