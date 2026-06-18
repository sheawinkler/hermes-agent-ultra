//! Extended Eastmoney push2 fields for basic / valuation dims (via `eastmoney_http`).

use async_trait::async_trait;
use tracing::debug;

use crate::error::TradingError;
use crate::http::default_client;
use crate::providers::eastmoney_http::{self, AshareSnapshot};
use crate::providers::fundamentals::FundamentalsProvider;
use crate::research::types::{FundamentalsSnapshot, ProvenanceSource};
use crate::settlement::is_a_share;
use crate::symbol::normalize_symbol;

#[derive(Debug, Clone, Default)]
pub struct EastmoneyBasicProvider {
    client: reqwest::Client,
}

impl EastmoneyBasicProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    pub(crate) fn snapshot_to_fundamentals(snap: AshareSnapshot) -> FundamentalsSnapshot {
        let mut out = FundamentalsSnapshot {
            symbol: snap.symbol,
            name: snap.name,
            price: snap.price,
            change_pct: snap.change_pct,
            pe: snap.pe,
            pb: snap.pb,
            market_cap_yi: snap.market_cap_yi,
            circulating_cap_yi: snap.circulating_cap_yi,
            shares_outstanding_yi: snap.shares_outstanding_yi,
            ..Default::default()
        };
        macro_rules! mark {
            ($field:expr) => {
                out.provenance
                    .insert($field.into(), ProvenanceSource::Provider);
            };
        }
        if out.name.is_some() {
            mark!("name");
        }
        if out.price.is_some() {
            mark!("price");
        }
        if out.pe.is_some() {
            mark!("pe");
        }
        if out.pb.is_some() {
            mark!("pb");
        }
        if out.market_cap_yi.is_some() {
            mark!("market_cap_yi");
        }
        if out.shares_outstanding_yi.is_some() {
            mark!("shares_outstanding_yi");
        }
        out
    }
}

#[async_trait]
impl FundamentalsProvider for EastmoneyBasicProvider {
    fn name(&self) -> &str {
        "eastmoney_basic"
    }

    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let canonical = normalize_symbol(symbol);
        if !is_a_share(&canonical) {
            return Err(TradingError::SymbolNotFound(format!(
                "Basic provider A-share only: {symbol}"
            )));
        }
        debug!(symbol = %canonical, "eastmoney basic fetch");
        let snap = eastmoney_http::fetch_a_share_snapshot(&self.client, &canonical).await?;
        Ok(Self::snapshot_to_fundamentals(snap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_to_fundamentals_provenance() {
        let snap = AshareSnapshot {
            symbol: "600519.SH".into(),
            source: "eastmoney".into(),
            name: Some("贵州茅台".into()),
            price: Some(1407.0),
            change: None,
            change_pct: Some(0.1),
            volume: None,
            pe: Some(18.0),
            pb: Some(8.0),
            market_cap_yi: Some(1500.0),
            circulating_cap_yi: None,
            shares_outstanding_yi: Some(12.0),
        };
        let f = EastmoneyBasicProvider::snapshot_to_fundamentals(snap);
        assert!(f.provenance.contains_key("price"));
        assert!(f.provenance.contains_key("market_cap_yi"));
    }
}
