//! Nous OAuth subscriber-token reader, ported from
//! `tools/managed_tool_gateway.py::read_nous_access_token`.
//!
//! Lookup precedence (highest first):
//!
//! 1. `TOOL_GATEWAY_USER_TOKEN` — explicit override (most useful in CI /
//!    integration tests).
//! 2. `auth.json::providers.nous.access_token`, **only when not expiring**
//!    — i.e. `expires_at` is more than `_REFRESH_SKEW` seconds away.
//! 3. A user-supplied refresh hook ([`TokenReader`]). The Python reference
//!    refreshes the OAuth token via the Nous OAuth flow inside
//!    `hermes_cli.auth.resolve_nous_access_token`; in the Rust workspace we
//!    expose this as an injectable trait so the CLI can wire in a real
//!    refresh implementation without `hermes-config` taking a dependency on
//!    `hermes-cli`.
//! 4. The cached (potentially-expiring) token, as a last-resort fallback.
//!    Matches Python's behaviour when the refresh path raises.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde_json::Value;

/// Skew applied to `expires_at` checks. Tokens whose remaining lifetime is
/// at or below this many seconds are considered "expiring" and trigger a
/// refresh.
pub const NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 120;

/// Env var that lets operators bypass `auth.json` entirely.
const TOKEN_OVERRIDE_ENV: &str = "TOOL_GATEWAY_USER_TOKEN";

/// Parsed view of `auth.json::providers.nous`, returned by
/// [`read_nous_provider_state`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NousProviderState {
    pub access_token: Option<String>,
    pub expires_at: Option<String>,
}

/// Pluggable refresh hook. The CLI layer can register one that drives the
/// Nous OAuth refresh flow; absent a registration, we degrade gracefully.
pub trait TokenReader: Send + Sync {
    /// Refresh and return the current access token, if available.
    fn refresh(&self, skew_seconds: i64) -> Option<String>;
}

/// Resolve the Hermes auth-store path. Wraps `crate::paths`.
fn auth_json_path() -> PathBuf {
    crate::paths::auth_json_path()
}

/// Read `providers.nous` out of `auth.json`. Returns `Default` (all-`None`)
/// when the file is missing or malformed — never panics, never propagates
/// errors. Mirrors Python's `_read_nous_provider_state`.
pub fn read_nous_provider_state() -> NousProviderState {
    let path = auth_json_path();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return NousProviderState::default(),
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return NousProviderState::default(),
    };
    let providers = match value.get("providers") {
        Some(Value::Object(map)) => map,
        _ => return NousProviderState::default(),
    };
    let nous = match providers.get("nous") {
        Some(Value::Object(map)) => map,
        _ => return NousProviderState::default(),
    };
    NousProviderState {
        access_token: nous
            .get("access_token")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        expires_at: nous
            .get("expires_at")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    }
}

/// Parse an ISO-8601 timestamp like Python's `datetime.fromisoformat`.
/// Accepts a trailing `Z` (UTC) and naive timestamps (interpreted as UTC).
fn parse_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalised = if trimmed.ends_with('Z') {
        format!("{}+00:00", &trimmed[..trimmed.len() - 1])
    } else {
        trimmed.to_string()
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalised) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S") {
        return Some(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc));
    }
    None
}

/// Returns true when the cached token expires within `skew_seconds` (or
/// has no parseable `expires_at`, matching Python).
fn access_token_is_expiring(expires_at: Option<&str>, skew_seconds: i64) -> bool {
    let raw = match expires_at {
        Some(s) => s,
        None => return true,
    };
    let parsed = match parse_timestamp(raw) {
        Some(dt) => dt,
        None => return true,
    };
    let remaining = (parsed - Utc::now()).num_seconds();
    remaining <= skew_seconds.max(0)
}

