//! Top-level resolver entry-point: combine the URL builder, token reader,
//! and feature-flag check into a single `Option<ManagedToolGatewayConfig>`.
//!
//! Mirrors Python's `tools.managed_tool_gateway.resolve_managed_tool_gateway`
//! and `is_managed_tool_gateway_ready`.

use super::auth::{read_nous_access_token, TokenReader};
use super::config::{build_vendor_gateway_url, GatewayBuilder, ManagedToolGatewayConfig};
use super::selection::managed_nous_tools_enabled;

/// Optional injection points for hermetic tests / custom hosts.
///
/// In production callers pass `ResolveOptions::default()`, which uses the
/// env-driven URL builder and the disk-backed token reader (no automatic
/// refresh — the CLI layer is expected to wire one in via `token_reader`
/// when it owns the OAuth flow).
#[derive(Default)]
pub struct ResolveOptions<'a> {
    pub gateway_builder: Option<&'a dyn GatewayBuilder>,
    pub token_reader: Option<&'a dyn TokenReader>,
}

/// Resolve a managed gateway config for `vendor`, or `None` if any of the
/// three required pieces is missing:
///
/// * the `HERMES_ENABLE_NOUS_MANAGED_TOOLS` feature flag is off, or
/// * no gateway URL could be derived, or
/// * no Nous access token is reachable.
pub fn resolve_managed_tool_gateway(
    vendor: &str,
    opts: ResolveOptions<'_>,
) -> Option<ManagedToolGatewayConfig> {
    if !managed_nous_tools_enabled() {
        return None;
    }

    let gateway_origin = match opts.gateway_builder {
        Some(b) => b.build(vendor),
        None => build_vendor_gateway_url(vendor),
    };
    if gateway_origin.is_empty() {
        return None;
    }

    let token = read_nous_access_token(opts.token_reader)?;
    if token.is_empty() {
        return None;
    }

    Some(ManagedToolGatewayConfig {
        vendor: vendor.to_string(),
        gateway_origin,
        nous_user_token: token,
        managed_mode: true,
    })
}

