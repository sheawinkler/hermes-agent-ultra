//! `DimFetcher` trait — Rust counterpart of UZI `BaseFetcher`.

use async_trait::async_trait;

use super::types::{DimResult, FetcherSpec};
use crate::research::fetchers::context::FetchContext;

/// Fetch one UZI dimension (22 fetchers in registry).
#[async_trait]
pub trait DimFetcher: Send + Sync {
    fn spec(&self) -> &FetcherSpec;

    async fn fetch(&self, ctx: &FetchContext) -> DimResult;
}
