//! Dimension 7 · industry.

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;

pub struct IndustryFetcher;

impl IndustryFetcher {
    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::INDUSTRY,
        depends_on: &[dim_keys::BASIC],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_data", "0_basic"],
        web_only: false,
    };
}

#[async_trait]
impl DimFetcher for IndustryFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        let industry = ctx.prior_industry().unwrap_or_else(|| "综合".into());
        DimResult::ok(
            dim_keys::INDUSTRY,
            &ctx.symbol,
            json!({
                "industry": industry,
                "industry_pe": null,
                "growth": null,
            }),
            "0_basic",
            if industry == "综合" {
                DimQuality::Missing
            } else {
                DimQuality::Partial
            },
        )
    }
}
