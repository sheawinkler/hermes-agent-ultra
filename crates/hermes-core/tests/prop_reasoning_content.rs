//! Property 13: ReasoningContent multi-format parsing
//! **Validates: Requirement 2.7**
//!
//! For any valid reasoning content value (plain string, object with text field,
//! array of objects with text fields), ReasoningContent::from_value correctly
//! parses and extracts the text content.

use proptest::prelude::*;

use hermes_core::{ReasoningContent, ReasoningFormat};

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

/// Non-empty text for reasoning content.
fn arb_reasoning_text() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 .,!?]{1,100}"
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// Plain string values should parse as Simple format.
    #[test]
    fn prop_string_format_parses(text in arb_reasoning_text()) {
        let value = serde_json::Value::String(text.clone());
        let result = ReasoningContent::from_value(&value);

        prop_assert!(result.is_some(),
            "from_value should succeed for string '{}'", text);

        let rc = result.unwrap();
        prop_assert_eq!(&rc.text, &text);
        prop_assert_eq!(rc.format, ReasoningFormat::Simple);
    }

    /// Object with "text" field should parse as Details format.
    #[test]
    fn prop_object_format_parses(text in arb_reasoning_text()) {
        let value = serde_json::json!({ "text": text });
        let result = ReasoningContent::from_value(&value);

        prop_assert!(result.is_some(),
            "from_value should succeed for object with text '{}'", text);

        let rc = result.unwrap();
        prop_assert_eq!(&rc.text, &text);
        prop_assert_eq!(rc.format, ReasoningFormat::Details);
    }

    /// Array of objects with "text" fields should parse as Details format.
    #[test]
    fn prop_array_format_parses(
        texts in proptest::collection::vec(arb_reasoning_text(), 1..5)
    ) {
        let arr: Vec<serde_json::Value> = texts
            .iter()
            .map(|t| serde_json::json!({ "text": t }))
            .collect();
        let value = serde_json::Value::Array(arr);
        let result = ReasoningContent::from_value(&value);

        prop_assert!(result.is_some(),
            "from_value should succeed for array of {} items", texts.len());

        let rc = result.unwrap();
        let expected = texts.join("\n");
        prop_assert_eq!(&rc.text, &expected);
        prop_assert_eq!(rc.format, ReasoningFormat::Details);
    }

    /// Null, bool, and number values should return None.
    #[test]
    fn prop_invalid_types_return_none(
        val in prop_oneof![
            Just(serde_json::Value::Null),
            proptest::bool::ANY.prop_map(serde_json::Value::Bool),
            (0i64..1000).prop_map(|n| serde_json::json!(n)),
        ]
    ) {
        let result = ReasoningContent::from_value(&val);
        prop_assert!(result.is_none(),
            "from_value should return None for {:?}", val);
    }
}
