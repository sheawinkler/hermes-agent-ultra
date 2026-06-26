//! In-memory cache of the latest `analyze_stock` structured result per symbol+depth.
//!
//! Populated by the handler before RTK/registry truncation; consumed by slash delivery.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use hermes_trading::research::analyze::AnalyzeStockResult;

fn cache() -> &'static Mutex<HashMap<String, AnalyzeStockResult>> {
    static CACHE: OnceLock<Mutex<HashMap<String, AnalyzeStockResult>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(symbol: &str, depth: &str) -> String {
    format!("{}|{}", symbol.trim().to_uppercase(), depth.trim())
}

/// Store structured analysis output (overwrites prior entry for the same symbol+depth).
pub fn store(symbol: &str, depth: &str, result: AnalyzeStockResult) {
    if let Ok(mut guard) = cache().lock() {
        guard.insert(cache_key(symbol, depth), result);
    }
}

/// Peek cached result without consuming (web-fill phase before slash delivery).
#[must_use]
pub fn get(symbol: &str, depth: &str) -> Option<AnalyzeStockResult> {
    cache()
        .lock()
        .ok()
        .and_then(|guard| guard.get(&cache_key(symbol, depth)).cloned())
}

/// Take (consume) cached result for slash delivery.
#[must_use]
pub fn take(symbol: &str, depth: &str) -> Option<AnalyzeStockResult> {
    cache()
        .lock()
        .ok()
        .and_then(|mut guard| guard.remove(&cache_key(symbol, depth)))
}

#[cfg(test)]
pub(crate) fn clear_for_tests() {
    if let Ok(mut guard) = cache().lock() {
        guard.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_trading::research::synthesis::SynthesisReport;

    fn minimal_result(symbol: &str, depth: &str) -> AnalyzeStockResult {
        AnalyzeStockResult {
            symbol: symbol.into(),
            depth: depth.into(),
            dcf: serde_json::json!({}),
            comps: serde_json::json!({}),
            three_statement: serde_json::json!({}),
            lbo: serde_json::json!({}),
            scores: serde_json::json!({"fundamental_score": 5.0, "dimensions": {}}),
            personas: serde_json::json!({"panel_consensus": 1.0, "investors": []}),
            data_confidence: hermes_trading::research::types::DataConfidence {
                score: 0.5,
                present: vec![],
                missing: vec![],
            },
            missing_dims: vec![],
            dim_summary: vec![],
            used_fallback: vec![],
            summary_markdown: format!("## {symbol} · scan"),
            synthesis: SynthesisReport {
                headline: "h".into(),
                verdict: "hold".into(),
                confidence_tier: "medium".into(),
                key_metrics: vec![],
                risks: vec![],
                missing_highlights: vec![],
                panel_summary: hermes_trading::research::synthesis::PanelSummary {
                    consensus: 1.0,
                    vote_buy: 1,
                    vote_avoid: 0,
                    investor_count: 1,
                },
                dcf_one_liner: "dcf".into(),
            },
        }
    }

    #[test]
    fn store_get_and_take() {
        store("600522.SH", "medium", minimal_result("600522.SH", "medium"));
        assert!(get("600522.sh", "medium").is_some());
        let got = take("600522.SH", "medium").expect("cached");
        assert_eq!(got.symbol, "600522.SH");
        assert!(get("600522.SH", "medium").is_none());
        assert!(take("600522.SH", "medium").is_none(), "take consumes entry");
    }
}
