//! Fundamentals data providers (P1).

use async_trait::async_trait;

use crate::error::TradingError;
use crate::providers::eastmoney_capital_flow::EastmoneyCapitalFlowProvider;
use crate::providers::eastmoney_financials::EastmoneyFinancialsProvider;
use crate::providers::eastmoney_lhb::EastmoneyLhbProvider;
use crate::providers::eastmoney_valuation::EastmoneyValuationProvider;
use crate::research::types::{FundamentalsSnapshot, ProvenanceSource};

/// Fetch structured fundamentals for equity research.
#[async_trait]
pub trait FundamentalsProvider: Send + Sync {
    fn name(&self) -> &str;
    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError>;
}

/// Aggregate multiple providers into one snapshot.
pub struct FundamentalsAggregator {
    pub financials: EastmoneyFinancialsProvider,
    pub valuation: EastmoneyValuationProvider,
    pub capital_flow: EastmoneyCapitalFlowProvider,
    pub lhb: EastmoneyLhbProvider,
}

impl FundamentalsAggregator {
    #[must_use]
    pub fn new() -> Self {
        Self {
            financials: EastmoneyFinancialsProvider::new(),
            valuation: EastmoneyValuationProvider::new(),
            capital_flow: EastmoneyCapitalFlowProvider::new(),
            lhb: EastmoneyLhbProvider::new(),
        }
    }

    /// Merge all provider results into one snapshot.
    pub async fn fetch_all(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let mut snap = FundamentalsSnapshot {
            symbol: symbol.to_string(),
            ..Default::default()
        };

        merge_provider(&mut snap, self.financials.fetch(symbol).await);
        merge_provider(&mut snap, self.valuation.fetch(symbol).await);
        merge_provider(&mut snap, self.capital_flow.fetch(symbol).await);
        merge_provider(&mut snap, self.lhb.fetch(symbol).await);

        Ok(snap)
    }
}

impl Default for FundamentalsAggregator {
    fn default() -> Self {
        Self::new()
    }
}

fn merge_provider(
    target: &mut FundamentalsSnapshot,
    result: Result<FundamentalsSnapshot, TradingError>,
) {
    let Ok(part) = result else {
        return;
    };
    target.merge_provider_snapshot(&part);
}

#[allow(dead_code)]
pub fn mark(_field: &str) -> (ProvenanceSource, bool) {
    (ProvenanceSource::Provider, false)
}
