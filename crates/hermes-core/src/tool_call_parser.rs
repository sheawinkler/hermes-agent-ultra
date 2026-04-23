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

impl ToolCallParser for HermesToolCallParser {
    fn parse(&self, content: &str) -> Result<Vec<ToolCall>, AgentError> {
        let mut calls = Vec::new();

        let func_calls_re = Regex::new(r"(?s)<function_calls>(.*?)</function_calls>").unwrap();
        let invoke_re = Regex::new(r#"(?s)<invoke\s+name="([^"]+)">(.*?)</invoke>"#).unwrap();
        let param_re = Regex::new(r#"<parameter\s+name="([^"]+)">(.*?)</parameter>"#).unwrap();

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

        // Also try ```tool_call blocks if no XML calls were found
        if calls.is_empty() {
            let tool_call_re = Regex::new(r"(?s)```tool_call\s*\n(.*?)\n```").unwrap();
            for caps in tool_call_re.captures_iter(content) {
                let raw = caps.get(1).unwrap().as_str().trim();
                let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
                    AgentError::InvalidToolCall(format!("Invalid JSON in tool_call block: {}", e))
                })?;

                let name = parsed
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        AgentError::InvalidToolCall(
                            "Missing 'name' field in tool_call block".to_string(),
                        )
                    })?
                    .to_string();

                let arguments = parsed
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()))
                    .to_string();

                calls.push(ToolCall {
                    id: next_call_id(),
                    function: FunctionCall { name, arguments },
                    extra_content: None,
                });
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
}
