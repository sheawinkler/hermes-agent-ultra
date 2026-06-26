//! analyze_stock tool: DCF + scoring + persona panel for a symbol.

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{Value, json};

use hermes_core::{JsonSchema, ToolError, ToolHandler, ToolSchema, tool_schema};
use hermes_trading::research::models::CompsPeer;
use hermes_trading::research::profile::AnalysisProfile;
use hermes_trading::research::report::{render_institutional_html, write_equity_report};
use hermes_trading::research::synthesis::{ReportPaths, build_synthesis_format_output};
use hermes_trading::{
    QuoteRouter, QuoteSource, analyze_stock, enrich_snapshot, snapshot_from_inputs,
};

use crate::analyze_stock_cache;

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
        let depth = params
            .get("depth")
            .and_then(|v| v.as_str())
            .unwrap_or("medium");
        let profile = AnalysisProfile::from_depth_str(depth);
        let mut format = params
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("json");
        if profile.is_lite() && !params.as_object().is_some_and(|m| m.contains_key("format")) {
            format = "markdown";
        }
        validate_format_for_profile(format, &profile)?;
        let use_providers = params
            .get("use_providers")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let write_report = params
            .get("write_report")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if write_report && profile.is_lite() {
            return Err(ToolError::InvalidParams(
                "write_report requires depth=medium (/analyze-stock).".into(),
            ));
        }
        let narrative = params.get("narrative").and_then(|v| v.as_str());

        let router = QuoteRouter::new();
        let quote = router
            .fetch_quote_with_source(symbol, QuoteSource::Auto, false)
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Failed to fetch quote: {e}")))?;

        let mut snap = snapshot_from_inputs(&quote, fundamentals);
        let (raw_dims, collect) = if use_providers {
            let enriched = enrich_snapshot(&mut snap, symbol, Some(quote), &profile).await;
            (Some(enriched.raw_dims), Some(enriched.collect))
        } else {
            (None, None)
        };

        let peers = parse_peers(peers_json);
        let result = analyze_stock(
            &snap,
            raw_dims.as_ref(),
            peers.as_deref(),
            &profile,
            collect.as_ref(),
        );
        analyze_stock_cache::store(symbol, depth, result.clone());

        let mut saved_report_paths: Option<ReportPaths> = None;
        if write_report {
            let html = render_institutional_html(&result, narrative);
            let paths = write_equity_report(&result, &html, None)
                .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write report: {e}")))?;
            let report_paths = paths_to_json(&paths);
            saved_report_paths = Some(report_paths.clone());
            if format == "html" {
                return Ok(serde_json::to_string_pretty(&json!({
                    "report_paths": report_paths,
                    "html": html,
                }))
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))?);
            }
            if format == "synthesis" {
                let mut out = build_synthesis_format_output(&result);
                out.report_paths = Some(report_paths);
                return Ok(serde_json::to_string_pretty(&out).map_err(|e| {
                    ToolError::ExecutionFailed(format!("Serialization error: {e}"))
                })?);
            }
        }

        if format == "html" {
            return Ok(render_institutional_html(&result, narrative));
        }

        if format == "synthesis" {
            let out = build_synthesis_format_output(&result);
            return Ok(serde_json::to_string_pretty(&out)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))?);
        }

        if format == "markdown" && profile.is_lite() {
            let body = result.summary_markdown;
            return Ok(maybe_prefix_report_paths(
                saved_report_paths.as_ref(),
                &body,
            ));
        }

        if format == "markdown" {
            let json_body = serde_json::to_string_pretty(&result)
                .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))?;
            let body = format!(
                "{}\n\n<!-- full JSON below; do not replace the markdown tables above -->\n{}",
                result.summary_markdown, json_body
            );
            return Ok(maybe_prefix_report_paths(
                saved_report_paths.as_ref(),
                &body,
            ));
        }

        let json_body = serde_json::to_string_pretty(&result)
            .map_err(|e| ToolError::ExecutionFailed(format!("Serialization error: {e}")))?;
        let body = format!(
            "{}\n\n<!-- full JSON below; do not replace the markdown tables above -->\n{}",
            result.summary_markdown, json_body
        );
        Ok(maybe_prefix_report_paths(
            saved_report_paths.as_ref(),
            &body,
        ))
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
            "depth".into(),
            json!({
                "type": "string",
                "enum": ["lite", "medium"],
                "description": "lite (/quick-scan): 8 core dims + Top 10 judges + trap; medium (/analyze-stock): full pipeline. Default medium."
            }),
        );
        props.insert(
            "fundamentals".into(),
            json!({
                "type": "object",
                "description": "Optional fundamentals JSON to enrich analysis when use_providers=false"
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
                "description": "Run UZI-style HTTP fetchers + DCF/scoring/persona panel. Default true; set false to skip provider fetch."
            }),
        );
        props.insert(
            "format".into(),
            json!({
                "type": "string",
                "enum": ["json", "markdown", "html", "synthesis"],
                "description": "json (default, medium): summary_markdown + full JSON; markdown: tables only; lite defaults to quick-scan markdown; html: institutional one-page report; synthesis: slim JSON with synthesis + core metrics"
            }),
        );
        props.insert(
            "narrative".into(),
            json!({
                "type": "string",
                "description": "LLM narrative text to embed when format=html or write_report=true"
            }),
        );
        props.insert(
            "write_report".into(),
            json!({
                "type": "boolean",
                "description": "When true (medium depth), write full-report-standalone.html + analysis.json under {HERMES_HOME}/reports/{symbol}_{date}/ and include report_paths in the response"
            }),
        );

        tool_schema(
            "analyze_stock",
            "Listed-stock research: DCF, scoring, persona panel. depth=lite for /quick-scan (Top 10 judges, no web); depth=medium for /analyze-stock (66 judges). \
             Call **before** web_search for valuation requests. \
             Medium: paste full summary_markdown (19 dims + 66 judges). Lite: quick-scan markdown only. \
             Use format=synthesis for structured verdict JSON; format=html + write_report=true to deliver a standalone report file.",
            JsonSchema::object(props, vec!["symbol".into()]),
        )
    }
}