/// Convenience predicate. Mirrors Python's `is_managed_tool_gateway_ready`.
pub fn is_managed_tool_gateway_ready(vendor: &str, opts: ResolveOptions<'_>) -> bool {
    resolve_managed_tool_gateway(vendor, opts).is_some()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::super::auth::TokenReader;
    use super::super::config::GatewayBuilder;
    use super::*;
    use crate::managed_gateway::test_lock;
    use serde_json::json;

    /// Combined HERMES_HOME + env-flag + token-override guard.
    struct Guard {
        _tmp: tempfile::TempDir,
        original_home: Option<String>,
        original_flag: Option<String>,
        original_token: Option<String>,
        original_domain: Option<String>,
        original_scheme: Option<String>,
        original_vendor: Option<String>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl Guard {
        fn new(payload: Option<&serde_json::Value>) -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let original_home = std::env::var("HERMES_HOME").ok();
            let original_flag = std::env::var("HERMES_ENABLE_NOUS_MANAGED_TOOLS").ok();
            let original_token = std::env::var("TOOL_GATEWAY_USER_TOKEN").ok();
            let original_domain = std::env::var("TOOL_GATEWAY_DOMAIN").ok();
            let original_scheme = std::env::var("TOOL_GATEWAY_SCHEME").ok();
            let original_vendor = std::env::var("FIRECRAWL_GATEWAY_URL").ok();

            std::env::set_var("HERMES_HOME", tmp.path());
            std::env::remove_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS");
            std::env::remove_var("TOOL_GATEWAY_USER_TOKEN");
            std::env::remove_var("TOOL_GATEWAY_DOMAIN");
            std::env::remove_var("TOOL_GATEWAY_SCHEME");
            std::env::remove_var("FIRECRAWL_GATEWAY_URL");

            if let Some(p) = payload {
                let path = tmp.path().join("auth.json");
                std::fs::write(&path, serde_json::to_vec_pretty(p).unwrap()).unwrap();
            }

            Self {
                _tmp: tmp,
                original_home,
                original_flag,
                original_token,
                original_domain,
                original_scheme,
                original_vendor,
                _g: g,
            }
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            for (k, v) in [
                ("HERMES_HOME", self.original_home.take()),
                (
                    "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                    self.original_flag.take(),
                ),
                ("TOOL_GATEWAY_USER_TOKEN", self.original_token.take()),
                ("TOOL_GATEWAY_DOMAIN", self.original_domain.take()),
                ("TOOL_GATEWAY_SCHEME", self.original_scheme.take()),
                ("FIRECRAWL_GATEWAY_URL", self.original_vendor.take()),
            ] {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    fn iso_in(secs: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn returns_none_when_feature_flag_off() {
        let payload = json!({
            "providers": {"nous": {
                "access_token": "tok",
                "expires_at": iso_in(3600),
            }}
        });
        let _g = Guard::new(Some(&payload));
        assert!(resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()).is_none());
    }

    #[test]
    fn returns_none_when_token_missing() {
        let _g = Guard::new(None);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        assert!(resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()).is_none());
    }

    #[test]
    fn returns_some_when_fully_configured() {
        let payload = json!({
            "providers": {"nous": {
                "access_token": "tok-xyz",
                "expires_at": iso_in(3600),
            }}
        });
        let _g = Guard::new(Some(&payload));
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");

        let cfg =
            resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()).expect("config");
        assert_eq!(cfg.vendor, "firecrawl");
        assert_eq!(
            cfg.gateway_origin,
            "https://firecrawl-gateway.nousresearch.com"
        );
        assert_eq!(cfg.nous_user_token, "tok-xyz");
        assert!(cfg.managed_mode);
    }

    #[test]
    fn explicit_token_override_works_without_auth_json() {
        let _g = Guard::new(None);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "ci-token");

        let cfg =
            resolve_managed_tool_gateway("openai-audio", ResolveOptions::default()).expect("cfg");
        assert_eq!(cfg.vendor, "openai-audio");
        assert_eq!(cfg.nous_user_token, "ci-token");
    }

    #[test]
    fn injected_gateway_builder_overrides_url_resolution() {
        struct StaticBuilder;
        impl GatewayBuilder for StaticBuilder {
            fn build(&self, vendor: &str) -> String {
                format!("https://test.local/{vendor}")
            }
        }
        let _g = Guard::new(None);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "ci");
        let opts = ResolveOptions {
            gateway_builder: Some(&StaticBuilder),
            token_reader: None,
        };
        let cfg = resolve_managed_tool_gateway("firecrawl", opts).unwrap();
        assert_eq!(cfg.gateway_origin, "https://test.local/firecrawl");
    }

    #[test]
    fn injected_token_reader_used_when_disk_token_expiring() {
        struct StubReader;
        impl TokenReader for StubReader {
            fn refresh(&self, _: i64) -> Option<String> {
                Some("refreshed".into())
            }
        }
        let payload = json!({
            "providers": {"nous": {
                "access_token": "stale",
                "expires_at": iso_in(5),
            }}
        });
        let _g = Guard::new(Some(&payload));
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        let opts = ResolveOptions {
            gateway_builder: None,
            token_reader: Some(&StubReader),
        };
        let cfg = resolve_managed_tool_gateway("firecrawl", opts).unwrap();
        assert_eq!(cfg.nous_user_token, "refreshed");
    }

    #[test]
    fn is_ready_predicate_matches_resolve() {
        let _g = Guard::new(None);
        assert!(!is_managed_tool_gateway_ready(
            "firecrawl",
            ResolveOptions::default()
        ));

        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "tok");
        assert!(is_managed_tool_gateway_ready(
            "firecrawl",
            ResolveOptions::default()
        ));
    }

    #[test]
    fn empty_token_in_env_is_ignored() {
        let _g = Guard::new(None);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "   ");
        assert!(resolve_managed_tool_gateway("firecrawl", ResolveOptions::default()).is_none());
    }

    #[test]
    fn empty_gateway_origin_returns_none() {
        struct EmptyBuilder;
        impl GatewayBuilder for EmptyBuilder {
            fn build(&self, _: &str) -> String {
                String::new()
            }
        }
        let _g = Guard::new(None);
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "tok");
        let opts = ResolveOptions {
            gateway_builder: Some(&EmptyBuilder),
            token_reader: None,
        };
        assert!(resolve_managed_tool_gateway("firecrawl", opts).is_none());
    }
}
