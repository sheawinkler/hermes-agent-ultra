//! POI domain types for extract → compare → update pipeline.

use serde::{Deserialize, Serialize};

/// Provenance of an extracted interest signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    Declared,
    Lang,
    Tech,
    Path,
    Keyword,
    Llm,
    Rules,
}

impl SignalSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Lang => "lang",
            Self::Tech => "tech",
            Self::Path => "path",
            Self::Keyword => "keyword",
            Self::Llm => "llm",
            Self::Rules => "rules",
        }
    }

    pub fn from_topic_id(topic_id: &str) -> Self {
        let id = topic_id.trim().to_ascii_lowercase();
        if id.starts_with("interest:") {
            return Self::Declared;
        }
        if let Some(ns) = id.split(':').next() {
            return match ns {
                "lang" => Self::Lang,
                "tech" => Self::Tech,
                "path" => Self::Path,
                "keyword" => Self::Keyword,
                "llm" => Self::Llm,
                "domain" | "topic" => Self::Rules,
                _ => Self::Rules,
            };
        }
        Self::Rules
    }

    /// Default confidence when the extractor does not set one explicitly.
    pub fn default_confidence(self) -> f64 {
        match self {
            Self::Declared => 0.92,
            Self::Lang => 0.78,
            Self::Tech => 0.72,
            Self::Path => 0.45,
            Self::Keyword => 0.38,
            Self::Llm => 0.7,
            Self::Rules => 0.5,
        }
    }

    /// Insert directly as `active` when evidence is still low (high-trust sources).
    pub fn inserts_as_active(self, confidence: f64, promote_min_confidence: f64) -> bool {
        match self {
            Self::Declared => true,
            Self::Lang | Self::Tech | Self::Llm => confidence >= promote_min_confidence,
            Self::Rules => confidence >= promote_min_confidence.max(0.68),
            Self::Path | Self::Keyword => false,
        }
    }
}

/// Lifecycle state stored in SQLite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TopicStatus {
    Candidate,
    Active,
    Rejected,
}

impl TopicStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Active => "active",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "candidate" => Self::Candidate,
            "rejected" => Self::Rejected,
            _ => Self::Active,
        }
    }
}

/// Controls optional extract passes (e.g. keywords only at session end).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractOptions {
    pub include_keywords: bool,
}

/// Result summary after a pipeline batch apply.
#[derive(Debug, Clone, Default)]
pub struct PoiApplyReport {
    pub inserted: u32,
    pub reinforced: u32,
    pub merged: u32,
    pub promoted: u32,
    pub skipped: u32,
}
