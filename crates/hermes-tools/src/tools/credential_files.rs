use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

pub struct CredentialFilesHandler;

#[async_trait]
impl ToolHandler for CredentialFilesHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let path = params.get("path").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'path'".into()));
        }
        let exists = tokio::fs::metadata(path).await.is_ok();
        Ok(json!({"path":path,"exists":exists}).to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("path".into(), json!({"type":"string"}));
        tool_schema("credential_files", "Check credential file existence/metadata.", JsonSchema::object(props, vec!["path".into()]))
    }
}
