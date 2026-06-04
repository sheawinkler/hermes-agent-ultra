//! Sanitize tool JSON schemas for broad LLM-backend compatibility.
//!
//! Some local inference backends (notably llama.cpp's `json-schema-to-grammar`
//! converter used to build GBNF tool-call parsers) are strict about what JSON
//! Schema shapes they accept. Schemas that OpenAI / Anthropic / most cloud
//! providers silently accept can make llama.cpp fail the entire request.
//!
//! This module walks the final tool schema tree and fixes known-hostile constructs
//! in-place on a deep copy.
//!
//! # Python alignment
//!
//! Corresponds to `hermes-agent/tools/schema_sanitizer.py`.

use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, info};

/// Sanitize a list of tool schemas for compatibility with strict LLM backends.
///
/// Input is an OpenAI-format tool list:
/// `[{"type": "function", "function": {"name": ..., "parameters": {...}}}]`
///
/// Returns a deep copy with each tool's parameter schema sanitized.
pub fn sanitize_tool_schemas(tools: Vec<Value>) -> Vec<Value> {
    if tools.is_empty() {
        return tools;
    }

    tools.into_iter().map(sanitize_single_tool).collect()
}

/// Deep-copy and sanitize a single OpenAI-format tool entry.
fn sanitize_single_tool(tool: Value) -> Value {
    let mut out = tool.clone();

    if let Some(function) = out.get_mut("function")
        && let Some(fn_obj) = function.as_object_mut() {
            // Get or create parameters
            let params = fn_obj.get("parameters").cloned();

            if !params.as_ref().is_some_and(|p| p.is_object()) {
                // Missing / non-object parameters → substitute minimal valid shape
                fn_obj.insert(
                    "parameters".to_string(),
                    json!({"type": "object", "properties": {}}),
                );
                return out;
            }

            let tool_name = fn_obj
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("<tool>");

            // Sanitize recursively
            let mut sanitized = sanitize_node(params.unwrap(), tool_name);

            // Guarantee top-level is an object with properties
            if let Some(obj) = sanitized.as_object_mut() {
                if obj.get("type").and_then(|t| t.as_str()) != Some("object") {
                    obj.insert("type".to_string(), json!("object"));
                }
                if !obj.contains_key("properties") || !obj["properties"].is_object() {
                    obj.insert("properties".to_string(), json!({}));
                }
            } else {
                sanitized = json!({"type": "object", "properties": {}});
            }

            // Collapse nullable unions
            sanitized = strip_nullable_unions(sanitized, true);

            // Strip top-level combinators
            sanitized = strip_top_level_combinators(sanitized, tool_name);

            fn_obj.insert("parameters".to_string(), sanitized);
        }

    out
}

/// Top-level forbidden keys for strict backends (e.g., OpenAI Codex).
const TOP_LEVEL_FORBIDDEN_KEYS: &[&str] = &["allOf", "anyOf", "oneOf", "enum", "not"];

/// Drop combinator keywords from the top-level of a function parameters schema.
///
/// OpenAI's Codex backend rejects requests with combinators at the top level.
fn strip_top_level_combinators(params: Value, path: &str) -> Value {
    if let Some(obj) = params.as_object() {
        let mut out = obj.clone();
        for key in TOP_LEVEL_FORBIDDEN_KEYS {
            if out.remove(*key).is_some() {
                debug!(
                    "schema_sanitizer[{}]: stripped top-level '{}' combinator \
                     from tool parameters (strict-backend compat)",
                    path, key
                );
            }
        }
        Value::Object(out)
    } else {
        params
    }
}

