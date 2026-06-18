//! Equity research: valuation models, scoring, persona panel (0py, no Python).

pub mod analyze;
pub mod fetchers;
pub mod models;
pub mod personas;
pub mod report;
pub mod scoring;
pub mod types;

pub use analyze::{analyze_stock, snapshot_from_inputs};
pub use fetchers::{CollectOptions, CollectOutput, collect_dims, enrich_snapshot};
pub use types::{
    DataConfidence, DcfAssumptions, FeatureVector, FundamentalsSnapshot, ProvenanceSource,
};
