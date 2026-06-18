//! Dimension 16 · 龙虎榜.

use async_trait::async_trait;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::http::default_client;
use crate::providers::eastmoney_lhb::fetch_lhb_dim;
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::settlement::is_a_share;

pub struct LhbFetcher {
    client: reqwest::Client,
}

impl LhbFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::LHB,
        depends_on: &[],
        markets: &[Market::A],
        sources: &["eastmoney_lhb"],
        web_only: false,
    };
}

impl Default for LhbFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DimFetcher for LhbFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        if !is_a_share(&ctx.symbol) {
            return DimResult::skipped(dim_keys::LHB, &ctx.symbol, "龙虎榜仅 A 股");
        }
        match fetch_lhb_dim(&self.client, &ctx.symbol).await {
            Ok(data) => {
                let count = data
                    .get("lhb_count_30d")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                DimResult::ok(
                    dim_keys::LHB,
                    &ctx.symbol,
                    data,
                    "eastmoney_lhb",
                    if count > 0 {
                        DimQuality::Partial
                    } else {
                        DimQuality::Missing
                    },
                )
            }
            Err(e) => DimResult::error(dim_keys::LHB, &ctx.symbol, "eastmoney_lhb", e.to_string()),
        }
    }
}
