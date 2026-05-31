//! Platform adapter trait re-export and base implementation.
//!
//! Re-exports `PlatformAdapter` from `hermes-core` and provides a
//! `BasePlatformAdapter` with common fields (token, webhook_url, proxy)
//! and helper methods shared by all platform adapters.

use std::time::Duration;

use reqwest::{Client, ClientBuilder};
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

pub const DEFAULT_PLATFORM_HTTP_KEEPALIVE_EXPIRY_SECS: f64 = 2.0;
pub const DEFAULT_PLATFORM_HTTP_MAX_KEEPALIVE: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlatformHttpClientLimits {
    pub keepalive_expiry: Duration,
    pub max_keepalive_connections: usize,
}

impl Default for PlatformHttpClientLimits {
    fn default() -> Self {
        Self {
            keepalive_expiry: Duration::from_secs_f64(DEFAULT_PLATFORM_HTTP_KEEPALIVE_EXPIRY_SECS),
            max_keepalive_connections: DEFAULT_PLATFORM_HTTP_MAX_KEEPALIVE,
        }
    }
}

fn positive_env_f64(name: &str) -> Option<f64> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn positive_env_usize(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

/// Shared HTTP pool limits for long-lived platform adapters.
///
/// The env names intentionally match upstream's Python/httpx helper so
/// operator deployments can keep the same knobs while the implementation uses
/// reqwest instead of httpx.
pub fn platform_http_client_limits() -> PlatformHttpClientLimits {
    PlatformHttpClientLimits {
        keepalive_expiry: Duration::from_secs_f64(
            positive_env_f64("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY")
                .unwrap_or(DEFAULT_PLATFORM_HTTP_KEEPALIVE_EXPIRY_SECS),
        ),
        max_keepalive_connections: positive_env_usize("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE")
            .unwrap_or(DEFAULT_PLATFORM_HTTP_MAX_KEEPALIVE),
    }
}

/// Build a reqwest client builder with the shared platform-adapter pool limits.
pub fn platform_http_client_builder() -> ClientBuilder {
    let limits = platform_http_client_limits();
    Client::builder()
        .pool_idle_timeout(limits.keepalive_expiry)
        .pool_max_idle_per_host(limits.max_keepalive_connections)
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
        let mut builder = platform_http_client_builder();

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
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
    fn platform_http_client_limits_default_tightens_idle_pool() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY");
        std::env::remove_var("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE");
        let limits = platform_http_client_limits();
        assert!(limits.keepalive_expiry > Duration::ZERO);
        assert!(limits.keepalive_expiry < Duration::from_secs(5));
        assert!((1..=50).contains(&limits.max_keepalive_connections));
        assert!(platform_http_client_builder().build().is_ok());
    }

    #[test]
    fn platform_http_client_limits_honor_env_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY", "7.5");
        std::env::set_var("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE", "25");
        let limits = platform_http_client_limits();
        assert_eq!(limits.keepalive_expiry, Duration::from_secs_f64(7.5));
        assert_eq!(limits.max_keepalive_connections, 25);
        std::env::remove_var("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY");
        std::env::remove_var("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE");
    }

    #[test]
    fn platform_http_client_limits_reject_garbage_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY", "not-a-number");
        std::env::set_var("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE", "0");
        let limits = platform_http_client_limits();
        assert_eq!(
            limits.keepalive_expiry,
            Duration::from_secs_f64(DEFAULT_PLATFORM_HTTP_KEEPALIVE_EXPIRY_SECS)
        );
        assert_eq!(
            limits.max_keepalive_connections,
            DEFAULT_PLATFORM_HTTP_MAX_KEEPALIVE
        );
        std::env::remove_var("HERMES_GATEWAY_HTTPX_KEEPALIVE_EXPIRY");
        std::env::remove_var("HERMES_GATEWAY_HTTPX_MAX_KEEPALIVE");
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
