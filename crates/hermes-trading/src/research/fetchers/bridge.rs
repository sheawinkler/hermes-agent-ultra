//! Apply dimension results → `FundamentalsSnapshot` + feature fields.

use serde_json::Value;

use super::types::{CollectOutput, DimQuality, DimResult};
use crate::research::types::{FundamentalsSnapshot, ProvenanceSource};

/// Merge all HTTP dimension outputs into one snapshot (additive).
pub fn apply_dims_to_snapshot(snap: &mut FundamentalsSnapshot, output: &CollectOutput) {
    for result in output.dims.values() {
        if matches!(result.quality, DimQuality::Skipped | DimQuality::Error) {
            continue;
        }
        apply_one_dim(snap, result);
    }
}

fn apply_one_dim(snap: &mut FundamentalsSnapshot, result: &DimResult) {
    match result.dim_key.as_str() {
        "0_basic" => apply_basic(snap, &result.data),
        "1_financials" => apply_financials(snap, &result.data),
        "2_kline" => apply_kline(snap, &result.data),
        "10_valuation" => apply_valuation(snap, &result.data),
        "16_lhb" => apply_lhb(snap, &result.data),
        "7_industry" => apply_industry(snap, &result.data),
        _ => {}
    }
}

fn mark(snap: &mut FundamentalsSnapshot, field: &str) {
    snap.provenance
        .insert(field.into(), ProvenanceSource::Provider);
}

fn set_f64(snap: &mut FundamentalsSnapshot, field: &str, data: &Value, key: &str) {
    if let Some(v) = data.get(key).and_then(|v| v.as_f64()) {
        match field {
            "price" => snap.price = Some(v),
            "pe" => snap.pe = Some(v),
            "pb" => snap.pb = Some(v),
            "eps" => snap.eps = Some(v),
            "ps" => snap.ps = Some(v),
            "bvps" => snap.bvps = Some(v),
            "market_cap_yi" => snap.market_cap_yi = Some(v),
            "shares_outstanding_yi" => snap.shares_outstanding_yi = Some(v),
            "roe_latest" => snap.roe_latest = Some(v),
            "net_margin" => snap.net_margin = Some(v),
            "gross_margin" => snap.gross_margin = Some(v),
            "revenue_growth_latest" => snap.revenue_growth_latest = Some(v),
            "fcf_latest_yi" => snap.fcf_latest_yi = Some(v),
            "revenue_latest_yi" => snap.revenue_latest_yi = Some(v),
            "equity_yi" => snap.equity_yi = Some(v),
            "total_debt_yi" => snap.total_debt_yi = Some(v),
            "cash_yi" => snap.cash_yi = Some(v),
            "ebitda_yi" => snap.ebitda_yi = Some(v),
            "debt_ratio" => snap.debt_ratio = Some(v),
            "current_ratio" => snap.current_ratio = Some(v),
            "fcf_margin" => snap.fcf_margin = Some(v),
            "max_drawdown_1y" => snap.max_drawdown_1y = Some(v),
            "pe_quantile_5y" => snap.pe_quantile_5y = Some(v),
            "industry_pe" => snap.industry_pe = Some(v),
            _ => {}
        }
        mark(snap, field);
    }
}

fn apply_basic(snap: &mut FundamentalsSnapshot, data: &Value) {
    if let Some(v) = data.get("name").and_then(|v| v.as_str()) {
        snap.name = Some(v.to_string());
        mark(snap, "name");
    }
    if let Some(v) = data.get("industry").and_then(|v| v.as_str()) {
        snap.industry = Some(v.to_string());
        mark(snap, "industry");
    }
    set_f64(snap, "price", data, "price");
    set_f64(snap, "pe", data, "pe_ttm");
    set_f64(snap, "pb", data, "pb");
    set_f64(snap, "eps", data, "eps");
    set_f64(snap, "market_cap_yi", data, "market_cap_yi");
    set_f64(snap, "shares_outstanding_yi", data, "shares_outstanding_yi");
}

