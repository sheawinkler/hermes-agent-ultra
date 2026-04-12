//! OAuth credential management for provider integrations.
//!
//! This module provides a lightweight token cache with auto-refresh support.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;

use hermes_core::AgentError;

/// A cached OAuth access token with expiration metadata.
#[derive(Debug, Clone)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in_secs: u64,
}

impl OAuthToken {
    pub fn bearer(&self) -> String {
        format!("Bearer {}", self.access_token)
    }
}

/// A callback used to fetch/refresh OAuth tokens.
pub type TokenFetcher = Arc<
    dyn Fn(Option<String>) -> futures::future::BoxFuture<'static, Result<OAuthToken, AgentError>>
        + Send
        + Sync,
>;

#[derive(Debug, Clone)]
struct CachedToken {
    token: OAuthToken,
    issued_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        // Refresh 30 seconds before hard expiry.
        let ttl = Duration::from_secs(self.token.expires_in_secs.saturating_sub(30));
        self.issued_at.elapsed() >= ttl
    }
}

/// OAuth manager with auto-refresh and in-memory caching.
#[derive(Clone)]
pub struct OAuthManager {
    fetcher: TokenFetcher,
    cache: Arc<Mutex<Option<CachedToken>>>,
}

impl OAuthManager {
    pub fn new(fetcher: TokenFetcher) -> Self {
        Self {
            fetcher,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Return a valid access token, refreshing when needed.
    pub async fn access_token(&self) -> Result<String, AgentError> {
        let mut guard = self.cache.lock().await;

        if let Some(cached) = guard.as_ref() {
            if !cached.is_expired() {
                return Ok(cached.token.access_token.clone());
            }
        }

        let refresh = guard.as_ref().and_then(|c| c.token.refresh_token.clone());

        let token = (self.fetcher)(refresh).await?;
        *guard = Some(CachedToken {
            token: token.clone(),
            issued_at: Instant::now(),
        });

        Ok(token.access_token)
    }

    /// Return the Authorization header value.
    pub async fn authorization_header(&self) -> Result<String, AgentError> {
        let token = self.access_token().await?;
        Ok(format!("Bearer {}", token))
    }

    /// Force clear token cache.
    pub async fn clear(&self) {
        let mut guard = self.cache.lock().await;
        *guard = None;
    }
}
