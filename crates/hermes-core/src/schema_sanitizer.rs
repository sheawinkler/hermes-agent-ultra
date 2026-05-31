use serde_json::{Map, Value};

const TOP_LEVEL_FORBIDDEN_KEYS: &[&str] = &["allOf", "oneOf", "anyOf", "enum", "not"];

pub fn sanitize_tool_schemas(tools: Option<&Value>) -> Option<Value> {
    let mut out = tools?.clone();
    for_each_tool_parameters_mut(&mut out, true, |params| {
        *params = sanitize_tool_parameters(params);
    });
    Some(out)
}

pub fn sanitize_tool_parameters(params: &Value) -> Value {
    let mut root = match params {
        Value::Object(obj) => Value::Object(obj.clone()),
        Value::String(kind) => schema_for_type(kind),
        _ => default_object_schema(),
    };
    sanitize_schema_node(&mut root, true);
    root
}

pub fn strip_pattern_and_format(tools: Option<&mut Value>) -> usize {
    let Some(tools) = tools else {
        return 0;
    };
    let mut stripped = 0;
    for_each_tool_parameters_mut(tools, false, |params| {
        stripped += strip_schema_keywords(params, &["pattern", "format"]);
    });
    stripped
}

pub fn strip_slash_enum(tools: Option<&mut Value>) -> usize {
    let Some(tools) = tools else {
        return 0;
    };
    let mut stripped = 0;
    for_each_tool_parameters_mut(tools, false, |params| {
        stripped += strip_slash_enum_in_schema(params);
    });
    stripped
}

fn default_object_schema() -> Value {
    serde_json::json!({"type": "object", "properties": {}})
}

fn schema_for_type(kind: &str) -> Value {
    let kind = kind.trim();
    if kind == "object" || kind.is_empty() {
        default_object_schema()
    } else {
        serde_json::json!({"type": kind})
    }
}

fn for_each_tool_parameters_mut<F>(tools: &mut Value, insert_missing: bool, mut f: F)
where
    F: FnMut(&mut Value),
{
    let Some(items) = tools.as_array_mut() else {
        return;
    };
    for tool in items {
        if let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) {
            if insert_missing {
                let params = function
                    .entry("parameters".to_string())
                    .or_insert_with(default_object_schema);
                f(params);
            } else if let Some(params) = function.get_mut("parameters") {
                f(params);
            }
            continue;
        }

        if let Some(obj) = tool.as_object_mut() {
            if let Some(params) = obj.get_mut("parameters") {
                f(params);
            }
        }
    }
}

fn sanitize_schema_node(node: &mut Value, top_level: bool) {
    match node {
        Value::String(kind) => {
            *node = schema_for_type(kind);
        }
        Value::Object(_) => {}
        _ if top_level => {
            *node = default_object_schema();
        }
        _ => return,
    }

    let Some(obj) = node.as_object_mut() else {
        return;
    };

    if top_level {
        for key in TOP_LEVEL_FORBIDDEN_KEYS {
            obj.remove(*key);
        }
    }

    collapse_nullable_type_array(obj);
    coerce_object_type(obj);

    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for value in props.values_mut() {
            sanitize_schema_node(value, false);
        }
    }

    if let Some(items) = obj.get_mut("items") {
        sanitize_schema_node(items, false);
    }

    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() || additional.is_string() {
            sanitize_schema_node(additional, false);
        }
    }

    for key in ["anyOf"] {
        if let Some(branches) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for branch in branches {
                sanitize_schema_node(branch, false);
            }
        }
    }

    if let Some(defs) = obj.get_mut("$defs").and_then(Value::as_object_mut) {
        for value in defs.values_mut() {
            sanitize_schema_node(value, false);
        }
    }

    prune_required(obj);
}

fn collapse_nullable_type_array(obj: &mut Map<String, Value>) {
    let Some(types) = obj.get("type").and_then(Value::as_array) else {
        return;
    };
    let mut saw_null = false;
    let mut first_non_null = None;
    for typ in types {
        match typ.as_str() {
            Some("null") => saw_null = true,
            Some(other) if first_non_null.is_none() => first_non_null = Some(other.to_string()),
            _ => {}
        }
    }
    if let Some(kind) = first_non_null {
        obj.insert("type".to_string(), Value::String(kind));
        if saw_null {
            obj.insert("nullable".to_string(), Value::Bool(true));
        }
    }
}

