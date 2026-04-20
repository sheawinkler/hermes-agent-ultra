//! `ManagedToolGatewayConfig` and per-vendor URL building, ported from
//! `tools/managed_tool_gateway.py`.
//!
//! Three precedence layers (highest first) for the gateway origin URL:
//!
//! 1. `<VENDOR>_GATEWAY_URL`   — explicit vendor override (e.g.
//!    `FIRECRAWL_GATEWAY_URL`, `OPENAI_AUDIO_GATEWAY_URL`)
//! 2. `TOOL_GATEWAY_DOMAIN` + `TOOL_GATEWAY_SCHEME` — shared domain,
//!    vendors live at `https://{vendor}-gateway.{domain}`
//! 3. Hard-coded default `https://{vendor}-gateway.nousresearch.com`

use std::fmt;

/// Default Nous-hosted gateway domain (matches Python).
pub const DEFAULT_TOOL_GATEWAY_DOMAIN: &str = "nousresearch.com";
const DEFAULT_TOOL_GATEWAY_SCHEME: &str = "https";

/// Validation error for `TOOL_GATEWAY_SCHEME`. The only allowed values
/// are `"http"` and `"https"`; anything else is rejected.
///
/// We deliberately don't depend on `hermes_core::ToolError` here so this
/// module can live in `hermes-config` without dragging tool-layer types
/// into the lower layer. Callers that want a `ToolError` can convert at
/// the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewaySchemeError {
    InvalidScheme(String),
}

impl fmt::Display for GatewaySchemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScheme(v) => write!(
                f,
                "TOOL_GATEWAY_SCHEME must be 'http' or 'https' (got {v:?})"
            ),
        }
    }
}

impl std::error::Error for GatewaySchemeError {}

/// Resolved configuration for a single vendor passthrough.
///
/// Equivalent to Python's frozen `ManagedToolGatewayConfig` dataclass.
/// Always carries `managed_mode = true`; the boolean is kept for parity
/// with downstream call-sites that branch on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedToolGatewayConfig {
    pub vendor: String,
    pub gateway_origin: String,
    pub nous_user_token: String,
    pub managed_mode: bool,
}

impl ManagedToolGatewayConfig {
    /// Construct an authenticated `Authorization: Bearer ...` header value
    /// for upstream requests.
    pub fn bearer_header(&self) -> String {
        format!("Bearer {}", self.nous_user_token)
    }
}

/// Trait for synchronous "given a vendor, return the gateway origin URL".
/// Mirrors Python's `gateway_builder` callable injection so tests can
/// supply hermetic builders.
pub trait GatewayBuilder: Send + Sync {
    fn build(&self, vendor: &str) -> String;
}

/// Default implementation that consults `TOOL_GATEWAY_*` env vars.
pub struct EnvGatewayBuilder;

impl GatewayBuilder for EnvGatewayBuilder {
    fn build(&self, vendor: &str) -> String {
        build_vendor_gateway_url(vendor)
    }
}

/// Return the configured shared gateway URL scheme (`http` or `https`).
///
/// Errors when `TOOL_GATEWAY_SCHEME` is set to something other than the
/// two supported values. Mirrors Python's `get_tool_gateway_scheme`.
pub fn get_tool_gateway_scheme() -> Result<String, GatewaySchemeError> {
    let raw = std::env::var("TOOL_GATEWAY_SCHEME").unwrap_or_default();
    let scheme = raw.trim().to_ascii_lowercase();
    if scheme.is_empty() {
        return Ok(DEFAULT_TOOL_GATEWAY_SCHEME.to_string());
    }
    if scheme == "http" || scheme == "https" {
        return Ok(scheme);
    }
    Err(GatewaySchemeError::InvalidScheme(raw))
}

