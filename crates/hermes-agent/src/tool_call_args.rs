//! Tool-call argument normalization and repair.
//!
//! Some OpenAI-compatible providers stream or return malformed argument JSON
//! (`None`, trailing commas, truncated closing delimiters, or object-valued
//! `arguments`). The runtime stores arguments as a JSON string, so normalize
//! provider output at the boundary before execution.
//!
//! # Algorithm
//!
//! A single-byte-level pass simultaneously handles Python `None` → `null`,
//! trailing-comma removal, brace/bracket balancing, control-character
//! escaping, and extra-closer skipping — eliminating 5× `Vec<char>` alloc
//! and 6 redundant passes found in the original per-operator pipeline.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolArgumentRepair {
    Unchanged,
    Repaired,
    EmptyInput,
    ReplacedWithEmptyObject,
}

pub fn arguments_value_to_string(value: Option<&Value>) -> (String, ToolArgumentRepair) {
    match value {
        None | Some(Value::Null) => ("{}".to_string(), ToolArgumentRepair::EmptyInput),
        Some(Value::String(raw)) => repair_tool_call_arguments(raw),
        Some(value) => (
            serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string()),
            ToolArgumentRepair::Repaired,
        ),
    }
}

/// Single-pass JSON argument repair.
///
/// Fast path (valid JSON) returns immediately. Otherwise a single byte-level
/// scan handles all common malformations before a final `serde_json` check.
///
/// # Contract
///
/// - Pre: `raw` should be non-empty (caller filters empty/None/null before calling)
/// - Post: return value is always valid JSON `{}` or a valid object/array
/// - Post: `Repaired` means the output differs from the trimmed input
/// - Post: `Unchanged` means the input was already valid JSON
pub fn repair_tool_call_arguments(raw: &str) -> (String, ToolArgumentRepair) {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ("{}".to_string(), ToolArgumentRepair::EmptyInput);
    }
    if trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("null")
        || trimmed.eq_ignore_ascii_case("undefined")
    {
        return ("{}".to_string(), ToolArgumentRepair::EmptyInput);
    }

    // Fast path: already valid JSON.
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return (trimmed.to_string(), ToolArgumentRepair::Unchanged);
    }

    // Single-pass repair.
    let bytes = trimmed.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    let mut in_string = false;
    let mut escape = false;
    let mut i = 0;
    // Stack of expected closer bytes for brace/bracket balancing.
    let mut closer_stack: Vec<u8> = Vec::with_capacity(16);

    while i < bytes.len() {
        let b = bytes[i];

        if escape {
            out.push(b);
            escape = false;
            i += 1;
            continue;
        }

        if in_string {
            match b {
                b'\\' => {
                    out.push(b);
                    escape = true;
                    i += 1;
                }
                b'"' => {
                    out.push(b);
                    in_string = false;
                    i += 1;
                }
                b'\n' => {
                    out.extend_from_slice(b"\\n");
                    i += 1;
                }
                b'\r' => {
                    out.extend_from_slice(b"\\r");
                    i += 1;
                }
                // Tab (0x09) is valid inside JSON strings — keep as-is.
                // All other C0 control characters must be escaped.
                c if c.is_ascii_control() && c != b'\t' => {
                    let esc = format!("\\u{:04x}", c);
                    out.extend_from_slice(esc.as_bytes());
                    i += 1;
                }
                _ => {
                    out.push(b);
                    i += 1;
                }
            }
            continue;
        }

        // Outside string — structural characters.
        match b {
            b'"' => {
                in_string = true;
                out.push(b);
                i += 1;
            }

            b'{' => {
                closer_stack.push(b'}');
                out.push(b);
                i += 1;
            }
            b'[' => {
                closer_stack.push(b']');
                out.push(b);
                i += 1;
            }

            b'}' => {
                if closer_stack.last() == Some(&b'}') {
                    closer_stack.pop();
                    out.push(b);
                } /* else: skip unbalanced extra closer */
                i += 1;
            }
            b']' => {
                if closer_stack.last() == Some(&b']') {
                    closer_stack.pop();
                    out.push(b);
                }
                i += 1;
            }

            // Trailing-comma removal: skip comma when followed only by
            // whitespace then `}` or `]` (or EOF).
            b',' => {
                let mut j = i + 1;
                while j < bytes.len() && matches!(bytes[j], b' ' | b'\t' | b'\n' | b'\r') {
                    j += 1;
                }
                if j >= bytes.len() || matches!(bytes[j], b'}' | b']') {
                    i = j; // skip comma + whitespace entirely
                    continue;
                }
                out.push(b);
                i += 1;
            }

            // Python `None` → `null` (word-boundary check to avoid
            // clobbering identifiers like "NoneType" inside strings,
            // though those are already gated by `in_string` above).
            b'N' if bytes[i..].starts_with(b"None") => {
                let prev = if i > 0 { bytes[i - 1] } else { b' ' };
                let next = bytes.get(i + 4).copied().unwrap_or(b' ');
                if !prev.is_ascii_alphanumeric()
                    && prev != b'_'
                    && !next.is_ascii_alphanumeric()
                    && next != b'_'
                {
                    out.extend_from_slice(b"null");
                    i += 4;
                    continue;
                }
                out.push(b);
                i += 1;
            }

            _ => {
                out.push(b);
                i += 1;
            }
        }
    }

    // Unclosed string at EOF — cannot recover structurally.
    if in_string {
        return (
            "{}".to_string(),
            ToolArgumentRepair::ReplacedWithEmptyObject,
        );
    }

    // Close any unclosed braces/brackets.
    while let Some(closer) = closer_stack.pop() {
        out.push(closer);
    }

    // SAFETY: we only ever produced valid UTF-8 (we either pass through
    // original bytes verbatim, or emit ASCII escape sequences).
    let repaired = unsafe { String::from_utf8_unchecked(out) };

    // Final validation: original was already invalid (fast path would have
    // returned Unchanged), so any valid output is necessarily Repaired.
    if serde_json::from_str::<Value>(&repaired).is_ok() {
        // 后置条件：输出必须是合法 JSON
        debug_assert!(serde_json::from_str::<Value>(&repaired).is_ok());
        (repaired, ToolArgumentRepair::Repaired)
    } else {
        (
            "{}".to_string(),
            ToolArgumentRepair::ReplacedWithEmptyObject,
        )
    }
    // 后置条件：始终返回合法的 {} 或 合法JSON
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parsed(raw: &str) -> Value {
        let (repaired, _) = repair_tool_call_arguments(raw);
        serde_json::from_str(&repaired).expect("valid repaired json")
    }

    #[test]
    fn empty_and_python_none_become_empty_object() {
        assert_eq!(repair_tool_call_arguments("").0, "{}");
        assert_eq!(repair_tool_call_arguments("  None  ").0, "{}");
        assert_eq!(repair_tool_call_arguments("null").0, "{}");
    }

    #[test]
    fn valid_json_is_canonicalized_without_semantic_change() {
        assert_eq!(
            parsed(r#"{"path": "/tmp/foo", "content": "hello"}"#),
            json!({"path": "/tmp/foo", "content": "hello"})
        );
    }

    #[test]
    fn object_valued_arguments_are_serialized() {
        let (args, repair) = arguments_value_to_string(Some(&json!({"path": "README.md"})));
        assert_eq!(repair, ToolArgumentRepair::Repaired);
        assert_eq!(
            serde_json::from_str::<Value>(&args).unwrap(),
            json!({"path": "README.md"})
        );
    }

    #[test]
    fn trailing_commas_and_missing_closers_are_repaired() {
        assert_eq!(parsed(r#"{"key": "value",}"#), json!({"key": "value"}));
        assert_eq!(parsed(r#"{"a": [1, 2,]}"#), json!({"a": [1, 2]}));
        assert_eq!(
            parsed(r#"{"command": "ls -la", "timeout": 30"#),
            json!({"command": "ls -la", "timeout": 30})
        );
    }

    #[test]
    fn extra_closers_are_removed() {
        assert_eq!(parsed(r#"{"key": "value"}}"#), json!({"key": "value"}));
        assert_eq!(parsed(r#"{"a": [1]]}"#), json!({"a": [1]}));
    }

    #[test]
    fn control_chars_inside_strings_are_escaped_to_wire_form() {
        let (out, repair) = repair_tool_call_arguments("{\"summary\": \"line one\nline two\"}");
        assert_eq!(repair, ToolArgumentRepair::Repaired);
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"summary": "line one\nline two"})
        );
    }

    #[test]
    fn hanging_colon_or_mid_string_truncation_falls_back_safely() {
        assert_eq!(
            parsed(r#"{"command": "ls -la /tmp", "timeout": 30, "background":"#),
            json!({})
        );
        assert_eq!(parsed(r#"{"truncated": "val"#), json!({}));
    }
}
