//! Optional Bearer API key, IP allowlist, and per-IP rate limiting for `hermes-http`.
//!
//! Environment:
//! - `HERMES_HTTP_API_KEY` — if set, require `Authorization: Bearer <key>` for all routes except `/health`, and `/metrics` unless `HERMES_HTTP_METRICS_REQUIRE_AUTH=1`.
//! - `HERMES_HTTP_ALLOWED_IPS` — comma-separated client IPs (e.g. `127.0.0.1,::1`). When non-empty, only these IPs may access routes other than `/health` (metrics follow the same rule unless exempt below).
//! - `HERMES_HTTP_RATE_LIMIT_PER_MINUTE` — max requests per client IP per rolling 60s window (0 = disabled).
//! - `HERMES_HTTP_METRICS_REQUIRE_AUTH` — set to `1` to protect `/metrics` with the same Bearer rules.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, Response, StatusCode};
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

impl HttpSecurity {
    pub fn from_env() -> Self {
        let api_key = std::env::var("HERMES_HTTP_API_KEY")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| Arc::from(s.into_boxed_str()));
        let metrics_require_auth =
            std::env::var("HERMES_HTTP_METRICS_REQUIRE_AUTH").ok().as_deref() == Some("1");
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

    #[test]
    fn parse_allowed_ips_trims() {
        let v = parse_allowed_ips(" 127.0.0.1 , ::1 ");
        assert_eq!(v.len(), 2);
        assert!(v.contains(&"127.0.0.1".parse().unwrap()));
        assert!(v.contains(&"::1".parse().unwrap()));
    }
}
