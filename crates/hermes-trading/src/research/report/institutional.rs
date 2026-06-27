//! Institutional standalone HTML report (content-first layout).

use crate::research::analyze::AnalyzeStockResult;
use crate::research::report::sections::escape_html;
use crate::research::report::sections::{
    render_appendix, render_comps_section, render_core_section, render_dcf_section,
    render_dimensions_section_with_raw_limit, render_external_section, render_flows_section,
    render_fundamentals_section, render_panel_section, render_sector_section, render_shell_start,
    render_warn_banner,
};
use crate::research::report_filter::{
    show_gaps_section, user_missing_dims, user_missing_highlights,
};
use crate::research::scoring::{PanelResult, ScoreDimensionsResult};

const CONFIDENCE_WARN_THRESHOLD: f64 = 0.55;

/// UZI-style reports target ~400KB; tiered trim keeps DEEP SCAN + DCF under pressure.
pub const MAX_HTML_BYTES: usize = 220_000;

#[derive(Debug, Clone, Copy)]
struct InstitutionalRenderOpts {
    raw_dump_max_bytes: usize,
    panel_include_full: bool,
    include_appendix: bool,
    narrative_max_chars: Option<usize>,
}

impl Default for InstitutionalRenderOpts {
    fn default() -> Self {
        Self {
            raw_dump_max_bytes: 1500,
            panel_include_full: true,
            include_appendix: true,
            narrative_max_chars: None,
        }
    }
}

const TRIM_TIERS: &[(&str, InstitutionalRenderOpts)] = &[
    (
        "full",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 1500,
            panel_include_full: true,
            include_appendix: true,
            narrative_max_chars: None,
        },
    ),
    (
        "raw_dump_800",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 800,
            panel_include_full: true,
            include_appendix: true,
            narrative_max_chars: None,
        },
    ),
    (
        "no_raw_dump",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 0,
            panel_include_full: true,
            include_appendix: true,
            narrative_max_chars: None,
        },
    ),
    (
        "panel_top20_only",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 0,
            panel_include_full: false,
            include_appendix: true,
            narrative_max_chars: None,
        },
    ),
    (
        "no_appendix",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 0,
            panel_include_full: false,
            include_appendix: false,
            narrative_max_chars: None,
        },
    ),
    (
        "narrative_4k",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 0,
            panel_include_full: false,
            include_appendix: false,
            narrative_max_chars: Some(4000),
        },
    ),
    (
        "minimal",
        InstitutionalRenderOpts {
            raw_dump_max_bytes: 0,
            panel_include_full: false,
            include_appendix: false,
            narrative_max_chars: Some(0),
        },
    ),
];

/// Render institutional HTML from a completed analysis (uses embedded `synthesis` + `content`).
#[must_use]
pub fn render_institutional_html(result: &AnalyzeStockResult, narrative: Option<&str>) -> String {
    render_institutional_html_with_cap(result, narrative, MAX_HTML_BYTES)
}

#[must_use]
fn render_institutional_html_with_cap(
    result: &AnalyzeStockResult,
    narrative: Option<&str>,
    max_bytes: usize,
) -> String {
    let mut last = String::new();
    let mut last_tier = "full";

    for (tier, opts) in TRIM_TIERS {
        last = render_institutional_html_inner(result, narrative, opts);
        last_tier = tier;
        if last.len() <= max_bytes {
            if *tier != "full" {
                tracing::warn!(
                    symbol = %result.symbol,
                    bytes = last.len(),
                    max = max_bytes,
                    tier = tier,
                    "institutional HTML trimmed to fit size cap"
                );
            }
            return last;
        }
    }

    tracing::error!(
        symbol = %result.symbol,
        bytes = last.len(),
        max = max_bytes,
        tier = last_tier,
        "institutional HTML still exceeds size cap after all trim tiers"
    );
    last
}

