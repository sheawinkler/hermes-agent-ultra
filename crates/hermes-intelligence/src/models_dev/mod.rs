//! models.dev registry integration.
//!
//! Port of Python `agent.models_dev` (670 LoC). Fetches the community
//! registry at <https://models.dev/api.json> with an in-memory + disk
//! cache, and exposes typed helpers for context lookup, capability
//! resolution, agentic-model filtering and fuzzy search.
//!
//! # Architecture
//!
//! - [`types`]    — `ModelInfo`, `ProviderInfo`, `ModelCapabilities`
//! - [`mapping`]  — Hermes ↔ models.dev provider ID translation
//! - [`cache`]    — atomic disk-cache load/save
//! - [`parse`]    — defensive JSON → typed-struct converters
//! - [`client`]   — `ModelsDevClient` (HTTP + cache + queries)
//!
//! # Quick start
//!
//! ```ignore
//! use hermes_intelligence::models_dev::default_client;
//!
//! # async fn run() {
//! let client = default_client();
//! let _ = client.fetch(false).await; // populate from network/disk
//! let ctx = client.lookup_context("anthropic", "claude-sonnet-4-5");
//! # }
//! ```
//!
//! For tests that need to avoid the network, construct a custom
//! [`ModelsDevClient`] and call [`ModelsDevClient::seed_cache`].

pub mod cache;
pub mod client;
pub mod mapping;
pub mod parse;
pub mod types;

pub use client::{ModelsDevClient, SearchHit, MODELS_DEV_URL};
pub use mapping::{forward_map, resolve_models_dev_id, reverse_map, to_hermes, to_models_dev};
pub use types::{InterleavedFlag, ModelCapabilities, ModelInfo, ProviderInfo};

use std::sync::OnceLock;

/// Process-wide default [`ModelsDevClient`].
///
/// Lazily initialised on first call. Uses the production endpoint and
/// `<HERMES_HOME>/models_dev_cache.json`. Convenient for code that doesn't
/// need to mock the registry.
pub fn default_client() -> &'static ModelsDevClient {
    static CLIENT: OnceLock<ModelsDevClient> = OnceLock::new();
    CLIENT.get_or_init(ModelsDevClient::default_production)
}