/// Build the gateway origin URL for a single vendor.
///
/// Equivalent to Python's `build_vendor_gateway_url`. Trailing slashes are
/// stripped; vendor identifiers are converted to upper-snake-case for the
/// per-vendor env var lookup (e.g. `openai-audio` → `OPENAI_AUDIO_GATEWAY_URL`).
pub fn build_vendor_gateway_url(vendor: &str) -> String {
    let vendor_key = format!(
        "{}_GATEWAY_URL",
        vendor.to_ascii_uppercase().replace('-', "_")
    );
    if let Ok(explicit) = std::env::var(&vendor_key) {
        let trimmed = explicit.trim().trim_end_matches('/');
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    // Schema fallback ignores the error from `get_tool_gateway_scheme` to
    // match Python's "ValueError raised at first call site" behaviour: we
    // re-validate later in the resolver path. A misconfigured scheme env
    // var still produces a syntactically-valid URL here.
    let scheme =
        get_tool_gateway_scheme().unwrap_or_else(|_| DEFAULT_TOOL_GATEWAY_SCHEME.to_string());

    let shared_domain = std::env::var("TOOL_GATEWAY_DOMAIN")
        .unwrap_or_default()
        .trim()
        .trim_matches('/')
        .to_string();
    if !shared_domain.is_empty() {
        return format!("{scheme}://{vendor}-gateway.{shared_domain}");
    }

    format!("{scheme}://{vendor}-gateway.{DEFAULT_TOOL_GATEWAY_DOMAIN}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;

    struct EnvGuard {
        keys: Vec<&'static str>,
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new(keys: &[&'static str]) -> Self {
            let g = test_lock::lock();
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in keys {
                std::env::remove_var(k);
            }
            Self {
                keys: keys.to_vec(),
                original,
                _g: g,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
            let _ = &self.keys;
        }
    }

    #[test]
    fn scheme_defaults_to_https() {
        let _g = EnvGuard::new(&["TOOL_GATEWAY_SCHEME"]);
        std::env::remove_var("TOOL_GATEWAY_SCHEME");
        assert_eq!(get_tool_gateway_scheme().unwrap(), "https");

        std::env::set_var("TOOL_GATEWAY_SCHEME", "");
        assert_eq!(get_tool_gateway_scheme().unwrap(), "https");

        std::env::set_var("TOOL_GATEWAY_SCHEME", "HTTP");
        assert_eq!(get_tool_gateway_scheme().unwrap(), "http");
    }

    #[test]
    fn scheme_rejects_garbage() {
        let _g = EnvGuard::new(&["TOOL_GATEWAY_SCHEME"]);
        std::env::set_var("TOOL_GATEWAY_SCHEME", "ftp");
        assert!(get_tool_gateway_scheme().is_err());
    }

    #[test]
    fn vendor_url_default() {
        let _g = EnvGuard::new(&[
            "TOOL_GATEWAY_SCHEME",
            "TOOL_GATEWAY_DOMAIN",
            "FIRECRAWL_GATEWAY_URL",
        ]);
        assert_eq!(
            build_vendor_gateway_url("firecrawl"),
            "https://firecrawl-gateway.nousresearch.com",
        );
    }

    #[test]
    fn vendor_url_shared_domain_override() {
        let _g = EnvGuard::new(&[
            "TOOL_GATEWAY_SCHEME",
            "TOOL_GATEWAY_DOMAIN",
            "OPENAI_AUDIO_GATEWAY_URL",
        ]);
        std::env::set_var("TOOL_GATEWAY_DOMAIN", "tools.example.com");
        std::env::set_var("TOOL_GATEWAY_SCHEME", "http");
        assert_eq!(
            build_vendor_gateway_url("openai-audio"),
            "http://openai-audio-gateway.tools.example.com",
        );
    }

    #[test]
    fn vendor_url_explicit_override_wins() {
        let _g = EnvGuard::new(&[
            "TOOL_GATEWAY_SCHEME",
            "TOOL_GATEWAY_DOMAIN",
            "FIRECRAWL_GATEWAY_URL",
        ]);
        std::env::set_var("TOOL_GATEWAY_DOMAIN", "tools.example.com");
        std::env::set_var("FIRECRAWL_GATEWAY_URL", "https://my.gateway.dev/firecrawl/");
        assert_eq!(
            build_vendor_gateway_url("firecrawl"),
            "https://my.gateway.dev/firecrawl",
        );
    }

    #[test]
    fn vendor_key_handles_dashes() {
        let _g = EnvGuard::new(&["BROWSER_USE_GATEWAY_URL"]);
        std::env::set_var("BROWSER_USE_GATEWAY_URL", "https://b.example/u");
        assert_eq!(
            build_vendor_gateway_url("browser-use"),
            "https://b.example/u"
        );
    }

    #[test]
    fn shared_domain_strips_slashes() {
        let _g = EnvGuard::new(&["TOOL_GATEWAY_DOMAIN", "TOOL_GATEWAY_SCHEME"]);
        std::env::set_var("TOOL_GATEWAY_DOMAIN", "/tools.example.com/");
        assert_eq!(
            build_vendor_gateway_url("modal"),
            "https://modal-gateway.tools.example.com",
        );
    }

    #[test]
    fn config_bearer_header() {
        let cfg = ManagedToolGatewayConfig {
            vendor: "firecrawl".into(),
            gateway_origin: "https://firecrawl-gateway.nousresearch.com".into(),
            nous_user_token: "tok-xyz".into(),
            managed_mode: true,
        };
        assert_eq!(cfg.bearer_header(), "Bearer tok-xyz");
    }

    #[test]
    fn env_gateway_builder_routes_through_build_vendor_gateway_url() {
        let _g = EnvGuard::new(&[
            "TOOL_GATEWAY_SCHEME",
            "TOOL_GATEWAY_DOMAIN",
            "FIRECRAWL_GATEWAY_URL",
        ]);
        let b = EnvGatewayBuilder;
        assert_eq!(
            b.build("firecrawl"),
            "https://firecrawl-gateway.nousresearch.com",
        );
    }
}
