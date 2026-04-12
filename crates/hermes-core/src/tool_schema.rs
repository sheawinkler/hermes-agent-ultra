use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// JSON Schema definition used for tool parameter validation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonSchema {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub schema_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<IndexMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
    #[serde(rename = "additionalProperties", skip_serializing_if = "Option::is_none")]
    pub additional_properties: Option<bool>,
}

impl JsonSchema {
    /// Create a new JSON Schema with the given type.
    pub fn new(schema_type: impl Into<String>) -> Self {
        Self {
            schema_type: Some(schema_type.into()),
            properties: None,
            required: None,
            additional_properties: None,
        }
    }

    /// Create an object-type schema with the given properties and required fields.
    pub fn object(
        properties: IndexMap<String, serde_json::Value>,
        required: Vec<String>,
    ) -> Self {
        Self {
            schema_type: Some("object".to_string()),
            properties: Some(properties),
            required: Some(required),
            additional_properties: Some(false),
        }
    }
}

/// Schema definition for a tool, describing its name, description, and parameters.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub parameters: JsonSchema,
}

impl ToolSchema {
    /// Create a new tool schema.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: JsonSchema,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// Helper function to create a tool schema concisely.
pub fn tool_schema(
    name: impl Into<String>,
    description: impl Into<String>,
    params_schema: JsonSchema,
) -> ToolSchema {
    ToolSchema::new(name, description, params_schema)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_json_schema_new() {
        let schema = JsonSchema::new("string");
        assert_eq!(schema.schema_type.as_deref(), Some("string"));
        assert!(schema.properties.is_none());
    }

    #[test]
    fn test_json_schema_object() {
        let mut props = IndexMap::new();
        props.insert("name".to_string(), json!({"type": "string"}));
        let schema = JsonSchema::object(props, vec!["name".to_string()]);
        assert_eq!(schema.schema_type.as_deref(), Some("object"));
        assert!(schema.properties.unwrap().contains_key("name"));
    }

    #[test]
    fn test_tool_schema_creation() {
        let ts = tool_schema("read_file", "Read a file", JsonSchema::new("object"));
        assert_eq!(ts.name, "read_file");
    }

    #[test]
    fn test_serde_roundtrip() {
        let ts = tool_schema("test", "A test tool", JsonSchema::new("object"));
        let json = serde_json::to_string(&ts).unwrap();
        let ts2: ToolSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, ts2);
    }
}