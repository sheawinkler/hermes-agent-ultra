//! Trading tool registrations (get_market_data, run_backtest, get_backtest_report, list_strategies, create_strategy).
//!
//! Requires the `trading-research` Cargo feature.

use super::{RegistryContext, reg};
use std::sync::Arc;
use tokio::sync::Mutex;

pub fn register(ctx: &RegistryContext<'_>) {
    #[cfg(feature = "trading-research")]
    {
        let store: Arc<dyn crate::backends::trading::RunCardStore> =
            Arc::new(crate::backends::trading::FileRunCardStore::default_path());

        // Build the strategy registry: built-ins + user strategies from disk.
        let strategies_dir = hermes_config::hermes_home()
            .join("trading")
            .join("strategies");
        let mut registry = hermes_strategies::StrategyRegistry::with_builtins();
        // Fix 4: Load user strategies from disk at startup for cross-session persistence.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(registry.load_from_dir(&strategies_dir));
        });
        let strategy_registry = Arc::new(Mutex::new(registry));

        reg(
            ctx,
            "trading",
            Arc::new(crate::tools::trading_quote::GetQuoteHandler::new()),
            "💹",
            vec![],
        );
        reg(
            ctx,
            "trading",
            Arc::new(crate::tools::trading_market_data::GetMarketDataHandler::new()),
            "📈",
            vec![],
        );
        reg(
            ctx,
            "trading",
            Arc::new(crate::tools::trading_backtest::RunBacktestHandler::new(
                store.clone(),
                strategy_registry.clone(),
            )),
            "📊",
            vec![],
        );
        reg(
            ctx,
            "trading",
            Arc::new(crate::tools::trading_report::GetBacktestReportHandler::new(
                store,
            )),
            "📑",
            vec![],
        );
        reg(
            ctx,
            "trading",
            Arc::new(
                crate::tools::trading_strategies::ListStrategiesHandler::new(
                    strategy_registry.clone(),
                ),
            ),
            "📝",
            vec![],
        );
        reg(
            ctx,
            "trading",
            Arc::new(crate::tools::trading_analyze_stock::AnalyzeStockHandler::new()),
            "📋",
            vec![],
        );
    }
}
