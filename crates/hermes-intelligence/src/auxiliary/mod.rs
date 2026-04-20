//! Auxiliary client — multi-provider router for cheap side tasks.
//!
//! Mirrors the Python [`agent.auxiliary_client`] module: every consumer (title
//! generation, context compression, web extraction, vision analysis, …)
//! shares one resolution chain so adding a new provider takes one line, not
//! ten copy-pasted call sites.
//!
//! # Quick start
//!
//! ```no_run
//! use std::sync::Arc;
//! use hermes_intelligence::auxiliary::{
//!     AuxiliaryClient, AuxiliaryRequest, AuxiliarySource, AuxiliaryTask,
//!     ProviderCandidate, build_title_request,
//! };
//! # use hermes_core::LlmProvider;
//! # async fn _example(openrouter: Arc<dyn LlmProvider>) -> Result<(), Box<dyn std::error::Error>> {
//! let client = AuxiliaryClient::builder()
//!     .add_candidate(ProviderCandidate::new(
//!         AuxiliarySource::OpenRouter,
//!         "google/gemini-3-flash-preview",
//!         openrouter,
//!     ))
//!     .build();
//!
//! let resp = client.call(build_title_request("hi\nhow are you?")).await?;
//! println!("title = {:?}", resp.text());
//! # Ok(()) }
//! ```
//!
//! # Modules
//!
//! * [`task`] — catalogue of well-known auxiliary tasks
//! * [`candidate`] — provider-candidate types and chain construction
//! * [`config`] — per-task settings (env / config file overrides)
//! * [`client`] — main [`AuxiliaryClient`] entry point
//! * [`builtins`] — convenience builders for title / compression / classify
//! * [`error`] — [`AuxiliaryError`] and classification helpers

pub mod builtins;
pub mod candidate;
pub mod client;
pub mod config;
pub mod error;
pub mod task;

pub use builtins::{
    build_classify_request, build_compression_request, build_session_search_request,
    build_title_request, build_web_extract_request,
};
pub use candidate::{AuxiliarySource, ProviderCandidate, ProviderChain};
pub use client::{AuxiliaryClient, AuxiliaryClientBuilder, AuxiliaryRequest, AuxiliaryResponse};
pub use config::{
    resolve_task_settings, AuxiliaryConfig, ExplicitOverrides, ResolvedTaskSettings, TaskOverride,
};
pub use error::{is_connection_error, is_payment_error, AuxiliaryError, AuxiliaryResult};
pub use task::AuxiliaryTask;
