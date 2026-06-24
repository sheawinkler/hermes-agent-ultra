use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::errors::AgentError;
use crate::types::{FunctionCall, ToolCall};

// ---------------------------------------------------------------------------
// ToolCallParser trait
// ---------------------------------------------------------------------------

/// Trait for extensible tool call parsers.
pub trait ToolCallParser: Send + Sync {
    /// Parse tool calls from raw LLM text content.
    fn parse(&self, content: &str) -> Result<Vec<ToolCall>, AgentError>;

    /// Clone this parser into a boxed value.
    fn clone_box(&self) -> Box<dyn ToolCallParser>;
}

// ---------------------------------------------------------------------------
// Hermes XML parser implementation
// ---------------------------------------------------------------------------

/// The default Hermes-format tool call parser.
///
/// Parses `<function_calls>` XML blocks containing `<invoke>` and
/// `<parameter>` elements.
#[derive(Clone)]
pub struct HermesToolCallParser;

static CALL_ID_COUNTER: LazyLock<Mutex<u64>> = LazyLock::new(|| Mutex::new(0));

fn next_call_id() -> String {
    let mut guard = CALL_ID_COUNTER.lock().unwrap();
    *guard += 1;
    format!("call_{}", guard)
}

fn tool_calls_from_json_value(value: &serde_json::Value) -> Vec<ToolCall> {
    fn parse_one(obj: &serde_json::Map<String, serde_json::Value>) -> Option<ToolCall> {
        let name = obj.get("name")?.as_str()?.trim();
        if name.is_empty() {
            return None;
        }
        let arguments = obj
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()))
            .to_string();
        Some(ToolCall {
            id: next_call_id(),
            function: FunctionCall {
                name: name.to_string(),
                arguments,
            },
            extra_content: None,
        })
    }

    match value {
        serde_json::Value::Object(map) => parse_one(map).into_iter().collect(),
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_object().and_then(parse_one))
            .collect(),
        _ => Vec::new(),
    }
}

fn extract_json_string_value_from_key(raw: &str, key_idx: usize) -> Option<String> {
    let after_key = raw.get(key_idx..)?;
    let colon_rel = after_key.find(':')?;
    let mut i = key_idx + colon_rel + 1;
    let bytes = raw.as_bytes();
    while i < raw.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= raw.len() || bytes[i] != b'"' {
        return None;
    }
    i += 1;
    let start = i;
    let mut escaped = false;
    while i < raw.len() {
        let b = bytes[i];
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == b'"' {
            return Some(raw[start..i].to_string());
        }
        i += 1;
    }
    None
}

fn find_balanced_json_object_bounds(raw: &str, open_idx: usize) -> Option<(usize, usize)> {
    let bytes = raw.as_bytes();
    if open_idx >= raw.len() || bytes[open_idx] != b'{' {
        return None;
    }
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (idx, ch) in raw[open_idx..].char_indices() {
        let abs = open_idx + idx;
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth = depth.saturating_add(1),
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((open_idx, abs + ch.len_utf8()));
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_loose_json_tool_calls(raw: &str) -> Vec<ToolCall> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        let strict = tool_calls_from_json_value(&value);
        if !strict.is_empty() {
            return strict;
        }
    }

    let mut calls = Vec::new();
    let mut cursor = 0usize;
    while cursor < raw.len() {
        let Some(name_rel) = raw[cursor..].find("\"name\"") else {
            break;
        };
        let name_key_idx = cursor + name_rel;
        let Some(name) = extract_json_string_value_from_key(raw, name_key_idx) else {
            cursor = name_key_idx + 6;
            continue;
        };
        let Some(args_key_rel) = raw[name_key_idx..].find("\"arguments\"") else {
            cursor = name_key_idx + 6;
            continue;
        };
        let args_key_idx = name_key_idx + args_key_rel;
        let Some(open_rel) = raw[args_key_idx..].find('{') else {
            cursor = args_key_idx + 11;
            continue;
        };
        let open_idx = args_key_idx + open_rel;
        let Some((_, end_idx)) = find_balanced_json_object_bounds(raw, open_idx) else {
            cursor = open_idx + 1;
            continue;
        };
        let args_slice = &raw[open_idx..end_idx];
        let arguments = serde_json::from_str::<serde_json::Value>(args_slice)
            .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
            .to_string();
        calls.push(ToolCall {
            id: next_call_id(),
            function: FunctionCall { name, arguments },
            extra_content: None,
        });
        cursor = end_idx;
    }
    calls
}