/// Collapse `anyOf` / `oneOf` nullable unions to the non-null branch.
///
/// MCP / Pydantic optional fields commonly arrive as:
/// `{"anyOf": [{"type": "string"}, {"type": "null"}], "default": null}`
///
/// Anthropic's tool input-schema validator rejects the null branch.
/// Tool optionality is already represented by the parent object's `required` array,
/// so we collapse the union to the single non-null variant.
///
/// # Arguments
/// * `schema` - JSON-Schema fragment
/// * `keep_nullable_hint` - If true, set `nullable: true` on the replacement
pub fn strip_nullable_unions(schema: Value, keep_nullable_hint: bool) -> Value {
    match schema {
        Value::Array(arr) => Value::Array(
            arr.into_iter()
                .map(|v| strip_nullable_unions(v, keep_nullable_hint))
                .collect(),
        ),
        Value::Object(obj) => {
            let stripped: HashMap<String, Value> = obj
                .into_iter()
                .map(|(k, v)| (k, strip_nullable_unions(v, keep_nullable_hint)))
                .collect();

            // Try to collapse anyOf/oneOf with null branches
            for key in &["anyOf", "oneOf"] {
                if let Some(Value::Array(variants)) = stripped.get(*key) {
                    let non_null: Vec<&Value> = variants
                        .iter()
                        .filter(|item| {
                            !matches!(
                                item.as_object().and_then(|o| o.get("type")),
                                Some(Value::String(s)) if s == "null"
                            )
                        })
                        .collect();

                    // Only collapse when we dropped a null branch AND exactly one non-null survives
                    if non_null.len() == 1 && non_null.len() != variants.len() {
                        let mut replacement = if let Some(obj) = non_null[0].as_object() {
                            obj.clone()
                        } else {
                            serde_json::Map::new()
                        };

                        if keep_nullable_hint {
                            replacement
                                .entry("nullable".to_string())
                                .or_insert(json!(true));
                        }

                        // Carry over metadata
                        for meta_key in &["title", "description", "default", "examples"] {
                            if let Some(val) = stripped.get(*meta_key)
                                && !replacement.contains_key(*meta_key) {
                                    replacement.insert(meta_key.to_string(), val.clone());
                                }
                        }

                        return strip_nullable_unions(Value::Object(replacement), keep_nullable_hint);
                    }
                }
            }

            Value::Object(stripped.into_iter().collect())
        }
        _ => schema,
    }
}

/// Recursively sanitize a JSON-Schema fragment.
///
/// - Replaces bare-string schema values with `{"type": <value>}`
/// - Injects `properties: {}` into object-typed nodes missing it
/// - Normalizes `type: [X, "null"]` arrays to single `type: X`
/// - Recurses into properties, items, additionalProperties, combinators
fn sanitize_node(node: Value, path: &str) -> Value {
    // Malformed: bare string like "object"
    if let Some(s) = node.as_str() {
        if matches!(
            s,
            "object" | "string" | "number" | "integer" | "boolean" | "array" | "null"
        ) {
            debug!(
                "schema_sanitizer[{}]: replacing bare-string schema '{}' with {{'type': '{}'}}",
                path, s, s
            );
            return if s == "object" {
                json!({"type": "object", "properties": {}})
            } else {
                json!({"type": s})
            };
        }
        // Other stray strings → replace with permissive object
        debug!(
            "schema_sanitizer[{}]: replacing non-schema string '{}' with empty object schema",
            path, s
        );
        return json!({"type": "object", "properties": {}});
    }

    // Handle arrays
    if let Some(arr) = node.as_array() {
        return Value::Array(
            arr.iter()
                .enumerate()
                .map(|(i, item)| sanitize_node(item.clone(), &format!("{}[{}]", path, i)))
                .collect(),
        );
    }

    // Handle objects
    if let Some(obj) = node.as_object() {
        let mut out = serde_json::Map::new();

        for (key, value) in obj {
            // Normalize type: [X, "null"] → type: X
            if key == "type"
                && let Some(arr) = value.as_array() {
                    let non_null: Vec<&Value> = arr
                        .iter()
                        .filter(|t| !matches!(t.as_str(), Some("null")))
                        .collect();

                    if non_null.len() == 1
                        && let Some(s) = non_null[0].as_str() {
                            out.insert("type".to_string(), json!(s));
                            if arr.iter().any(|t| t.as_str() == Some("null")) {
                                out.entry("nullable".to_string()).or_insert(json!(true));
                            }
                            continue;
                        }

                    // Fallback: pick first non-null string type
                    if let Some(first) = non_null
                        .iter()
                        .find_map(|t| t.as_str().filter(|s| *s != "null"))
                    {
                        out.insert("type".to_string(), json!(first));
                        continue;
                    }

                    // All-null or empty → treat as object
                    out.insert("type".to_string(), json!("object"));
                    continue;
                }

            // Recurse into nested schema structures
            if matches!(key.as_str(), "properties" | "$defs" | "definitions")
                && let Some(nested_obj) = value.as_object() {
                    let sanitized: serde_json::Map<_, _> = nested_obj
                        .iter()
                        .map(|(sub_k, sub_v)| {
                            (
                                sub_k.clone(),
                                sanitize_node(sub_v.clone(), &format!("{}.{}.{}", path, key, sub_k)),
                            )
                        })
                        .collect();
                    out.insert(key.clone(), Value::Object(sanitized));
                    continue;
                }

            if matches!(key.as_str(), "items" | "additionalProperties") {
                if value.is_boolean() {
                    // Keep boolean as-is
                    out.insert(key.clone(), value.clone());
                } else {
                    out.insert(
                        key.clone(),
                        sanitize_node(value.clone(), &format!("{}.{}", path, key)),
                    );
                }
                continue;
            }

            if matches!(key.as_str(), "anyOf" | "oneOf" | "allOf")
                && let Some(arr) = value.as_array() {
                    let sanitized: Vec<Value> = arr
                        .iter()
                        .enumerate()
                        .map(|(i, item)| {
                            sanitize_node(item.clone(), &format!("{}.{}[{}]", path, key, i))
                        })
                        .collect();
                    out.insert(key.clone(), Value::Array(sanitized));
                    continue;
                }

            // Don't recurse into required, enum, examples (they're not schemas)
            if matches!(key.as_str(), "required" | "enum" | "examples") {
                out.insert(key.clone(), value.clone());
                continue;
            }

            // Recurse into other dict/list values
            let sanitized = if value.is_object() || value.is_array() {
                sanitize_node(value.clone(), &format!("{}.{}", path, key))
            } else {
                value.clone()
            };
            out.insert(key.clone(), sanitized);
        }

        // Object nodes without properties: inject empty properties
        if out.get("type").and_then(|t| t.as_str()) == Some("object")
            && !out.get("properties").is_some_and(|p| p.is_object())
        {
            out.insert("properties".to_string(), json!({}));
        }

        // Prune required entries that don't exist in properties
        if out.get("type").and_then(|t| t.as_str()) == Some("object")
            && let Some(Value::Array(required)) = out.get("required") {
                let props = out
                    .get("properties")
                    .and_then(|p| p.as_object())
                    .map(|o| o.keys().collect::<Vec<_>>())
                    .unwrap_or_default();

                let valid: Vec<Value> = required
                    .iter()
                    .filter(|r| {
                        r.as_str()
                            .is_some_and(|s| props.iter().any(|p| p.as_str() == s))
                    })
                    .cloned()
                    .collect();

                if valid.is_empty() {
                    out.remove("required");
                } else if valid.len() != required.len() {
                    out.insert("required".to_string(), Value::Array(valid));
                }
            }

        return Value::Object(out);
    }

    // Scalar values pass through unchanged
    node
}

