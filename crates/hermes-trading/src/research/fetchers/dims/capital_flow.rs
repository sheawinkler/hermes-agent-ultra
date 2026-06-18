//! Dimension 12 · capital flow.

use async_trait::async_trait;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::http::default_client;
use crate::providers::eastmoney_capital_flow::fetch_capital_flow_dim;
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::settlement::is_a_share;

pub struct CapitalFlowFetcher {
    client: reqwest::Client,
}

impl CapitalFlowFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::CAPITAL_FLOW,
        depends_on: &[],
        markets: &[Market::A, Market::H],
        sources: &["eastmoney_fflow"],
        web_only: false,
    };
}

impl Default for CapitalFlowFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DimFetcher for CapitalFlowFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        if !is_a_share(&ctx.symbol) {
            return DimResult::skipped(
                dim_keys::CAPITAL_FLOW,
                &ctx.symbol,
                "港美股资金流用 web_search",
            );
        }
        match fetch_capital_flow_util(&self.client, &ctx.symbol).await {
            Ok(data) => {
                let quality = if data
                    .get("main_fund_5d_net_yi")
                    .and_then(|v| v.as_f64())
                    .is_some()
                {
                    DimQuality::Partial
                } else {
                    DimQuality::Missing
                };
                DimResult::ok(
                    dim_keys::CAPITAL_FLOW,
                    &ctx.symbol,
                    data,
                    "eastmoney_fflow",
                    quality,
                )
            }
            Err(e) => DimResult::error(
                dim_keys::CAPITAL_FLOW,
                &ctx.symbol,
                "eastmoney_fflow",
                e.to_string(),
            ),
        }
    }
}

async fn fetch_capital_flow_util(
    client: &reqwest::Client,
    symbol: &str,
) -> Result<serde_json::Value, crate::error::TradingError> {
    fetch_capital_flow_dim(client, symbol).await
}
