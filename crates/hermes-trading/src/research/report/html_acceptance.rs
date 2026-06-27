//! HTML acceptance helpers: structural CSS checks + numeric parity vs source JSON.

use serde_json::Value;

use crate::research::models::comps::{CompsOk, CompsResult};
use crate::research::report::sections::escape_html;

/// Extract inner HTML of `<section … id="{id}"…>…</section>` (first match).
#[must_use]
pub fn extract_section_by_id(html: &str, id: &str) -> Option<String> {
    let needle = format!(r#"id="{id}""#);
    let id_pos = html.find(&needle)?;
    let start = html[..id_pos].rfind("<section")?;
    let mut depth = 0i32;
    let mut i = start;
    while i < html.len() {
        if html[i..].starts_with("<section") {
            depth += 1;
            i += "<section".len();
            continue;
        }
        if html[i..].starts_with("</section>") {
            depth -= 1;
            i += "</section>".len();
            if depth == 0 {
                return Some(html[start..i].to_string());
            }
            continue;
        }
        i += html[i..].chars().next().map_or(1, |c| c.len_utf8());
    }
    None
}

/// Required CSS hooks for P0/P1 institutional layout.
#[must_use]
pub fn institutional_style_markers_present(html: &str) -> Vec<&'static str> {
    const MARKERS: &[&str] = &[
        "dashboard-bento",
        "deep-scan",
        "dim-card",
        "dcf-block",
        "comps-block",
        "comps-summary",
        "comps-stats",
        "comps-peers",
        "sens-heatmap",
        "section-tag",
        "section-line",
    ];
    MARKERS
        .iter()
        .copied()
        .filter(|m| !html.contains(m))
        .collect()
}

fn parse_comps_ok(comps: &Value) -> Option<CompsOk> {
    if comps.get("skipped").is_some() || comps.get("error").is_some() {
        return None;
    }
    match serde_json::from_value::<CompsResult>(comps.clone()) {
        Ok(CompsResult::Ok(ok)) => Some(ok),
        _ => serde_json::from_value(comps.clone()).ok(),
    }
}

/// Assert COMPS section numbers/strings match `result.comps` JSON.
pub fn assert_comps_html_parity(html: &str, comps: &Value) -> Result<(), String> {
    let ok = parse_comps_ok(comps).ok_or_else(|| "comps skipped or error".to_string())?;
    let section = extract_section_by_id(html, "section-comps")
        .ok_or_else(|| "missing #section-comps".to_string())?;

    if !section.contains("07 / COMPS") {
        return Err("COMPS section tag missing".into());
    }
    for class in ["comps-block", "comps-summary", "comps-stats", "comps-peers"] {
        if !section.contains(class) {
            return Err(format!("COMPS section missing .{class}"));
        }
    }

    let n = ok.peers.len().to_string();
    if !section.contains(&format!(r#"<div class="v">{n}</div>"#)) {
        return Err(format!("peer count {n} not in COMPS KPI"));
    }

    if let Some(pe_med) = ok.peer_stats.get("pe").map(|s| s.median) {
        let med_label = format!("{pe_med:.1}x");
        if !section.contains(&med_label) {
            return Err(format!("PE median {med_label} not in COMPS HTML"));
        }
        let med_cell = format!("{pe_med:.2}");
        if !section.contains(&med_cell) {
            return Err(format!("PE median cell {med_cell} not in stats table"));
        }
    }

    if let Some(pct) = ok.target_percentile.get("pe") {
        let pct_label = format!("{pct:.0}%");
        if !section.contains(&pct_label) {
            return Err(format!("PE percentile {pct_label} not in COMPS HTML"));
        }
    }

    if let Some(implied) = ok.implied_price.get("via_median_pe") {
        let px = format!("¥{implied:.2}");
        if !section.contains(&px) {
            return Err(format!("implied price {px} not in COMPS HTML"));
        }
    }

    let verdict = escape_html(&ok.valuation_verdict);
    if !section.contains(&verdict) {
        return Err(format!("valuation_verdict {:?} not in COMPS HTML", ok.valuation_verdict));
    }

    for peer in &ok.peers {
        if let Some(name) = peer.name.as_deref().filter(|n| !n.is_empty())
            && !section.contains(name)
        {
            return Err(format!("peer name {name} missing from COMPS table"));
        }
        if let Some(pe) = peer.pe {
            let pe_str = format!("{pe:.2}");
            if !section.contains(&pe_str) {
                return Err(format!("peer PE {pe_str} missing from COMPS table"));
            }
        }
    }

    if let Some(name) = ok.target.name.as_deref().filter(|n| !n.is_empty()) {
        if !section.contains(name) {
            return Err(format!("target name {name} missing from COMPS table"));
        }
    }
    if let Some(pe) = ok.target.pe {
        let pe_str = format!("{pe:.2}");
        if !section.contains(&pe_str) {
            return Err(format!("target PE {pe_str} missing from COMPS table"));
        }
    }

    Ok(())
}

/// Assert DCF KPI strings match `result.dcf` JSON.
pub fn assert_dcf_html_parity(html: &str, dcf: &Value) -> Result<(), String> {
    let section = extract_section_by_id(html, "section-valuation")
        .ok_or_else(|| "missing #section-valuation".to_string())?;

    let intrinsic = dcf
        .get("intrinsic_per_share")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "dcf missing intrinsic_per_share".to_string())?;
    let px = format!("¥{intrinsic:.2}");
    if !section.contains(&px) {
        return Err(format!("intrinsic {px} not in DCF section"));
    }

    let sm = dcf
        .get("safety_margin_pct")
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "dcf missing safety_margin_pct".to_string())?;
    let sm_label = format!("{sm:+.1}%");
    if !section.contains(&sm_label) {
        return Err(format!("safety margin {sm_label} not in DCF section"));
    }

    let wacc = dcf
        .get("wacc_breakdown")
        .and_then(|v| v.get("wacc"))
        .and_then(|v| v.as_f64())
        .ok_or_else(|| "dcf missing wacc".to_string())?;
    let wacc_label = format!("{:.2}%", wacc * 100.0);
    if !section.contains(&wacc_label) {
        return Err(format!("WACC {wacc_label} not in DCF section"));
    }

    if !section.contains("sens-heatmap") {
        return Err("DCF heatmap table missing".into());
    }

    Ok(())
}