// =============================================================================
// Reactive strip — only invoked when llama.cpp rejects a schema
// =============================================================================

const STRIP_ON_RECOVERY_KEYS: &[&str] = &["pattern", "format"];

/// Strip `pattern` and `format` JSON Schema keywords from tool schemas.
///
/// This is a *reactive* sanitizer invoked only when llama.cpp's
/// `json-schema-to-grammar` converter has rejected a tool schema.
///
/// Returns the number of keywords stripped.
pub fn strip_pattern_and_format(tools: &mut [Value]) -> usize {
    if tools.is_empty() {
        return 0;
    }

    let mut stripped = 0;

    fn walk(node: &mut Value, stripped: &mut usize) {
        match node {
            Value::Object(obj) => {
                // Only strip as a sibling of `type` (i.e., when this is a schema node)
                let is_schema_node = obj.contains_key("type")
                    || obj.contains_key("anyOf")
                    || obj.contains_key("oneOf")
                    || obj.contains_key("allOf");

                if is_schema_node {
                    for key in STRIP_ON_RECOVERY_KEYS {
                        if obj.remove(*key).is_some() {
                            *stripped += 1;
                        }
                    }
                }

                // Recurse into all values
                for value in obj.values_mut() {
                    walk(value, stripped);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    walk(item, stripped);
                }
            }
            _ => {}
        }
    }

    for tool in tools.iter_mut() {
        // OpenAI-format: {"function": {"parameters": {...}}}
        if let Some(function) = tool.get_mut("function")
            && let Some(params) = function.get_mut("parameters") {
                walk(params, &mut stripped);
                continue;
            }

        // Responses-format: {"parameters": {...}}
        if let Some(params) = tool.get_mut("parameters") {
            walk(params, &mut stripped);
        }
    }

    if stripped > 0 {
        info!(
            "schema_sanitizer: stripped {} pattern/format keyword(s) from \
             tool schemas (llama.cpp grammar-parse recovery)",
            stripped
        );
    }

    stripped
}

