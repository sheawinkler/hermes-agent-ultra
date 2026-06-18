//! Eastmoney financials provider (三表摘要 + 历史序列).

use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use crate::error::TradingError;
use crate::http::{default_client, send_with_retry};
use crate::providers::fundamentals::FundamentalsProvider;
use crate::research::types::{FundamentalsSnapshot, ProvenanceSource};
use crate::settlement::is_a_share;
use crate::symbol::normalize_symbol;

const F10_MAIN_URL: &str =
    "https://emweb.securities.eastmoney.com/PC_HSF10/NewFinanceAnalysis/MainTargetAjax";
const F10_DEBT_URL: &str =
    "https://emweb.securities.eastmoney.com/PC_HSF10/NewFinanceAnalysis/DebtAjax";
const F10_CASHFLOW_URL: &str =
    "https://emweb.securities.eastmoney.com/PC_HSF10/NewFinanceAnalysis/CashFlowAjax";

#[derive(Debug, Clone)]
pub struct EastmoneyFinancialsProvider {
    client: reqwest::Client,
}

impl EastmoneyFinancialsProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    fn yi_from_yuan(v: Option<f64>) -> Option<f64> {
        v.map(|n| n / 1e8)
    }
}

impl Default for EastmoneyFinancialsProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Deserialize)]
struct MainTargetResponse {
    #[serde(default)]
    data: Vec<MainTargetRow>,
}

#[derive(Debug, Deserialize)]
struct MainTargetRow {
    #[serde(rename = "REPORT_DATE")]
    report_date: Option<String>,
    #[serde(rename = "TOTALOPERATEREVE")]
    revenue: Option<f64>,
    #[serde(rename = "PARENTNETPROFIT")]
    net_profit: Option<f64>,
    #[serde(rename = "XSMLL")]
    gross_margin: Option<f64>,
    #[serde(rename = "ROEJQ")]
    roe: Option<f64>,
    #[serde(rename = "XSJLL")]
    net_margin_pct: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct DebtResponse {
    #[serde(default)]
    data: Vec<DebtRow>,
}

#[derive(Debug, Deserialize)]
struct DebtRow {
    #[serde(rename = "TOTAL_LIABILITIES")]
    total_liabilities: Option<f64>,
    #[serde(rename = "TOTAL_EQUITY")]
    total_equity: Option<f64>,
    #[serde(rename = "MONETARYFUNDS")]
    monetary_funds: Option<f64>,
    #[serde(rename = "CURRENT_RATIO")]
    current_ratio: Option<f64>,
    #[serde(rename = "ASSET_LIAB_RATIO")]
    asset_liab_ratio: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct CashFlowResponse {
    #[serde(default)]
    data: Vec<CashFlowRow>,
}

#[derive(Debug, Deserialize)]
struct CashFlowRow {
    #[serde(rename = "NETCASH_OPERATE")]
    net_cash_operate: Option<f64>,
}

#[async_trait]
impl FundamentalsProvider for EastmoneyFinancialsProvider {
    fn name(&self) -> &str {
        "eastmoney_financials"
    }

    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let canonical = normalize_symbol(symbol);
        if !is_a_share(&canonical) {
            return Err(TradingError::SymbolNotFound(format!(
                "Financials only for A-shares: {symbol}"
            )));
        }
        let code: String = canonical
            .split('.')
            .next()
            .unwrap_or(&canonical)
            .to_string();
        let mut snap = FundamentalsSnapshot {
            symbol: canonical,
            ..Default::default()
        };

        self.fill_main_targets(&code, &mut snap).await?;
        self.fill_debt(&code, &mut snap).await?;
        self.fill_cashflow(&code, &mut snap).await?;

