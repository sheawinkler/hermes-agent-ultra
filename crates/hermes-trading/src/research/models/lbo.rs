//! Quick LBO test (UZI fin_models.quick_lbo).

use serde::{Deserialize, Serialize};

use crate::research::types::{FeatureVector, HasFallbacks};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LboAssumptions {
    pub entry_multiple: f64,
    pub debt_multiple: f64,
    pub exit_multiple: f64,
    pub hold_years: u32,
    pub ebitda_growth: f64,
    pub interest_rate: f64,
}

impl Default for LboAssumptions {
    fn default() -> Self {
        Self {
            entry_multiple: 8.0,
            debt_multiple: 5.0,
            exit_multiple: 8.0,
            hold_years: 5,
            ebitda_growth: 0.08,
            interest_rate: 0.06,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LboResult {
    pub method: String,
    pub entry_ebitda_yi: f64,
    pub entry_multiple: f64,
    pub entry_ev_yi: f64,
    pub entry_debt_yi: f64,
    pub entry_equity_yi: f64,
    pub leverage_turns: f64,
    pub ebitda_path: Vec<f64>,
    pub debt_schedule: Vec<f64>,
    pub exit_ebitda_yi: f64,
    pub exit_multiple: f64,
    pub exit_ev_yi: f64,
    pub exit_equity_yi: f64,
    pub moic: f64,
    pub irr_pct: f64,
    pub pass_pe_test: bool,
    pub verdict: String,
    pub methodology_log: Vec<String>,
    pub used_fallback: Vec<String>,
}

impl HasFallbacks for LboResult {
    fn used_fallback(&self) -> &[String] {
        &self.used_fallback
    }
}

/// Private-equity style quick LBO test.
#[must_use]
pub fn quick_lbo(features: &FeatureVector, assumptions: Option<LboAssumptions>) -> LboResult {
    let a = assumptions.unwrap_or_default();
    let mut used_fallback = Vec::new();

    let mut ebitda = features.ebitda_yi.unwrap_or(0.0);
    if ebitda <= 0.0 {
        let rev = features.revenue_latest_yi.unwrap_or(0.0);
        let nm = features.net_margin.unwrap_or(0.0) / 100.0;
        let ni = rev * nm;
        ebitda = if ni > 0.0 { ni / 0.6 } else { rev * 0.15 };
        used_fallback.push("ebitda_from_revenue_margin".into());
    }

    let entry_ev = a.entry_multiple * ebitda;
    let entry_debt = a.debt_multiple * ebitda;
    let entry_equity = entry_ev - entry_debt;

    let mut path = Vec::new();
    let mut cur = ebitda;
    for _ in 1..=a.hold_years {
        cur *= 1.0 + a.ebitda_growth;
        path.push(round2(cur));
    }

    let mut debt = entry_debt;
    let mut debt_schedule = vec![round2(debt)];
    for y_ebitda in &path {
        let interest = debt * a.interest_rate;
        let fcf = y_ebitda * 0.5 - interest;
        let paydown = (fcf * 0.7).max(0.0);
        debt = (debt - paydown).max(0.0);
        debt_schedule.push(round2(debt));
    }

    let exit_ebitda = *path.last().unwrap_or(&ebitda);
    let exit_ev = a.exit_multiple * exit_ebitda;
    let exit_debt = *debt_schedule.last().unwrap_or(&0.0);
    let exit_equity = exit_ev - exit_debt;

    let (moic, irr) = if entry_equity > 0.0 && exit_equity > 0.0 {
        let m = exit_equity / entry_equity;
        let i = m.powf(1.0 / a.hold_years as f64) - 1.0;
        (round2(m), round1(i * 100.0))
    } else {
        (0.0, 0.0)
    };

    let pass_pe_test = irr >= 0.20;
    let verdict = if irr >= 0.20 {
        "🟢 PE 买方可赚 20%+ IRR"
    } else if irr >= 0.15 {
        "🟡 PE 买方 15-20% IRR"
    } else {
        "🔴 低于 PE 收益门槛"
    };

    let methodology_log = vec![
        format!(
            "Step 1 · 入场 EBITDA {ebitda:.1} 亿 × {}x = EV {entry_ev:.1} 亿",
            a.entry_multiple
        ),
        format!(
            "Step 2 · {}x 杠杆 → 债 {entry_debt:.1} 亿 + 股本 {entry_equity:.1} 亿",
            a.debt_multiple
        ),
        format!(
            "Step 3 · {} 年 {:.0}% 成长 → Y{} EBITDA {exit_ebitda:.1} 亿",
            a.hold_years,
            a.ebitda_growth * 100.0,
            a.hold_years,
        ),
        format!(
            "Step 4 · 退出 {}x × {exit_ebitda:.1} = {exit_ev:.1} 亿 EV",
            a.exit_multiple
        ),
        format!(
            "Step 5 · 退出股权 {exit_equity:.1} 亿 / 入场股权 {entry_equity:.1} 亿 = {moic:.2}x MOIC ({irr:.1}% IRR)"
        ),
    ];

    LboResult {
        method: "Quick LBO Test".into(),
        entry_ebitda_yi: round2(ebitda),
        entry_multiple: a.entry_multiple,
        entry_ev_yi: round2(entry_ev),
        entry_debt_yi: round2(entry_debt),
        entry_equity_yi: round2(entry_equity),
        leverage_turns: a.debt_multiple,
        ebitda_path: path,
        debt_schedule,
        exit_ebitda_yi: round2(exit_ebitda),
        exit_multiple: a.exit_multiple,
        exit_ev_yi: round2(exit_ev),
        exit_equity_yi: round2(exit_equity),
        moic,
        irr_pct: irr,
        pass_pe_test,
        verdict: verdict.into(),
        methodology_log,
        used_fallback,
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}
fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::types::FeatureVector;

    #[test]
    fn lbo_smoke() {
        let f = FeatureVector {
            revenue_latest_yi: Some(52.0),
            net_margin: Some(12.5),
            ebitda_yi: Some(10.0),
            ..Default::default()
        };
        let r = quick_lbo(&f, None);
        assert!((r.irr_pct - 21.7).abs() < 0.5);
    }
}
