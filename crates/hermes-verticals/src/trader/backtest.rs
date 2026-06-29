use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestRequest {
    pub symbol: String,
    pub start_date: String,
    pub end_date: String,
    pub strategy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub total_return_pct: f64,
    pub max_drawdown_pct: f64,
    pub equity_curve: Vec<(String, f64)>,
}

pub fn run_backtest_stub(req: &BacktestRequest) -> BacktestResult {
    BacktestResult {
        total_return_pct: 0.0,
        max_drawdown_pct: 0.0,
        equity_curve: vec![(req.start_date.clone(), 1.0), (req.end_date.clone(), 1.0)],
    }
}
