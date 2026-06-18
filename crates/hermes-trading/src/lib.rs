//! Trading: 0py market data and backtesting library for Hermes Agent.
//!
//! This crate provides:
//! - `MarketDataProvider` trait and implementations for fetching OHLCV data
//! - `BacktestEngine` for running template-based backtests
//!
//! **0py constraint**: No Python runtime, PyO3, or Python subprocess dependencies.

pub mod backtest;
pub mod cache;
pub mod error;
pub mod http;
pub mod indicators;
pub mod network_preflight;
pub mod provider;
pub mod providers;
pub mod quote_cache;
pub mod quote_data;
pub mod quote_provider;
pub mod research;
pub mod settlement;
pub mod symbol;
pub mod types;

pub use backtest::{BacktestEngine, Period, RunCard, SignalKind, StrategyInfo, StrategyRegistry};
pub use cache::DiskCache;
pub use error::TradingError;
pub use indicators::{rsi, sma};
pub use provider::MarketDataProvider;
#[cfg(any(test, feature = "test-mock"))]
pub use providers::MockProvider;
#[cfg(any(test, feature = "test-mock"))]
pub use providers::MockQuoteProvider;
pub use providers::{
    AutoRouter, BinanceProvider, BinanceQuoteProvider, DataSource, EastmoneyBasicProvider,
    EastmoneyCapitalFlowProvider, EastmoneyFinancialsProvider, EastmoneyLhbProvider,
    EastmoneyProvider, EastmoneyQuoteProvider, EastmoneyValuationProvider, FundamentalsAggregator,
    FundamentalsProvider, QuoteRouter, QuoteSource, StubProvider, YahooProvider,
};
pub use quote_cache::QuoteCache;
pub use quote_data::QuoteData;
pub use quote_provider::QuoteProvider;
pub use research::{
    CollectOptions, CollectOutput, DataConfidence, FeatureVector, FundamentalsSnapshot,
    analyze_stock, collect_dims, enrich_snapshot, snapshot_from_inputs,
};
pub use settlement::{SettlementMode, is_a_share, settlement_for_symbol};
pub use symbol::{is_hk_share, is_us_share, normalize_symbol};
pub use types::{Interval, OhlcvData, OhlcvRequest, OhlcvRow, mark_partial};
