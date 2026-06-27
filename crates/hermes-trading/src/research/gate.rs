//! Wave 2b entry gates G1–G2 (offline metrics + live smoke helpers).

use crate::research::fetchers::DimSummaryEntry;
use crate::research::types::{DataConfidence, FundamentalsSnapshot};

/// G1: share of collected HTTP dims that are `full` or `partial` (excludes skipped web dims).
#[must_use]
pub fn g1_hard_dim_ratio(summary: &[DimSummaryEntry]) -> f64 {
    if summary.is_empty() {
        return 0.0;
    }
    let ok = summary
        .iter()
        .filter(|e| e.quality == "full" || e.quality == "partial")
        .count();
    ok as f64 / summary.len() as f64
}

#[must_use]
pub fn g1_passes(summary: &[DimSummaryEntry], min_ratio: f64) -> bool {
    g1_hard_dim_ratio(summary) >= min_ratio
}

/// G2: snapshot-weighted confidence floor.
#[must_use]
pub fn g2_passes(snap: &FundamentalsSnapshot, min_score: f64) -> bool {
    DataConfidence::from_snapshot(snap).score >= min_score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::fetchers::DimSummaryEntry;

    #[test]
    fn g1_counts_full_and_partial_only() {
        let summary = vec![
            DimSummaryEntry {
                dim: "0_basic".into(),
                quality: "full".into(),
                source: "akshare".into(),
            },
            DimSummaryEntry {
                dim: "1_financials".into(),
                quality: "partial".into(),
                source: "akshare".into(),
            },
            DimSummaryEntry {
                dim: "2_kline".into(),
                quality: "full".into(),
                source: "akshare".into(),
            },
            DimSummaryEntry {
                dim: "15_events".into(),
                quality: "missing".into(),
                source: "akshare".into(),
            },
        ];
        assert!((g1_hard_dim_ratio(&summary) - 0.75).abs() < 1e-6);
        assert!(g1_passes(&summary, 0.70));
    }

    #[test]
    fn g2_uses_data_confidence_weights() {
        let snap = FundamentalsSnapshot {
            symbol: "600519.SH".into(),
            price: Some(1680.0),
            pe: Some(28.5),
            pb: Some(8.2),
            fcf_latest_yi: Some(600.0),
            revenue_latest_yi: Some(1500.0),
            net_margin: Some(52.0),
            roe_latest: Some(32.0),
            pe_quantile_5y: Some(35.0),
            debt_ratio: Some(18.0),
            market_cap_yi: Some(21000.0),
            shares_outstanding_yi: Some(12.56),
            industry: Some("白酒".into()),
            ..Default::default()
        };
        assert!(g2_passes(&snap, 0.55));
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_gate_600519_medium() {
        live_gate_medium("600519.SH", 0.55, true).await;
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_gate_688126_medium() {
        live_gate_medium("688126.SH", 0.40, true).await;
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_html_600519_smoke() {
        use crate::research::report::institutional::MAX_HTML_BYTES;
        use crate::research::report::render_institutional_html;

        let result = live_analyze_medium("600519.SH").await;
        let html = render_institutional_html(&result, Some("E2E live HTML smoke"));
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("600519.SH"));
        assert!(html.contains("公司基本面"));
        assert!(html.contains("板块与同业"));
        assert!(html.contains("01 / CORE"));
        assert!(html.contains("05 / DEEP SCAN"));
        assert!(html.contains("06 / VALUATION"));
        assert!(html.contains("已展示 19 维"));
        assert!(html.contains(&result.synthesis.headline));
        assert!(html.len() > 200);
        assert!(
            html.len() < MAX_HTML_BYTES,
            "HTML too large: {} bytes",
            html.len()
        );
        eprintln!("live HTML smoke ok: {} bytes", html.len());
    }

    async fn live_analyze_medium(symbol: &str) -> crate::research::analyze::AnalyzeStockResult {
        use crate::providers::{QuoteRouter, QuoteSource};
        use crate::research::analyze::analyze_stock;
        use crate::research::fetchers::enrich_snapshot;
        use crate::research::profile::AnalysisProfile;
        use crate::research::snapshot_from_inputs;

        let profile = AnalysisProfile::medium();
        let router = QuoteRouter::new();
        let quote = router
            .fetch_quote_with_source(symbol, QuoteSource::Auto, false)
            .await
            .expect("quote for live smoke");
        let mut snap = snapshot_from_inputs(&quote, None);
        let enriched = enrich_snapshot(&mut snap, symbol, Some(quote), &profile).await;
        analyze_stock(
            &snap,
            Some(&enriched.raw_dims),
            None,
            &profile,
            Some(&enriched.collect),
        )
    }

    #[tokio::test]
    #[ignore = "live network"]
    async fn live_gate_g2_600519_target_065() {
        let result = live_analyze_medium("600519.SH").await;
        let score = result.data_confidence.score;
        eprintln!("600519 G2 after data supplement: {score:.3}");
        assert!(
            score >= 0.65,
            "600519 G2 target 0.65 not met: {score:.3} missing={:?}",
            result.data_confidence.missing
        );
    }

    async fn live_gate_medium(symbol: &str, g2_min: f64, assert_g2: bool) {
        let result = live_analyze_medium(symbol).await;
        let g2_score = result.data_confidence.score;
        let g1_ratio = g1_hard_dim_ratio(&result.dim_summary);
        eprintln!(
            "{symbol} G1: {:.1}% ({}/{})",
            g1_ratio * 100.0,
            result
                .dim_summary
                .iter()
                .filter(|d| d.quality == "full" || d.quality == "partial")
                .count(),
            result.dim_summary.len()
        );
        for d in &result.dim_summary {
            eprintln!("  {} = {} ({})", d.dim, d.quality, d.source);
        }
        eprintln!(
            "{symbol} G2 confidence={:.3} present={:?} missing={:?}",
            result.data_confidence.score,
            result.data_confidence.present,
            result.data_confidence.missing
        );
        assert!(
            g1_passes(&result.dim_summary, 0.70),
            "{symbol} G1 below 70%: {g1_ratio:.1}%"
        );
        if assert_g2 {
            assert!(
                g2_score >= g2_min,
                "{symbol} G2 below {g2_min}: {g2_score:.3}"
            );
        } else if g2_score < g2_min {
            eprintln!("{symbol} G2 WARN: {g2_score:.3} < {g2_min} (offline golden still passes)");
        }
    }
}
