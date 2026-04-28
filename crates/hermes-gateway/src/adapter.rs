//! Platform adapter trait re-export and base implementation.
//!
//! Re-exports `PlatformAdapter` from `hermes-core` and provides a
//! `BasePlatformAdapter` with common fields (token, webhook_url, proxy)
//! and helper methods shared by all platform adapters.

use reqwest::Client;
use serde::{Deserialize, Serialize};

use hermes_core::errors::GatewayError;

// ---------------------------------------------------------------------------
// Re-export the PlatformAdapter trait
// ---------------------------------------------------------------------------

pub use hermes_core::traits::PlatformAdapter;

// ---------------------------------------------------------------------------
// ProxyConfig (local, mirrors hermes-config's ProxyConfig)
// ---------------------------------------------------------------------------

/// Proxy configuration for a platform adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterProxyConfig {
    /// HTTP proxy URL (e.g., "http://proxy:8080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,

    /// SOCKS5 proxy URL (e.g., "socks5://proxy:1080").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks_proxy: Option<String>,
}

impl Default for AdapterProxyConfig {
    fn default() -> Self {
        Self {
            http_proxy: None,
            socks_proxy: None,
        }
    }
}

// ---------------------------------------------------------------------------
// BasePlatformAdapter
// ---------------------------------------------------------------------------

/// Common configuration and helpers shared by all platform adapters.
///
/// Concrete adapter implementations should embed this struct and delegate
/// common operations (token validation, HTTP client construction) to it.
pub struct BasePlatformAdapter {
    /// Authentication token for the platform API.
    pub token: String,

    /// Webhook URL for receiving platform events.
    pub webhook_url: Option<String>,

    /// Proxy configuration for outbound requests.
    pub proxy: AdapterProxyConfig,

    /// Whether the adapter is currently running.
    pub running: std::sync::atomic::AtomicBool,
}

/// Return a non-reversible token descriptor suitable for logs.
///
/// This avoids logging raw token prefixes/suffixes while still giving operators
/// enough signal to confirm configuration changes.
pub fn describe_secret(secret: &str) -> String {
    let trimmed = secret.trim();
    if trimmed.is_empty() {
        return "missing".to_string();
    }
    // FNV-1a 64-bit fingerprint (stable, fast, non-cryptographic).
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in trimmed.as_bytes() {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x00000100000001B3);
    }
    format!("len={}, fp={hash:016x}", trimmed.chars().count())
}

impl BasePlatformAdapter {
    /// Create a new `BasePlatformAdapter` with the given token.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            webhook_url: None,
            proxy: AdapterProxyConfig::default(),
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Set the webhook URL.
    pub fn with_webhook_url(mut self, url: impl Into<String>) -> Self {
        self.webhook_url = Some(url.into());
        self
    }

    /// Set the proxy configuration.
    pub fn with_proxy(mut self, proxy: AdapterProxyConfig) -> Self {
        self.proxy = proxy;
        self
    }

    /// Validate that the token is non-empty.
    pub fn validate_token(&self) -> Result<(), GatewayError> {
        if self.token.trim().is_empty() {
            return Err(GatewayError::Auth("Token must not be empty".into()));
        }
        Ok(())
    }

    /// Build a `reqwest::Client` configured with proxy settings if any.
    pub fn build_client(&self) -> Result<Client, GatewayError> {
        let mut builder = Client::builder();

        if let Some(ref http_proxy) = self.proxy.http_proxy {
            let proxy = reqwest::Proxy::all(http_proxy).map_err(|e| {
                GatewayError::ConnectionFailed(format!("Invalid HTTP proxy: {}", e))
            })?;
            builder = builder.proxy(proxy);
        }

        if let Some(ref socks_proxy) = self.proxy.socks_proxy {
            let proxy = reqwest::Proxy::all(socks_proxy).map_err(|e| {
                GatewayError::ConnectionFailed(format!("Invalid SOCKS proxy: {}", e))
            })?;
            builder = builder.proxy(proxy);
        }

        builder.build().map_err(|e| {
            GatewayError::ConnectionFailed(format!("Failed to build HTTP client: {}", e))
        })
    }

    /// Mark the adapter as running.
    pub fn mark_running(&self) {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Mark the adapter as stopped.
    pub fn mark_stopped(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Check whether the adapter is currently running.
    pub fn is_running(&self) -> bool {
        self.running.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_adapter_validate_token_ok() {
        let base = BasePlatformAdapter::new("valid-token");
        assert!(base.validate_token().is_ok());
    }

    #[test]
    fn base_adapter_validate_token_empty() {
        let base = BasePlatformAdapter::new("  ");
        assert!(base.validate_token().is_err());
    }

    #[test]
    fn base_adapter_build_client_no_proxy() {
        let base = BasePlatformAdapter::new("token");
        assert!(base.build_client().is_ok());
    }

    #[test]
    fn base_adapter_running_state() {
        let base = BasePlatformAdapter::new("token");
        assert!(!base.is_running());
        base.mark_running();
        assert!(base.is_running());
        base.mark_stopped();
        assert!(!base.is_running());
    }

    #[test]
    fn proxy_config_default() {
        let proxy = AdapterProxyConfig::default();
        assert!(proxy.http_proxy.is_none());
        assert!(proxy.socks_proxy.is_none());
    }

    #[test]
    fn describe_secret_masks_value() {
        let raw = "super-sensitive-token-value";
        let masked = describe_secret(raw);
        assert!(!masked.contains(raw));
        assert!(masked.contains("len="));
        assert!(masked.contains("fp="));
    }

    #[test]
    fn describe_secret_empty() {
        assert_eq!(describe_secret(""), "missing");
        assert_eq!(describe_secret("   "), "missing");
    }
}
