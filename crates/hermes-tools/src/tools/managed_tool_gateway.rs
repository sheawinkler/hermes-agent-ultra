//! Generic `managed_tool_gateway` tool — dispatches an arbitrary tool
//! call through an HTTP gateway.
//!
//! Two configuration paths are supported:
//!
//! 1. **Legacy explicit override.** Set `HERMES_MANAGED_TOOL_GATEWAY_URL`
//!    (and optionally `HERMES_MANAGED_TOOL_GATEWAY_TOKEN`). The handler
//!    POSTs `{tool, args}` to `$URL/invoke` with `Authorization: Bearer
//!    $TOKEN`.
//!
//! 2. **Vendor-routed Nous mode.** When the legacy env vars are not set
//!    but the caller supplies a `"vendor"` parameter, we delegate to
//!    `hermes_config::managed_gateway::resolve_managed_tool_gateway(vendor)`,
//!    which honours `HERMES_ENABLE_NOUS_MANAGED_TOOLS`, the per-vendor
//!    URL builder, and the Nous OAuth token reader.

use std::time::Duration;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use hermes_config::managed_gateway::{resolve_managed_tool_gateway, ResolveOptions};

const GATEWAY_URL_ENV: &str = "HERMES_MANAGED_TOOL_GATEWAY_URL";
const GATEWAY_TOKEN_ENV: &str = "HERMES_MANAGED_TOOL_GATEWAY_TOKEN";

pub struct ManagedToolGatewayHandler;

struct ResolvedTransport {
    base_url: String,
    bearer: Option<String>,
    via_vendor: Option<String>,
}

impl ManagedToolGatewayHandler {
    fn resolve_transport(vendor: Option<&str>) -> Option<ResolvedTransport> {
        if let Ok(url) = std::env::var(GATEWAY_URL_ENV) {
            let trimmed = url.trim().trim_end_matches('/');
            if !trimmed.is_empty() {
                let token = std::env::var(GATEWAY_TOKEN_ENV)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                return Some(ResolvedTransport {
                    base_url: trimmed.to_string(),
                    bearer: token,
                    via_vendor: None,
                });
            }
        }

        let vendor = vendor?;
        let cfg = resolve_managed_tool_gateway(vendor, ResolveOptions::default())?;
        Some(ResolvedTransport {
            base_url: cfg.gateway_origin.trim_end_matches('/').to_string(),
            bearer: Some(cfg.nous_user_token),
            via_vendor: Some(vendor.to_string()),
        })
    }
}