fn validate_format_for_profile(format: &str, profile: &AnalysisProfile) -> Result<(), ToolError> {
    if profile.is_lite() && matches!(format, "html" | "synthesis") {
        return Err(ToolError::InvalidParams(
            "format=html and format=synthesis require depth=medium (/analyze-stock). \
             /quick-scan uses markdown only."
                .into(),
        ));
    }
    Ok(())
}

fn paths_to_json(paths: &hermes_trading::research::report::WrittenReportPaths) -> ReportPaths {
    ReportPaths {
        html: paths.html.display().to_string(),
        analysis_json: paths.analysis_json.display().to_string(),
    }
}

fn maybe_prefix_report_paths(paths: Option<&ReportPaths>, body: &str) -> String {
    let Some(paths) = paths else {
        return body.to_string();
    };
    format!(
        "Report saved:\n- HTML: {}\n- analysis.json: {}\n\n{body}",
        paths.html, paths.analysis_json
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "live network"]
    async fn medium_markdown_includes_json_suffix() {
        use hermes_core::ToolHandler;
        use serde_json::json;

        let handler = AnalyzeStockHandler::new();
        let raw = handler
            .execute(json!({
                "symbol": "600522.SH",
                "depth": "medium",
                "use_providers": true,
                "format": "markdown"
            }))
            .await
            .expect("markdown medium");
        assert!(
            raw.contains("<!-- full JSON"),
            "medium format=markdown must retain JSON suffix for slash delivery"
        );
    }

    #[test]
    fn lite_rejects_html_and_synthesis_format() {
        let profile = AnalysisProfile::lite();
        assert!(validate_format_for_profile("html", &profile).is_err());
        assert!(validate_format_for_profile("synthesis", &profile).is_err());
        assert!(validate_format_for_profile("markdown", &profile).is_ok());
        assert!(validate_format_for_profile("json", &profile).is_ok());
    }

    #[test]
    fn medium_allows_html_and_synthesis_format() {
        let profile = AnalysisProfile::medium();
        assert!(validate_format_for_profile("html", &profile).is_ok());
        assert!(validate_format_for_profile("synthesis", &profile).is_ok());
    }
}
