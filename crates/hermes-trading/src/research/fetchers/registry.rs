//! 22-dimension fetcher registry (mirrors UZI `pipeline/fetchers/registry.py`).

use std::sync::Arc;

use super::dims::{
    BasicFetcher, CapitalFlowFetcher, FinancialsFetcher, FundHoldersFetcher, IndustryFetcher,
    KlineFetcher, LhbFetcher, PeersFetcher, ValuationFetcher, chain_fetcher, contests_fetcher,
    events_fetcher, futures_fetcher, governance_fetcher, macro_fetcher, materials_fetcher,
    moat_fetcher, policy_fetcher, research_fetcher, sentiment_fetcher, trap_fetcher,
};
use super::r#trait::DimFetcher;
use crate::research::fetchers::dim_keys;

/// Build the full UZI fetcher registry.
#[must_use]
pub fn build_registry() -> Vec<Arc<dyn DimFetcher>> {
    vec![
        Arc::new(BasicFetcher::new()),
        Arc::new(FinancialsFetcher::new()),
        Arc::new(KlineFetcher),
        Arc::new(macro_fetcher()),
        Arc::new(PeersFetcher),
        Arc::new(chain_fetcher()),
        Arc::new(FundHoldersFetcher),
        Arc::new(research_fetcher()),
        Arc::new(IndustryFetcher),
        Arc::new(materials_fetcher()),
        Arc::new(futures_fetcher()),
        Arc::new(ValuationFetcher::new()),
        Arc::new(governance_fetcher()),
        Arc::new(CapitalFlowFetcher::new()),
        Arc::new(policy_fetcher()),
        Arc::new(moat_fetcher()),
        Arc::new(events_fetcher()),
        Arc::new(LhbFetcher::new()),
        Arc::new(sentiment_fetcher()),
        Arc::new(trap_fetcher()),
        Arc::new(contests_fetcher()),
    ]
}

/// Execution order respecting `depends_on`.
pub const EXEC_ORDER: &[&str] = &[
    dim_keys::BASIC,
    dim_keys::FINANCIALS,
    dim_keys::KLINE,
    dim_keys::VALUATION,
    dim_keys::CAPITAL_FLOW,
    dim_keys::LHB,
    dim_keys::FUND_HOLDERS,
    dim_keys::PEERS,
    dim_keys::INDUSTRY,
    dim_keys::MACRO,
    dim_keys::FUTURES,
    dim_keys::POLICY,
    dim_keys::CHAIN,
    dim_keys::RESEARCH,
    dim_keys::MATERIALS,
    dim_keys::GOVERNANCE,
    dim_keys::MOAT,
    dim_keys::EVENTS,
    dim_keys::SENTIMENT,
    dim_keys::TRAP,
    dim_keys::CONTESTS,
];

#[must_use]
pub fn fetcher_for<'a>(
    registry: &'a [Arc<dyn DimFetcher>],
    dim_key: &str,
) -> Option<&'a dyn DimFetcher> {
    registry
        .iter()
        .find(|f| f.spec().dim_key == dim_key)
        .map(|f| f.as_ref())
}

#[must_use]
pub fn list_dim_keys() -> Vec<&'static str> {
    dim_keys::ALL.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_21_entries() {
        assert_eq!(build_registry().len(), 21);
    }

    #[test]
    fn exec_order_starts_with_basic() {
        assert_eq!(EXEC_ORDER[0], dim_keys::BASIC);
    }

    #[test]
    fn all_exec_keys_registered() {
        let reg = build_registry();
        for key in EXEC_ORDER {
            assert!(fetcher_for(&reg, key).is_some(), "missing fetcher {key}");
        }
    }
}
