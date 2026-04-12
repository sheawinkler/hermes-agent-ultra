//! Optional Bearer API key, IP allowlist, and per-IP rate limiting for `hermes-http`.
//!
//! Environment:
//! - `HERMES_HTTP_API_KEY` — if set, require `Authorization: Bearer <key>` for all routes except `/health`, and `/metrics` unless `HERMES_HTTP_METRICS_REQUIRE_AUTH=1`.
//! - `HERMES_HTTP_ALLOWED_IPS` — comma-separated client IPs (e.g. `127.0.0.1,::1`). When non-empty, only these IPs may access routes other than `/health` (metrics follow the same rule unless exempt below).
//! - `HERMES_HTTP_RATE_LIMIT_PER_MINUTE` — max requests per client IP per rolling 60s window (0 = disabled).
//! - `HERMES_HTTP_METRICS_REQUIRE_AUTH` — set to `1` to protect `/metrics` with the same Bearer rules.
//!
//! Policy admin routes (`/v1/policy/*`):
//! - `HERMES_POLICY_ADMIN_TOKEN` — preferred shared secret; mutating routes require header `X-Hermes-Policy-Admin: <same value>` (same env var for CLI `/policy`).
//! - `HERMES_HTTP_POLICY_ADMIN_KEY` — legacy alias for the same secret if `HERMES_POLICY_ADMIN_TOKEN` is unset.
//! - `HERMES_HTTP_POLICY_REQUIRE_ADMIN=1` — reject policy mutations with 503 unless an admin secret is configured (see above).
//! - `HERMES_HTTP_POLICY_ALLOWED_ACTORS` — comma-separated allowlist for JSON field `actor` on policy mutations; when non-empty, unknown actors get 403.
//! - `HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN=1` — require `X-Hermes-Policy-Admin` on `GET /v1/policy/export` (requires admin secret env to be set).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Request, Response, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct HttpSecurity {
    pub api_key: Option<Arc<str>>,
    pub metrics_require_auth: bool,
    pub rate_limit_per_minute: u32,
    /// When `Some` and non-empty, client IP must be in this set (not applied to `/health`).
    pub allowed_ips: Option<Vec<IpAddr>>,
}

/// Guards policy lifecycle HTTP APIs (admin token + actor allowlist).
#[derive(Clone, Default)]
pub struct PolicyGuardConfig {
    /// When set, mutating routes require `X-Hermes-Policy-Admin` matching this value.
    pub admin_key: Option<Arc<str>>,
    /// When true and `admin_key` is missing, policy mutations return 503.
    pub require_admin: bool,
    /// When non-empty, JSON `actor` must be one of these (trimmed, exact match).
    pub allowed_actors: Vec<String>,
    /// When true, `GET /v1/policy/export` requires a valid `X-Hermes-Policy-Admin` header.
    pub export_require_admin: bool,
}

impl PolicyGuardConfig {
    pub fn from_env() -> Self {
        let admin_key = std::env::var("HERMES_POLICY_ADMIN_TOKEN")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .or_else(|| {
                std::env::var("HERMES_HTTP_POLICY_ADMIN_KEY")
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .map(|s| Arc::from(s.into_boxed_str()));
        let require_admin = std::env::var("HERMES_HTTP_POLICY_REQUIRE_ADMIN")
            .ok()
            .as_deref()
            == Some("1");
        let export_require_admin = std::env::var("HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN")
            .ok()
            .as_deref()
            == Some("1");
        let allowed_actors: Vec<String> = std::env::var("HERMES_HTTP_POLICY_ALLOWED_ACTORS")
            .ok()
            .map(|s| {
                s.split(',')
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            admin_key,
            require_admin,
            allowed_actors,
            export_require_admin,
        }
    }

    /// Validate `actor` for policy mutation bodies.
    pub fn check_actor(&self, actor: &str) -> Result<(), &'static str> {
        let actor = actor.trim();
        if actor.is_empty() {
            return Err("actor must be non-empty");
        }
        if !self.allowed_actors.is_empty() && !self.allowed_actors.iter().any(|a| a == actor) {
            return Err("actor not in HERMES_HTTP_POLICY_ALLOWED_ACTORS");
        }
        Ok(())
    }

    /// Require admin header for mutating policy routes.
    pub fn check_mutation_admin(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        if self.require_admin && self.admin_key.is_none() {
            return Err("HERMES_HTTP_POLICY_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty");
        }
        if let Some(ref key) = self.admin_key {
            let got = header_policy_admin(headers);
            if got != Some(key.as_ref()) {
                return Err("missing or invalid X-Hermes-Policy-Admin");
            }
        }
        Ok(())
    }

    /// Optional admin for read-only export.
    pub fn check_export_admin(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        if !self.export_require_admin {
            return Ok(());
        }
        let Some(ref key) = self.admin_key else {
            return Err("HERMES_HTTP_POLICY_EXPORT_REQUIRE_ADMIN is set but HERMES_POLICY_ADMIN_TOKEN (or HERMES_HTTP_POLICY_ADMIN_KEY) is empty");
        };
        let got = header_policy_admin(headers);
        if got != Some(key.as_ref()) {
            return Err("missing or invalid X-Hermes-Policy-Admin");
        }
        Ok(())
    }
}

fn header_policy_admin(headers: &HeaderMap) -> Option<&str> {
    const NAME: &str = "x-hermes-policy-admin";
    headers
        .get(NAME)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

impl HttpSecurity {
    pub fn from_env() -> Self {
        let api_key = std::env::var("HERMES_HTTP_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| Arc::from(s.into_boxed_str()));
        let metrics_require_auth = std::env::var("HERMES_HTTP_METRICS_REQUIRE_AUTH")
            .ok()
            .as_deref()
            == Some("1");
        let rate_limit_per_minute = std::env::var("HERMES_HTTP_RATE_LIMIT_PER_MINUTE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let allowed_ips = parse_allowed_ips_from_env();
        Self {
            api_key,
            metrics_require_auth,
            rate_limit_per_minute,
            allowed_ips,
        }
    }
}

fn parse_allowed_ips_from_env() -> Option<Vec<IpAddr>> {
    let raw = std::env::var("HERMES_HTTP_ALLOWED_IPS").ok()?;
    let ips = parse_allowed_ips(&raw);
    if ips.is_empty() {
        None
    } else {
        Some(ips)
    }
}

/// Parse a comma-separated IP list (for tests and tooling).
pub fn parse_allowed_ips(raw: &str) -> Vec<IpAddr> {
    raw.split(',')
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .collect()
}

#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    per_minute: u32,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            per_minute,
        }
    }

    pub async fn allow(&self, key: String) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let mut g = self.inner.lock().await;
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let v = g.entry(key).or_default();
        v.retain(|t| now.duration_since(*t) < window);
        if v.len() as u32 >= self.per_minute {
            return false;
        }
        v.push(now);
        true
    }
}

fn client_key(req: &Request<Body>) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn client_ip(req: &Request<Body>) -> Option<IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
}

