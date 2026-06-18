//! Listed-equity research tool ordering (no user-message keyword routing).
//!
//! When `analyze_stock` is available, defer `web_search` / `web_extract` until
//! `analyze_stock` has run for an A-share symbol touched via `get_quote`, `get_market_data`,
//! `resolve_a_share_symbol`, seeded from the user message, or when the search query references a ticker.

use hermes_core::ToolSchema;
use hermes_core::{ToolCall, ToolResult};
use serde_json::Value;

const BLOCK_MSG: &str = "Listed-equity pipeline: call analyze_stock(symbol, use_providers=true) before web_search/web_extract. \
analyze_stock fetches hard data + DCF + scoring; use web_search only after it returns, to fill data_confidence gaps.";

#[derive(Debug, Clone, Default)]
pub struct EquityResearchGate {
    enabled: bool,
    pending_symbol: Option<String>,
    analyze_done: bool,
}

impl EquityResearchGate {
    #[must_use]
    pub fn from_tool_schemas(schemas: &[ToolSchema]) -> Self {
        let enabled = schemas.iter().any(|t| t.name == "analyze_stock");
        Self {
            enabled,
            ..Default::default()
        }
    }

    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Seed symbol from user-message resolution (not keyword routing).
    pub fn seed_pending_symbol(&mut self, symbol: &str) {
        if self.enabled && is_a_share_symbol(symbol) {
            self.pending_symbol = Some(symbol.to_string());
        }
    }

    /// Remove deferred web tools; returns synthetic error results for the model.
    pub fn gate_tool_calls(&mut self, tool_calls: &mut Vec<ToolCall>) -> Vec<ToolResult> {
        if !self.enabled {
            return Vec::new();
        }

        self.ingest_symbol_tools(tool_calls);

        if self.analyze_done {
            return Vec::new();
        }

        let mut blocked = Vec::new();
        let mut kept = Vec::new();
        for tc in tool_calls.drain(..) {
            if self.should_block_web_tool(&tc) {
                blocked.push(ToolResult::err(tc.id.clone(), BLOCK_MSG));
            } else {
                kept.push(tc);
            }
        }
        *tool_calls = kept;
        blocked
    }

    pub fn record_tool_batch(&mut self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if !self.enabled {
            return;
        }
        for (tc, result) in tool_calls.iter().zip(results.iter()) {
            if result.is_error {
                continue;
            }
            match tc.function.name.as_str() {
                "get_quote" | "resolve_a_share_symbol" | "get_market_data" => {
                    if let Some(sym) = symbol_from_tool_json(&result.content) {
                        if is_a_share_symbol(&sym) {
                            self.pending_symbol = Some(sym);
                        }
                    }
                }
                "analyze_stock" => {
                    self.analyze_done = true;
                    if let Some(sym) = symbol_from_tool_json(&result.content) {
                        self.pending_symbol = Some(sym);
                    }
                }
                _ => {}
            }
        }
    }

