use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

#[derive(Clone, Default)]
pub struct ToolResultStorageHandler {
    store: Arc<Mutex<HashMap<String, String>>>,
}

#[async_trait]
impl ToolHandler for ToolResultStorageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("get");
        let key = params.get("key").and_then(|v| v.as_str()).unwrap_or("");
        if key.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'key'".into()));
        }
        match action {
            "set" => {
                let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");
                self.store
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .insert(key.to_string(), value.to_string());
                Ok(json!({"status":"stored","key":key}).to_string())
            }
            _ => {
                let value = self
                    .store
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .get(key)
                    .cloned();
                Ok(json!({"key":key,"value":value}).to_string())
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["get","set"]}),
        );
        props.insert("key".into(), json!({"type":"string"}));
        props.insert("value".into(), json!({"type":"string"}));
        tool_schema(
            "tool_result_storage",
            "Persist/retrieve tool results by key.",
            JsonSchema::object(props, vec!["key".into()]),
        )
    }
}
