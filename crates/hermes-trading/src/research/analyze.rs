//! End-to-end stock analysis orchestrator.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::quote_data::QuoteData;
use crate::research::models::{
    CompsPeer, CompsTarget, ThreeStmtResult, build_comps_table, compute_dcf, project_three_stmt,
    quick_lbo,
};
use crate::research::scoring::{generate_panel, score_dimensions};
use crate::research::types::{DataConfidence, FeatureVector, FundamentalsSnapshot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalyzeStockResult {
    pub symbol: String,
    pub dcf: Value,
    pub comps: Value,
    pub three_statement: Value,
    pub lbo: Value,
    pub scores: Value,
    pub personas: Value,
    pub data_confidence: DataConfidence,
    pub missing_dims: Vec<String>,
    pub used_fallback: Vec<String>,
}

/// Run full analysis pipeline on a fundamentals snapshot.
#[must_use]
pub fn analyze_stock(
    snap: &FundamentalsSnapshot,
    raw_dims: Option<&Value>,
    peers: Option<&[CompsPeer]>,
) -> AnalyzeStockResult {
    let features: FeatureVector = snap.clone();
    let mut used_fallback = Vec::new();

    let dcf = compute_dcf(&features, None);
    used_fallback.extend(dcf.used_fallback.clone());

    let target = CompsTarget {
        price: snap.price,
        pe: snap.pe,
        pb: snap.pb,
        eps: snap.eps,
        bvps: snap.bvps,
        ..Default::default()
    };
    let comps = match peers {
        Some(p) if !p.is_empty() => {
            serde_json::to_value(build_comps_table(target, p)).unwrap_or(Value::Null)
        }
        _ => serde_json::json!({"error": "no peers provided"}),
    };

    let three_stmt = match project_three_stmt(&features, None) {
        ThreeStmtResult::Ok(ok) => {
            used_fallback.extend(ok.used_fallback.clone());
            serde_json::to_value(ok).unwrap_or(Value::Null)
        }
        ThreeStmtResult::Error { error, .. } => {
            serde_json::json!({"error": error})
        }
    };

    let lbo = quick_lbo(&features, None);
    used_fallback.extend(lbo.used_fallback.clone());

    let dims = raw_dims
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    let scored = score_dimensions(&snap.symbol, &dims, &features);
    let missing_dims: Vec<String> = scored
        .dimensions
        .values()
        .flat_map(|d| d.missing.clone())
        .collect();
    let panel = generate_panel(&scored, &features);

    AnalyzeStockResult {
        symbol: snap.symbol.clone(),
        dcf: serde_json::to_value(&dcf).unwrap_or(Value::Null),
        comps,
        three_statement: three_stmt,
        lbo: serde_json::to_value(&lbo).unwrap_or(Value::Null),
        scores: serde_json::to_value(&scored).unwrap_or(Value::Null),
        personas: serde_json::to_value(&panel).unwrap_or(Value::Null),
        data_confidence: DataConfidence::from_snapshot(snap),
        missing_dims,
        used_fallback,
    }
}

/// Build snapshot from quote + optional JSON fundamentals.
#[must_use]
pub fn snapshot_from_inputs(
    quote: &QuoteData,
    fundamentals: Option<&Value>,
) -> FundamentalsSnapshot {
    let mut snap = FundamentalsSnapshot::from_quote(quote);
    if let Some(f) = fundamentals {
        snap.merge_json(f);
    }
    snap
}
