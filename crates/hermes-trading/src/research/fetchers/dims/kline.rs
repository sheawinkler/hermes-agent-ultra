//! Dimension 2 · K-line / technical.

use async_trait::async_trait;
use serde_json::json;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use super::kline_util::compute_kline_stats;
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::settlement::is_a_share;

pub struct KlineFetcher;

impl KlineFetcher {
    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::KLINE,
        depends_on: &[],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_push2his", "yahoo_chart_v8"],
        web_only: false,
    };
}

#[async_trait]
impl DimFetcher for KlineFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        if !is_a_share(&ctx.symbol) {
            return DimResult::skipped(
                dim_keys::KLINE,
                &ctx.symbol,
                "非 A 股 K 线暂仅支持 web_search / get_market_data",
            );
        }
        match compute_kline_stats(&ctx.symbol).await {
            Ok(stats) => DimResult::ok(
                dim_keys::KLINE,
                &ctx.symbol,
                json!({
                    "stage": stats.stage,
                    "ma_align": stats.ma_align,
                    "ma5": stats.ma5,
                    "ma20": stats.ma20,
                    "ma60": stats.ma60,
                    "rsi14": stats.rsi14,
                    "kline_stats": { "max_drawdown": stats.max_drawdown },
                }),
                "eastmoney_push2his",
                if stats.stage.is_empty() {
                    DimQuality::Partial
                } else {
                    DimQuality::Full
                },
            ),
            Err(e) => DimResult::error(
                dim_keys::KLINE,
                &ctx.symbol,
                "eastmoney_push2his",
                e.to_string(),
            ),
        }
    }
}