fn coerce_object_type(obj: &mut Map<String, Value>) {
    let has_properties = obj.get("properties").is_some();
    let missing_or_null_type = obj
        .get("type")
        .is_none_or(|value| value.is_null() || value.as_str() == Some(""));
    if has_properties && missing_or_null_type {
        obj.insert("type".to_string(), Value::String("object".to_string()));
    }

    if obj.get("type").and_then(Value::as_str) == Some("object") {
        let needs_properties = !obj.get("properties").is_some_and(Value::is_object);
        if needs_properties {
            obj.insert("properties".to_string(), Value::Object(Map::new()));
        }
    }
}

fn prune_required(obj: &mut Map<String, Value>) {
    let Some(required) = obj.get("required").and_then(Value::as_array) else {
        return;
    };
    let Some(props) = obj.get("properties").and_then(Value::as_object) else {
        obj.remove("required");
        return;
    };
    let kept: Vec<Value> = required
        .iter()
        .filter_map(Value::as_str)
        .filter(|name| props.contains_key(*name))
        .map(|name| Value::String(name.to_string()))
        .collect();
    if kept.is_empty() {
        obj.remove("required");
    } else {
        obj.insert("required".to_string(), Value::Array(kept));
    }
}

fn strip_schema_keywords(node: &mut Value, keywords: &[&str]) -> usize {
    let Some(obj) = node.as_object_mut() else {
        return 0;
    };
    let mut stripped = 0;
    for key in keywords {
        if obj.remove(*key).is_some() {
            stripped += 1;
        }
    }
    stripped += recurse_schema_children(obj, |child| strip_schema_keywords(child, keywords));
    stripped
}

fn strip_slash_enum_in_schema(node: &mut Value) -> usize {
    let Some(obj) = node.as_object_mut() else {
        return 0;
    };
    let mut stripped = 0;
    let enum_has_slash = obj
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| {
            values
                .iter()
                .any(|value| value.as_str().is_some_and(|s| s.contains('/')))
        });
    if enum_has_slash {
        obj.remove("enum");
        stripped += 1;
    }
    stripped += recurse_schema_children(obj, strip_slash_enum_in_schema);
    stripped
}