fn render_institutional_html_inner(
    result: &AnalyzeStockResult,
    narrative: Option<&str>,
    opts: &InstitutionalRenderOpts,
) -> String {
    let missing_dims = user_missing_dims(&result.missing_dims);
    let mut syn = result.synthesis.clone();
    syn.missing_highlights =
        user_missing_highlights(&result.data_confidence.missing, &missing_dims);
    let syn = &syn;
    let scored: ScoreDimensionsResult =
        serde_json::from_value(result.scores.clone()).unwrap_or(ScoreDimensionsResult {
            ticker: result.symbol.clone(),
            fundamental_score: 0.0,
            dimensions: Default::default(),
        });
    let panel: PanelResult =
        serde_json::from_value(result.personas.clone()).unwrap_or(PanelResult {
            investors: Vec::new(),
            vote_distribution: Default::default(),
            signal_distribution: Default::default(),
            panel_consensus: scored.fundamental_score,
        });

    let identity = crate::research::report::ReportIdentity::from_analyze_result(result);

    let mut html = render_shell_start(&identity, syn);
    if result.data_confidence.score < CONFIDENCE_WARN_THRESHOLD {
        html.push_str(&render_warn_banner(result.data_confidence.score));
    }
    html.push_str(&render_core_section(
        &identity,
        syn,
        &result.content,
        &result.raw_dims,
        &scored,
    ));
    html.push_str(&render_fundamentals_section(&result.content.fundamentals));
    html.push_str(&render_sector_section(&result.content.sector));
    html.push_str(&render_comps_section(&result.comps));
    html.push_str(&render_external_section(&result.content.external));
    html.push_str(&render_flows_section(&result.content.flows_events));
    html.push_str(&render_dimensions_section_with_raw_limit(
        &scored,
        &result.content.external,
        &result.raw_dims,
        opts.raw_dump_max_bytes,
    ));
    html.push_str(&render_panel_section(&panel, opts.panel_include_full));
    html.push_str(&render_dcf_section(&result.dcf));
    if show_gaps_section(&missing_dims, &syn.missing_highlights) {
        html.push_str(&render_gaps_section(&missing_dims, &syn.missing_highlights));
    }
    if !syn.risks.is_empty() {
        html.push_str(&render_risks_section(&syn.risks));
    }
    if opts.include_appendix {
        html.push_str(&render_appendix(result));
    }
    if let Some(text) = narrative_for_render(narrative, opts.narrative_max_chars) {
        html.push_str(&render_narrative_section(&text));
    }
    html.push_str("</body></html>");
    html
}

fn narrative_for_render(text: Option<&str>, max_chars: Option<usize>) -> Option<String> {
    let text = text?;
    match max_chars {
        None => Some(text.to_string()),
        Some(0) => None,
        Some(max) => {
            let count = text.chars().count();
            if count <= max {
                Some(text.to_string())
            } else {
                Some(format!("{}…", text.chars().take(max).collect::<String>()))
            }
        }
    }
}

fn render_gaps_section(missing_dims: &[String], highlights: &[String]) -> String {
    use crate::research::report::dim_viz::render_missing_chip;
    let mut chips: Vec<String> = highlights
        .iter()
        .map(|h| render_missing_chip(&escape_html(h)))
        .collect();
    for d in missing_dims {
        let esc = escape_html(d);
        if !highlights.iter().any(|h| h == d) {
            chips.push(render_missing_chip(&esc));
        }
    }
    format!(
        r#"<section class="card"><h2>数据缺口</h2><div class="chips">{}</div></section>"#,
        chips.join("")
    )
}

