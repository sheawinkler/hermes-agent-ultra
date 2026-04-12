use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

const GATEWAY_URL_ENV: &str = "HERMES_MANAGED_TOOL_GATEWAY_URL";
const GATEWAY_TOKEN_ENV: &str = "HERMES_MANAGED_TOOL_GATEWAY_TOKEN";

pub struct ManagedToolGatewayHandler;

#[async_trait]
impl ToolHandler for ManagedToolGatewayHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let target_tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        if target_tool.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'tool'".into()));
        }

        let base = match std::env::var(GATEWAY_URL_ENV) {
            Ok(s) if !s.trim().is_empty() => s.trim_end_matches('/').to_string(),
            _ => {
                return Ok(json!({
                    "status": "unconfigured",
                    "tool": target_tool,
                    "hint": format!(
                        "Set {} to the base URL of a managed gateway that accepts POST /invoke (JSON body: {{tool, args}}).",
                        GATEWAY_URL_ENV
                    ),
                })
                .to_string());
            }
        };

        let url = format!("{}/invoke", base);
        let args = params
            .get("args")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let body = json!({ "tool": target_tool, "args": args });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("http client: {e}")))?;

        let mut req = client.post(&url).json(&body);
        if let Ok(tok) = std::env::var(GATEWAY_TOKEN_ENV) {
            let tok = tok.trim();
            if !tok.is_empty() {
                req = req.bearer_auth(tok);
            }
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("managed_tool_gateway request: {e}")))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("managed_tool_gateway body: {e}")))?;

        if !status.is_success() {
            return Ok(json!({
                "status": "upstream_error",
                "http_status": status.as_u16(),
                "tool": target_tool,
                "body": text,
            })
            .to_string());
        }

        Ok(json!({
            "status": "delegated",
            "tool": target_tool,
            "result": serde_json::from_str::<Value>(&text).unwrap_or(json!(text)),
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("tool".into(), json!({"type":"string"}));
        props.insert("args".into(), json!({"type":"object"}));
        tool_schema(
            "managed_tool_gateway",
            "Dispatch a managed tool call through an HTTP gateway: POST $HERMES_MANAGED_TOOL_GATEWAY_URL/invoke with JSON {tool, args}.",
            JsonSchema::object(props, vec!["tool".into()]),
        )
    }
}
