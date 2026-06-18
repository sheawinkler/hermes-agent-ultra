//! resolve_a_share_symbol — Chinese name / code → canonical `.SH`/`.SZ` symbol.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};
use hermes_trading::providers::akshare::resolve_a_share_symbol;

#[derive(Default)]
pub struct ResolveAShareSymbolHandler;

impl ResolveAShareSymbolHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for ResolveAShareSymbolHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let query = params
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'query' parameter".into()))?;

        let symbol = resolve_a_share_symbol(query)
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        serde_json::to_string_pretty(&json!({
            "query": query,
            "symbol": symbol,
            "_orchestration": "Call analyze_stock(symbol, use_providers=true) next for valuation/DCF/scoring — before web_search.",
        }))
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "query".into(),
            json!({
                "type": "string",
                "description": "Chinese stock name or 6-digit code (e.g. 牧原股份, 600519, 600519.SH)"
            }),
        );

        tool_schema(
            "resolve_a_share_symbol",
            "Resolve A-share Chinese name or 6-digit code to canonical symbol (600519.SH / 000001.SZ). \
             After resolution, call analyze_stock for fundamentals/valuation — not web_search first.",
            JsonSchema::object(props, vec!["query".into()]),
        )
    }
}
