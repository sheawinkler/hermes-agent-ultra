//! Dimension 1 · financials (三表摘要).

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::providers::EastmoneyFinancialsProvider;
use crate::providers::FundamentalsProvider;
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;

pub struct FinancialsFetcher {
    provider: EastmoneyFinancialsProvider,
}

impl FinancialsFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            provider: EastmoneyFinancialsProvider::new(),
        }
    }

    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::FINANCIALS,
        depends_on: &[],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_f10", "yahoo"],
        web_only: false,
    };
}

impl Default for FinancialsFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DimFetcher for FinancialsFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        let ticker = &ctx.symbol;
        match self.provider.fetch(ticker).await {
            Ok(snap) => {
                let quality = if snap.roe_latest.is_some() && snap.net_margin.is_some() {
                    DimQuality::Full
                } else if snap.revenue_latest_yi.is_some() {
                    DimQuality::Partial
                } else {
                    DimQuality::Missing
                };
                DimResult::ok(
                    dim_keys::FINANCIALS,
                    ticker,
                    json!({
                        "roe": snap.roe_latest,
                        "net_margin": snap.net_margin,
                        "gross_margin": snap.gross_margin,
                        "revenue_growth": snap.revenue_growth_latest,
                        "revenue_latest_yi": snap.revenue_latest_yi,
                        "fcf_yi": snap.fcf_latest_yi,
                        "fcf_positive": snap.fcf_positive,
                        "equity_yi": snap.equity_yi,
                        "total_debt_yi": snap.total_debt_yi,
                        "cash_yi": snap.cash_yi,
                        "ebitda_yi": snap.ebitda_yi,
                        "roe_history": snap.roe_history,
                        "revenue_history": snap.revenue_history,
                        "financial_health": {
                            "debt_ratio": snap.debt_ratio,
                            "current_ratio": snap.current_ratio,
                            "fcf_margin": snap.fcf_margin,
                        },
                    }),
                    "eastmoney_f10",
                    quality,
                )
            }
            Err(e) => {
                DimResult::error(dim_keys::FINANCIALS, ticker, "eastmoney_f10", e.to_string())
            }
        }
    }
}