fn ip_allowed(security: &HttpSecurity, req: &Request<Body>) -> bool {
    let Some(ref allow) = security.allowed_ips else {
        return true;
    };
    if allow.is_empty() {
        return true;
    }
    let Some(ip) = client_ip(req) else {
        return false;
    };
    allow.contains(&ip)
}

pub async fn request_guard(
    security: Arc<HttpSecurity>,
    rate: Arc<RateLimiter>,
    req: Request<Body>,
    next: Next,
) -> Response<Body> {
    if req.method() == axum::http::Method::OPTIONS {
        return next.run(req).await;
    }

    let path = req.uri().path();
    if path == "/health" {
        return next.run(req).await;
    }
    if path == "/metrics" && !security.metrics_require_auth {
        if !ip_allowed(&security, &req) {
            hermes_telemetry::record_http_reject();
            return (StatusCode::FORBIDDEN, "client IP not in allowlist").into_response();
        }
        return next.run(req).await;
    }

    if !ip_allowed(&security, &req) {
        hermes_telemetry::record_http_reject();
        return (StatusCode::FORBIDDEN, "client IP not in allowlist").into_response();
    }

    if let Some(key) = security.api_key.as_ref() {
        let token = req
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer ").map(str::trim));
        let ok = token == Some(key.as_ref());
        if !ok {
            hermes_telemetry::record_http_reject();
            return (
                StatusCode::UNAUTHORIZED,
                "missing or invalid Authorization: Bearer token",
            )
                .into_response();
        }
    }

    let ck = client_key(&req);
    if !rate.allow(ck).await {
        hermes_telemetry::record_http_reject();
        return (StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded").into_response();
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parse_allowed_ips_trims() {
        let v = parse_allowed_ips(" 127.0.0.1 , ::1 ");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"127.0.0.1".parse().unwrap()));
        assert!(v.contains(&"::1".parse().unwrap()));
    }

    #[test]
    fn policy_guard_admin_header() {
        let mut p = PolicyGuardConfig::default();
        p.admin_key = Some(Arc::from("secret-token"));
        let mut ok = HeaderMap::new();
        ok.insert(
            axum::http::HeaderName::from_static("x-hermes-policy-admin"),
            HeaderValue::from_static("secret-token"),
        );
        assert!(p.check_mutation_admin(&ok).is_ok());

        let bad = HeaderMap::new();
        assert!(p.check_mutation_admin(&bad).is_err());
    }

    #[test]
    fn policy_guard_actor_allowlist() {
        let p = PolicyGuardConfig {
            admin_key: None,
            require_admin: false,
            allowed_actors: vec!["ops".to_string(), "trainer".to_string()],
            export_require_admin: false,
        };
        assert!(p.check_actor("ops").is_ok());
        assert!(p.check_actor("trainer").is_ok());
        assert!(p.check_actor("hacker").is_err());
    }
}