/// Read a Nous OAuth subscriber access token.
///
/// Returns `None` when no token can be sourced from any layer. The
/// optional `reader` is invoked when the cached token is missing or close
/// to expiry; pass `None` to skip refresh (tests, server contexts without
/// the CLI loaded).
pub fn read_nous_access_token(reader: Option<&dyn TokenReader>) -> Option<String> {
    if let Ok(explicit) = std::env::var(TOKEN_OVERRIDE_ENV) {
        let trimmed = explicit.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let state = read_nous_provider_state();
    let cached = state
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    if let Some(ref tok) = cached {
        if !access_token_is_expiring(
            state.expires_at.as_deref(),
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        ) {
            return Some(tok.clone());
        }
    }

    if let Some(reader) = reader {
        if let Some(refreshed) = reader.refresh(NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS) {
            let trimmed = refreshed.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    cached
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::managed_gateway::test_lock;
    use serde_json::json;

    /// Sets `HERMES_HOME` to a tempdir and writes an optional `auth.json`.
    /// Restored when the guard drops.
    struct AuthGuard {
        _tmp: tempfile::TempDir,
        original_home: Option<String>,
        original_token: Option<String>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl AuthGuard {
        fn new(auth_json: Option<&Value>) -> Self {
            let g = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let original_home = std::env::var("HERMES_HOME").ok();
            let original_token = std::env::var(TOKEN_OVERRIDE_ENV).ok();

            std::env::set_var("HERMES_HOME", tmp.path());
            std::env::remove_var(TOKEN_OVERRIDE_ENV);

            if let Some(payload) = auth_json {
                let path = tmp.path().join("auth.json");
                std::fs::write(&path, serde_json::to_vec_pretty(payload).unwrap()).unwrap();
            }

            Self {
                _tmp: tmp,
                original_home,
                original_token,
                _g: g,
            }
        }
    }

    impl Drop for AuthGuard {
        fn drop(&mut self) {
            match self.original_home.take() {
                Some(v) => std::env::set_var("HERMES_HOME", v),
                None => std::env::remove_var("HERMES_HOME"),
            }
            match self.original_token.take() {
                Some(v) => std::env::set_var(TOKEN_OVERRIDE_ENV, v),
                None => std::env::remove_var(TOKEN_OVERRIDE_ENV),
            }
        }
    }

    fn iso_seconds_from_now(secs: i64) -> String {
        (Utc::now() + chrono::Duration::seconds(secs))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    #[test]
    fn parse_timestamp_accepts_z_and_offset() {
        let dt1 = parse_timestamp("2024-01-01T12:00:00Z").unwrap();
        let dt2 = parse_timestamp("2024-01-01T12:00:00+00:00").unwrap();
        assert_eq!(dt1, dt2);
        assert!(parse_timestamp("not a date").is_none());
        assert!(parse_timestamp("").is_none());
    }

    #[test]
    fn parse_timestamp_naive_assumed_utc() {
        let dt = parse_timestamp("2024-01-01T12:00:00").unwrap();
        assert_eq!(dt.to_rfc3339(), "2024-01-01T12:00:00+00:00");
    }

    #[test]
    fn expiring_when_no_timestamp() {
        assert!(access_token_is_expiring(None, 60));
        assert!(access_token_is_expiring(Some(""), 60));
        assert!(access_token_is_expiring(Some("garbage"), 60));
    }

    #[test]
    fn expiring_when_within_skew() {
        let near = iso_seconds_from_now(30);
        assert!(access_token_is_expiring(Some(&near), 60));

        let far = iso_seconds_from_now(3600);
        assert!(!access_token_is_expiring(Some(&far), 60));
    }

    #[test]
    fn read_state_returns_default_when_missing() {
        let _g = AuthGuard::new(None);
        assert_eq!(read_nous_provider_state(), NousProviderState::default());
    }

    #[test]
    fn read_state_returns_default_when_malformed() {
        // Each AuthGuard holds the global env lock — drop the first before
        // creating the second to avoid a self-deadlock.
        {
            let _g = AuthGuard::new(Some(&json!({"providers": "garbage"})));
            assert_eq!(read_nous_provider_state(), NousProviderState::default());
        }
        {
            let _g = AuthGuard::new(Some(&json!({"providers": {"nous": "still bad"}})));
            assert_eq!(read_nous_provider_state(), NousProviderState::default());
        }
    }

    #[test]
    fn read_state_returns_provider_block() {
        let payload = json!({
            "providers": {
                "nous": {
                    "access_token": "tok-1",
                    "expires_at": "2099-01-01T00:00:00Z",
                    "extra": "ignored"
                }
            }
        });
        let _g = AuthGuard::new(Some(&payload));
        let state = read_nous_provider_state();
        assert_eq!(state.access_token.as_deref(), Some("tok-1"));
        assert_eq!(state.expires_at.as_deref(), Some("2099-01-01T00:00:00Z"));
    }

    #[test]
    fn explicit_env_override_wins() {
        let payload = json!({
            "providers": {"nous": {"access_token": "from-disk"}}
        });
        let _g = AuthGuard::new(Some(&payload));
        std::env::set_var(TOKEN_OVERRIDE_ENV, "  override-token  ");
        assert_eq!(
            read_nous_access_token(None),
            Some("override-token".to_string())
        );
    }

    #[test]
    fn returns_cached_token_when_far_from_expiry() {
        let payload = json!({
            "providers": {
                "nous": {
                    "access_token": "fresh-tok",
                    "expires_at": iso_seconds_from_now(3600),
                }
            }
        });
        let _g = AuthGuard::new(Some(&payload));
        assert_eq!(read_nous_access_token(None), Some("fresh-tok".to_string()));
    }

    #[test]
    fn calls_refresh_when_token_expiring() {
        struct StubReader;
        impl TokenReader for StubReader {
            fn refresh(&self, _: i64) -> Option<String> {
                Some("refreshed-tok".into())
            }
        }
        let payload = json!({
            "providers": {
                "nous": {
                    "access_token": "old-tok",
                    "expires_at": iso_seconds_from_now(10),
                }
            }
        });
        let _g = AuthGuard::new(Some(&payload));
        assert_eq!(
            read_nous_access_token(Some(&StubReader)),
            Some("refreshed-tok".to_string())
        );
    }

    #[test]
    fn falls_back_to_cached_token_when_refresh_returns_none() {
        struct StubReader;
        impl TokenReader for StubReader {
            fn refresh(&self, _: i64) -> Option<String> {
                None
            }
        }
        let payload = json!({
            "providers": {
                "nous": {
                    "access_token": "stale-tok",
                    "expires_at": iso_seconds_from_now(10),
                }
            }
        });
        let _g = AuthGuard::new(Some(&payload));
        assert_eq!(
            read_nous_access_token(Some(&StubReader)),
            Some("stale-tok".to_string())
        );
    }

    #[test]
    fn returns_none_when_nothing_configured() {
        let _g = AuthGuard::new(None);
        assert_eq!(read_nous_access_token(None), None);
    }

    #[test]
    fn no_refresh_when_reader_absent_and_token_expiring() {
        let payload = json!({
            "providers": {
                "nous": {
                    "access_token": "stale-tok",
                    "expires_at": iso_seconds_from_now(10),
                }
            }
        });
        let _g = AuthGuard::new(Some(&payload));
        assert_eq!(read_nous_access_token(None), Some("stale-tok".to_string()));
    }
}