/// Strip `enum` keywords whose string values contain a forward slash.
///
/// xAI's endpoints compile tool schemas to a grammar that rejects `enum`
/// values containing `/`. This is commonly hit by MCP-derived tools with
/// HuggingFace model IDs.
///
/// Returns the number of enums stripped.
pub fn strip_slash_enum(tools: &mut [Value]) -> usize {
    if tools.is_empty() {
        return 0;
    }

    let mut stripped = 0;

    fn walk(node: &mut Value, stripped: &mut usize) {
        match node {
            Value::Object(obj) => {
                if let Some(Value::Array(enum_val)) = obj.get("enum")
                    && enum_val
                        .iter()
                        .any(|v| v.as_str().is_some_and(|s| s.contains('/')))
                    {
                        obj.remove("enum");
                        *stripped += 1;
                    }

                for value in obj.values_mut() {
                    walk(value, stripped);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    walk(item, stripped);
                }
            }
            _ => {}
        }
    }

    for tool in tools.iter_mut() {
        if let Some(function) = tool.get_mut("function")
            && let Some(params) = function.get_mut("parameters") {
                walk(params, &mut stripped);
                continue;
            }

        if let Some(params) = tool.get_mut("parameters") {
            walk(params, &mut stripped);
        }
    }

    if stripped > 0 {
        info!(
            "schema_sanitizer: stripped {} enum keyword(s) containing '/' \
             from tool schemas (xAI Responses grammar-compile recovery)",
            stripped
        );
    }

    stripped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_bare_string_schema() {
        let input = json!("object");
        let result = sanitize_node(input, "test");
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_sanitize_nullable_type_array() {
        let input = json!({"type": ["string", "null"]});
        let result = sanitize_node(input, "test");
        assert_eq!(result, json!({"type": "string", "nullable": true}));
    }

    #[test]
    fn test_strip_nullable_unions() {
        let input = json!({
            "anyOf": [
                {"type": "string"},
                {"type": "null"}
            ],
            "description": "Optional field"
        });
        let result = strip_nullable_unions(input, true);
        assert_eq!(
            result,
            json!({
                "type": "string",
                "nullable": true,
                "description": "Optional field"
            })
        );
    }

    #[test]
    fn test_inject_properties_for_object() {
        let input = json!({"type": "object"});
        let result = sanitize_node(input, "test");
        assert_eq!(result, json!({"type": "object", "properties": {}}));
    }

    #[test]
    fn test_prune_invalid_required() {
        let input = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "required": ["name", "invalid_field"]
        });
        let result = sanitize_node(input, "test");
        let expected = json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"}
            },
            "required": ["name"]
        });
        assert_eq!(result, expected);
    }

    #[test]
    fn test_strip_pattern_and_format() {
        let mut tools = vec![json!({
            "function": {
                "parameters": {
                    "type": "object",
                    "properties": {
                        "email": {
                            "type": "string",
                            "format": "email",
                            "pattern": "^[a-z]+@[a-z]+\\.[a-z]+$"
                        }
                    }
                }
            }
        })];
        let count = strip_pattern_and_format(&mut tools);
        assert_eq!(count, 2);
        assert!(!tools[0]["function"]["parameters"]["properties"]["email"]
            .as_object()
            .unwrap()
            .contains_key("format"));
    }

    #[test]
    fn test_strip_slash_enum() {
        let mut tools = vec![json!({
            "parameters": {
                "type": "object",
                "properties": {
                    "model": {
                        "type": "string",
                        "enum": ["Qwen/Qwen3.5-0.8B", "openai/gpt-4"]
                    }
                }
            }
        })];
        let count = strip_slash_enum(&mut tools);
        assert_eq!(count, 1);
        assert!(!tools[0]["parameters"]["properties"]["model"]
            .as_object()
            .unwrap()
            .contains_key("enum"));
    }

    #[test]
    fn test_sanitize_full_tool() {
        let input = json!({
            "type": "function",
            "function": {
                "name": "test_tool",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "optional_field": {
                            "anyOf": [
                                {"type": "string"},
                                {"type": "null"}
                            ]
                        }
                    }
                }
            }
        });
        let result = sanitize_single_tool(input);
        let params = &result["function"]["parameters"];
        assert_eq!(params["type"], "object");
        assert!(params["properties"]["optional_field"]["nullable"].as_bool().unwrap_or(false));
    }
}
