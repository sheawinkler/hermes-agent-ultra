//! UZI-compatible dimension fetch result types (`DimResult` / `FetcherSpec`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Market bucket aligned with UZI `market_router`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum Market {
    #[default]
    A,
    H,
    U,
}

impl Market {
    #[must_use]
    pub fn from_symbol(symbol: &str) -> Self {
        let upper = symbol.to_uppercase();
        if upper.ends_with(".HK") {
            Self::H
        } else if upper.ends_with(".SH") || upper.ends_with(".SZ") {
            Self::A
        } else {
            Self::U
        }
    }
}

/// Quality bucket (mirrors UZI `pipeline/schema.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimQuality {
    Full,
    Partial,
    Missing,
    Skipped,
    Error,
}

/// Static metadata for a dimension fetcher (mirrors UZI `FetcherSpec`).
#[derive(Debug, Clone, Copy)]
pub struct FetcherSpec {
    pub dim_key: &'static str,
    pub depends_on: &'static [&'static str],
    pub markets: &'static [Market],
    pub sources: &'static [&'static str],
    /// When true, Hermes expects `web_search` / agent JSON instead of Rust HTTP.
    pub web_only: bool,
}

/// One dimension fetch outcome — shape matches UZI `fetch_*.py` return + extras.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DimResult {
    pub dim_key: String,
    pub ticker: String,
    pub data: Value,
    pub source: String,
    pub fallback: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub quality: DimQuality,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub top_level: Map<String, Value>,
}

impl DimResult {
    #[must_use]
    pub fn ok(dim_key: &str, ticker: &str, data: Value, source: &str, quality: DimQuality) -> Self {
        Self {
            dim_key: dim_key.to_string(),
            ticker: ticker.to_string(),
            data,
            source: source.to_string(),
            fallback: quality != DimQuality::Full,
            error: None,
            quality,
            top_level: Map::new(),
        }
    }

    #[must_use]
    pub fn skipped(dim_key: &str, ticker: &str, note: &str) -> Self {
        Self {
            dim_key: dim_key.to_string(),
            ticker: ticker.to_string(),
            data: serde_json::json!({ "_note": note }),
            source: "web_search".into(),
            fallback: true,
            error: None,
            quality: DimQuality::Skipped,
            top_level: Map::new(),
        }
    }

    #[must_use]
    pub fn error(dim_key: &str, ticker: &str, source: &str, err: impl Into<String>) -> Self {
        Self {
            dim_key: dim_key.to_string(),
            ticker: ticker.to_string(),
            data: Value::Object(Map::new()),
            source: source.to_string(),
            fallback: true,
            error: Some(err.into()),
            quality: DimQuality::Error,
            top_level: Map::new(),
        }
    }

    /// UZI scoring expects `{ dim_key: { data: {...} } }`.
    #[must_use]
    pub fn as_raw_dim_entry(&self) -> (String, Value) {
        (
            self.dim_key.clone(),
            serde_json::json!({
                "ticker": self.ticker,
                "data": self.data,
                "source": self.source,
                "fallback": self.fallback,
                "error": self.error,
            }),
        )
    }
}

/// Output of `collect_dims`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectOutput {
    pub ticker: String,
    pub market: Market,
    pub dims: std::collections::BTreeMap<String, DimResult>,
}

impl CollectOutput {
    #[must_use]
    pub fn build_raw_dims(&self) -> Value {
        let mut obj = Map::new();
        for (key, result) in &self.dims {
            let (_, entry) = result.as_raw_dim_entry();
            obj.insert(key.clone(), entry);
        }
        Value::Object(obj)
    }
}
