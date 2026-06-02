//! Tool-call argument normalization and repair.
//!
//! Some OpenAI-compatible providers stream or return malformed argument JSON
//! (`None`, trailing commas, truncated closing delimiters, or object-valued
//! `arguments`). The runtime stores arguments as a JSON string, so normalize
//! provider output at the boundary before execution.

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

    if let Some(canonical) = parse_canonical_json(trimmed) {
        return (canonical, ToolArgumentRepair::Unchanged);
    }

    let control_escaped = escape_control_chars_in_strings(trimmed);
    if control_escaped != trimmed {
        if let Some(canonical) = parse_canonical_json(&control_escaped) {
            return (canonical, ToolArgumentRepair::Repaired);
        }
    }

    let mut candidate = trimmed.to_string();
    replace_python_none_literals(&mut candidate);
    candidate = strip_trailing_commas(&candidate);
    candidate = balance_json_delimiters(&candidate);
    candidate = strip_trailing_commas(&candidate);

    if let Some(canonical) = parse_canonical_json(&candidate) {
        return (canonical, ToolArgumentRepair::Repaired);
    }

    let escaped = escape_control_chars_in_strings(&candidate);
    if let Some(canonical) = parse_canonical_json(&escaped) {
        return (canonical, ToolArgumentRepair::Repaired);
    }

    (
        "{}".to_string(),
        ToolArgumentRepair::ReplacedWithEmptyObject,
    )
}

fn parse_canonical_json(raw: &str) -> Option<String> {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| serde_json::to_string(&value).ok())
}

fn replace_python_none_literals(candidate: &mut String) {
    let mut out = String::with_capacity(candidate.len());
    let mut in_string = false;
    let mut escape = false;
    let chars: Vec<char> = candidate.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if starts_word_at(&chars, i, "None") {
            out.push_str("null");
            i += 4;
            continue;
        }
        out.push(ch);
        i += 1;
    }
    *candidate = out;
}

fn starts_word_at(chars: &[char], start: usize, word: &str) -> bool {
    let word_chars: Vec<char> = word.chars().collect();
    if start + word_chars.len() > chars.len() {
        return false;
    }
    for (idx, expected) in word_chars.iter().enumerate() {
        if chars[start + idx] != *expected {
            return false;
        }
    }
    let prev = start
        .checked_sub(1)
        .and_then(|i| chars.get(i))
        .copied()
        .unwrap_or(' ');
    let next = chars.get(start + word_chars.len()).copied().unwrap_or(' ');
    !prev.is_ascii_alphanumeric() && prev != '_' && !next.is_ascii_alphanumeric() && next != '_'
}

fn strip_trailing_commas(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escape = false;
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if ch == '"' {
            in_string = true;
            out.push(ch);
            i += 1;
            continue;
        }
        if ch == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j == chars.len() || matches!(chars.get(j), Some('}' | ']')) {
                i += 1;
                continue;
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

fn balance_json_delimiters(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 8);
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;

    for ch in raw.chars() {
        if in_string {
            out.push(ch);
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
            }
            '{' | '[' => {
                stack.push(ch);
                out.push(ch);
            }
            '}' => {
                if matches!(stack.last(), Some('{')) {
                    stack.pop();
                    out.push(ch);
                }
            }
            ']' => {
                if matches!(stack.last(), Some('[')) {
                    stack.pop();
                    out.push(ch);
                }
            }
            _ => out.push(ch),
        }
    }

    if in_string {
        return raw.to_string();
    }

    while let Some(ch) = stack.pop() {
        out.push(match ch {
            '{' => '}',
            '[' => ']',
            _ => unreachable!(),
        });
    }
    out
}

fn escape_control_chars_in_strings(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut in_string = false;
    let mut escape = false;
    for ch in raw.chars() {
        if in_string {
            if escape {
                out.push(ch);
                escape = false;
                continue;
            }
            match ch {
                '\\' => {
                    out.push(ch);
                    escape = true;
                }
                '"' => {
                    out.push(ch);
                    in_string = false;
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                _ => out.push(ch),
            }
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
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
