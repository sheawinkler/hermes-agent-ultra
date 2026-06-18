//! Eastmoney capital flow provider (主力净流入).

use async_trait::async_trait;
use tracing::debug;

use crate::error::TradingError;
use crate::http::default_client;
use crate::providers::eastmoney::EastmoneyProvider;
use crate::providers::eastmoney_http;
use crate::providers::fundamentals::FundamentalsProvider;
use crate::research::types::FundamentalsSnapshot;
use crate::settlement::is_a_share;
use crate::symbol::normalize_symbol;

#[derive(Debug, Clone, Default)]
pub struct EastmoneyCapitalFlowProvider {
    client: reqwest::Client,
}

impl EastmoneyCapitalFlowProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }

    /// Parse eastmoney fflow kline CSV; field index 1 = 主力净流入 (元).
    pub fn parse_main_flow_yi(klines: &[String], days: usize) -> Option<f64> {
        let sum: f64 = klines
            .iter()
            .rev()
            .take(days)
            .filter_map(|line| line.split(',').nth(1)?.parse::<f64>().ok())
            .sum();
        if sum == 0.0 && klines.is_empty() {
            return None;
        }
        Some(sum / 1e8)
    }
}

#[async_trait]
impl FundamentalsProvider for EastmoneyCapitalFlowProvider {
    fn name(&self) -> &str {
        "eastmoney_capital_flow"
    }

    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let canonical = normalize_symbol(symbol);
        if !is_a_share(&canonical) {
            return Err(TradingError::SymbolNotFound(format!(
                "Capital flow A-share only: {symbol}"
            )));
        }
        let secid = EastmoneyProvider::to_secid(&canonical)?;
        debug!(%secid, "fetching capital flow");
        let klines = eastmoney_http::fetch_push2_fflow_klines(&self.client, &secid).await?;
        let _ = Self::parse_main_flow_yi(&klines, 5);

        Ok(FundamentalsSnapshot {
            symbol: canonical,
            ..Default::default()
        })
    }
}

/// Fetch raw capital-flow dim payload for scoring.
pub async fn fetch_capital_flow_dim(
    client: &reqwest::Client,
    symbol: &str,
) -> Result<serde_json::Value, TradingError> {
    let canonical = normalize_symbol(symbol);
    let secid = EastmoneyProvider::to_secid(&canonical)?;
    let klines = eastmoney_http::fetch_push2_fflow_klines(client, &secid).await?;
    let main_5d = EastmoneyCapitalFlowProvider::parse_main_flow_yi(&klines, 5);
    let main_20d = EastmoneyCapitalFlowProvider::parse_main_flow_yi(&klines, 20);
    Ok(serde_json::json!({
        "main_fund_5d_net_yi": main_5d,
        "main_fund_20d_net_yi": main_20d,
        "main_fund_flow_20d": klines.len(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_main_flow_sum() {
        let lines = vec![
            "2024-06-13,100000000.0,0".into(),
            "2024-06-14,200000000.0,0".into(),
        ];
        assert_eq!(
            EastmoneyCapitalFlowProvider::parse_main_flow_yi(&lines, 2),
            Some(3.0)
        );
    }
}