#[async_trait]
impl ToolHandler for ManagedToolGatewayHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let target_tool = params.get("tool").and_then(|v| v.as_str()).unwrap_or("");
        if target_tool.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'tool'".into()));
        }

        let vendor = params
            .get("vendor")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let transport = match Self::resolve_transport(vendor) {
            Some(t) => t,
            None => {
                return Ok(json!({
                    "status": "unconfigured",
                    "tool": target_tool,
                    "hint": format!(
                        "Set {GATEWAY_URL_ENV} to the base URL of a managed gateway that accepts \
                         POST /invoke (JSON body: {{tool, args}}), or pass a 'vendor' parameter \
                         and enable HERMES_ENABLE_NOUS_MANAGED_TOOLS with a Nous access token."
                    ),
                })
                .to_string());
            }
        };

        let url = format!("{}/invoke", transport.base_url);
        let args = params.get("args").cloned().unwrap_or_else(|| json!({}));
        let body = json!({ "tool": target_tool, "args": args });

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| ToolError::ExecutionFailed(format!("http client: {e}")))?;

        let mut req = client.post(&url).json(&body);
        if let Some(tok) = &transport.bearer {
            req = req.bearer_auth(tok);
        }

        let resp = req.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("managed_tool_gateway request: {e}"))
        })?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("managed_tool_gateway body: {e}")))?;

        if !status.is_success() {
            let mut err = json!({
                "status": "upstream_error",
                "http_status": status.as_u16(),
                "tool": target_tool,
                "body": text,
            });
            if let Some(v) = transport.via_vendor {
                err["vendor"] = json!(v);
            }
            return Ok(err.to_string());
        }

        let mut payload = json!({
            "status": "delegated",
            "tool": target_tool,
            "result": serde_json::from_str::<Value>(&text).unwrap_or(json!(text)),
        });
        if let Some(v) = transport.via_vendor {
            payload["vendor"] = json!(v);
        }
        Ok(payload.to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("tool".into(), json!({"type":"string"}));
        props.insert("args".into(), json!({"type":"object"}));
        props.insert(
            "vendor".into(),
            json!({
                "type": "string",
                "description": "Optional vendor key (e.g. 'firecrawl', 'openai-audio'). When set and legacy HERMES_MANAGED_TOOL_GATEWAY_URL is unset, the call is routed via the Nous-managed gateway resolver."
            }),
        );
        tool_schema(
            "managed_tool_gateway",
            "Dispatch a managed tool call through an HTTP gateway: POST $HERMES_MANAGED_TOOL_GATEWAY_URL/invoke with JSON {tool, args}, or pass a 'vendor' to route via the Nous gateway resolver.",
            JsonSchema::object(props, vec!["tool".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvGuard {
        _g: std::sync::MutexGuard<'static, ()>,
        original: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let g = test_lock::lock();
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            Self { _g: g, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[tokio::test]
    async fn unconfigured_returns_hint() {
        let _g = EnvGuard::new(&[
            GATEWAY_URL_ENV,
            GATEWAY_TOKEN_ENV,
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
        ]);
        let h = ManagedToolGatewayHandler;
        let out = h
            .execute(json!({"tool": "anything"}))
            .await
            .expect("ok response");
        assert!(out.contains("\"status\":\"unconfigured\""), "{out}");
    }

    #[tokio::test]
    async fn missing_tool_param_errors() {
        let h = ManagedToolGatewayHandler;
        let err = h.execute(json!({})).await.expect_err("must error");
        match err {
            ToolError::InvalidParams(msg) => assert!(msg.contains("tool")),
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn resolve_transport_prefers_legacy_env() {
        let _g = EnvGuard::new(&[
            GATEWAY_URL_ENV,
            GATEWAY_TOKEN_ENV,
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
        ]);
        std::env::set_var(GATEWAY_URL_ENV, "https://legacy.example.com/");
        std::env::set_var(GATEWAY_TOKEN_ENV, "legacy-tok");
        let t = ManagedToolGatewayHandler::resolve_transport(Some("firecrawl")).unwrap();
        assert_eq!(t.base_url, "https://legacy.example.com");
        assert_eq!(t.bearer.as_deref(), Some("legacy-tok"));
        assert!(t.via_vendor.is_none());
    }

    #[test]
    fn resolve_transport_falls_back_to_vendor_resolver() {
        let _g = EnvGuard::new(&[
            GATEWAY_URL_ENV,
            GATEWAY_TOKEN_ENV,
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
            "TOOL_GATEWAY_DOMAIN",
        ]);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "vendor-tok");
        let t = ManagedToolGatewayHandler::resolve_transport(Some("firecrawl")).unwrap();
        assert_eq!(t.base_url, "https://firecrawl-gateway.nousresearch.com");
        assert_eq!(t.bearer.as_deref(), Some("vendor-tok"));
        assert_eq!(t.via_vendor.as_deref(), Some("firecrawl"));
    }

    #[test]
    fn resolve_transport_returns_none_when_vendor_path_disabled() {
        let _g = EnvGuard::new(&[
            GATEWAY_URL_ENV,
            GATEWAY_TOKEN_ENV,
            "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
            "TOOL_GATEWAY_USER_TOKEN",
        ]);
        // Feature flag stays off; no legacy URL → resolver returns None.
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "vendor-tok");
        assert!(ManagedToolGatewayHandler::resolve_transport(Some("firecrawl")).is_none());
        // No vendor and no legacy URL → also None.
        assert!(ManagedToolGatewayHandler::resolve_transport(None).is_none());
    }
}