fn render_risks_section(risks: &[String]) -> String {
    let items: String = risks
        .iter()
        .map(|r| format!("<li>{}</li>", escape_html(r)))
        .collect();
    format!(r#"<section class="card"><h2>关键风险</h2><ul class="risk">{items}</ul></section>"#)
}

fn render_narrative_section(text: &str) -> String {
    format!(
        r#"<section class="card"><h2>分析结论</h2><div class="narrative">{}</div></section>"#,
        escape_html(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::analyze::analyze_stock;
    use crate::research::fetchers::bridge::apply_dims_to_snapshot;
    use crate::research::fetchers::types::{CollectOutput, DimQuality, DimResult, Market};
    use crate::research::profile::AnalysisProfile;
    use crate::research::report::labels::DIM_ORDER;
    use crate::research::types::FundamentalsSnapshot;
    use serde_json::{Value, json};

    fn moutai_result() -> AnalyzeStockResult {
        let symbol = "600519.SH";
        let dims = json!({
            "0_basic": { "data": { "name": "贵州茅台", "industry": "白酒", "price": 1680.0, "pe_ttm": 28.5, "pb": 8.2, "market_cap_yi": 21000, "shares_outstanding_yi": 12.56, "eps": 58.0 } },
            "1_financials": { "data": { "roe": 32.0, "net_margin": 52.0, "revenue_latest_yi": 1500, "revenue_history": [1200.0, 1300.0, 1400.0, 1500.0], "roe_history": [28.0, 30.0, 31.0, 32.0], "financial_health": { "debt_ratio": 18.0, "current_ratio": 2.1 }, "eps": 58.0 } },
            "2_kline": { "data": { "stage": "Stage 2 上升", "ma_align": "多头排列", "ma5": 1670.0, "ma20": 1650.0, "ma60": 1600.0 } },
            "10_valuation": { "data": { "pe_ttm": 28.5, "pe_percentile": 35.0 } },
            "4_peers": { "data": { "peer_table": [
                { "name": "五粮液", "ticker": "000858", "pe": 18.0, "pb": 4.2, "roe": 22.0 },
                { "name": "泸州老窖", "ticker": "000568", "pe": 22.0, "pb": 5.1, "roe": 28.0 }
            ] } },
            "6_research": { "data": { "research_count": 10 } },
            "7_industry": { "data": { "industry": "白酒", "growth": 12.0, "industry_pe": 22.0 } },
            "6_fund_holders": { "data": { "holder_change_ratio": -8.0, "holder_count": 95000 } },
            "12_capital_flow": { "data": { "main_fund_5d_net_yi": 3.5 } }
        });
        let mut collect = CollectOutput {
            ticker: symbol.into(),
            market: Market::A,
            dims: Default::default(),
        };
        if let Some(obj) = dims.as_object() {
            for (key, wrapper) in obj {
                let data = wrapper.get("data").cloned().unwrap_or(Value::Null);
                collect.dims.insert(
                    key.clone(),
                    DimResult::ok(key, symbol, data, "fixture", DimQuality::Partial),
                );
            }
        }
        let raw_dims = collect.build_raw_dims();
        let mut snap = FundamentalsSnapshot {
            symbol: symbol.into(),
            ..Default::default()
        };
        apply_dims_to_snapshot(&mut snap, &collect);
        analyze_stock(
            &snap,
            Some(&raw_dims),
            None,
            &AnalysisProfile::medium(),
            Some(&collect),
        )
    }

    fn heavy_result() -> AnalyzeStockResult {
        let mut result = moutai_result();
        let blob = "x".repeat(4000);
        let mut raw = serde_json::Map::new();
        for key in DIM_ORDER {
            raw.insert(
                (*key).into(),
                json!({ "data": { "payload": blob, "nested": { "more": blob.clone() } } }),
            );
        }
        raw.insert(
            "6_fund_holders".into(),
            json!({ "data": { "payload": blob } }),
        );
        result.raw_dims = Value::Object(raw);
        result
    }

    #[test]
    fn p0_html_preview_dump() {
        use std::path::PathBuf;

        let result = moutai_result();
        let html = render_institutional_html(
            &result,
            Some("P0 预览：institutional HTML 含 CORE / DEEP SCAN / DCF 三节。"),
        );
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/p0-report-preview.html");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create target dir");
        }
        std::fs::write(&path, &html).expect("write preview html");

        let checks = [
            ("01 / CORE", html.contains("01 / CORE")),
            ("05 / DEEP SCAN", html.contains("05 / DEEP SCAN")),
            ("06 / VALUATION", html.contains("06 / VALUATION")),
            ("07 / COMPS", html.contains("07 / COMPS")),
            ("DCF VALUATION", html.contains("DCF VALUATION")),
            ("COMPS VALUATION", html.contains("COMPS VALUATION")),
            ("sens-heatmap", html.contains("sens-heatmap")),
            ("dim-card", html.contains("dim-card")),
            ("查看原始数据", html.contains("查看原始数据")),
        ];
        for (label, ok) in checks {
            assert!(ok, "preview html missing {label}");
        }
        assert!(html.len() <= MAX_HTML_BYTES);

        eprintln!("\n=== P0 HTML PREVIEW ===");
        eprintln!(
            "path: {}",
            path.canonicalize().unwrap_or(path.clone()).display()
        );
        eprintln!("bytes: {}", html.len());
        for (label, ok) in checks {
            eprintln!("  [{ok}] {label}");
        }
        eprintln!("=======================\n");
    }

    #[test]
    fn institutional_html_contains_content_sections() {
        let html = render_institutional_html(&moutai_result(), None);
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("600519.SH"));
        assert!(html.contains("01 / CORE"));
        assert!(html.contains("核心结论"));
        assert!(html.contains("公司基本面"));
        assert!(html.contains("板块与同业"));
        assert!(html.contains("07 / COMPS"));
        assert!(html.contains("COMPS VALUATION"));
        assert!(html.contains("政策 / 宏观 / 舆情"));
        assert!(html.contains("全维深度透视"));
        assert!(html.contains("dim-card"));
        assert!(html.contains("dim-viz"));
        assert!(html.contains("66 位评委"));
        assert!(html.contains("06 / VALUATION"));
        assert!(html.contains("DCF VALUATION"));
        assert!(html.contains("sens-heatmap"));
        assert!(html.len() <= MAX_HTML_BYTES);
    }

    #[test]
    fn institutional_html_shows_warn_when_low_confidence() {
        let mut result = moutai_result();
        result.data_confidence.score = 0.40;
        result.synthesis.confidence_tier = "low".into();
        let html = render_institutional_html(&result, None);
        assert!(html.contains("数据置信度"));
    }

    #[test]
    fn institutional_html_trims_preserves_dcf_and_scan() {
        let narrative = "分析".repeat(20_000);
        let html = render_institutional_html_with_cap(&heavy_result(), Some(&narrative), 90_000);
        assert!(
            html.len() <= 90_000,
            "expected trim under cap, got {} bytes",
            html.len()
        );
        assert!(html.contains("05 / DEEP SCAN"));
        assert!(html.contains("06 / VALUATION"));
        assert!(html.contains("DCF VALUATION"));
        assert!(html.contains("sens-heatmap"));
        assert!(html.contains("dim-card"));
    }

    #[test]
    fn trim_tiers_drop_raw_dump_before_dcf() {
        let full = render_institutional_html_inner(
            &heavy_result(),
            None,
            &InstitutionalRenderOpts::default(),
        );
        let no_raw = render_institutional_html_inner(&heavy_result(), None, &TRIM_TIERS[2].1);
        assert!(full.contains("查看原始数据"));
        assert!(!no_raw.contains("查看原始数据"));
        assert!(no_raw.contains("06 / VALUATION"));
    }
}
