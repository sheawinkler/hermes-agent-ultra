//! Dimension 10 · valuation.

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::providers::{EastmoneyBasicProvider, EastmoneyValuationProvider, FundamentalsProvider};
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::settlement::is_a_share;

pub struct ValuationFetcher {
    basic: EastmoneyBasicProvider,
    valuation: EastmoneyValuationProvider,
}

impl ValuationFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            basic: EastmoneyBasicProvider::new(),
            valuation: EastmoneyValuationProvider::new(),
        }
    }

    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::VALUATION,
        depends_on: &[],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_push2"],
        web_only: false,
    };
}

impl Default for ValuationFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DimFetcher for ValuationFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        let ticker = &ctx.symbol;
        if is_a_share(ticker) {
            let basic = self.basic.fetch(ticker).await.ok();
            let val = self.valuation.fetch(ticker).await.ok();
            let pe = basic
                .as_ref()
                .and_then(|b| b.pe)
                .or(val.as_ref().and_then(|v| v.pe));
            let pb = basic.as_ref().and_then(|b| b.pb);
            let quality = if pe.is_some() {
                DimQuality::Partial
            } else {
                DimQuality::Missing
            };
            return DimResult::ok(
                dim_keys::VALUATION,
                ticker,
                json!({
                    "pe_ttm": pe,
                    "pb": pb,
                    "pe_percentile": null,
                    "pb_percentile": null,
                }),
                "eastmoney_push2",
                quality,
            );
        }
        DimResult::skipped(
            dim_keys::VALUATION,
            ticker,
            "港美股估值分位需 web_search / yahoo",
        )
    }
}
