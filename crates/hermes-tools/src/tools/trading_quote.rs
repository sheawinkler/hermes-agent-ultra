//! get_quote tool: Fetch live spot quote for a symbol.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};

#[derive(Default)]
pub struct GetQuoteHandler;

impl GetQuoteHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for GetQuoteHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'symbol' parameter".into()))?;

        let source = params
            .get("source")
            .and_then(|v| v.as_str())
            .map(hermes_trading::QuoteSource::parse)
            .transpose()
            .map_err(|e| ToolError::InvalidParams(e.to_string()))?
            .unwrap_or(hermes_trading::QuoteSource::Auto);

        let refresh = params
            .get("refresh")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let router = hermes_trading::QuoteRouter::new();
        let data = router
            .fetch_quote_with_source(symbol, source, refresh)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to fetch quote: {e}")))?;

        let mut out = serde_json::to_value(&data)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))?;
        if is_a_share_symbol(symbol) {
            if let Some(obj) = out.as_object_mut() {
                obj.insert(
                    "_orchestration".into(),
                    json!("For A-share valuation/fundamentals on this symbol, call analyze_stock(symbol, use_providers=true) next — before web_search."),
                );
            }
        }
        serde_json::to_string_pretty(&out)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "symbol".into(),
            json!({
                "type": "string",
                "description": "Symbol identifier. Examples: 'AAPL' (US), '0700.HK' (HK), '000001.SZ' (A-share), 'BTC-USDT' (crypto). Bare 'BTC'/'ETH' are normalized to USDT pairs."
            }),
        );
        props.insert(
            "source".into(),
            json!({
                "type": "string",
                "description": "Quote source: prefer 'auto' (default). Override only when debugging.",
                "enum": ["auto", "yahoo", "eastmoney", "binance"]
            }),
        );
        props.insert(
            "refresh".into(),
            json!({
                "type": "boolean",
                "description": "Bypass quote cache and force network fetch (default: false)"
            }),
        );

        tool_schema(
            "get_quote",
            "Fetch live spot price quote. Supports US/HK (Yahoo), A-share (Eastmoney), and crypto (Binance). \
             Use source=auto. Crypto: BTC-USDT not BTC-USD. \
             For A-share **fundamentals/valuation/DCF** requests, prefer `analyze_stock` (not get_quote + web_search). \
             Use get_quote alone only when the user wants spot price with no research narrative.",
            JsonSchema::object(props, vec!["symbol".into()]),
        )
    }
}

fn is_a_share_symbol(sym: &str) -> bool {
    sym.ends_with(".SH") || sym.ends_with(".SZ")
}
