//! Per-dimension fetcher implementations (mirrors UZI `fetch_*.py`).

pub mod basic;
pub mod capital_flow;
pub mod financials;
pub mod fund_holders;
pub mod industry;
pub mod kline;
pub mod kline_util;
pub mod lhb;
pub mod peers;
pub mod valuation;
pub mod web_skipped;

pub use basic::BasicFetcher;
pub use capital_flow::CapitalFlowFetcher;
pub use financials::FinancialsFetcher;
pub use fund_holders::FundHoldersFetcher;
pub use industry::IndustryFetcher;
pub use kline::KlineFetcher;
pub use lhb::LhbFetcher;
pub use peers::PeersFetcher;
pub use valuation::ValuationFetcher;
pub use web_skipped::{
    WebSkippedFetcher, chain_fetcher, contests_fetcher, events_fetcher, futures_fetcher,
    governance_fetcher, macro_fetcher, materials_fetcher, moat_fetcher, policy_fetcher,
    research_fetcher, sentiment_fetcher, trap_fetcher,
};
