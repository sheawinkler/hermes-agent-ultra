//! UZI-style 22-dimension data fetchers (0py HTTP layer).
//!
//! Mirrors `UZI-Skill/.../pipeline/fetchers/`. Web-only dims delegate to Hermes `web_search`.
//! **HTTP transport SOP:** `docs/sop/equity_research_data.md` (read before adding dims).

pub mod bridge;
pub mod collect;
pub mod context;
pub mod dim_keys;
pub mod dims;
pub mod registry;
pub mod r#trait;
pub mod types;

pub use bridge::apply_dims_to_snapshot;
pub use collect::{CollectOptions, collect_dims, enrich_snapshot};
pub use registry::{EXEC_ORDER, build_registry, list_dim_keys};
pub use types::{CollectOutput, DimQuality, DimResult, FetcherSpec, Market};
