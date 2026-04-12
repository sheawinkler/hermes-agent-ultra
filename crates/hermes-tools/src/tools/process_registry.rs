use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

#[derive(Clone, Default)]
pub struct ProcessRegistryHandler {
    entries: Arc<Mutex<HashMap<String, i64>>>,
}

#[async_trait]
impl ToolHandler for ProcessRegistryHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("list");
        match action {
            "register" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let pid = params.get("pid").and_then(|v| v.as_i64()).unwrap_or(0);
                if name.is_empty() || pid <= 0 {
                    return Err(ToolError::InvalidParams(
                        "register requires name + pid".into(),
                    ));
                }
                self.entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .insert(name.to_string(), pid);
                Ok(json!({"status":"registered","name":name,"pid":pid}).to_string())
            }
            _ => {
                let entries = self
                    .entries
                    .lock()
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(json!({"entries": entries.clone()}).to_string())
            }
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({"type":"string","enum":["list","register"]}),
        );
        props.insert("name".into(), json!({"type":"string"}));
        props.insert("pid".into(), json!({"type":"integer"}));
        tool_schema(
            "process_registry",
            "Register/list background process metadata.",
            JsonSchema::object(props, vec![]),
        )
    }
}
