use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

pub struct UrlSafetyHandler;

#[async_trait]
impl ToolHandler for UrlSafetyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'url'".into()));
        }
        let risky = url.starts_with("http://");
        Ok(
            json!({"url":url,"safe":!risky,"reason":if risky {"non_https"} else {"ok"}})
                .to_string(),
        )
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("url".into(), json!({"type":"string"}));
        tool_schema(
            "url_safety",
            "Check whether a URL is safe to access.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}
