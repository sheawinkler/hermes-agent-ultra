//! Golden oracle helpers for `AgentLoop::messages_for_api_call` migration.
//!
//! Future zero-copy implementations must produce byte-identical canonical JSON
//! to the legacy path exercised by `tests/api_message_golden.rs`.

use hermes_core::Message;
use serde_json::Value;

/// Stable JSON representation of API-bound messages for golden diffing.
pub fn canonical_messages_json(messages: &[Message]) -> Value {
    Value::Array(
        messages
            .iter()
            .map(|m| serde_json::to_value(m).expect("Message serializes"))
            .collect(),
    )
}

/// Assert two message slices match under canonical JSON encoding.
pub fn assert_messages_oracle_eq(actual: &[Message], expected: &[Message]) {
    let a = canonical_messages_json(actual);
    let b = canonical_messages_json(expected);
    assert_eq!(
        a, b,
        "API message oracle mismatch\nactual: {a}\nexpected: {b}"
    );
}

/// Assert legacy and candidate builders match (dual-run / 对拍).
pub fn assert_dual_run_eq(legacy: &[Message], candidate: &[Message]) {
    assert_messages_oracle_eq(legacy, candidate);
}
