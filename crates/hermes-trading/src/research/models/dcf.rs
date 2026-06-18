//!  2-stage DCF with the Gordon Growth terminal + 5×5 sensitivity (UZI fin_models.compute_dcf).

use serde::{Deserialize, Serialize};

use super::wacc::{WaccInputs, compute_wacc};
use crate::research::types::{DcfAssumptions, FeatureVector, HasFallbacks};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SensitivityTable {
    pub wacc_axis: Vec<String>,
    pub g_axis: Vec<String>,
    pub values_per_share: Vec<Vec<f64>>,
    pub center_cell: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DcfResult {
    pub method: String,
    pub wacc_breakdown: super::wacc::WaccResult,
    pub base_fcf_yi: f64,
    pub projected_fcf_yi: Vec<f64>,
    pub pv_fcf_yi: Vec<f64>,
    pub year_labels: Vec<String>,
    pub pv_explicit_yi: f64,
    pub terminal_value_yi: f64,
    pub tv_pv_yi: f64,
    pub tv_pct_of_ev: f64,
    pub enterprise_value_yi: f64,
    pub net_debt_yi: f64,
    pub equity_value_yi: f64,
    pub shares_yi: f64,
    pub intrinsic_per_share: f64,
    pub current_price: f64,
    pub safety_margin_pct: f64,
    pub verdict: String,
    pub sensitivity_table: SensitivityTable,
    pub assumptions: DcfAssumptions,
    pub methodology_log: Vec<String>,
    pub used_fallback: Vec<String>,
}

impl HasFallbacks for DcfResult {
    fn used_fallback(&self) -> &[String] {
        &self.used_fallback
    }
}

/// 2-stage DCF with Gordon Growth terminal value.
#[must_use]
pub fn compute_dcf(features: &FeatureVector, assumptions: Option<DcfAssumptions>) -> DcfResult {
    let a = assumptions.unwrap_or_default();
    let mut used_fallback = Vec::new();

    let wacc_info = compute_wacc(Some(WaccInputs {
        beta: a.beta,
        tax: a.tax,
        target_debt_ratio: a.target_debt_ratio,
        ..Default::default()
    }));
    let wacc = wacc_info.wacc;

    let mut fcf0 = features.fcf_latest_yi.unwrap_or(0.0);
    if fcf0 <= 0.0 {
        let rev = features.revenue_latest_yi.unwrap_or(0.0);
        let nm = features.net_margin.unwrap_or(0.0) / 100.0;
        fcf0 = rev * nm * 0.8;
        used_fallback.push("fcf_from_revenue_margin".into());
    }
    if fcf0 <= 0.0 {
        fcf0 = features.market_cap_yi.unwrap_or(0.0) * 0.05;
        used_fallback.push("fcf_from_market_cap_yield".into());
    }

    let stage1 = a.stage1_years as usize;
    let stage2 = a.stage2_years as usize;
    let mut projected_fcf = Vec::with_capacity(stage1 + stage2);
    let mut year_labels = Vec::with_capacity(stage1 + stage2);
    let mut cur = fcf0;

    for i in 1..=stage1 {
        cur *= 1.0 + a.stage1_growth;
        projected_fcf.push(round3(cur));
        year_labels.push(format!("Y{i}"));
    }
    for i in 1..=stage2 {
        cur *= 1.0 + a.stage2_growth;
        projected_fcf.push(round3(cur));
        year_labels.push(format!("Y{stage1_y}", stage1_y = stage1 + i));
    }

    let mut pv_fcf = Vec::new();
    for (idx, fcf) in projected_fcf.iter().enumerate() {
        let df = 1.0 / (1.0 + wacc).powi((idx + 1) as i32);
        pv_fcf.push(round3(fcf * df));
    }
    let pv_explicit = round3(pv_fcf.iter().sum());

    let terminal_fcf = projected_fcf.last().copied().unwrap_or(0.0) * (1.0 + a.terminal_g);
    let tv_at_end = if wacc - a.terminal_g <= 0.0 {
        0.0
    } else {
        terminal_fcf / (wacc - a.terminal_g)
    };
    let n_years = projected_fcf.len();
    let tv_pv = round3(tv_at_end / (1.0 + wacc).powi(n_years as i32));

    let enterprise_value = round3(pv_explicit + tv_pv);
    let net_debt = features.total_debt_yi.unwrap_or(0.0) - features.cash_yi.unwrap_or(0.0);
    let equity_value = round3(enterprise_value - net_debt);

    let mut shares_yi = features.shares_outstanding_yi.unwrap_or(0.0);
    if shares_yi <= 0.0 {
        let mc = features.market_cap_yi.unwrap_or(0.0);
        let px = features.price.unwrap_or(0.0);
        shares_yi = if px > 0.0 { mc / px } else { 1.0 };
        used_fallback.push("shares_from_market_cap_price".into());
    }

    let per_share = if shares_yi > 0.0 {
        round2(equity_value / shares_yi)
    } else {
        0.0
    };

    let cur_price = features.price.unwrap_or(0.0);
    let safety_margin = if cur_price > 0.0 && per_share > 0.0 {
        round1((per_share - cur_price) / cur_price * 100.0)
    } else {
        0.0
    };

    let sensitivity = sensitivity_table(fcf0, &a, net_debt, shares_yi, wacc, a.terminal_g);

    let tv_pct = if enterprise_value > 0.0 {
        round1(tv_pv / enterprise_value * 100.0)
    } else {
        0.0
    };

    let verdict = dcf_verdict(safety_margin);
    let methodology_log = build_methodology_log(
        &wacc_info,
        fcf0,
        &a,
        pv_explicit,
        tv_pv,
        tv_pct,
        enterprise_value,
        net_debt,
        equity_value,
        per_share,
        cur_price,
        safety_margin,
    );

    DcfResult {
        method: "DCF (2-stage + Gordon Growth terminal)".into(),
        wacc_breakdown: wacc_info,
        base_fcf_yi: round3(fcf0),
        projected_fcf_yi: projected_fcf,
        pv_fcf_yi: pv_fcf,
        year_labels,
        pv_explicit_yi: pv_explicit,
        terminal_value_yi: round3(tv_at_end),
        tv_pv_yi: tv_pv,
        tv_pct_of_ev: tv_pct,
        enterprise_value_yi: enterprise_value,
        net_debt_yi: round3(net_debt),
        equity_value_yi: equity_value,
        shares_yi: round3(shares_yi),
        intrinsic_per_share: per_share,
        current_price: cur_price,
        safety_margin_pct: safety_margin,
        verdict,
        sensitivity_table: sensitivity,
        assumptions: a,
        methodology_log,
        used_fallback,
    }
}

fn sensitivity_table(
    fcf0: f64,
    a: &DcfAssumptions,
    net_debt: f64,
    shares_yi: f64,
    wacc_center: f64,
    g_center: f64,
) -> SensitivityTable {
    let wacc_row = [
        wacc_center - 0.02,
        wacc_center - 0.01,
        wacc_center,
        wacc_center + 0.01,
        wacc_center + 0.02,
    ];
    let g_col = [
        g_center - 0.01,
        g_center - 0.005,
        g_center,
        g_center + 0.005,
        g_center + 0.01,
    ];

    let stage1 = a.stage1_years as usize;
    let stage2 = a.stage2_years as usize;
    let mut rows = Vec::new();

    for w in wacc_row {
        let mut row = Vec::new();
        for g in g_col {
            let mut cur = fcf0;
            let mut proj = Vec::new();
            for _ in 0..stage1 {
                cur *= 1.0 + a.stage1_growth;
                proj.push(cur);
            }
            for _ in 0..stage2 {
                cur *= 1.0 + a.stage2_growth;
                proj.push(cur);
            }
            let pv_exp: f64 = proj
                .iter()
                .enumerate()
                .map(|(i, f)| f / (1.0 + w).powi((i + 1) as i32))
                .sum();
            let tv = if w - g > 0.0 {
                proj.last().copied().unwrap_or(0.0) * (1.0 + g) / (w - g)
            } else {
                0.0
            };
            let tv_pv = tv / (1.0 + w).powi(proj.len() as i32);
            let ev = pv_exp + tv_pv;
            let eq = ev - net_debt;
            let ps = if shares_yi > 0.0 { eq / shares_yi } else { 0.0 };
            row.push(round2(ps));
        }
        rows.push(row);
    }

    let center_cell = rows[2][2];
    SensitivityTable {
        wacc_axis: wacc_row
            .iter()
            .map(|w| format!("{}%", round1(w * 100.0)))
            .collect(),
        g_axis: g_col
            .iter()
            .map(|g| format!("{}%", round1(g * 100.0)))
            .collect(),
        values_per_share: rows,
        center_cell,
    }
}

fn dcf_verdict(safety_margin: f64) -> String {
    if safety_margin >= 30.0 {
        "🟢 深度低估 — 安全边际充足".into()
    } else if safety_margin >= 15.0 {
        "🟡 略微低估 — 可关注".into()
    } else if safety_margin >= -15.0 {
        "⚪ 基本合理".into()
    } else if safety_margin >= -30.0 {
        "🟠 略微高估".into()
    } else {
        "🔴 明显高估".into()
    }
}

#[allow(clippy::too_many_arguments)]
fn build_methodology_log(
    wacc_info: &super::wacc::WaccResult,
    fcf0: f64,
    a: &DcfAssumptions,
    pv_explicit: f64,
    tv_pv: f64,
    tv_pct: f64,
    enterprise_value: f64,
    net_debt: f64,
    equity_value: f64,
    per_share: f64,
    cur_price: f64,
    safety_margin: f64,
) -> Vec<String> {
    vec![
        format!(
            "Step 1 · WACC: CAPM k_e={:.2}%, 税后 k_d={:.2}%, 加权 WACC={:.2}%",
            wacc_info.cost_of_equity * 100.0,
            wacc_info.after_tax_kd * 100.0,
            wacc_info.wacc * 100.0
        ),
        format!("Step 2 · 基期 FCF={fcf0:.2} 亿"),
        format!(
            "Step 3 · 两段增长 {:.0}% ({}年) → {:.0}% ({}年)",
            a.stage1_growth * 100.0,
            a.stage1_years,
            a.stage2_growth * 100.0,
            a.stage2_years
        ),
        format!("Step 4 · 显式期 PV 合计 {pv_explicit:.1} 亿"),
        format!(
            "Step 5 · 终值 @ g={:.1}% → PV={:.1} 亿（占 EV 的 {:.0}%）",
            a.terminal_g * 100.0,
            tv_pv,
            tv_pct
        ),
        format!(
            "Step 6 · EV {enterprise_value:.1} 亿 − 净债 {net_debt:.1} 亿 = 股权价值 {equity_value:.1} 亿"
        ),
        format!(
            "Step 7 · 每股内在价值 ¥{per_share:.2}（当前价 ¥{cur_price:.2}，安全边际 {safety_margin:+.1}%）"
        ),
    ]
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::types::FeatureVector;

    fn smoke_features() -> FeatureVector {
        FeatureVector {
            symbol: "TEST".into(),
            price: Some(18.5),
            market_cap_yi: Some(260.0),
            shares_outstanding_yi: Some(14.0),
            revenue_latest_yi: Some(52.0),
            net_margin: Some(12.5),
            pe: Some(35.0),
            pb: Some(2.8),
            total_debt_yi: Some(10.0),
            cash_yi: Some(40.0),
            fcf_latest_yi: Some(6.5),
            ebitda_yi: Some(10.0),
            equity_yi: Some(92.0),
            ..Default::default()
        }
    }

    #[test]
    fn dcf_smoke_matches_uzi() {
        let r = compute_dcf(&smoke_features(), None);
        assert!((r.intrinsic_per_share - 18.39).abs() < 0.02);
        assert!((r.safety_margin_pct - (-0.6)).abs() < 0.2);
        assert!((r.sensitivity_table.center_cell - 18.39).abs() < 0.02);
        assert!(r.used_fallback.is_empty());
    }

    #[test]
    fn dcf_fcf_fallback() {
        let mut f = smoke_features();
        f.fcf_latest_yi = Some(0.0);
        let r = compute_dcf(&f, None);
        assert!(
            r.used_fallback
                .contains(&"fcf_from_revenue_margin".to_string())
        );
        assert!(r.intrinsic_per_share > 0.0);
    }
}