/// After web merge, external section and filled DEEP SCAN cards should show overlay text.
pub fn assert_web_merge_html(html: &str, overlay_snippets: &[&str]) -> Result<(), String> {
    if !html.contains("<h3>宏观环境</h3>") {
        return Err("macro section missing after web merge".into());
    }
    for snippet in overlay_snippets {
        if !html.contains(snippet) {
            return Err(format!("overlay snippet {snippet:?} not found in HTML"));
        }
    }
    Ok(())
}

/// Filled web dim must not still show the generic stub suffix in its label line.
pub fn assert_dim_label_not_stub(html: &str, filled_label: &str, stub_prefix: &str) -> Result<(), String> {
    if !html.contains(filled_label) {
        return Err(format!("filled label {filled_label:?} not in HTML"));
    }
    if html.contains(&format!("{stub_prefix} · 待 web 补数")) {
        return Err(format!("stub suffix still on {stub_prefix}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::analyze::{apply_external_context, analyze_stock};
    use crate::research::fetchers::bridge::apply_dims_to_snapshot;
    use crate::research::fetchers::types::{CollectOutput, DimQuality, DimResult, Market};
    use crate::research::profile::AnalysisProfile;
    use crate::research::report::ExternalContextOverlay;
    use crate::research::report::institutional::render_institutional_html;
    use crate::research::types::FundamentalsSnapshot;
    use serde_json::json;

    fn moutai_result() -> crate::research::analyze::AnalyzeStockResult {
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

    fn write_preview(name: &str, html: &str) -> std::path::PathBuf {
        use std::path::PathBuf;
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target").join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create target dir");
        }
        std::fs::write(&path, html).expect("write preview html");
        path
    }

    #[test]
    fn html_acceptance_moutai_comps_and_dcf_parity() {
        let result = moutai_result();
        let html = render_institutional_html(&result, None);

        let missing_styles = institutional_style_markers_present(&html);
        assert!(
            missing_styles.is_empty(),
            "missing CSS markers: {missing_styles:?}"
        );

        assert_comps_html_parity(&html, &result.comps).expect("COMPS parity");
        assert_dcf_html_parity(&html, &result.dcf).expect("DCF parity");

        let path = write_preview("p1-report-preview-moutai.html", &html);
        eprintln!("acceptance preview: {}", path.display());
    }

    #[test]
    fn html_acceptance_web_merge_parity_and_preview() {
        let mut result = moutai_result();
        let overlay = ExternalContextOverlay {
            macro_bullets: vec!["货币政策偏宽松".into()],
            policy_bullets: vec!["消费税政策总体稳定".into()],
            sentiment_bullets: vec!["机构关注度维持高位".into()],
            moat_bullets: vec!["品牌与渠道双护城河".into()],
            chain_bullets: vec!["白酒上游包材成本稳定".into()],
            governance_bullets: vec!["管理层稳定、分红意愿强".into()],
            rate_cycle: Some("宽松".into()),
            fx_trend: Some("人民币偏弱".into()),
            geo_risk: Some("地缘可控".into()),
            commodity: Some("大宗底部".into()),
            sources: vec!["gov.cn".into()],
            ..Default::default()
        };
        apply_external_context(&mut result, &overlay);

        let html = render_institutional_html(
            &result,
            Some("P1 验收：web merge 后 COMPS + DEEP SCAN + 外部专节"),
        );

        assert_comps_html_parity(&html, &result.comps).expect("COMPS parity after merge");
        assert_dcf_html_parity(&html, &result.dcf).expect("DCF parity after merge");
        assert_web_merge_html(
            &html,
            &[
                "货币政策偏宽松",
                "消费税政策总体稳定",
                "品牌与渠道双护城河",
                "白酒上游包材成本稳定",
            ],
        )
        .expect("web merge HTML");
        assert_dim_label_not_stub(&html, "品牌与渠道双护城河", "护城河需定性评估")
            .expect("moat dim not stub");
        assert_dim_label_not_stub(&html, "白酒上游包材成本稳定", "原材料成本关注中")
            .expect("materials dim not stub");

        assert!(html.contains("<h3>护城河</h3>"));
        assert!(html.contains("macro-quad") || html.contains("宽松"));

        let path = write_preview("p1-report-preview-web-filled.html", &html);
        eprintln!("web-filled preview: {}", path.display());
        eprintln!("bytes: {}", html.len());
    }
}
