//! analyze_stock tool: DCF + scoring + persona panel for a symbol.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};
use hermes_trading::research::models::CompsPeer;
use hermes_trading::research::report::render_html_report;
use hermes_trading::{
    QuoteRouter, QuoteSource, analyze_stock, enrich_snapshot, snapshot_from_inputs,
};

#[derive(Default)]
pub struct AnalyzeStockHandler;

impl AnalyzeStockHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl ToolHandler for AnalyzeStockHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let symbol = params
            .get("symbol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'symbol' parameter".into()))?;

        let fundamentals = params.get("fundamentals");
        let peers_json = params.get("peers");
        let format = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("json");
        let use_providers = params
            .get("use_providers")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let router = QuoteRouter::new();
        let quote = router
            .fetch_quote_with_source(symbol, QuoteSource::Auto, false)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to fetch quote: {e}")))?;

        let mut snap = snapshot_from_inputs(&quote, fundamentals);
        let raw_dims = if use_providers {
            Some(enrich_snapshot(&mut snap, symbol).await)
        } else {
            None
        };

        let peers = parse_peers(peers_json);
        let result = analyze_stock(&snap, raw_dims.as_ref(), peers.as_deref());

        if format == "html" {
            let narrative = params.get("narrative").and_then(|v| v.as_str());
            let val = serde_json::to_value(&result)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            return Ok(render_html_report(&val, narrative));
        }

        serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "symbol".into(),
            json!({
                "type": "string",
                "description": "Stock symbol (e.g. 600519.SH, AAPL)"
            }),
        );
        props.insert(
            "fundamentals".into(),
            json!({
                "type": "object",
                "description": "Optional fundamentals JSON from web_search to enrich analysis"
            }),
        );
        props.insert(
            "peers".into(),
            json!({
                "type": "array",
                "description": "Optional peer list for comps analysis [{pe, pb, ...}]"
            }),
        );
        props.insert(
            "use_providers".into(),
            json!({
                "type": "boolean",
                "description": "Run UZI-style 22-dim HTTP fetchers + DCF/scoring/persona panel (A-share Eastmoney). Sets raw_dims for scoring."
            }),
        );
        props.insert(
            "format".into(),
            json!({
                "type": "string",
                "enum": ["json", "html"],
                "description": "Output format (default json)"
            }),
        );
        props.insert(
            "narrative".into(),
            json!({
                "type": "string",
                "description": "LLM narrative text to embed when format=html"
            }),
        );

        tool_schema(
            "analyze_stock",
            "Run institutional equity research: DCF, comps, scoring, 66-investor panel. Returns data_confidence and used_fallback.",
            JsonSchema::object(props, vec!["symbol".into()]),
        )
    }
}

fn parse_peers(value: Option<&Value>) -> Option<Vec<CompsPeer>> {
    let arr = value?.as_array()?;
    let mut peers = Vec::new();
    for item in arr {
        peers.push(CompsPeer {
            name: item
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            ticker: item
                .get("ticker")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            pe: item.get("pe").and_then(|v| v.as_f64()),
            pb: item.get("pb").and_then(|v| v.as_f64()),
            ps: item.get("ps").and_then(|v| v.as_f64()),
            ev_ebitda: item.get("ev_ebitda").and_then(|v| v.as_f64()),
            ev_sales: item.get("ev_sales").and_then(|v| v.as_f64()),
            roe: item.get("roe").and_then(|v| v.as_f64()),
            net_margin: item.get("net_margin").and_then(|v| v.as_f64()),
            revenue_growth: item.get("revenue_growth").and_then(|v| v.as_f64()),
        });
    }
    Some(peers)
}