fn recurse_schema_children<F>(obj: &mut Map<String, Value>, mut f: F) -> usize
where
    F: FnMut(&mut Value) -> usize,
{
    let mut count = 0;
    if let Some(props) = obj.get_mut("properties").and_then(Value::as_object_mut) {
        for value in props.values_mut() {
            count += f(value);
        }
    }
    if let Some(items) = obj.get_mut("items") {
        count += f(items);
    }
    if let Some(additional) = obj.get_mut("additionalProperties") {
        if additional.is_object() {
            count += f(additional);
        }
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(branches) = obj.get_mut(key).and_then(Value::as_array_mut) {
            for branch in branches {
                count += f(branch);
            }
        }
    }
    if let Some(defs) = obj.get_mut("$defs").and_then(Value::as_object_mut) {
        for value in defs.values_mut() {
            count += f(value);
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, parameters: Value) -> Value {
        json!({"type": "function", "function": {"name": name, "parameters": parameters}})
    }

    fn responses_tool(name: &str, parameters: Value) -> Value {
        json!({"type": "function", "name": name, "parameters": parameters})
    }

    fn sanitize(tools: Value) -> Value {
        sanitize_tool_schemas(Some(&tools)).expect("sanitized tools")
    }

    #[test]
    fn object_without_properties_gets_empty_properties() {
        let out = sanitize(json!([tool("t", json!({"type": "object"}))]));
        assert_eq!(
            out[0]["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn nested_object_without_properties_gets_empty_properties() {
        let out = sanitize(json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "arguments": {"type": "object", "description": "free-form"}
                },
                "required": ["name"]
            })
        )]));
        let args = &out[0]["function"]["parameters"]["properties"]["arguments"];
        assert_eq!(args["type"], "object");
        assert_eq!(args["properties"], json!({}));
        assert_eq!(args["description"], "free-form");
    }

    #[test]
    fn bare_string_property_values_are_replaced_with_schema_dicts() {
        let out = sanitize(json!([tool(
            "t",
            json!({"type": "object", "properties": {"payload": "object", "name": "string"}})
        )]));
        assert_eq!(
            out[0]["function"]["parameters"]["properties"]["payload"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            out[0]["function"]["parameters"]["properties"]["name"],
            json!({"type": "string"})
        );
    }

    #[test]
    fn nullable_type_array_collapses_to_single_string() {
        let out = sanitize(json!([tool(
            "t",
            json!({"type": "object", "properties": {"maybe_name": {"type": ["string", "null"]}}})
        )]));
        let prop = &out[0]["function"]["parameters"]["properties"]["maybe_name"];
        assert_eq!(prop["type"], "string");
        assert_eq!(prop["nullable"], true);
    }

    #[test]
    fn anyof_nested_objects_are_sanitized() {
        let out = sanitize(json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {
                    "opt": {"anyOf": [{"type": "object"}, {"type": "string"}]}
                }
            })
        )]));
        let variants = &out[0]["function"]["parameters"]["properties"]["opt"]["anyOf"];
        assert_eq!(variants[0], json!({"type": "object", "properties": {}}));
        assert_eq!(variants[1], json!({"type": "string"}));
    }

    #[test]
    fn missing_or_non_dict_parameters_get_default_object_schema() {
        let tools = json!([
            {"type": "function", "function": {"name": "missing"}},
            tool("string_params", json!("object"))
        ]);
        let out = sanitize(tools);
        assert_eq!(
            out[0]["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            out[1]["function"]["parameters"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn required_fields_are_pruned_to_existing_properties() {
        let out = sanitize(json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name", "missing_field"]
            })
        )]));
        assert_eq!(
            out[0]["function"]["parameters"]["required"],
            json!(["name"])
        );

        let out = sanitize(json!([tool(
            "t",
            json!({"type": "object", "properties": {}, "required": ["x", "y"]})
        )]));
        assert!(out[0]["function"]["parameters"].get("required").is_none());
    }

    #[test]
    fn well_formed_schema_and_additional_properties_are_preserved_or_sanitized() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path"},
                "payload": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": true
                },
                "dict_field": {
                    "type": "object",
                    "additionalProperties": {"type": "object"}
                }
            },
            "required": ["path"]
        });
        let out = sanitize(json!([tool("read_file", schema)]));
        let params = &out[0]["function"]["parameters"];
        assert_eq!(params["properties"]["path"]["description"], "File path");
        assert_eq!(
            params["properties"]["payload"]["additionalProperties"],
            true
        );
        assert_eq!(
            params["properties"]["dict_field"]["additionalProperties"],
            json!({"type": "object", "properties": {}})
        );
    }

    #[test]
    fn sanitize_does_not_mutate_input() {
        let original = json!([tool(
            "t",
            json!({"type": "object", "properties": {"x": {"type": "object"}}})
        )]);
        let _ = sanitize_tool_schemas(Some(&original));
        assert!(original[0]["function"]["parameters"]["properties"]["x"]
            .get("properties")
            .is_none());
    }

    #[test]
    fn items_and_nested_required_are_sanitized() {
        let out = sanitize(json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {
                    "bag": {"type": "array", "items": {"type": "object"}},
                    "filter": {
                        "type": "object",
                        "properties": {"field": {"type": "string"}},
                        "required": ["field", "missing"]
                    }
                }
            })
        )]));
        assert_eq!(
            out[0]["function"]["parameters"]["properties"]["bag"]["items"],
            json!({"type": "object", "properties": {}})
        );
        assert_eq!(
            out[0]["function"]["parameters"]["properties"]["filter"]["required"],
            json!(["field"])
        );
    }

    #[test]
    fn empty_and_none_tools_are_stable() {
        assert_eq!(sanitize_tool_schemas(Some(&json!([]))), Some(json!([])));
        assert_eq!(sanitize_tool_schemas(None), None);
    }

    #[test]
    fn strip_pattern_and_format_removes_schema_keywords_only() {
        let mut tools = json!([tool(
            "search_files",
            json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regex pattern"},
                    "date": {"type": "string", "pattern": "\\d+", "format": "date-time"}
                },
                "required": ["pattern"]
            })
        )]);
        let stripped = strip_pattern_and_format(Some(&mut tools));
        assert_eq!(stripped, 2);
        let params = &tools[0]["function"]["parameters"];
        assert!(params["properties"].get("pattern").is_some());
        assert_eq!(params["properties"]["pattern"]["type"], "string");
        assert!(params["properties"]["date"].get("pattern").is_none());
        assert!(params["properties"]["date"].get("format").is_none());
    }

    #[test]
    fn strip_pattern_and_format_recurses_and_is_idempotent() {
        let mut tools = json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "anyOf": [
                            {"type": "string", "pattern": "[A-Z]+", "format": "uuid"},
                            {"type": "integer"}
                        ]
                    }
                }
            })
        )]);
        assert_eq!(strip_pattern_and_format(Some(&mut tools)), 2);
        assert_eq!(strip_pattern_and_format(Some(&mut tools)), 0);
        let variants = &tools[0]["function"]["parameters"]["properties"]["value"]["anyOf"];
        assert!(variants[0].get("pattern").is_none());
        assert!(variants[0].get("format").is_none());
    }

    #[test]
    fn strip_pattern_and_format_handles_responses_and_mixed_formats() {
        let mut tools = json!([
            tool(
                "search",
                json!({"type": "object", "properties": {"query": {"type": "string", "pattern": "^[a-z]+$"}}})
            ),
            responses_tool(
                "get_time",
                json!({"type": "object", "properties": {"tz": {"type": "string", "format": "date-time"}}})
            )
        ]);
        assert_eq!(strip_pattern_and_format(Some(&mut tools)), 2);
        assert!(tools[0]["function"]["parameters"]["properties"]["query"]
            .get("pattern")
            .is_none());
        assert!(tools[1]["parameters"]["properties"]["tz"]
            .get("format")
            .is_none());
    }

    #[test]
    fn strip_pattern_and_format_empty_and_none_return_zero() {
        let mut empty = json!([]);
        assert_eq!(strip_pattern_and_format(Some(&mut empty)), 0);
        assert_eq!(strip_pattern_and_format(None), 0);
    }

    #[test]
    fn top_level_forbidden_combinators_are_stripped_but_nested_allof_survives() {
        let out = sanitize(json!([tool(
            "memory",
            json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["add", "replace"]},
                    "config": {
                        "type": "object",
                        "properties": {"mode": {"type": "string"}},
                        "allOf": [{"required": ["mode"]}]
                    }
                },
                "required": ["action"],
                "allOf": [{"then": {"required": ["content"]}}],
                "oneOf": [{"required": ["action"]}],
                "anyOf": [{"required": ["action"]}],
                "enum": ["bogus-top-level"],
                "not": {"required": ["y"]}
            })
        )]));
        let params = &out[0]["function"]["parameters"];
        for key in TOP_LEVEL_FORBIDDEN_KEYS {
            assert!(params.get(*key).is_none(), "{key} should be stripped");
        }
        assert_eq!(params["required"], json!(["action"]));
        assert_eq!(
            params["properties"]["config"]["allOf"],
            json!([{"required": ["mode"]}])
        );
    }

    #[test]
    fn strip_slash_enum_removes_enums_containing_slashes() {
        let mut tools = json!([
            tool(
                "train",
                json!({"type": "object", "properties": {"model": {"type": "string", "enum": ["Qwen/Qwen3.5", "local"]}}})
            ),
            responses_tool(
                "pick",
                json!({"type": "object", "properties": {"mode": {"type": "string", "enum": ["fast", "slow"]}}})
            )
        ]);
        assert_eq!(strip_slash_enum(Some(&mut tools)), 1);
        assert!(tools[0]["function"]["parameters"]["properties"]["model"]
            .get("enum")
            .is_none());
        assert_eq!(
            tools[1]["parameters"]["properties"]["mode"]["enum"],
            json!(["fast", "slow"])
        );
    }

    #[test]
    fn strip_slash_enum_recurses_and_is_idempotent() {
        let mut tools = json!([tool(
            "t",
            json!({
                "type": "object",
                "properties": {
                    "value": {
                        "anyOf": [
                            {"type": "string", "enum": ["owner/repo"]},
                            {"type": "null"}
                        ]
                    }
                }
            })
        )]);
        assert_eq!(strip_slash_enum(Some(&mut tools)), 1);
        assert_eq!(strip_slash_enum(Some(&mut tools)), 0);
        let variants = &tools[0]["function"]["parameters"]["properties"]["value"]["anyOf"];
        assert!(variants[0].get("enum").is_none());
    }

    #[test]
    fn strip_slash_enum_empty_and_none_return_zero() {
        let mut empty = json!([]);
        assert_eq!(strip_slash_enum(Some(&mut empty)), 0);
        assert_eq!(strip_slash_enum(None), 0);
    }
}