        Ok(snap)
    }
}

impl EastmoneyFinancialsProvider {
    async fn fill_main_targets(
        &self,
        code: &str,
        snap: &mut FundamentalsSnapshot,
    ) -> Result<(), TradingError> {
        let url = format!("{F10_MAIN_URL}?companyType=4&reportDateType=0&code={code}");
        debug!(url = %url, "fetching financials main targets");
        let client = self.client.clone();
        let resp = send_with_retry(|| client.get(&url)).await?;
        if !resp.status().is_success() {
            return Ok(());
        }
        let parsed: MainTargetResponse = resp.json().await?;
        if parsed.data.is_empty() {
            return Ok(());
        }

        let mut annual: Vec<&MainTargetRow> = parsed
            .data
            .iter()
            .filter(|r| {
                r.report_date
                    .as_deref()
                    .is_some_and(|d| d.contains("12-31"))
            })
            .collect();
        if annual.len() < 3 {
            annual = parsed.data.iter().collect();
        }
        annual.sort_by(|a, b| a.report_date.cmp(&b.report_date));

        for row in &annual {
            if let Some(roe) = row.roe {
                snap.roe_history.push(roe);
            }
            if let Some(rev) = row.revenue {
                snap.revenue_history.push(rev / 1e8);
            }
        }
        if !snap.roe_history.is_empty() {
            snap.provenance
                .insert("roe_history".into(), ProvenanceSource::Provider);
        }
        if !snap.revenue_history.is_empty() {
            snap.provenance
                .insert("revenue_history".into(), ProvenanceSource::Provider);
        }

        let latest = parsed.data.first();
        if let Some(row) = latest {
            if let Some(rev) = row.revenue {
                snap.revenue_latest_yi = Self::yi_from_yuan(Some(rev));
                snap.provenance
                    .insert("revenue_latest_yi".into(), ProvenanceSource::Provider);
            }
            if let (Some(rev), Some(np)) = (row.revenue, row.net_profit)
                && rev > 0.0
            {
                snap.net_margin = Some(np / rev * 100.0);
                snap.provenance
                    .insert("net_margin".into(), ProvenanceSource::Provider);
            } else if let Some(nm) = row.net_margin_pct {
                snap.net_margin = Some(nm);
                snap.provenance
                    .insert("net_margin".into(), ProvenanceSource::Provider);
            }
            if let Some(gm) = row.gross_margin {
                snap.gross_margin = Some(gm);
                snap.provenance
                    .insert("gross_margin".into(), ProvenanceSource::Provider);
            }
            if let Some(roe) = row.roe {
                snap.roe_latest = Some(roe);
                snap.provenance
                    .insert("roe_latest".into(), ProvenanceSource::Provider);
            }
            if snap.revenue_history.len() >= 2 {
                let prev = snap.revenue_history[snap.revenue_history.len() - 2];
                let last = snap.revenue_history[snap.revenue_history.len() - 1];
                if prev > 0.0 {
                    snap.revenue_growth_latest = Some((last - prev) / prev * 100.0);
                    snap.provenance
                        .insert("revenue_growth_latest".into(), ProvenanceSource::Provider);
                }
            }
        }
        Ok(())
    }

    async fn fill_debt(
        &self,
        code: &str,
        snap: &mut FundamentalsSnapshot,
    ) -> Result<(), TradingError> {
        let url = format!("{F10_DEBT_URL}?companyType=4&reportDateType=0&code={code}");
        debug!(url = %url, "fetching balance sheet");
        let client = self.client.clone();
        let resp = send_with_retry(|| client.get(&url)).await?;
        if !resp.status().is_success() {
            return Ok(());
        }
        let parsed: DebtResponse = resp.json().await?;
        let Some(row) = parsed.data.first() else {
            return Ok(());
        };
        if let Some(v) = row.total_equity {
            snap.equity_yi = Self::yi_from_yuan(Some(v));
            snap.provenance
                .insert("equity_yi".into(), ProvenanceSource::Provider);
        }
        if let Some(v) = row.total_liabilities {
            snap.total_debt_yi = Self::yi_from_yuan(Some(v));
            snap.provenance
                .insert("total_debt_yi".into(), ProvenanceSource::Provider);
        }
        if let Some(v) = row.monetary_funds {
            snap.cash_yi = Self::yi_from_yuan(Some(v));
            snap.provenance
                .insert("cash_yi".into(), ProvenanceSource::Provider);
        }
        if let Some(v) = row.current_ratio {
            snap.current_ratio = Some(v);
            snap.provenance
                .insert("current_ratio".into(), ProvenanceSource::Provider);
        }
        if let Some(v) = row.asset_liab_ratio {
            snap.debt_ratio = Some(v);
            snap.provenance
                .insert("debt_ratio".into(), ProvenanceSource::Provider);
        }
        Ok(())
    }

    async fn fill_cashflow(
        &self,
        code: &str,
        snap: &mut FundamentalsSnapshot,
    ) -> Result<(), TradingError> {
        let url = format!("{F10_CASHFLOW_URL}?companyType=4&reportDateType=0&code={code}");
        debug!(url = %url, "fetching cash flow");
        let client = self.client.clone();
        let resp = send_with_retry(|| client.get(&url)).await?;
        if !resp.status().is_success() {
            return Ok(());
        }
        let parsed: CashFlowResponse = resp.json().await?;
        let Some(row) = parsed.data.first() else {
            return Ok(());
        };
        if let Some(ocf) = row.net_cash_operate {
            let fcf_yi = ocf / 1e8;
            snap.fcf_latest_yi = Some(fcf_yi);
            snap.fcf_positive = Some(fcf_yi > 0.0);
            snap.provenance
                .insert("fcf_latest_yi".into(), ProvenanceSource::Provider);
            if let Some(np_yi) = snap
                .revenue_latest_yi
                .zip(snap.net_margin)
                .map(|(r, m)| r * m / 100.0)
                && np_yi > 0.0
            {
                snap.fcf_margin = Some(fcf_yi / np_yi * 100.0);
                snap.provenance
                    .insert("fcf_margin".into(), ProvenanceSource::Provider);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yi_conversion() {
        assert_eq!(
            EastmoneyFinancialsProvider::yi_from_yuan(Some(893_500_000_000.0)),
            Some(8935.0)
        );
    }
}
