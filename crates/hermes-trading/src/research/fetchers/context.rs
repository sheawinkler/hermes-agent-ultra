//! Shared fetch context passed between dimension fetchers.

use std::collections::BTreeMap;

use super::types::{DimResult, Market};

/// Context for a single symbol collection run (prior dims for `depends_on`).
#[derive(Debug, Clone)]
pub struct FetchContext {
    pub symbol: String,
    pub market: Market,
    pub prior: BTreeMap<String, DimResult>,
}

impl FetchContext {
    #[must_use]
    pub fn new(symbol: impl Into<String>) -> Self {
        let symbol = symbol.into();
        let market = Market::from_symbol(&symbol);
        Self {
            symbol,
            market,
            prior: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn prior_data(&self, dim_key: &str) -> Option<&serde_json::Value> {
        self.prior.get(dim_key).map(|r| &r.data)
    }

    #[must_use]
    pub fn prior_industry(&self) -> Option<String> {
        self.prior_data("0_basic")
            .and_then(|d| d.get("industry"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    }
}
