//! Dimension 4 · peers.

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;

pub struct PeersFetcher;

impl PeersFetcher {
    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::PEERS,
        depends_on: &[dim_keys::BASIC],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_data", "web_search"],
        web_only: false,
    };
}

#[async_trait]
impl DimFetcher for PeersFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        let industry = ctx.prior_industry().unwrap_or_default();
        DimResult::ok(
            dim_keys::PEERS,
            &ctx.symbol,
            json!({
                "industry": industry,
                "peer_table": [],
                "_note": "同业 PE/PB 需 agent 传 peers 参数或 web_search",
            }),
            "agent_or_web",
            DimQuality::Missing,
        )
    }
}
