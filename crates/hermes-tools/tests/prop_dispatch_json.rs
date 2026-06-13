//! Bounded invariant coverage: tool dispatch always returns valid JSON
//! **Validates: Requirements 4.4, 4.5, 4.6, 3.6, 3.7**
//!
//! For representative tool names and parameters, ToolRegistry::dispatch returns
//! a parseable JSON string even when the tool does not exist.

use hermes_tools::ToolRegistry;

fn params_cases() -> Vec<serde_json::Value> {
    vec![
        serde_json::Value::Null,
        serde_json::json!({}),
        serde_json::json!({"key": "val"}),
        serde_json::json!(42),
        serde_json::json!([1, 2, 3]),
    ]
}

#[test]
fn dispatch_nonexistent_returns_valid_json() {
    let registry = ToolRegistry::new();

    for name in ["a", "missing_tool", "lookup_123", "toolwithlongname"] {
        for params in params_cases() {
            let result = registry.dispatch(name, params);
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
            assert!(parsed.is_ok(), "dispatch returned invalid JSON: {result}");
        }
    }
}

#[test]
fn tool_error_is_valid_json() {
    for msg in ["", "simple error", "quoted \"error\"", "line one\nline two"] {
        let result = ToolRegistry::tool_error(msg);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
        assert!(
            parsed.is_ok(),
            "tool_error returned invalid JSON for msg '{}': {}",
            msg,
            result
        );
    }
}

#[test]
fn tool_result_is_valid_json() {
    for data in ["", "ok", "value with spaces", "line one\nline two"] {
        let result = ToolRegistry::tool_result(data);
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(&result);
        assert!(
            parsed.is_ok(),
            "tool_result returned invalid JSON for data '{}': {}",
            data,
            result
        );
    }
}
