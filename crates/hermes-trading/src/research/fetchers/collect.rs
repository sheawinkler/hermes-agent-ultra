//! Dimension collection orchestrator (mirrors UZI `pipeline/collect.py`).

use tracing::debug;

use super::bridge::apply_dims_to_snapshot;
use super::context::FetchContext;
use super::registry::{EXEC_ORDER, build_registry, fetcher_for};
use super::types::CollectOutput;
use crate::research::types::FundamentalsSnapshot;

/// Options for dimension collection.
#[derive(Debug, Clone, Default)]
pub struct CollectOptions {
    /// When true, run web-only fetchers (they return `Skipped` stubs).
    pub include_web_dims: bool,
}

/// Collect registered HTTP dimensions for one symbol.
pub async fn collect_dims(symbol: &str, opts: &CollectOptions) -> CollectOutput {
    let registry = build_registry();
    let mut ctx = FetchContext::new(symbol);
    let mut output = CollectOutput {
        ticker: ctx.symbol.clone(),
        market: ctx.market,
        dims: Default::default(),
    };

    for dim_key in EXEC_ORDER {
        let Some(fetcher) = fetcher_for(&registry, dim_key) else {
            continue;
        };
        if fetcher.spec().web_only && !opts.include_web_dims {
            continue;
        }
        if !fetcher.spec().markets.contains(&ctx.market) {
            debug!(dim = dim_key, ?ctx.market, "skip dim for market");
            continue;
        }
        let result = fetcher.fetch(&ctx).await;
        ctx.prior.insert(result.dim_key.clone(), result.clone());
        output.dims.insert(result.dim_key.clone(), result);
    }

    output
}

/// Collect HTTP dims, merge snapshot, return raw_dims for scoring.
pub async fn enrich_snapshot(snap: &mut FundamentalsSnapshot, symbol: &str) -> serde_json::Value {
    let output = collect_dims(symbol, &CollectOptions::default()).await;
    apply_dims_to_snapshot(snap, &output);
    output.build_raw_dims()
}

#[cfg(test)]
mod tests {
    use crate::research::fetchers::types::Market;

    #[test]
    fn market_detection() {
        assert_eq!(Market::from_symbol("600809.SH"), Market::A);
        assert_eq!(Market::from_symbol("AAPL"), Market::U);
    }
}