fn apply_financials(snap: &mut FundamentalsSnapshot, data: &Value) {
    set_f64(snap, "roe_latest", data, "roe");
    set_f64(snap, "net_margin", data, "net_margin");
    set_f64(snap, "gross_margin", data, "gross_margin");
    set_f64(snap, "revenue_growth_latest", data, "revenue_growth");
    set_f64(snap, "fcf_latest_yi", data, "fcf_yi");
    set_f64(snap, "revenue_latest_yi", data, "revenue_latest_yi");
    set_f64(snap, "equity_yi", data, "equity_yi");
    set_f64(snap, "total_debt_yi", data, "total_debt_yi");
    set_f64(snap, "cash_yi", data, "cash_yi");
    set_f64(snap, "ebitda_yi", data, "ebitda_yi");
    if let Some(h) = data.get("financial_health") {
        set_f64(snap, "debt_ratio", h, "debt_ratio");
        set_f64(snap, "current_ratio", h, "current_ratio");
        set_f64(snap, "fcf_margin", h, "fcf_margin");
    }
    if let Some(arr) = data.get("roe_history").and_then(|v| v.as_array()) {
        snap.roe_history = arr.iter().filter_map(|v| v.as_f64()).collect();
        if !snap.roe_history.is_empty() {
            mark(snap, "roe_history");
        }
    }
    if let Some(arr) = data.get("revenue_history").and_then(|v| v.as_array()) {
        snap.revenue_history = arr.iter().filter_map(|v| v.as_f64()).collect();
        if !snap.revenue_history.is_empty() {
            mark(snap, "revenue_history");
        }
    }
    if data
        .get("fcf_positive")
        .and_then(|v| v.as_bool())
        .is_some_and(|b| b)
    {
        snap.fcf_positive = Some(true);
        mark(snap, "fcf_positive");
    }
}

fn apply_kline(snap: &mut FundamentalsSnapshot, data: &Value) {
    if let Some(v) = data.get("stage").and_then(|v| v.as_str()) {
        snap.stage = Some(v.to_string());
        mark(snap, "stage");
    }
    if let Some(v) = data.get("ma_align").and_then(|v| v.as_str()) {
        snap.ma_align = Some(v.to_string());
        mark(snap, "ma_align");
    }
    if let Some(stats) = data.get("kline_stats") {
        set_f64(snap, "max_drawdown_1y", stats, "max_drawdown");
    }
}

fn apply_valuation(snap: &mut FundamentalsSnapshot, data: &Value) {
    set_f64(snap, "pe", data, "pe_ttm");
    set_f64(snap, "pb", data, "pb");
    set_f64(snap, "ps", data, "ps_ttm");
    set_f64(snap, "eps", data, "eps");
    set_f64(snap, "bvps", data, "bvps");
    set_f64(snap, "pe_quantile_5y", data, "pe_percentile");
    set_f64(snap, "industry_pe", data, "industry_pe");
}

fn apply_lhb(snap: &mut FundamentalsSnapshot, data: &Value) {
    if let Some(arr) = data.get("matched_youzi").and_then(|v| v.as_array()) {
        snap.matched_youzi = arr
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        if !snap.matched_youzi.is_empty() {
            mark(snap, "matched_youzi");
        }
    }
}

fn apply_industry(snap: &mut FundamentalsSnapshot, data: &Value) {
    if let Some(v) = data.get("industry").and_then(|v| v.as_str()) {
        snap.industry = Some(v.to_string());
        mark(snap, "industry");
    }
    set_f64(snap, "industry_pe", data, "industry_pe");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::fetchers::types::{CollectOutput, DimQuality, Market};

    #[test]
    fn build_raw_dims_shape() {
        let mut output = CollectOutput {
            ticker: "600809.SH".into(),
            market: Market::A,
            dims: Default::default(),
        };
        output.dims.insert(
            "1_financials".into(),
            DimResult::ok(
                "1_financials",
                "600809.SH",
                serde_json::json!({"roe": 28.0, "net_margin": 32.0}),
                "eastmoney_financials",
                DimQuality::Partial,
            ),
        );
        let raw = output.build_raw_dims();
        assert!(
            raw.get("1_financials")
                .and_then(|v| v.get("data"))
                .is_some()
        );
    }
}
