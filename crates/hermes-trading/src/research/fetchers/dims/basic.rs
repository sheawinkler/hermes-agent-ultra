//! Dimension 0 · basic quote / identity.

use async_trait::async_trait;
use serde_json::json;
use tracing::warn;

use super::super::r#trait::DimFetcher;
use super::super::types::{DimQuality, DimResult, FetcherSpec, Market};
use crate::providers::EastmoneyBasicProvider;
use crate::providers::FundamentalsProvider;
use crate::providers::QuoteRouter;
use crate::providers::QuoteSource;
use crate::quote_data::QuoteData;
use crate::research::fetchers::context::FetchContext;
use crate::research::fetchers::dim_keys;
use crate::research::types::FundamentalsSnapshot;
use crate::settlement::is_a_share;

pub struct BasicFetcher {
    basic: EastmoneyBasicProvider,
    quotes: QuoteRouter,
}

impl BasicFetcher {
    #[must_use]
    pub fn new() -> Self {
        Self {
            basic: EastmoneyBasicProvider::new(),
            quotes: QuoteRouter::new(),
        }
    }

    pub const SPEC: FetcherSpec = FetcherSpec {
        dim_key: dim_keys::BASIC,
        depends_on: &[],
        markets: &[Market::A, Market::H, Market::U],
        sources: &["eastmoney_push2", "tencent_qt", "yahoo"],
        web_only: false,
    };

    fn snap_has_core(snap: &FundamentalsSnapshot) -> bool {
        snap.name.is_some() && snap.price.is_some()
    }

    fn dim_from_snap(snap: &FundamentalsSnapshot, source: &str) -> DimResult {
        let ticker = snap.symbol.clone();
        let data = json!({
            "name": snap.name,
            "price": snap.price,
            "pe_ttm": snap.pe,
            "pb": snap.pb,
            "market_cap_yi": snap.market_cap_yi,
            "shares_outstanding_yi": snap.shares_outstanding_yi,
            "change_pct": snap.change_pct,
        });
        DimResult::ok(
            dim_keys::BASIC,
            &ticker,
            data,
            source,
            if snap.market_cap_yi.is_some() {
                DimQuality::Full
            } else {
                DimQuality::Partial
            },
        )
    }

    fn dim_from_quote(ticker: &str, q: &QuoteData) -> DimResult {
        DimResult::ok(
            dim_keys::BASIC,
            ticker,
            json!({
                "name": q.short_name,
                "price": q.price,
                "pe_ttm": q.pe_ratio,
                "change_pct": q.change_pct,
            }),
            q.source.as_str(),
            DimQuality::Partial,
        )
    }

    async fn fetch_a_share(&self, ticker: &str) -> DimResult {
        match self.basic.fetch(ticker).await {
            Ok(snap) if Self::snap_has_core(&snap) => {
                return Self::dim_from_snap(&snap, "eastmoney_push2");
            }
            Ok(snap) => {
                warn!(
                    symbol = %ticker,
                    "basic dim partial from eastmoney, trying quote router"
                );
                if let Ok(q) = self
                    .quotes
                    .fetch_quote_with_source(ticker, QuoteSource::Auto, false)
                    .await
                {
                    return Self::merge_snap_and_quote(ticker, snap, &q);
                }
            }
            Err(e) => {
                warn!(
                    symbol = %ticker,
                    error = %e,
                    "eastmoney basic failed, trying quote router"
                );
            }
        }

        match self
            .quotes
            .fetch_quote_with_source(ticker, QuoteSource::Auto, false)
            .await
        {
            Ok(q) => Self::dim_from_quote(ticker, &q),
            Err(e) => DimResult::error(dim_keys::BASIC, ticker, "quote_router", e.to_string()),
        }
    }

    fn merge_snap_and_quote(
        _ticker: &str,
        mut snap: FundamentalsSnapshot,
        q: &QuoteData,
    ) -> DimResult {
        if snap.name.is_none() {
            snap.name.clone_from(&q.short_name);
        }
        if snap.price.is_none() {
            snap.price = q.price;
        }
        if snap.pe.is_none() {
            snap.pe = q.pe_ratio;
        }
        if snap.change_pct.is_none() {
            snap.change_pct = q.change_pct;
        }
        let source = if q.source == "tencent_qt" {
            "eastmoney_push2+tencent_qt"
        } else {
            q.source.as_str()
        };
        Self::dim_from_snap(&snap, source)
    }
}

impl Default for BasicFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl DimFetcher for BasicFetcher {
    fn spec(&self) -> &FetcherSpec {
        &Self::SPEC
    }

    async fn fetch(&self, ctx: &FetchContext) -> DimResult {
        let ticker = &ctx.symbol;
        if is_a_share(ticker) {
            return self.fetch_a_share(ticker).await;
        }

        match self
            .quotes
            .fetch_quote_with_source(ticker, QuoteSource::Auto, false)
            .await
        {
            Ok(q) => Self::dim_from_quote(ticker, &q),
            Err(e) => DimResult::error(dim_keys::BASIC, ticker, "quote_router", e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_snap_and_quote_fills_gaps() {
        let snap = FundamentalsSnapshot {
            symbol: "600519.SH".into(),
            market_cap_yi: Some(1500.0),
            ..Default::default()
        };
        let q = QuoteData {
            symbol: "600519.SH".into(),
            market_date: None,
            as_of: None,
            price: Some(1407.0),
            change: None,
            change_pct: Some(0.1),
            volume: None,
            currency: None,
            exchange: None,
            short_name: Some("贵州茅台".into()),
            pe_ratio: Some(18.0),
            high_52w: None,
            low_52w: None,
            source: "tencent_qt".into(),
            partial: false,
        };
        let dim = BasicFetcher::merge_snap_and_quote("600519.SH", snap, &q);
        assert!(dim.error.is_none());
        assert_eq!(dim.source, "eastmoney_push2+tencent_qt");
        assert_eq!(dim.data.get("price").and_then(|v| v.as_f64()), Some(1407.0));
        assert_eq!(
            dim.data.get("name").and_then(|v| v.as_str()),
            Some("贵州茅台")
        );
    }
}
