//! 5-year 3-statement projection (UZI fin_models.project_three_stmt).

use serde::{Deserialize, Serialize};

use crate::research::types::{FeatureVector, HasFallbacks};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreeStmtAssumptions {
    pub revenue_growth_y1: f64,
    pub revenue_growth_y2: f64,
    pub revenue_growth_y3: f64,
    pub revenue_growth_y4: f64,
    pub revenue_growth_y5: f64,
    pub gross_margin: f64,
    pub opex_pct_revenue: f64,
    pub tax_rate: f64,
    pub capex_pct_revenue: f64,
    pub dep_pct_revenue: f64,
    pub nwc_pct_revenue: f64,
}

impl Default for ThreeStmtAssumptions {
    fn default() -> Self {
        Self {
            revenue_growth_y1: 0.12,
            revenue_growth_y2: 0.10,
            revenue_growth_y3: 0.08,
            revenue_growth_y4: 0.06,
            revenue_growth_y5: 0.05,
            gross_margin: 0.35,
            opex_pct_revenue: 0.18,
            tax_rate: super::wacc::DEFAULT_TAX,
            capex_pct_revenue: 0.05,
            dep_pct_revenue: 0.04,
            nwc_pct_revenue: 0.10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IncomeStatement {
    pub revenue: Vec<f64>,
    pub cogs: Vec<f64>,
    pub gross_profit: Vec<f64>,
    pub opex: Vec<f64>,
    pub ebit: Vec<f64>,
    pub tax: Vec<f64>,
    pub net_income: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CashFlow {
    pub net_income: Vec<f64>,
    pub dep_amort: Vec<f64>,
    pub nwc_change: Vec<f64>,
    pub ocf: Vec<f64>,
    pub capex: Vec<f64>,
    pub fcf: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BalanceSheet {
    pub equity_rollforward: Vec<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)]
pub enum ThreeStmtResult {
    Ok(ThreeStmtOk),
    Error {
        error: String,
        methodology_log: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreeStmtOk {
    pub method: String,
    pub years: Vec<String>,
    pub income_statement: IncomeStatement,
    pub cash_flow: CashFlow,
    pub balance_sheet: BalanceSheet,
    pub assumptions: ThreeStmtAssumptions,
    pub growth_path: Vec<String>,
    pub methodology_log: Vec<String>,
    pub used_fallback: Vec<String>,
}

impl HasFallbacks for ThreeStmtOk {
    fn used_fallback(&self) -> &[String] {
        &self.used_fallback
    }
}

/// Simplified 5-year IS / BS / CF forecast.
#[must_use]
pub fn project_three_stmt(
    features: &FeatureVector,
    assumptions: Option<ThreeStmtAssumptions>,
) -> ThreeStmtResult {
    let a = assumptions.unwrap_or_default();
    let mut used_fallback = Vec::new();

    let rev0 = features.revenue_latest_yi.unwrap_or(0.0);
    if rev0 <= 0.0 {
        return ThreeStmtResult::Error {
            error: "no base revenue".into(),
            methodology_log: vec!["缺少基期营收".into()],
        };
    }

    let years = vec!["Y1", "Y2", "Y3", "Y4", "Y5"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let growth = [
        a.revenue_growth_y1,
        a.revenue_growth_y2,
        a.revenue_growth_y3,
        a.revenue_growth_y4,
        a.revenue_growth_y5,
    ];

    let mut rev = Vec::new();
    let mut cogs = Vec::new();
    let mut gross = Vec::new();
    let mut opex = Vec::new();
    let mut ebit = Vec::new();
    let mut tax = Vec::new();
    let mut ni = Vec::new();

    let mut prev_rev = rev0;
    for g in growth {
        let r = prev_rev * (1.0 + g);
        let c = r * (1.0 - a.gross_margin);
        let gp = r - c;
        let op = r * a.opex_pct_revenue;
        let e = gp - op;
        let t = e * a.tax_rate;
        let n = e - t;
        rev.push(round2(r));
        cogs.push(round2(c));
        gross.push(round2(gp));
        opex.push(round2(op));
        ebit.push(round2(e));
        tax.push(round2(t));
        ni.push(round2(n));
        prev_rev = r;
    }

    let dep: Vec<f64> = rev.iter().map(|r| round2(r * a.dep_pct_revenue)).collect();
    let capex: Vec<f64> = rev
        .iter()
        .map(|r| round2(r * a.capex_pct_revenue))
        .collect();
    let nwc_chg: Vec<f64> = rev
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let prev = if i == 0 { rev0 } else { rev[i - 1] };
            round2((r - prev) * a.nwc_pct_revenue)
        })
        .collect();
    let ocf: Vec<f64> = ni
        .iter()
        .enumerate()
        .map(|(i, n)| round2(n + dep[i] - nwc_chg[i]))
        .collect();
    let fcf: Vec<f64> = ocf
        .iter()
        .enumerate()
        .map(|(i, o)| round2(o - capex[i]))
        .collect();

    let mut equity0 = features.equity_yi.unwrap_or(0.0);
    if equity0 <= 0.0 {
        let pb = features.pb.unwrap_or(2.0).max(0.1);
        equity0 = features.market_cap_yi.unwrap_or(0.0) / pb;
        used_fallback.push("equity_from_market_cap_pb".into());
    }
    let mut equity_series = Vec::new();
    let mut eq = equity0;
    for n in &ni {
        eq += n;
        equity_series.push(round2(eq));
    }

    let growth_path: Vec<String> = growth
        .iter()
        .map(|g| format!("{:.0}%", g * 100.0))
        .collect();
    let methodology_log = vec![
        format!(
            "Step 1 · 基期营收 {rev0:.1} 亿 · 5 年增速路径 {}",
            growth_path.join(" → ")
        ),
        format!(
            "Step 2 · 毛利率 {:.0}% · 运营费率 {:.0}%",
            a.gross_margin * 100.0,
            a.opex_pct_revenue * 100.0
        ),
        format!(
            "Step 3 · Y5 营收 {:.1} 亿 · 净利 {:.1} 亿",
            rev.last().copied().unwrap_or(0.0),
            ni.last().copied().unwrap_or(0.0)
        ),
        format!("Step 4 · 5 年累计 FCF {:.1} 亿", fcf.iter().sum::<f64>()),
    ];

    ThreeStmtResult::Ok(ThreeStmtOk {
        method: "3-Statement Projection (5-year, linked)".into(),
        years,
        income_statement: IncomeStatement {
            revenue: rev,
            cogs,
            gross_profit: gross,
            opex,
            ebit,
            tax,
            net_income: ni.clone(),
        },
        cash_flow: CashFlow {
            net_income: ni,
            dep_amort: dep,
            nwc_change: nwc_chg,
            ocf,
            capex,
            fcf,
        },
        balance_sheet: BalanceSheet {
            equity_rollforward: equity_series,
        },
        assumptions: a,
        growth_path,
        methodology_log,
        used_fallback,
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::research::types::FeatureVector;

    #[test]
    fn three_stmt_smoke() {
        let f = FeatureVector {
            revenue_latest_yi: Some(52.0),
            equity_yi: Some(92.0),
            ..Default::default()
        };
        let ThreeStmtResult::Ok(ok) = project_three_stmt(&f, None) else {
            panic!("expected ok");
        };
        assert!((ok.income_statement.net_income[4] - 9.82).abs() < 0.05);
    }
}
