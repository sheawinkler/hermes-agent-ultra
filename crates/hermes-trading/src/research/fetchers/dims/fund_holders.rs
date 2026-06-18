//! Dimension 6_fund_holders · 股东户数 / 基金持仓（P2 骨架）.

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::settlement::is_a_share;

pub struct FundHoldersFetcher;

impl FundHoldersFetcher {
    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::FUND_HOLDERS,
        depends_on: &[],
        markets: &[Market::A],
        sources: &["eastmoney_datacenter"],
        web_only: false,
    };
}

#[async_trait]
impl DimFetcher for FundHoldersFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        if !is_a_share(&ctx.symbol) {
            return DimResult::skipped(dim_keys::FUND_HOLDERS, &ctx.symbol, "仅 A 股");
        }
        // ponytail: full fund_hold_detail 9-endpoint chain deferred; agent can web_search
        DimResult::ok(
            dim_keys::FUND_HOLDERS,
            &ctx.symbol,
            json!({ "_note": "基金持仓完整链 P2 · 暂空" }),
            "eastmoney_datacenter",
            DimQuality::Missing,
        )
    }
}
