//! Meta-search orchestration: DDGS international chain + optional CN HTML engines.

pub mod cn;
pub mod config;
pub mod ddgs;
pub mod http_client;
pub mod merge;
pub mod orchestrator;
pub mod query_locale;

use serde::{Deserialize, Serialize};

/// Normalized search hit used across DDGS and CN engines before JSON serialization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub description: String,
    pub source: String,
}

impl SearchHit {
    pub fn new(title: impl Into<String>, url: impl Into<String>, description: impl Into<String>, source: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            url: url.into(),
            description: description.into(),
            source: source.into(),
        }
    }
}

/// HTML parse failures for CN engines (no network).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

/// Per-engine attempt recorded in `_trace`.
#[derive(Debug, Clone, Serialize)]
pub struct EngineAttempt {
    pub engine: String,
    pub status: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