impl ToolCallParser for HermesToolCallParser {
    fn parse(&self, content: &str) -> Result<Vec<ToolCall>, AgentError> {
        let mut calls = Vec::new();

        let func_calls_re = Regex::new(r"(?s)<function_calls>(.*?)</function_calls>").unwrap();
        let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
        let param_re = Regex::new(r#"<parameter\s+name="([^"]+)">(.*?)</parameter>"#).unwrap();
        let tool_call_re =
            Regex::new(r#"(?s)<tool_call\s+name=['"]([^'"]+)['"]\s*>(.*?)</tool_call>"#).unwrap();
        let argument_re =
            Regex::new(r#"(?s)<argument\s+name=['"]([^'"]+)['"]\s*>(.*?)</argument>"#).unwrap();
        let arguments_re = Regex::new(r#"(?s)<arguments>\s*(.*?)\s*</arguments>"#).unwrap();
        let tool_use_re = Regex::new(r#"(?s)<tool_use>\s*(.*?)\s*</tool_use>"#).unwrap();
        let tool_use_name_re = Regex::new(r#"(?s)<name>\s*([^<]+?)\s*</name>"#).unwrap();

        for fc_caps in func_calls_re.captures_iter(content) {
            let block = fc_caps.get(1).unwrap().as_str();
            for inv_caps in invoke_re.captures_iter(block) {
                let name = inv_caps.get(1).unwrap().as_str().to_string();
                let params_block = inv_caps.get(2).unwrap().as_str();

                let mut args = serde_json::Map::new();
                for param_caps in param_re.captures_iter(params_block) {
                    let pname = param_caps.get(1).unwrap().as_str().to_string();
                    let pval = param_caps.get(2).unwrap().as_str().trim().to_string();
                    let val: serde_json::Value =
                        serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                    args.insert(pname, val);
                }

                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::Value::Object(args).to_string(),
                    },
                    extra_content: None,
                });
            }
        }

        if calls.is_empty() {
            for call_caps in tool_call_re.captures_iter(content) {
                let name = call_caps.get(1).unwrap().as_str().to_string();
                let args_block = call_caps.get(2).unwrap().as_str();
                let mut args = serde_json::Map::new();
                if let Some(arguments_caps) = arguments_re.captures(args_block) {
                    let raw_value = arguments_caps.get(1).unwrap().as_str().trim();
                    let parsed_value = serde_json::from_str(raw_value)
                        .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
                    let arguments = if let serde_json::Value::Object(map) = parsed_value {
                        serde_json::Value::Object(map)
                    } else {
                        let mut fallback = serde_json::Map::new();
                        fallback.insert("value".to_string(), parsed_value);
                        serde_json::Value::Object(fallback)
                    };
                    calls.push(ToolCall {
                        id: next_call_id(),
                        function: FunctionCall {
                            name,
                            arguments: arguments.to_string(),
                        },
                        extra_content: None,
                    });
                    continue;
                }
                for arg_caps in argument_re.captures_iter(args_block) {
                    let arg_name = arg_caps.get(1).unwrap().as_str().to_string();
                    let raw_value = arg_caps.get(2).unwrap().as_str().trim();
                    let parsed = serde_json::from_str(raw_value)
                        .unwrap_or_else(|_| serde_json::Value::String(raw_value.to_string()));
                    args.insert(arg_name, parsed);
                }
                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::Value::Object(args).to_string(),
                    },
                    extra_content: None,
                });
            }
        }

        if calls.is_empty() {
            for use_caps in tool_use_re.captures_iter(content) {
                let block = use_caps.get(1).unwrap().as_str();
                let Some(name_caps) = tool_use_name_re.captures(block) else {
                    continue;
                };
                let name = name_caps.get(1).unwrap().as_str().trim().to_string();
                if name.is_empty() {
                    continue;
                }
                let arguments = if let Some(arguments_caps) = arguments_re.captures(block) {
                    let raw = arguments_caps.get(1).unwrap().as_str().trim();
                    let parsed_value = serde_json::from_str(raw)
                        .unwrap_or_else(|_| serde_json::Value::String(raw.to_string()));
                    if let serde_json::Value::Object(map) = parsed_value {
                        serde_json::Value::Object(map)
                    } else {
                        let mut fallback = serde_json::Map::new();
                        fallback.insert("value".to_string(), parsed_value);
                        serde_json::Value::Object(fallback)
                    }
                } else {
                    serde_json::Value::Object(serde_json::Map::new())
                };
                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: arguments.to_string(),
                    },
                    extra_content: None,
                });
            }
        }

        if calls.is_empty() {
            let func_shorthand_re = Regex::new(r#"(?s)<function=(\w+)>([\s\S]*?)</function>"#).unwrap();
            let param_shorthand_re =
                Regex::new(r#"(?s)<parameter=(\w+)>([\s\S]*?)</parameter>"#).unwrap();
            for func_caps in func_shorthand_re.captures_iter(content) {
                let name = func_caps.get(1).unwrap().as_str().to_string();
                let body = func_caps.get(2).unwrap().as_str();
                let mut args = serde_json::Map::new();
                for param_caps in param_shorthand_re.captures_iter(body) {
                    let pname = param_caps.get(1).unwrap().as_str().to_string();
                    let pval = param_caps.get(2).unwrap().as_str().trim().to_string();
                    let val: serde_json::Value =
                        serde_json::from_str(&pval).unwrap_or(serde_json::Value::String(pval));
                    args.insert(pname, val);
                }
                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::Value::Object(args).to_string(),
                    },
                    extra_content: None,
                });
            }
        }

        if calls.is_empty() {
            let bare_tool_call_re = Regex::new(r"(?s)<tool_call>\s*(.*?)\s*</tool_call>").unwrap();
            for call_caps in bare_tool_call_re.captures_iter(content) {
                let payload = call_caps.get(1).unwrap().as_str().trim();
                calls.extend(parse_loose_json_tool_calls(payload));
            }
        }

        // Also try ```tool_call blocks if no XML calls were found
        if calls.is_empty() {
            let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n(.*?)\n```").unwrap();
            for caps in tool_call_re.captures_iter(content) {
                let raw = caps.get(1).unwrap().as_str().trim();
                let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
                    AgentError::InvalidToolCall(format!("Invalid JSON in tool_call block: {}", e))
                })?;
                let mut parsed_calls = tool_calls_from_json_value(&parsed);
                if parsed_calls.is_empty() {
                    return Err(AgentError::InvalidToolCall(
                        "Missing 'name' field in tool_call block".to_string(),
                    ));
                }
                calls.append(&mut parsed_calls);
            }
        }

        Ok(calls)
    }

    fn clone_box(&self) -> Box<dyn ToolCallParser> {
        Box::new(self.clone())
    }
}

// ---------------------------------------------------------------------------
// Public parsing functions
// ---------------------------------------------------------------------------

/// Parse all tool calls from LLM text content using the default Hermes parser.
pub fn parse_tool_calls(content: &str) -> Result<Vec<ToolCall>, AgentError> {
    let parser = HermesToolCallParser;
    parser.parse(content)
}

/// Separate plain text from tool calls in LLM output content.
///
/// Returns a tuple of (plain_text, tool_calls).
pub fn separate_text_and_calls(content: &str) -> (String, Vec<ToolCall>) {
    let calls = parse_tool_calls(content).unwrap_or_default();
    let mut result = content.to_string();

    // Remove <function_calls>...</function_calls> blocks
    let func_calls_re = Regex::new(r"(?s)<function_calls>.*?</function_calls>").unwrap();
    result = func_calls_re.replace_all(&result, "").to_string();

    // Remove ```tool_call ... ``` blocks
    let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n.*?\n```").unwrap();
    result = tool_call_re.replace_all(&result, "").to_string();

    // Remove <tool_call ...>...</tool_call> blocks
    let alt_tool_call_re =
        Regex::new(r#"(?s)<tool_call\s+name=['"][^'"]+['"]\s*>.*?</tool_call>"#).unwrap();
    result = alt_tool_call_re.replace_all(&result, "").to_string();

    // Remove <tool_use>...</tool_use> blocks
    let tool_use_re = Regex::new(r"(?s)<tool_use>\s*.*?\s*</tool_use>").unwrap();
    result = tool_use_re.replace_all(&result, "").to_string();

    // Remove bare <tool_call>...</tool_call> blocks
    let bare_tool_call_re = Regex::new(r"(?s)<tool_call>\s*.*?\s*</tool_call>").unwrap();
    result = bare_tool_call_re.replace_all(&result, "").to_string();

    // Remove namespace-prefixed tool_call wrappers (e.g. <seed:tool_call>...</seed:tool_call>)
    let ns_tool_call_re = Regex::new(r#"(?s)<\w+:tool_call[^>]*>.*?</\w+:tool_call>"#).unwrap();
    result = ns_tool_call_re.replace_all(&result, "").to_string();

    // Remove Seed/Llama shorthand: <function=name><parameter=key>val</parameter></function>
    let func_shorthand_re = Regex::new(r#"(?s)<function=\w+>.*?</function>"#).unwrap();
    result = func_shorthand_re.replace_all(&result, "").to_string();
    let param_shorthand_re = Regex::new(r#"(?s)<parameter=\w+>.*?</parameter>"#).unwrap();
    result = param_shorthand_re.replace_all(&result, "").to_string();

    // Remove standalone <invoke name="...">...</invoke> blocks (without <function_calls>)
    let standalone_invoke_re = Regex::new(r#"(?s)<invoke\s+name="[^"]+">.*?</invoke>"#).unwrap();
    result = standalone_invoke_re.replace_all(&result, "").to_string();

    // Remove orphan <parameter name="...">...</parameter> tags left behind
    let parameter_re = Regex::new(r#"<parameter\s+name="[^"]+">.*?</parameter>"#).unwrap();
    result = parameter_re.replace_all(&result, "").to_string();

    // Trim excessive whitespace left behind
    let result = result.trim().to_string();

    (result, calls)
}

// ---------------------------------------------------------------------------
// Extensible parser registry
// ---------------------------------------------------------------------------

static PARSER_REGISTRY: LazyLock<Mutex<HashMap<String, Box<dyn ToolCallParser>>>> =
    LazyLock::new(|| {
        let mut map: HashMap<String, Box<dyn ToolCallParser>> = HashMap::new();
        map.insert("hermes".to_string(), Box::new(HermesToolCallParser));
        Mutex::new(map)
    });

/// Register a custom parser by name.
pub fn register_parser(name: impl Into<String>, parser: Box<dyn ToolCallParser>) {
    let mut registry = PARSER_REGISTRY.lock().unwrap();
    registry.insert(name.into(), parser);
}

/// Retrieve a parser by name from the global registry.
pub fn get_parser(name: &str) -> Option<Box<dyn ToolCallParser>> {
    let registry = PARSER_REGISTRY.lock().unwrap();
    registry.get(name).map(|p| p.clone_box())
}

// ---------------------------------------------------------------------------
// Round-trip formatting
// ---------------------------------------------------------------------------

/// Format a list of tool calls back into the Hermes `<function_calls>` XML format.
///
/// This enables round-trip fidelity: parsing then formatting should be
/// semantically equivalent to the original input.
pub fn format_tool_calls(calls: &[ToolCall]) -> String {
    if calls.is_empty() {
        return String::new();
    }

    let mut buf = String::from("<function_calls>\n");
    for tc in calls {
        buf.push_str(&format!("<invoke name=\"{}\">\n", tc.function.name));
        let args: serde_json::Value =
            serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::Value::Null);
        if let serde_json::Value::Object(map) = &args {
            for (key, val) in map {
                let val_str = match val {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                buf.push_str(&format!(
                    "<parameter name=\"{}\">{}</parameter>\n",
                    key, val_str
                ));
            }
        }
        buf.push_str("</invoke>\n");
    }
    buf.push_str("</function_calls>");
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hermes_xml_format() {
        let content = r#"
I'll look that up for you.

<function_calls>
<invoke name="search">
<parameter name="query">rust async traits</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "search");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["query"], "rust async traits");
    }

    #[test]
    fn test_parse_tool_call_json_format() {
        let content = r#"
Let me run that.
```tool_call
{"name": "calculator", "arguments": {"expr": "2+2"}}
```
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "calculator");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["expr"], "2+2");
    }

    #[test]
    fn test_invalid_json_in_tool_call_tags() {
        let content = r#"
```tool_call
{bad json!!!}
```
"#;
        let result = parse_tool_calls(content);
        assert!(result.is_err());
        match result.unwrap_err() {
            AgentError::InvalidToolCall(msg) => {
                assert!(msg.contains("Invalid JSON"));
            }
            other => panic!("expected InvalidToolCall, got {:?}", other),
        }
    }

    #[test]
    fn test_separate_text_and_calls() {
        let content = r#"
Hello! Let me search for that.

<function_calls>
<invoke name="search">
<parameter name="query">test</parameter>
</invoke>
</function_calls>
"#;
        let (text, calls) = separate_text_and_calls(content);
        assert!(!calls.is_empty());
        assert!(!text.contains("function_calls"));
        assert!(text.contains("Hello"));
    }

    #[test]
    fn test_no_tool_calls() {
        let content = "Just a plain message.";
        let calls = parse_tool_calls(content).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_tool_call_xml_variant() {
        let content = r#"
<tool_call name="skill_view">
<argument name="skill">contextlattice-master-router</argument>
</tool_call>
<tool_call name="terminal">
<argument name="command">"pwd"</argument>
</tool_call>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "skill_view");
        let args0: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args0["skill"], "contextlattice-master-router");
        assert_eq!(calls[1].function.name, "terminal");
        let args1: serde_json::Value = serde_json::from_str(&calls[1].function.arguments).unwrap();
        assert_eq!(args1["command"], "pwd");
    }

    #[test]
    fn test_parse_tool_call_xml_arguments_payload_variant() {
        let content = r#"
<tool_call name="contextlattice_context_pack">
<arguments>{"query":"repo state","max_items":20}</arguments>
</tool_call>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "contextlattice_context_pack");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["query"], "repo state");
        assert_eq!(args["max_items"], 20);
    }

    #[test]
    fn test_parse_tool_use_name_arguments_variant() {
        let content = r#"
<tool_use>
<name>shell_exec</name>
<arguments>{"cmd":"pwd","timeout_ms":10000}</arguments>
</tool_use>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "shell_exec");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["cmd"], "pwd");
        assert_eq!(args["timeout_ms"], 10000);
    }

    #[test]
    fn test_parse_bare_tool_call_json_payload() {
        let content = r#"
Proceeding.
<tool_call>
{"name":"terminal","arguments":{"command":"pwd"}}
</tool_call>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "terminal");
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        assert_eq!(args["command"], "pwd");
    }

    #[test]
    fn test_parse_tool_call_code_fence_json_array_payload() {
        let content = r#"
```tool_call
[{"name":"terminal","arguments":{"command":"pwd"}},{"name":"skill_view","arguments":{"skill":"solana"}}]
```
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "terminal");
        assert_eq!(calls[1].function.name, "skill_view");
    }

    #[test]
    fn test_separate_text_and_calls_removes_tool_call_xml_variant() {
        let content = r#"
Proceeding with discovery now.
<tool_call name="skill_view">
<argument name="skill">contextlattice-local-runtime-check</argument>
</tool_call>
"#;
        let (text, calls) = separate_text_and_calls(content);
        assert_eq!(calls.len(), 1);
        assert!(!text.contains("<tool_call"));
        assert!(text.contains("Proceeding with discovery now."));
    }

    #[test]
    fn test_separate_text_and_calls_removes_bare_tool_call_block() {
        let content = r#"
Ready.
<tool_call>
{"name":"terminal","arguments":{"command":"pwd"}}
</tool_call>
"#;
        let (text, calls) = separate_text_and_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(text.trim(), "Ready.");
    }

    #[test]
    fn test_separate_text_and_calls_removes_tool_use_block() {
        let content = r#"
Proceeding now.
<tool_use>
<name>contextlattice_search</name>
<arguments>{"query":"connectivity probe","limit":5}</arguments>
</tool_use>
"#;
        let (text, calls) = separate_text_and_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "contextlattice_search");
        assert_eq!(text.trim(), "Proceeding now.");
        assert!(!text.contains("<tool_use>"));
    }

    #[test]
    fn test_round_trip_format() {
        let calls = vec![ToolCall {
            id: "call_1".to_string(),
            function: FunctionCall {
                name: "read_file".to_string(),
                arguments: r#"{"path":"/tmp/test.txt"}"#.to_string(),
            },
            extra_content: None,
        }];
        let formatted = format_tool_calls(&calls);
        assert!(formatted.contains("<function_calls>"));
        assert!(formatted.contains("<invoke name=\"read_file\">"));
        assert!(formatted.contains("<parameter name=\"path\">"));
    }

    #[test]
    fn test_multiple_tool_calls_xml() {
        let content = r#"
<function_calls>
<invoke name="search">
<parameter name="query">rust</parameter>
</invoke>
<invoke name="read_file">
<parameter name="path">/tmp/a.txt</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].function.name, "search");
        assert_eq!(calls[1].function.name, "read_file");
    }

    #[test]
    fn test_get_parser_registry() {
        let parser = get_parser("hermes");
        assert!(parser.is_some());

        let parser = get_parser("nonexistent");
        assert!(parser.is_none());
    }

    #[test]
    fn test_parameter_json_value_parsing() {
        let content = r#"
<function_calls>
<invoke name="configure">
<parameter name="count">42</parameter>
<parameter name="enabled">true</parameter>
<parameter name="label">hello world</parameter>
</invoke>
</function_calls>
"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&calls[0].function.arguments).unwrap();
        // "42" is not valid JSON object value, so it should be a string
        assert_eq!(args["count"], 42);
        assert_eq!(args["enabled"], true);
        assert_eq!(args["label"], "hello world");
    }

    #[test]
    fn test_parse_standalone_invoke_not_parsed_without_function_calls_wrapper() {
        let content = r#"<invoke name="dispatch_async">
<parameter name="tool_name">send_message</parameter>
<parameter name="params">{"platform": "wecom", "recipient": "self", "message": "hello"}</parameter>
</invoke>"#;
        let calls = parse_tool_calls(content).unwrap();
        // Standalone <invoke> without <function_calls> wrapper should NOT be parsed
        // as a tool call (avoids executing unintended tool dispatches in cron context).
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_minimax_tool_call_not_parsed() {
        let content = r#"<minimax:tool_call>
<invoke name="dispatch_async">
<parameter name="tool_name">send_message</parameter>
<parameter name="params">{"message": "hello"}</parameter>
</invoke>
</minimax:tool_call>"#;
        let calls = parse_tool_calls(content).unwrap();
        // Namespace-prefixed wrappers should NOT be parsed as tool calls.
        assert!(calls.is_empty());
    }

    #[test]
    fn test_parse_seed_shorthand_function_parameter() {
        let content = r#"reasoning
<seed:tool_call>
<function=execute_command><parameter=command>powershell -Command "Get-Date"</parameter></function>
</seed:tool_call>"#;
        let calls = parse_tool_calls(content).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "execute_command");
        assert!(calls[0].function.arguments.contains("Get-Date"));
        let (text, _) = separate_text_and_calls(content);
        assert!(!text.contains("<function="));
        assert!(!text.contains("<seed:tool_call"));
    }

    #[test]
    fn test_separate_text_and_calls_strips_standalone_invoke() {
        let content = r#"I will remind you.
<invoke name="dispatch_async">
<parameter name="tool_name">send_message</parameter>
<parameter name="params">{"message": "time"}</parameter>
</invoke>"#;
        let (text, calls) = separate_text_and_calls(content);
        // Standalone <invoke> without <function_calls> is NOT parsed as a tool call.
        assert!(calls.is_empty());
        // But the XML markup is still stripped from the plain text.
        assert!(text.contains("I will remind you"));
        assert!(!text.contains("<invoke"));
    }

    #[test]
    fn test_separate_text_and_calls_strips_minimax_tool_call_wrapper() {
        let content = r#"Proceeding.
<minimax:tool_call>
<invoke name="dispatch_async">
<parameter name="tool_name">send_message</parameter>
<parameter name="params">{"message": "hi"}</parameter>
</invoke>
</minimax:tool_call>"#;
        let (text, calls) = separate_text_and_calls(content);
        // Namespace-prefixed wrappers are NOT parsed as tool calls.
        assert!(calls.is_empty());
        // But the markup is stripped from the plain text.
        assert_eq!(text.trim(), "Proceeding.");
        assert!(!text.contains("minimax:tool_call"));
        assert!(!text.contains("<invoke"));
        assert!(!text.contains("<parameter"));
    }

    #[test]
    fn test_parse_standalone_invoke_multiple_not_parsed() {
        let content = r#"
<invoke name="search">
<parameter name="query">test</parameter>
</invoke>
<invoke name="read">
<parameter name="path">file.txt</parameter>
</invoke>"#;
        let calls = parse_tool_calls(content).unwrap();
        // Standalone <invoke> blocks without <function_calls> should NOT be parsed.
        assert!(calls.is_empty());
    }
}
