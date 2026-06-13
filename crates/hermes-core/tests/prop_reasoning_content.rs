//! Bounded invariant coverage: ReasoningContent multi-format parsing
//! **Validates: Requirement 2.7**
//!
//! Plain strings, objects with text fields, and arrays of text objects parse
//! correctly while unsupported scalar types return None.

use hermes_core::{ReasoningContent, ReasoningFormat};

fn reasoning_text_cases() -> &'static [&'static str] {
    &[
        "short",
        "Reasoning with punctuation, numbers 123, and spaces.",
        "line one\nline two",
        "Symbols !?., are preserved",
    ]
}

#[test]
fn string_format_parses() {
    for text in reasoning_text_cases() {
        let value = serde_json::Value::String((*text).to_string());
        let rc = ReasoningContent::from_value(&value)
            .unwrap_or_else(|| panic!("from_value should succeed for string '{text}'"));

        assert_eq!(rc.text, *text);
        assert_eq!(rc.format, ReasoningFormat::Simple);
    }
}

#[test]
fn object_format_parses() {
    for text in reasoning_text_cases() {
        let value = serde_json::json!({ "text": text });
        let rc = ReasoningContent::from_value(&value)
            .unwrap_or_else(|| panic!("from_value should succeed for object text '{text}'"));

        assert_eq!(rc.text, *text);
        assert_eq!(rc.format, ReasoningFormat::Details);
    }
}

#[test]
fn array_format_parses() {
    let cases = [
        vec!["first"],
        vec!["first", "second"],
        vec!["alpha", "beta", "gamma", "delta"],
    ];

    for texts in cases {
        let arr: Vec<serde_json::Value> = texts
            .iter()
            .map(|text| serde_json::json!({ "text": text }))
            .collect();
        let value = serde_json::Value::Array(arr);
        let rc = ReasoningContent::from_value(&value)
            .unwrap_or_else(|| panic!("from_value should succeed for {} items", texts.len()));

        assert_eq!(rc.text, texts.join("\n"));
        assert_eq!(rc.format, ReasoningFormat::Details);
    }
}

#[test]
fn invalid_types_return_none() {
    let values = [
        serde_json::Value::Null,
        serde_json::Value::Bool(true),
        serde_json::Value::Bool(false),
        serde_json::json!(0),
        serde_json::json!(999),
    ];

    for value in values {
        assert!(
            ReasoningContent::from_value(&value).is_none(),
            "from_value should return None for {value:?}"
        );
    }
}