    fn ingest_symbol_tools(&mut self, tool_calls: &[ToolCall]) {
        for tc in tool_calls {
            match tc.function.name.as_str() {
                "get_quote" | "resolve_a_share_symbol" | "get_market_data" => {
                    if let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) {
                        if let Some(sym) = args.get("symbol").and_then(|v| v.as_str()) {
                            if is_a_share_symbol(sym) {
                                self.pending_symbol = Some(sym.to_string());
                            }
                        } else if tc.function.name == "resolve_a_share_symbol" {
                            if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                                if let Some(sym) = six_digit_a_share(q) {
                                    self.pending_symbol = Some(sym);
                                }
                            }
                        }
                    }
                }
                "analyze_stock" => {
                    if let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) {
                        if let Some(sym) = args.get("symbol").and_then(|v| v.as_str()) {
                            self.pending_symbol = Some(sym.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn should_block_web_tool(&self, tc: &ToolCall) -> bool {
        if self.analyze_done {
            return false;
        }
        let name = tc.function.name.as_str();
        if !matches!(name, "web_search" | "web_extract") {
            return false;
        }
        if self.pending_symbol.is_some() {
            return true;
        }
        if name == "web_search" {
            if let Ok(args) = serde_json::from_str::<Value>(&tc.function.arguments) {
                if let Some(q) = args.get("query").and_then(|v| v.as_str()) {
                    return query_references_listed_ticker(q);
                }
            }
        }
        false
    }
}

fn symbol_from_tool_json(content: &str) -> Option<String> {
    let v: Value = serde_json::from_str(content).ok()?;
    v.get("symbol").and_then(|s| s.as_str()).map(str::to_string)
}

fn is_a_share_symbol(sym: &str) -> bool {
    sym.ends_with(".SH") || sym.ends_with(".SZ")
}

fn six_digit_a_share(query: &str) -> Option<String> {
    let digits: String = query.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() != 6 {
        return None;
    }
    let suffix = if digits.starts_with('6') || digits.starts_with('9') {
        "SH"
    } else {
        "SZ"
    };
    Some(format!("{digits}.{suffix}"))
}

fn query_references_listed_ticker(query: &str) -> bool {
    if query.contains(".SH") || query.contains(".SZ") {
        return true;
    }
    let bytes = query.as_bytes();
    bytes
        .windows(6)
        .any(|w| w.iter().all(|c| c.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::FunctionCall;
    use hermes_core::JsonSchema;

    fn tc(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.into(),
            },
            extra_content: None,
        }
    }

    #[test]
    fn blocks_web_after_get_market_data_until_analyze() {
        let schemas = vec![ToolSchema::new(
            "analyze_stock",
            "",
            JsonSchema::new("object"),
        )];
        let mut gate = EquityResearchGate::from_tool_schemas(&schemas);
        let md_ok = ToolResult {
            tool_call_id: "1".into(),
            content: r#"{"symbol":"600584.SH","rows":[]}"#.into(),
            is_error: false,
        };
        gate.record_tool_batch(
            &[tc("1", "get_market_data", r#"{"symbol":"600584.SH"}"#)],
            &[md_ok],
        );
        let mut batch = vec![tc("2", "web_search", r#"{"query":"news"}"#)];
        let blocked = gate.gate_tool_calls(&mut batch);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn seed_from_user_message_blocks_web() {
        let schemas = vec![ToolSchema::new(
            "analyze_stock",
            "",
            JsonSchema::new("object"),
        )];
        let mut gate = EquityResearchGate::from_tool_schemas(&schemas);
        gate.seed_pending_symbol("600584.SH");
        let mut batch = vec![tc("1", "web_search", r#"{"query":"长电科技"}"#)];
        let blocked = gate.gate_tool_calls(&mut batch);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn blocks_web_after_a_share_quote_until_analyze() {
        let schemas = vec![ToolSchema::new(
            "analyze_stock",
            "",
            JsonSchema::new("object"),
        )];
        let mut gate = EquityResearchGate::from_tool_schemas(&schemas);
        let quote_ok = ToolResult {
            tool_call_id: "1".into(),
            content: r#"{"symbol":"300750.SZ","price":1.0}"#.into(),
            is_error: false,
        };
        gate.record_tool_batch(
            &[tc("1", "get_quote", r#"{"symbol":"300750.SZ"}"#)],
            &[quote_ok],
        );

        let mut batch = vec![tc("2", "web_search", r#"{"query":"foo"}"#)];
        let blocked = gate.gate_tool_calls(&mut batch);
        assert_eq!(blocked.len(), 1);
        assert!(batch.is_empty());

        let analyze_ok = ToolResult {
            tool_call_id: "3".into(),
            content: r#"{"symbol":"300750.SZ","dcf":{}}"#.into(),
            is_error: false,
        };
        gate.record_tool_batch(
            &[tc("3", "analyze_stock", r#"{"symbol":"300750.SZ"}"#)],
            &[analyze_ok],
        );
        let mut batch2 = vec![tc("4", "web_search", r#"{"query":"gap fill"}"#)];
        let blocked2 = gate.gate_tool_calls(&mut batch2);
        assert!(blocked2.is_empty());
        assert_eq!(batch2.len(), 1);
    }

    #[test]
    fn blocks_web_when_query_has_ticker_code() {
        let schemas = vec![ToolSchema::new(
            "analyze_stock",
            "",
            JsonSchema::new("object"),
        )];
        let mut gate = EquityResearchGate::from_tool_schemas(&schemas);
        let mut batch = vec![tc("1", "web_search", r#"{"query":"300750 earnings"}"#)];
        let blocked = gate.gate_tool_calls(&mut batch);
        assert_eq!(blocked.len(), 1);
    }

    #[test]
    fn no_block_without_analyze_stock_tool() {
        let mut gate = EquityResearchGate::from_tool_schemas(&[]);
        let mut batch = vec![tc("1", "web_search", r#"{"query":"300750"}"#)];
        let blocked = gate.gate_tool_calls(&mut batch);
        assert!(blocked.is_empty());
        assert_eq!(batch.len(), 1);
    }
}
