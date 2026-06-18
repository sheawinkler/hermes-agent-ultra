//! CAPM WACC computation (UZI fin_models.compute_wacc).

use serde::{Deserialize, Serialize};

pub const DEFAULT_RF: f64 = 0.025;
pub const DEFAULT_ERP: f64 = 0.06;
pub const DEFAULT_BETA: f64 = 1.0;
pub const DEFAULT_TAX: f64 = 0.25;
pub const DEFAULT_KD_PRETAX: f64 = 0.045;
pub const DEFAULT_TARGET_DEBT_RATIO: f64 = 0.30;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WaccInputs {
    pub rf: f64,
    pub erp: f64,
    pub beta: f64,
    pub cost_of_debt_pretax: f64,
    pub target_debt_ratio: f64,
    pub tax: f64,
}

impl Default for WaccInputs {
    fn default() -> Self {
        Self {
            rf: DEFAULT_RF,
            erp: DEFAULT_ERP,
            beta: DEFAULT_BETA,
            cost_of_debt_pretax: DEFAULT_KD_PRETAX,
            target_debt_ratio: DEFAULT_TARGET_DEBT_RATIO,
            tax: DEFAULT_TAX,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WaccResult {
    pub wacc: f64,
    pub cost_of_equity: f64,
    pub after_tax_kd: f64,
    pub equity_weight: f64,
    pub debt_weight: f64,
    pub inputs: WaccInputs,
}

/// CAPM cost of equity + after-tax cost of debt → WACC.
#[must_use]
pub fn compute_wacc(inputs: Option<WaccInputs>) -> WaccResult {
    let inp = inputs.unwrap_or_default();
    let cost_of_equity = inp.rf + inp.beta * inp.erp;
    let after_tax_kd = inp.cost_of_debt_pretax * (1.0 - inp.tax);
    let equity_weight = 1.0 - inp.target_debt_ratio;
    let wacc = equity_weight * cost_of_equity + inp.target_debt_ratio * after_tax_kd;

    WaccResult {
        wacc: round4(wacc),
        cost_of_equity: round4(cost_of_equity),
        after_tax_kd: round4(after_tax_kd),
        equity_weight,
        debt_weight: inp.target_debt_ratio,
        inputs: inp,
    }
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wacc_defaults_match_uzi() {
        let r = compute_wacc(None);
        assert!((r.wacc - 0.0696).abs() < 0.0001);
        assert!((r.cost_of_equity - 0.085).abs() < 0.0001);
    }
}
