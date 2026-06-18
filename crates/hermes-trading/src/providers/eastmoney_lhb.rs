//! Eastmoney 龙虎榜 provider.

use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde::Deserialize;
use tracing::debug;

use crate::error::TradingError;
use crate::http::{default_client, send_with_retry};
use crate::providers::fundamentals::FundamentalsProvider;
use crate::research::types::FundamentalsSnapshot;
use crate::settlement::is_a_share;
use crate::symbol::normalize_symbol;

const LHB_URL: &str = "https://datacenter-web.eastmoney.com/api/data/v1/get";

#[derive(Debug, Clone, Default)]
pub struct EastmoneyLhbProvider {
    client: reqwest::Client,
}

impl EastmoneyLhbProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: default_client(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LhbResponse {
    result: Option<LhbResult>,
}

#[derive(Debug, Deserialize)]
struct LhbResult {
    #[serde(default)]
    data: Vec<LhbRow>,
}

#[derive(Debug, Deserialize)]
struct LhbRow {
    #[serde(rename = "TRADE_DATE")]
    trade_date: Option<String>,
    #[serde(rename = "EXPLANATION")]
    explanation: Option<String>,
}

#[async_trait]
impl FundamentalsProvider for EastmoneyLhbProvider {
    fn name(&self) -> &str {
        "eastmoney_lhb"
    }

    async fn fetch(&self, symbol: &str) -> Result<FundamentalsSnapshot, TradingError> {
        let dim = fetch_lhb_dim(&self.client, symbol).await?;
        let count = dim
            .get("lhb_count_30d")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        Ok(FundamentalsSnapshot {
            symbol: normalize_symbol(symbol),
            matched_youzi: if count > 0 {
                vec!["lhb_activity".into()]
            } else {
                vec![]
            },
            ..Default::default()
        })
    }
}

/// Raw LHB dimension payload (30-day count for scoring dim 16).
pub async fn fetch_lhb_dim(
    client: &reqwest::Client,
    symbol: &str,
) -> Result<serde_json::Value, TradingError> {
    let canonical = normalize_symbol(symbol);
    if !is_a_share(&canonical) {
        return Err(TradingError::SymbolNotFound(format!(
            "LHB A-share only: {symbol}"
        )));
    }
    let code = canonical.split('.').next().unwrap_or(&canonical);
    let cutoff = (Utc::now() - Duration::days(30))
        .format("%Y-%m-%d")
        .to_string();
    let url = format!(
        "{LHB_URL}?reportName=RPT_DAILYBILLBOARD_DETAILS&columns=TRADE_DATE,EXPLANATION&filter=(SECURITY_CODE%3D%22{code}%22)&pageSize=50"
    );
    debug!(url = %url, "fetching lhb");
    let resp = send_with_retry(|| client.get(&url)).await?;
    if !resp.status().is_success() {
        return Ok(serde_json::json!({ "lhb_count_30d": 0 }));
    }
    let parsed: LhbResponse = resp.json().await?;
    let rows = parsed.result.map(|r| r.data).unwrap_or_default();
    let recent: Vec<_> = rows
        .iter()
        .filter(|r| {
            r.trade_date
                .as_deref()
                .is_some_and(|d| d >= cutoff.as_str())
        })
        .collect();
    let matched: Vec<String> = recent
        .iter()
        .filter_map(|r| r.explanation.as_deref())
        .filter(|s| s.contains('游'))
        .map(|s| s.chars().take(12).collect())
        .take(5)
        .collect();

    Ok(serde_json::json!({
        "lhb_count_30d": recent.len(),
        "matched_youzi": matched,
        "lhb_records": recent.len(),
    }))
}
