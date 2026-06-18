//! Eastmoney valuation provider (PE/PB from quote push2).

use async_trait::async_trait;

use crate::error::TradingError;
use crate::providers::eastmoney_quote::EastmoneyQuoteProvider;
use crate::providers::fundamentals::FundamentalsProvider;
use crate::quote_provider::QuoteProvider;
use crate::research::types::{FundamentalsSnapshot, ProvenanceSource};
use crate::settlement::is_a_share;
use crate::symbol::normalize_symbol;

#[derive(Debug, Clone, Default)]
pub struct EastmoneyValuationProvider {
    quote: EastmoneyQuoteProvider,
}

impl EastmoneyValuationProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            quote: EastmoneyQuoteProvider::new(),
        }
    }
}

#[async_trait]
impl FundamentalsProvider for EastmoneyValuationProvider {
    fn name(&self) -> &str {
        "eastmoney_valuation"
    }

    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let canonical = normalize_symbol(symbol);
        if !is_a_share(&canonical) {
            return Err(TradingError::SymbolNotFound(format!(
                "Valuation provider A-share only: {symbol}"
            )));
        }
        let q = self.quote.fetch_quote(&canonical).await?;
        let mut snap = FundamentalsSnapshot {
            symbol: canonical,
            price: q.price,
            pe: q.pe_ratio,
            ..Default::default()
        };
        if q.price.is_some() {
            snap.provenance
                .insert("price".into(), ProvenanceSource::Provider);
        }
        if q.pe_ratio.is_some() {
            snap.provenance
                .insert("pe".into(), ProvenanceSource::Provider);
        }
        Ok(snap)
    }
}
