//! Rust-native dashboard OpenID Connect authentication.
//!
//! This module deliberately keeps browser sessions separate from machine bearer
//! credentials. When OIDC is active, machine bearer access is disabled unless
//! the administrator explicitly enables its closed opt-in.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use axum::extract::{OriginalUri, Query};
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use jsonwebtoken::jwk::{
    AlgorithmParameters, Jwk, JwkSet, KeyAlgorithm, KeyOperations, PublicKeyUse,
};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::{Host, Url};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const SESSION_COOKIE: &str = "hermes_dashboard_session";
const LOGIN_COOKIE: &str = "hermes_dashboard_oidc_login";
const CALLBACK_PATH: &str = "/auth/oidc/callback";
const DEFAULT_SESSION_TTL_SECONDS: i64 = 8 * 60 * 60;
const MAX_SESSION_TTL_SECONDS: i64 = 31 * 24 * 60 * 60;
const DEFAULT_HTTP_TIMEOUT_MS: u64 = 10_000;
const MIN_HTTP_TIMEOUT_MS: u64 = 10;
const MAX_HTTP_TIMEOUT_MS: u64 = 60_000;
const MIN_SESSION_SECRET_BYTES: usize = 32;
const REFRESH_TOKEN_NONCE_BYTES: usize = 12;
const DEFAULT_JWKS_TTL: Duration = Duration::from_secs(300);
const MAX_JWKS_TTL: Duration = Duration::from_secs(3600);
const JWKS_CACHE_CAPACITY: usize = 8;
const SESSION_STORE_CAPACITY: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DashboardAuthMode {
    Bearer,
    None,
    Oidc,
}

impl DashboardAuthMode {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::None => "none",
            Self::Oidc => "oidc",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DashboardAuthConfig {
    pub mode: DashboardAuthMode,
    pub oidc: Option<DashboardOidcConfig>,
    pub config_error: Option<DashboardOidcConfigError>,
}

impl DashboardAuthConfig {
    pub fn from_env() -> Self {
        let provider = env_trim("HERMES_DASHBOARD_AUTH_PROVIDER")
            .or_else(|| env_trim("HERMES_DASHBOARD_AUTH_MODE"))
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_else(|| {
                if env_trim("HERMES_DASHBOARD_OIDC_ISSUER").is_some()
                    || env_trim("HERMES_DASHBOARD_OIDC_CLIENT_ID").is_some()
                {
                    "oidc".to_string()
                } else {
                    "bearer".to_string()
                }
            });

        match provider.as_str() {
            "none" | "off" | "disabled" => Self {
                mode: DashboardAuthMode::None,
                oidc: None,
                config_error: None,
            },
            "oidc" | "openid" | "sso" => match DashboardOidcConfig::from_env() {
                Ok(oidc) => Self {
                    mode: DashboardAuthMode::Oidc,
                    oidc: Some(oidc),
                    config_error: None,
                },
                Err(err) => Self {
                    mode: DashboardAuthMode::Oidc,
                    oidc: None,
                    config_error: Some(err),
                },
            },
            "bearer" | "api_key" | "apikey" => Self {
                mode: DashboardAuthMode::Bearer,
                oidc: None,
                config_error: None,
            },
            _ => Self {
                mode: DashboardAuthMode::Oidc,
                oidc: None,
                config_error: Some(DashboardOidcConfigError::InvalidAuthProvider),
            },
        }
    }

    pub fn oidc_enabled(&self) -> bool {
        self.mode == DashboardAuthMode::Oidc
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum DashboardOidcConfigError {
    #[error("dashboard authentication provider is unsupported")]
    InvalidAuthProvider,
    #[error("{0} is required for dashboard OIDC")]
    MissingRequired(&'static str),
    #[error("HERMES_DASHBOARD_SESSION_SECRET is weak; provide at least 32 nontrivial bytes")]
    WeakSessionSecret,
    #[error("HERMES_DASHBOARD_OIDC_ISSUER must be an absolute HTTPS URL or loopback HTTP URL")]
    InvalidIssuer,
    #[error(
        "HERMES_DASHBOARD_OIDC_REDIRECT_URI must be an absolute HTTPS URL or loopback HTTP URL"
    )]
    InvalidRedirectUri,
    #[error("HERMES_DASHBOARD_OIDC_REDIRECT_URI must use {CALLBACK_PATH} or an explicitly allowlisted callback path")]
    CallbackPathNotAllowed,
    #[error("HERMES_DASHBOARD_OIDC_ALLOWED_CALLBACK_PATHS contains an unsafe callback path")]
    InvalidCallbackPathAllowlist,
    #[error("HERMES_DASHBOARD_OIDC_SCOPES must include openid")]
    MissingOpenidScope,
    #[error("HERMES_DASHBOARD_OIDC_HTTP_TIMEOUT_MS must be an integer between 10 and 60000")]
    InvalidHttpTimeout,
    #[error("HERMES_DASHBOARD_SESSION_TTL_SECONDS must be an integer between 1 and 2678400")]
    InvalidSessionTtl,
    #[error("HERMES_DASHBOARD_COOKIE_SECURE must be true or false")]
    InvalidCookieSecureBoolean,
    #[error("HERMES_DASHBOARD_OIDC_ALLOW_MACHINE_BEARER must be true or false")]
    InvalidMachineBearerBoolean,
    #[error("HERMES_DASHBOARD_OIDC_ENABLE_PROVIDER_LOGOUT must be true or false")]
    InvalidProviderLogoutBoolean,
    #[error("HERMES_DASHBOARD_COOKIE_SECURE must match the redirect URI scheme")]
    CookieSecureMismatch,
    #[error("OIDC endpoint overrides must set authorization, token, and JWKS endpoints together")]
    PartialEndpointOverrides,
    #[error("configured OIDC endpoint must be an absolute HTTPS URL or loopback HTTP URL")]
    InvalidEndpointOverride,
}

#[derive(Clone)]
pub struct DashboardOidcConfig {
    pub issuer: String,
    pub client_id: String,
    client_secret: Option<String>,
    pub redirect_uri: String,
    pub callback_path: String,
    pub scopes: Vec<String>,
    pub allowed_emails: HashSet<String>,
    pub allowed_domains: HashSet<String>,
    pub session_ttl_seconds: i64,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub jwks_uri: Option<String>,
    pub cookie_secure: bool,
    pub allow_machine_bearer: bool,
    pub enable_provider_logout: bool,
    http_timeout: Duration,
    session_secret: Vec<u8>,
}

impl std::fmt::Debug for DashboardOidcConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DashboardOidcConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret_configured", &self.client_secret.is_some())
            .field("redirect_uri", &self.redirect_uri)
            .field("callback_path", &self.callback_path)
            .field("scopes", &self.scopes)
            .field("allowed_emails", &self.allowed_emails)
            .field("allowed_domains", &self.allowed_domains)
            .field("session_ttl_seconds", &self.session_ttl_seconds)
            .field("authorization_endpoint", &self.authorization_endpoint)
            .field("token_endpoint", &self.token_endpoint)
            .field("jwks_uri", &self.jwks_uri)
            .field("cookie_secure", &self.cookie_secure)
            .field("allow_machine_bearer", &self.allow_machine_bearer)
            .field("enable_provider_logout", &self.enable_provider_logout)
            .field("http_timeout", &self.http_timeout)
            .field(
                "session_secret_configured",
                &!self.session_secret.is_empty(),
            )
            .finish()
    }
}

impl DashboardOidcConfig {
    pub fn from_env() -> Result<Self, DashboardOidcConfigError> {
        let issuer = normalize_configured_issuer(&required_env("HERMES_DASHBOARD_OIDC_ISSUER")?)?;
        let client_id = required_env("HERMES_DASHBOARD_OIDC_CLIENT_ID")?;
        let redirect = env_trim("HERMES_DASHBOARD_OIDC_REDIRECT_URI")
            .unwrap_or_else(|| "http://127.0.0.1:8787/auth/oidc/callback".to_string());
        let callback_allowlist = callback_path_allowlist_from_env()?;
        let (redirect_uri, callback_path, redirect_is_https) =
            validate_redirect_uri(&redirect, &callback_allowlist)?;
        let session_secret = required_env("HERMES_DASHBOARD_SESSION_SECRET")
            .or_else(|_| required_env("HERMES_DASHBOARD_OIDC_SESSION_SECRET"))?;
        validate_session_secret(session_secret.as_bytes())?;
        let configured_cookie_secure = optional_cookie_secure_bool()?;
        let cookie_secure = redirect_is_https;
        if configured_cookie_secure.is_some_and(|value| value != cookie_secure) {
            return Err(DashboardOidcConfigError::CookieSecureMismatch);
        }

        let scopes = parse_list_env("HERMES_DASHBOARD_OIDC_SCOPES")
            .unwrap_or_else(|| vec!["openid".into(), "email".into(), "profile".into()]);
        if !scopes.iter().any(|scope| scope == "openid") {
            return Err(DashboardOidcConfigError::MissingOpenidScope);
        }

        let authorization_endpoint = env_trim("HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT");
        let token_endpoint = env_trim("HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT");
        let jwks_uri = env_trim("HERMES_DASHBOARD_OIDC_JWKS_URI");
        let configured_endpoint_count = [
            authorization_endpoint.is_some(),
            token_endpoint.is_some(),
            jwks_uri.is_some(),
        ]
        .into_iter()
        .filter(|configured| *configured)
        .count();
        if configured_endpoint_count != 0 && configured_endpoint_count != 3 {
            return Err(DashboardOidcConfigError::PartialEndpointOverrides);
        }
        for endpoint in [&authorization_endpoint, &token_endpoint, &jwks_uri]
            .into_iter()
            .flatten()
        {
            if parse_safe_http_url(endpoint).is_err() {
                return Err(DashboardOidcConfigError::InvalidEndpointOverride);
            }
        }

        let http_timeout = Duration::from_millis(http_timeout_from_env()?);
        Ok(Self {
            issuer,
            client_id,
            client_secret: env_trim("HERMES_DASHBOARD_OIDC_CLIENT_SECRET"),
            redirect_uri,
            callback_path,
            scopes,
            allowed_emails: parse_list_env("HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS")
                .unwrap_or_default()
                .into_iter()
                .map(|value| value.to_ascii_lowercase())
                .collect(),
            allowed_domains: parse_list_env("HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS")
                .unwrap_or_default()
                .into_iter()
                .map(|value| value.to_ascii_lowercase())
                .collect(),
            session_ttl_seconds: session_ttl_from_env()?,
            authorization_endpoint,
            token_endpoint,
            jwks_uri,
            cookie_secure,
            allow_machine_bearer: closed_bool_env(
                "HERMES_DASHBOARD_OIDC_ALLOW_MACHINE_BEARER",
                DashboardOidcConfigError::InvalidMachineBearerBoolean,
            )?,
            enable_provider_logout: closed_bool_env(
                "HERMES_DASHBOARD_OIDC_ENABLE_PROVIDER_LOGOUT",
                DashboardOidcConfigError::InvalidProviderLogoutBoolean,
            )?,
            http_timeout,
            session_secret: session_secret.into_bytes(),
        })
    }

    fn session_secret(&self) -> &[u8] {
        &self.session_secret
    }

    fn http_client(&self) -> Result<reqwest::Client, OidcProtocolError> {
        reqwest::Client::builder()
            .timeout(self.http_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| OidcProtocolError::HttpClientBuild)
    }

    pub fn is_callback_path(&self, path: &str) -> bool {
        path == self.callback_path
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OidcProviderMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub end_session_endpoint: Option<String>,
    #[serde(default)]
    pub revocation_endpoint: Option<String>,
}

#[derive(Clone, Deserialize)]
struct OidcTokenResponse {
    id_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn contains(&self, expected: &str) -> bool {
        match self {
            Self::One(value) => value == expected,
            Self::Many(values) => values.iter().any(|value| value == expected),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardIdTokenClaims {
    pub iss: String,
    pub sub: String,
    pub aud: Audience,
    #[serde(default)]
    pub azp: Option<String>,
    pub exp: u64,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub email_verified: Option<bool>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardSessionClaims {
    pub sid: String,
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub iss: String,
    pub exp: i64,
}

#[derive(Clone, Deserialize, Serialize)]
struct LoginCookie {
    state: String,
    nonce: String,
    code_verifier: String,
    return_to: String,
    exp: i64,
}

#[derive(Debug, Deserialize)]
pub struct OidcLoginParams {
    #[serde(default)]
    return_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OidcCallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    #[serde(default)]
    _error_description: Option<String>,
}

#[derive(Debug, Error)]
pub enum OidcProtocolError {
    #[error("OIDC HTTP client initialization failed")]
    HttpClientBuild,
    #[error("OIDC discovery request failed")]
    DiscoveryRequest,
    #[error("OIDC discovery endpoint returned an error")]
    DiscoveryRejected,
    #[error("OIDC discovery response is malformed")]
    DiscoveryMalformed,
    #[error("OIDC discovery issuer mismatch")]
    DiscoveryIssuerMismatch,
    #[error("OIDC provider metadata is unsafe or incomplete")]
    InvalidMetadata,
    #[error("OIDC authorization endpoint is invalid")]
    InvalidAuthorizationEndpoint,
    #[error("OIDC token exchange request failed")]
    TokenRequest,
    #[error("OIDC token endpoint returned an error")]
    TokenRejected,
    #[error("OIDC token response is malformed")]
    TokenMalformed,
    #[error("OIDC token revocation request failed")]
    RevocationRequest,
    #[error("OIDC token revocation endpoint returned an error")]
    RevocationRejected,
    #[error("OIDC JWKS request failed")]
    JwksRequest,
    #[error("OIDC JWKS endpoint returned an error")]
    JwksRejected,
    #[error("OIDC JWKS response is malformed")]
    JwksMalformed,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum OidcTokenVerificationError {
    #[error("OIDC token header is invalid")]
    InvalidHeader,
    #[error("OIDC token uses an unsupported signing algorithm")]
    UnsupportedAlgorithm,
    #[error("OIDC token key identifier is not present in JWKS")]
    UnknownKeyId,
    #[error("OIDC token key identifier is ambiguous")]
    AmbiguousKeyId,
    #[error("OIDC token key type does not match its signing algorithm")]
    KeyTypeMismatch,
    #[error("OIDC JWK algorithm does not match the token algorithm")]
    JwkAlgorithmMismatch,
    #[error("OIDC JWK cannot verify token signatures")]
    JwkUseMismatch,
    #[error("OIDC JWK is invalid")]
    InvalidJwk,
    #[error("OIDC token signature or registered claims were rejected")]
    TokenRejected,
    #[error("OIDC token audience mismatch")]
    AudienceMismatch,
    #[error("OIDC token nonce mismatch")]
    NonceMismatch,
}

#[derive(Debug, Error)]
enum OidcIdTokenError {
    #[error(transparent)]
    Provider(#[from] OidcProtocolError),
    #[error(transparent)]
    Verification(#[from] OidcTokenVerificationError),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DashboardSessionError {
    #[error("session cookie missing")]
    MissingCookie,
    #[error("session cookie is invalid")]
    InvalidCookie,
    #[error("session issuer does not match the active OIDC issuer")]
    IssuerMismatch,
    #[error("session expired")]
    Expired,
    #[error("session has been revoked")]
    Revoked,
}

#[derive(Debug, Error)]
enum SignedCookieError {
    #[error("cookie serialization failed")]
    Serialize,
    #[error("cookie signer initialization failed")]
    SignerInit,
    #[error("signed cookie is malformed")]
    Malformed,
    #[error("signed cookie signature is malformed")]
    SignatureMalformed,
    #[error("signed cookie signature mismatch")]
    SignatureMismatch,
    #[error("signed cookie body is malformed")]
    BodyMalformed,
    #[error("signed cookie payload is invalid")]
    PayloadInvalid,
}

#[derive(Debug, PartialEq, Eq)]
enum JwkSelectionError {
    Missing,
    Ambiguous,
    TypeMismatch,
    AlgorithmMismatch,
    KeyUseMismatch,
}

struct JwksCacheEntry {
    jwks: Arc<JwkSet>,
    expires_at: Instant,
}

struct JwksCacheState {
    entries: HashMap<String, JwksCacheEntry>,
    order: VecDeque<String>,
}

pub struct JwksCache {
    state: Mutex<JwksCacheState>,
    capacity: usize,
    fallback_ttl: Duration,
    max_ttl: Duration,
}

impl Default for JwksCache {
    fn default() -> Self {
        Self::new(JWKS_CACHE_CAPACITY, DEFAULT_JWKS_TTL, MAX_JWKS_TTL)
    }
}

impl JwksCache {
    pub fn new(capacity: usize, fallback_ttl: Duration, max_ttl: Duration) -> Self {
        Self {
            state: Mutex::new(JwksCacheState {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
            capacity: capacity.max(1),
            fallback_ttl,
            max_ttl,
        }
    }

    async fn get_or_fetch(
        &self,
        client: &reqwest::Client,
        jwks_uri: &str,
        force_refresh: bool,
    ) -> Result<Arc<JwkSet>, OidcProtocolError> {
        if !force_refresh {
            if let Some(jwks) = self.get(jwks_uri) {
                return Ok(jwks);
            }
        }

        let (jwks, ttl) = fetch_jwks(client, jwks_uri, self.fallback_ttl, self.max_ttl).await?;
        let jwks = Arc::new(jwks);
        self.insert(jwks_uri.to_string(), jwks.clone(), ttl);
        Ok(jwks)
    }

    fn get(&self, jwks_uri: &str) -> Option<Arc<JwkSet>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expired = state
            .entries
            .get(jwks_uri)
            .is_some_and(|entry| entry.expires_at <= Instant::now());
        if expired {
            state.entries.remove(jwks_uri);
            state.order.retain(|key| key != jwks_uri);
            return None;
        }
        let jwks = state.entries.get(jwks_uri)?.jwks.clone();
        touch_cache_key(&mut state.order, jwks_uri);
        Some(jwks)
    }

    fn insert(&self, jwks_uri: String, jwks: Arc<JwkSet>, ttl: Duration) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.entries.remove(&jwks_uri);
        state.order.retain(|key| key != &jwks_uri);
        if ttl.is_zero() {
            return;
        }
        while state.entries.len() >= self.capacity {
            if let Some(oldest) = state.order.pop_front() {
                state.entries.remove(&oldest);
            } else {
                break;
            }
        }
        state.order.push_back(jwks_uri.clone());
        state.entries.insert(
            jwks_uri,
            JwksCacheEntry {
                jwks,
                expires_at: Instant::now() + ttl,
            },
        );
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }
}

fn touch_cache_key(order: &mut VecDeque<String>, key: &str) {
    order.retain(|entry| entry != key);
    order.push_back(key.to_string());
}

#[derive(Clone)]
struct EncryptedRefreshToken {
    nonce: [u8; REFRESH_TOKEN_NONCE_BYTES],
    ciphertext: Vec<u8>,
}

#[derive(Clone)]
struct DashboardSessionRecord {
    issuer: String,
    sub: String,
    exp: i64,
    refresh_token: Option<EncryptedRefreshToken>,
}

struct DashboardSessionStoreState {
    entries: HashMap<String, DashboardSessionRecord>,
    order: VecDeque<String>,
}

struct DashboardSessionStore {
    state: Mutex<DashboardSessionStoreState>,
    capacity: usize,
}

impl DashboardSessionStore {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::new(DashboardSessionStoreState {
                entries: HashMap::new(),
                order: VecDeque::new(),
            }),
            capacity: capacity.max(1),
        }
    }

    fn insert(&self, sid: String, record: DashboardSessionRecord) {
        if record.exp <= Utc::now().timestamp() {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.entries.remove(&sid);
        state.order.retain(|entry| entry != &sid);
        while state.entries.len() >= self.capacity {
            if let Some(oldest) = state.order.pop_front() {
                state.entries.remove(&oldest);
            } else {
                break;
            }
        }
        state.order.push_back(sid.clone());
        state.entries.insert(sid, record);
    }

    fn get(&self, sid: &str) -> Option<DashboardSessionRecord> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let expired = state
            .entries
            .get(sid)
            .is_some_and(|record| record.exp <= Utc::now().timestamp());
        if expired {
            state.entries.remove(sid);
            state.order.retain(|entry| entry != sid);
            return None;
        }
        let record = state.entries.get(sid)?.clone();
        touch_cache_key(&mut state.order, sid);
        Some(record)
    }

    /// Atomically consumes a session before refresh or logout. A consumed
    /// session cookie cannot authorize a concurrent request or be replayed.
    fn take(&self, sid: &str) -> Option<DashboardSessionRecord> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.order.retain(|entry| entry != sid);
        let record = state.entries.remove(sid)?;
        (record.exp > Utc::now().timestamp()).then_some(record)
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entries
            .len()
    }
}

static GLOBAL_JWKS_CACHE: LazyLock<JwksCache> = LazyLock::new(JwksCache::default);
static GLOBAL_SESSION_STORE: LazyLock<DashboardSessionStore> =
    LazyLock::new(|| DashboardSessionStore::new(SESSION_STORE_CAPACITY));

pub fn oidc_exempt_path(path: &str, config: Option<&DashboardOidcConfig>) -> bool {
    path == "/auth/status"
        || path == "/auth/logout"
        || path == "/auth/oidc/login"
        || path == "/auth/oidc/refresh"
        || path == CALLBACK_PATH
        || config.is_some_and(|oidc| oidc.is_callback_path(path))
}

pub fn verify_session_from_headers(
    headers: &HeaderMap,
    config: &DashboardOidcConfig,
) -> Result<DashboardSessionClaims, DashboardSessionError> {
    let session = session_from_headers(headers, config)?;
    if session.exp <= Utc::now().timestamp() {
        return Err(DashboardSessionError::Expired);
    }
    let record = GLOBAL_SESSION_STORE
        .get(&session.sid)
        .ok_or(DashboardSessionError::Revoked)?;
    if record.issuer != session.iss || record.sub != session.sub || record.exp != session.exp {
        return Err(DashboardSessionError::Revoked);
    }
    Ok(session)
}

fn session_from_headers(
    headers: &HeaderMap,
    config: &DashboardOidcConfig,
) -> Result<DashboardSessionClaims, DashboardSessionError> {
    let raw = cookie_value(headers, SESSION_COOKIE).ok_or(DashboardSessionError::MissingCookie)?;
    let session: DashboardSessionClaims = verify_signed_json(raw, config.session_secret())
        .map_err(|_| DashboardSessionError::InvalidCookie)?;
    if session.iss != config.issuer {
        return Err(DashboardSessionError::IssuerMismatch);
    }
    Ok(session)
}

pub async fn auth_status(headers: HeaderMap) -> Response {
    let auth = DashboardAuthConfig::from_env();
    if auth.oidc_enabled() && auth.config_error.is_some() {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({
                "error": auth.config_error.as_ref().map(ToString::to_string),
                "mode": auth.mode.as_str(),
                "configured": false,
            }),
        );
    }

    let mut authenticated = false;
    let mut identity = Value::Null;
    if let Some(oidc) = &auth.oidc {
        if let Ok(session) = verify_session_from_headers(&headers, oidc) {
            authenticated = true;
            identity = json!({
                "sub": session.sub,
                "email": session.email,
                "name": session.name,
                "issuer": session.iss,
                "expires_at_unix": session.exp,
            });
        }
    }
    json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "mode": auth.mode.as_str(),
            "configured": true,
            "authenticated": authenticated,
            "identity": identity,
            "oidc": auth.oidc.as_ref().map(oidc_public_status),
        }),
    )
}

pub async fn auth_logout(headers: HeaderMap) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let secure = auth.oidc.as_ref().is_some_and(|oidc| oidc.cookie_secure);
    let Some(oidc) = &auth.oidc else {
        return local_logout_response(secure, "not_configured");
    };
    let session = take_session_for_logout(&headers, oidc);
    let Ok(client) = oidc.http_client() else {
        return local_logout_response(secure, "unavailable");
    };
    let Ok(metadata) = resolve_metadata(&client, oidc).await else {
        return local_logout_response(secure, "unavailable");
    };

    if let (Some(session), Some(endpoint)) = (&session, metadata.revocation_endpoint.as_deref()) {
        if let Some(refresh_token) = session
            .refresh_token
            .as_ref()
            .and_then(|token| decrypt_refresh_token(token, oidc.session_secret()))
        {
            let _ = revoke_refresh_token(&client, oidc, endpoint, &refresh_token).await;
        }
    }

    if !oidc.enable_provider_logout {
        return local_logout_response(secure, "disabled");
    }
    let Some(endpoint) = metadata.end_session_endpoint else {
        return local_logout_response(secure, "unsupported");
    };
    let Ok(location) = provider_logout_location(&endpoint, &oidc.redirect_uri) else {
        return local_logout_response(secure, "unsafe");
    };

    let mut response = redirect_response(&location);
    clear_cookie(&mut response, SESSION_COOKIE, secure);
    clear_cookie(&mut response, LOGIN_COOKIE, secure);
    response
}

pub async fn oidc_login(Query(params): Query<OidcLoginParams>) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let Some(oidc) = &auth.oidc else {
        return oidc_unavailable_response(&auth);
    };
    let Ok(client) = oidc.http_client() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": OidcProtocolError::HttpClientBuild.to_string()}),
        );
    };
    let metadata = match resolve_metadata(&client, &oidc).await {
        Ok(metadata) => metadata,
        Err(err) => {
            return json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": err.to_string()}),
            )
        }
    };

    let pkce = hermes_auth::generate_pkce_pair();
    let state = hermes_auth::generate_oauth_nonce();
    let nonce = hermes_auth::generate_oauth_nonce();
    let return_to = sanitize_return_to(params.return_to.as_deref());
    let login = LoginCookie {
        state: state.clone(),
        nonce: nonce.clone(),
        code_verifier: pkce.verifier,
        return_to,
        exp: Utc::now().timestamp() + 10 * 60,
    };
    let login_cookie = match sign_cookie(
        LOGIN_COOKIE,
        &login,
        oidc.session_secret(),
        10 * 60,
        oidc.cookie_secure,
    ) {
        Ok(cookie) => cookie,
        Err(err) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": err.to_string()}),
            )
        }
    };
    let auth_url = match build_authorization_url(&oidc, &metadata, &state, &nonce, &pkce.challenge)
    {
        Ok(url) => url,
        Err(err) => {
            return json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": err.to_string()}),
            )
        }
    };

    redirect_with_cookie(&auth_url, &login_cookie)
}

pub async fn oidc_callback(
    OriginalUri(uri): OriginalUri,
    Query(params): Query<OidcCallbackParams>,
    headers: HeaderMap,
) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let Some(oidc) = &auth.oidc else {
        return oidc_unavailable_response(&auth);
    };
    if !oidc.is_callback_path(uri.path()) {
        return json_response(
            StatusCode::NOT_FOUND,
            json!({"error": "OIDC callback path is not configured"}),
        );
    }
    let Some(state) = params
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "missing state"}));
    };
    let Some(raw_login) = cookie_value(&headers, LOGIN_COOKIE) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "missing OIDC login cookie"}),
        );
    };
    let login: LoginCookie = match verify_signed_json(raw_login, oidc.session_secret()) {
        Ok(login) => login,
        Err(_) => {
            return json_response(
                StatusCode::UNAUTHORIZED,
                json!({"error": "invalid OIDC login cookie"}),
            )
        }
    };
    if login.exp <= Utc::now().timestamp() {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "OIDC login cookie expired"}),
        );
    }
    if login.state != state {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "OIDC state mismatch"}),
        );
    }
    if params.error.is_some() {
        let mut response = json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "OIDC provider returned an authorization error"}),
        );
        clear_cookie(&mut response, LOGIN_COOKIE, oidc.cookie_secure);
        return response;
    }
    let Some(code) = params
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "missing code"}));
    };

    let Ok(client) = oidc.http_client() else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": OidcProtocolError::HttpClientBuild.to_string()}),
        );
    };
    let metadata = match resolve_metadata(&client, &oidc).await {
        Ok(metadata) => metadata,
        Err(err) => {
            return json_response(
                StatusCode::SERVICE_UNAVAILABLE,
                json!({"error": err.to_string()}),
            )
        }
    };
    let token = match exchange_code(&client, &oidc, &metadata, code, &login.code_verifier).await {
        Ok(token) => token,
        Err(err) => {
            return json_response(StatusCode::BAD_GATEWAY, json!({"error": err.to_string()}))
        }
    };
    let claims = match verify_id_token(
        &client,
        &token.id_token,
        &oidc,
        &metadata,
        Some(&login.nonce),
        &GLOBAL_JWKS_CACHE,
    )
    .await
    {
        Ok(claims) => claims,
        Err(OidcIdTokenError::Provider(err)) => {
            return json_response(StatusCode::BAD_GATEWAY, json!({"error": err.to_string()}));
        }
        Err(OidcIdTokenError::Verification(err)) => {
            return json_response(StatusCode::UNAUTHORIZED, json!({"error": err.to_string()}));
        }
    };
    if !claim_allowed(&claims, &oidc) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "OIDC identity is not allowlisted"}),
        );
    }

    let Some(session) = new_dashboard_session(&claims, oidc.session_ttl_seconds) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "OIDC token expiration is not usable for a dashboard session"}),
        );
    };
    let session_max_age = session.exp.saturating_sub(Utc::now().timestamp());
    let session_cookie = match sign_cookie(
        SESSION_COOKIE,
        &session,
        oidc.session_secret(),
        session_max_age,
        oidc.cookie_secure,
    ) {
        Ok(cookie) => cookie,
        Err(err) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": err.to_string()}),
            )
        }
    };
    let refresh_token = match encrypt_refresh_token(token.refresh_token, oidc.session_secret()) {
        Ok(token) => token,
        Err(()) => {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "OIDC refresh token could not be secured"}),
            );
        }
    };
    GLOBAL_SESSION_STORE.insert(
        session.sid.clone(),
        DashboardSessionRecord {
            issuer: session.iss.clone(),
            sub: session.sub.clone(),
            exp: session.exp,
            refresh_token,
        },
    );
    let mut response = redirect_with_cookie(&login.return_to, &session_cookie);
    clear_cookie(&mut response, LOGIN_COOKIE, oidc.cookie_secure);
    response
}

/// Exchanges the server-held refresh token once and rotates the local session.
/// The old session is consumed before any provider I/O, so it cannot be replayed
/// if a refresh fails or races with another request.
pub async fn oidc_refresh(headers: HeaderMap) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let Some(oidc) = &auth.oidc else {
        return oidc_unavailable_response(&auth);
    };
    let session = match verify_session_from_headers(&headers, oidc) {
        Ok(session) => session,
        Err(_) => return refresh_failure_response(oidc.cookie_secure),
    };
    let Some(record) = GLOBAL_SESSION_STORE.take(&session.sid) else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    if record.issuer != session.iss || record.sub != session.sub || record.exp != session.exp {
        return refresh_failure_response(oidc.cookie_secure);
    }
    let Some(refresh_token) = record
        .refresh_token
        .as_ref()
        .and_then(|token| decrypt_refresh_token(token, oidc.session_secret()))
    else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    let Ok(client) = oidc.http_client() else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    let Ok(metadata) = resolve_metadata(&client, oidc).await else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    let Ok(token) = exchange_refresh_token(&client, oidc, &metadata, &refresh_token).await else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    let claims = match verify_id_token(
        &client,
        &token.id_token,
        oidc,
        &metadata,
        None,
        &GLOBAL_JWKS_CACHE,
    )
    .await
    {
        Ok(claims)
            if claim_allowed(&claims, oidc)
                && refreshed_identity_matches_session(&claims, &record) =>
        {
            claims
        }
        _ => return refresh_failure_response(oidc.cookie_secure),
    };
    let Some(next_session) = new_dashboard_session(&claims, oidc.session_ttl_seconds) else {
        return refresh_failure_response(oidc.cookie_secure);
    };
    let session_max_age = next_session.exp.saturating_sub(Utc::now().timestamp());
    let session_cookie = match sign_cookie(
        SESSION_COOKIE,
        &next_session,
        oidc.session_secret(),
        session_max_age,
        oidc.cookie_secure,
    ) {
        Ok(cookie) => cookie,
        Err(_) => return refresh_failure_response(oidc.cookie_secure),
    };
    let next_refresh_token = match encrypt_refresh_token(token.refresh_token, oidc.session_secret())
    {
        Ok(Some(token)) => Some(token),
        Ok(None) => record.refresh_token,
        Err(()) => return refresh_failure_response(oidc.cookie_secure),
    };
    GLOBAL_SESSION_STORE.insert(
        next_session.sid.clone(),
        DashboardSessionRecord {
            issuer: next_session.iss.clone(),
            sub: next_session.sub.clone(),
            exp: next_session.exp,
            refresh_token: next_refresh_token,
        },
    );
    let mut response = json_response(StatusCode::OK, json!({"status": "ok", "refreshed": true}));
    if let Ok(cookie) = HeaderValue::from_str(&session_cookie) {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    clear_cookie(&mut response, LOGIN_COOKIE, oidc.cookie_secure);
    response
}

pub async fn discover_oidc_metadata(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<OidcProviderMetadata, OidcProtocolError> {
    let issuer = normalize_runtime_issuer(issuer).ok_or(OidcProtocolError::InvalidMetadata)?;
    let url = format!("{issuer}/.well-known/openid-configuration");
    let metadata = client
        .get(url)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .send()
        .await
        .map_err(|_| OidcProtocolError::DiscoveryRequest)?
        .error_for_status()
        .map_err(|_| OidcProtocolError::DiscoveryRejected)?
        .json::<OidcProviderMetadata>()
        .await
        .map_err(|_| OidcProtocolError::DiscoveryMalformed)?;
    validate_discovered_metadata(&metadata, &issuer)?;
    Ok(metadata)
}

async fn resolve_metadata(
    client: &reqwest::Client,
    config: &DashboardOidcConfig,
) -> Result<OidcProviderMetadata, OidcProtocolError> {
    match (
        &config.authorization_endpoint,
        &config.token_endpoint,
        &config.jwks_uri,
    ) {
        (Some(authorization_endpoint), Some(token_endpoint), Some(jwks_uri)) => {
            let metadata = OidcProviderMetadata {
                issuer: config.issuer.clone(),
                authorization_endpoint: authorization_endpoint.clone(),
                token_endpoint: token_endpoint.clone(),
                jwks_uri: jwks_uri.clone(),
                end_session_endpoint: None,
                revocation_endpoint: None,
            };
            validate_discovered_metadata(&metadata, &config.issuer)?;
            Ok(metadata)
        }
        (None, None, None) => discover_oidc_metadata(client, &config.issuer).await,
        _ => Err(OidcProtocolError::InvalidMetadata),
    }
}

fn validate_discovered_metadata(
    metadata: &OidcProviderMetadata,
    expected_issuer: &str,
) -> Result<(), OidcProtocolError> {
    let metadata_issuer =
        normalize_runtime_issuer(&metadata.issuer).ok_or(OidcProtocolError::InvalidMetadata)?;
    if metadata_issuer != expected_issuer {
        return Err(OidcProtocolError::DiscoveryIssuerMismatch);
    }
    for endpoint in [
        &metadata.authorization_endpoint,
        &metadata.token_endpoint,
        &metadata.jwks_uri,
    ] {
        if parse_safe_http_url(endpoint).is_err() {
            return Err(OidcProtocolError::InvalidMetadata);
        }
    }
    for endpoint in [
        metadata.end_session_endpoint.as_deref(),
        metadata.revocation_endpoint.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if parse_safe_http_url(endpoint).is_err() {
            return Err(OidcProtocolError::InvalidMetadata);
        }
    }
    Ok(())
}

fn build_authorization_url(
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    state: &str,
    nonce: &str,
    code_challenge: &str,
) -> Result<String, OidcProtocolError> {
    let mut url = Url::parse(&metadata.authorization_endpoint)
        .map_err(|_| OidcProtocolError::InvalidAuthorizationEndpoint)?;
    if parse_safe_http_url(url.as_str()).is_err() {
        return Err(OidcProtocolError::InvalidAuthorizationEndpoint);
    }
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &config.redirect_uri)
        .append_pair("scope", &config.scopes.join(" "))
        .append_pair("state", state)
        .append_pair("nonce", nonce)
        .append_pair("code_challenge", code_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url.to_string())
}

async fn exchange_code(
    client: &reqwest::Client,
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    code: &str,
    code_verifier: &str,
) -> Result<OidcTokenResponse, OidcProtocolError> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("client_id", config.client_id.clone()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("code_verifier", code_verifier.to_string()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    client
        .post(&metadata.token_endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .form(&form)
        .send()
        .await
        .map_err(|_| OidcProtocolError::TokenRequest)?
        .error_for_status()
        .map_err(|_| OidcProtocolError::TokenRejected)?
        .json::<OidcTokenResponse>()
        .await
        .map_err(|_| OidcProtocolError::TokenMalformed)
}

async fn exchange_refresh_token(
    client: &reqwest::Client,
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    refresh_token: &str,
) -> Result<OidcTokenResponse, OidcProtocolError> {
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("client_id", config.client_id.clone()),
        ("refresh_token", refresh_token.to_string()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    client
        .post(&metadata.token_endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .form(&form)
        .send()
        .await
        .map_err(|_| OidcProtocolError::TokenRequest)?
        .error_for_status()
        .map_err(|_| OidcProtocolError::TokenRejected)?
        .json::<OidcTokenResponse>()
        .await
        .map_err(|_| OidcProtocolError::TokenMalformed)
}

async fn revoke_refresh_token(
    client: &reqwest::Client,
    config: &DashboardOidcConfig,
    endpoint: &str,
    refresh_token: &str,
) -> Result<(), OidcProtocolError> {
    let mut form = vec![
        ("token", refresh_token.to_string()),
        ("token_type_hint", "refresh_token".to_string()),
        ("client_id", config.client_id.clone()),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    client
        .post(endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .form(&form)
        .send()
        .await
        .map_err(|_| OidcProtocolError::RevocationRequest)?
        .error_for_status()
        .map_err(|_| OidcProtocolError::RevocationRejected)?;
    Ok(())
}

async fn verify_id_token(
    client: &reqwest::Client,
    token: &str,
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    expected_nonce: Option<&str>,
    cache: &JwksCache,
) -> Result<DashboardIdTokenClaims, OidcIdTokenError> {
    let header = decode_header(token).map_err(|_| OidcTokenVerificationError::InvalidHeader)?;
    ensure_supported_algorithm(header.alg)?;
    let jwks = cache
        .get_or_fetch(client, &metadata.jwks_uri, false)
        .await?;
    let jwks = if matches!(
        select_jwk(&jwks, header.kid.as_deref(), header.alg),
        Err(JwkSelectionError::Missing)
    ) {
        cache.get_or_fetch(client, &metadata.jwks_uri, true).await?
    } else {
        jwks
    };
    Ok(verify_id_token_with_jwks(
        token,
        &jwks,
        config,
        expected_nonce,
    )?)
}

pub fn verify_id_token_with_jwks(
    token: &str,
    jwks: &JwkSet,
    config: &DashboardOidcConfig,
    expected_nonce: Option<&str>,
) -> Result<DashboardIdTokenClaims, OidcTokenVerificationError> {
    let header = decode_header(token).map_err(|_| OidcTokenVerificationError::InvalidHeader)?;
    ensure_supported_algorithm(header.alg)?;
    let jwk = select_jwk(jwks, header.kid.as_deref(), header.alg).map_err(|err| match err {
        JwkSelectionError::Missing => OidcTokenVerificationError::UnknownKeyId,
        JwkSelectionError::Ambiguous => OidcTokenVerificationError::AmbiguousKeyId,
        JwkSelectionError::TypeMismatch => OidcTokenVerificationError::KeyTypeMismatch,
        JwkSelectionError::AlgorithmMismatch => OidcTokenVerificationError::JwkAlgorithmMismatch,
        JwkSelectionError::KeyUseMismatch => OidcTokenVerificationError::JwkUseMismatch,
    })?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| OidcTokenVerificationError::InvalidJwk)?;
    let mut validation = Validation::new(header.alg);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.set_audience(&[config.client_id.as_str()]);
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.required_spec_claims.insert("aud".to_string());
    validation.required_spec_claims.insert("exp".to_string());
    validation.required_spec_claims.insert("iss".to_string());
    validation.required_spec_claims.insert("sub".to_string());
    let claims = decode::<DashboardIdTokenClaims>(token, &key, &validation)
        .map_err(|_| OidcTokenVerificationError::TokenRejected)?
        .claims;
    if !audience_is_acceptable(&claims, &config.client_id) {
        return Err(OidcTokenVerificationError::AudienceMismatch);
    }
    if let Some(expected) = expected_nonce {
        if claims.nonce.as_deref() != Some(expected) {
            return Err(OidcTokenVerificationError::NonceMismatch);
        }
    }
    Ok(claims)
}

fn ensure_supported_algorithm(algorithm: Algorithm) -> Result<(), OidcTokenVerificationError> {
    if matches!(
        algorithm,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    ) {
        Ok(())
    } else {
        Err(OidcTokenVerificationError::UnsupportedAlgorithm)
    }
}

fn select_jwk<'a>(
    set: &'a JwkSet,
    kid: Option<&str>,
    signing_algorithm: Algorithm,
) -> Result<&'a Jwk, JwkSelectionError> {
    let mut selected = None;
    for candidate in &set.keys {
        if kid.is_none_or(|kid| candidate.common.key_id.as_deref() == Some(kid)) {
            if selected.replace(candidate).is_some() {
                return Err(JwkSelectionError::Ambiguous);
            }
        }
    }
    let jwk = selected.ok_or(JwkSelectionError::Missing)?;
    let compatible_key_type = matches!(
        (signing_algorithm, &jwk.algorithm),
        (
            Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512,
            AlgorithmParameters::RSA(_)
        ) | (
            Algorithm::ES256 | Algorithm::ES384,
            AlgorithmParameters::EllipticCurve(_)
        )
    );
    if !compatible_key_type {
        return Err(JwkSelectionError::TypeMismatch);
    }
    if jwk
        .common
        .key_algorithm
        .is_some_and(|algorithm| !jwk_algorithm_matches(algorithm, signing_algorithm))
    {
        return Err(JwkSelectionError::AlgorithmMismatch);
    }
    let key_use_is_invalid = jwk
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|usage| usage != &PublicKeyUse::Signature);
    let key_operations_are_invalid = jwk
        .common
        .key_operations
        .as_ref()
        .is_some_and(|operations| {
            !operations
                .iter()
                .any(|operation| matches!(operation, KeyOperations::Verify))
        });
    if key_use_is_invalid || key_operations_are_invalid {
        return Err(JwkSelectionError::KeyUseMismatch);
    }
    Ok(jwk)
}

fn jwk_algorithm_matches(key_algorithm: KeyAlgorithm, signing_algorithm: Algorithm) -> bool {
    matches!(
        (key_algorithm, signing_algorithm),
        (KeyAlgorithm::RS256, Algorithm::RS256)
            | (KeyAlgorithm::RS384, Algorithm::RS384)
            | (KeyAlgorithm::RS512, Algorithm::RS512)
            | (KeyAlgorithm::ES256, Algorithm::ES256)
            | (KeyAlgorithm::ES384, Algorithm::ES384)
    )
}

async fn fetch_jwks(
    client: &reqwest::Client,
    jwks_uri: &str,
    fallback_ttl: Duration,
    max_ttl: Duration,
) -> Result<(JwkSet, Duration), OidcProtocolError> {
    let response = client
        .get(jwks_uri)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .send()
        .await
        .map_err(|_| OidcProtocolError::JwksRequest)?
        .error_for_status()
        .map_err(|_| OidcProtocolError::JwksRejected)?;
    let ttl = jwks_cache_ttl(response.headers(), fallback_ttl, max_ttl);
    let jwks = response
        .json::<JwkSet>()
        .await
        .map_err(|_| OidcProtocolError::JwksMalformed)?;
    Ok((jwks, ttl))
}

fn jwks_cache_ttl(headers: &HeaderMap, fallback_ttl: Duration, max_ttl: Duration) -> Duration {
    let Some(cache_control) = headers
        .get("cache-control")
        .and_then(|value| value.to_str().ok())
    else {
        return fallback_ttl.min(max_ttl);
    };
    if cache_control.split(',').map(str::trim).any(|directive| {
        directive.eq_ignore_ascii_case("no-store") || directive.eq_ignore_ascii_case("no-cache")
    }) {
        return Duration::ZERO;
    }
    let max_age = cache_control
        .split(',')
        .map(str::trim)
        .find_map(|directive| {
            let (name, value) = directive.split_once('=')?;
            if name.trim().eq_ignore_ascii_case("max-age") {
                value.trim().trim_matches('"').parse::<u64>().ok()
            } else {
                None
            }
        });
    max_age
        .map(Duration::from_secs)
        .unwrap_or(fallback_ttl)
        .min(max_ttl)
}

fn audience_is_acceptable(claims: &DashboardIdTokenClaims, client_id: &str) -> bool {
    claims.aud.contains(client_id)
        && match &claims.aud {
            Audience::One(_) => true,
            Audience::Many(_) => claims.azp.as_deref() == Some(client_id),
        }
}

fn claim_allowed(claims: &DashboardIdTokenClaims, config: &DashboardOidcConfig) -> bool {
    if config.allowed_emails.is_empty() && config.allowed_domains.is_empty() {
        return true;
    }
    if claims.email_verified != Some(true) {
        return false;
    }
    let Some(email) = claims
        .email
        .as_deref()
        .map(|value| value.to_ascii_lowercase())
    else {
        return false;
    };
    if config.allowed_emails.contains(&email) {
        return true;
    }
    let Some((_, domain)) = email.rsplit_once('@') else {
        return false;
    };
    config.allowed_domains.contains(domain)
}

fn refreshed_identity_matches_session(
    claims: &DashboardIdTokenClaims,
    session: &DashboardSessionRecord,
) -> bool {
    claims.iss == session.issuer && claims.sub == session.sub
}

fn new_dashboard_session(
    claims: &DashboardIdTokenClaims,
    configured_ttl_seconds: i64,
) -> Option<DashboardSessionClaims> {
    let now = Utc::now().timestamp();
    let upstream_exp = i64::try_from(claims.exp).unwrap_or(i64::MAX);
    let exp = now.saturating_add(configured_ttl_seconds).min(upstream_exp);
    (exp > now).then(|| DashboardSessionClaims {
        sid: Uuid::new_v4().to_string(),
        sub: claims.sub.clone(),
        email: claims.email.clone(),
        name: claims.name.clone(),
        iss: claims.iss.clone(),
        exp,
    })
}

fn refresh_token_cipher(session_secret: &[u8]) -> Result<Aes256Gcm, ()> {
    let mut digest = Sha256::new();
    digest.update(b"hermes-dashboard-oidc-refresh-token-v1");
    digest.update(session_secret);
    Aes256Gcm::new_from_slice(digest.finalize().as_slice()).map_err(|_| ())
}

fn encrypt_refresh_token(
    token: Option<String>,
    session_secret: &[u8],
) -> Result<Option<EncryptedRefreshToken>, ()> {
    let Some(token) = token.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    let cipher = refresh_token_cipher(session_secret)?;
    let entropy = URL_SAFE_NO_PAD
        .decode(hermes_auth::generate_oauth_nonce())
        .map_err(|_| ())?;
    let mut nonce = [0_u8; REFRESH_TOKEN_NONCE_BYTES];
    nonce.copy_from_slice(entropy.get(..REFRESH_TOKEN_NONCE_BYTES).ok_or(())?);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), token.as_bytes())
        .map_err(|_| ())?;
    Ok(Some(EncryptedRefreshToken { nonce, ciphertext }))
}

fn decrypt_refresh_token(
    encrypted: &EncryptedRefreshToken,
    session_secret: &[u8],
) -> Option<String> {
    let cipher = refresh_token_cipher(session_secret).ok()?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&encrypted.nonce),
            encrypted.ciphertext.as_ref(),
        )
        .ok()?;
    let token = String::from_utf8(plaintext).ok()?;
    (!token.trim().is_empty()).then_some(token)
}

fn take_session_for_logout(
    headers: &HeaderMap,
    config: &DashboardOidcConfig,
) -> Option<DashboardSessionRecord> {
    let session = session_from_headers(headers, config).ok()?;
    let record = GLOBAL_SESSION_STORE.take(&session.sid)?;
    (record.issuer == session.iss && record.sub == session.sub && record.exp == session.exp)
        .then_some(record)
}

fn refresh_failure_response(secure: bool) -> Response {
    let mut response = json_response(
        StatusCode::UNAUTHORIZED,
        json!({"error": "OIDC dashboard session refresh failed"}),
    );
    clear_cookie(&mut response, SESSION_COOKIE, secure);
    clear_cookie(&mut response, LOGIN_COOKIE, secure);
    response
}

fn oidc_public_status(config: &DashboardOidcConfig) -> Value {
    json!({
        "issuer": config.issuer,
        "client_id_configured": !config.client_id.is_empty(),
        "redirect_uri": config.redirect_uri,
        "callback_path": config.callback_path,
        "scopes": config.scopes,
        "allowed_email_count": config.allowed_emails.len(),
        "allowed_domain_count": config.allowed_domains.len(),
        "authorization_endpoint_configured": config.authorization_endpoint.is_some(),
        "token_endpoint_configured": config.token_endpoint.is_some(),
        "jwks_uri_configured": config.jwks_uri.is_some(),
        "cookie_secure": config.cookie_secure,
        "session_ttl_seconds": config.session_ttl_seconds,
        "machine_bearer_allowed": config.allow_machine_bearer,
        "provider_logout_enabled": config.enable_provider_logout,
    })
}

fn sign_cookie<T: Serialize>(
    name: &str,
    payload: &T,
    secret: &[u8],
    max_age_seconds: i64,
    secure: bool,
) -> Result<String, SignedCookieError> {
    let value = sign_json(payload, secret)?;
    let secure_attr = if secure { "; Secure" } else { "" };
    Ok(format!(
        "{name}={value}; Path=/; Max-Age={max_age_seconds}; HttpOnly; SameSite=Lax{secure_attr}"
    ))
}

fn sign_json<T: Serialize>(payload: &T, secret: &[u8]) -> Result<String, SignedCookieError> {
    let body = serde_json::to_vec(payload).map_err(|_| SignedCookieError::Serialize)?;
    let body_b64 = URL_SAFE_NO_PAD.encode(body);
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(secret).map_err(|_| SignedCookieError::SignerInit)?;
    mac.update(body_b64.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{body_b64}.{sig}"))
}

fn verify_signed_json<T: DeserializeOwned>(
    value: &str,
    secret: &[u8],
) -> Result<T, SignedCookieError> {
    let (body_b64, sig_b64) = value.split_once('.').ok_or(SignedCookieError::Malformed)?;
    let expected = URL_SAFE_NO_PAD
        .decode(sig_b64.as_bytes())
        .map_err(|_| SignedCookieError::SignatureMalformed)?;
    let mut mac =
        <HmacSha256 as Mac>::new_from_slice(secret).map_err(|_| SignedCookieError::SignerInit)?;
    mac.update(body_b64.as_bytes());
    mac.verify_slice(&expected)
        .map_err(|_| SignedCookieError::SignatureMismatch)?;
    let body = URL_SAFE_NO_PAD
        .decode(body_b64.as_bytes())
        .map_err(|_| SignedCookieError::BodyMalformed)?;
    serde_json::from_slice::<T>(&body).map_err(|_| SignedCookieError::PayloadInvalid)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let raw = headers.get(COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let Some((key, value)) = part.trim().split_once('=') else {
            continue;
        };
        if key == name {
            return Some(value);
        }
    }
    None
}

fn clear_cookie(response: &mut Response, name: &str, secure: bool) {
    let secure_attr = if secure { "; Secure" } else { "" };
    let cookie = format!("{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax{secure_attr}");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

fn redirect_with_cookie(location: &str, cookie: &str) -> Response {
    let mut response = redirect_response(location);
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    response
}

fn redirect_response(location: &str) -> Response {
    let mut response = (StatusCode::FOUND, "").into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(LOCATION, value);
    }
    response
}

fn local_logout_response(secure: bool, provider_logout: &str) -> Response {
    let mut response = json_response(
        StatusCode::OK,
        json!({
            "status": "ok",
            "logged_out": true,
            "provider_logout": provider_logout,
        }),
    );
    clear_cookie(&mut response, SESSION_COOKIE, secure);
    clear_cookie(&mut response, LOGIN_COOKIE, secure);
    response
}

fn provider_logout_location(
    endpoint: &str,
    redirect_uri: &str,
) -> Result<String, OidcProtocolError> {
    let mut endpoint =
        parse_safe_http_url(endpoint).map_err(|_| OidcProtocolError::InvalidMetadata)?;
    let mut local_return =
        parse_safe_http_url(redirect_uri).map_err(|_| OidcProtocolError::InvalidMetadata)?;
    local_return.set_path("/");
    local_return.set_query(None);
    local_return.set_fragment(None);
    endpoint
        .query_pairs_mut()
        .append_pair("post_logout_redirect_uri", local_return.as_str());
    Ok(endpoint.to_string())
}

fn json_response(status: StatusCode, payload: Value) -> Response {
    (status, axum::Json(payload)).into_response()
}

fn oidc_unavailable_response(auth: &DashboardAuthConfig) -> Response {
    let error = auth
        .config_error
        .as_ref()
        .map(ToString::to_string)
        .unwrap_or_else(|| "dashboard OIDC is not configured".to_string());
    json_response(StatusCode::SERVICE_UNAVAILABLE, json!({"error": error}))
}

fn sanitize_return_to(input: Option<&str>) -> String {
    let value = input.unwrap_or("/").trim();
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.starts_with("/\\")
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return "/".to_string();
    }
    let Ok(base) = Url::parse("https://hermes.invalid/") else {
        return "/".to_string();
    };
    let Ok(joined) = base.join(value) else {
        return "/".to_string();
    };
    if joined.scheme() != "https" || joined.host_str() != Some("hermes.invalid") {
        return "/".to_string();
    }
    value.to_string()
}

fn env_trim(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_env(key: &'static str) -> Result<String, DashboardOidcConfigError> {
    env_trim(key).ok_or(DashboardOidcConfigError::MissingRequired(key))
}

fn parse_list_env(key: &str) -> Option<Vec<String>> {
    let raw = env_trim(key)?;
    let values: Vec<String> = raw
        .split([',', ' '])
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect();
    (!values.is_empty()).then_some(values)
}

fn closed_bool_env(
    key: &str,
    invalid: DashboardOidcConfigError,
) -> Result<bool, DashboardOidcConfigError> {
    match env_trim(key).as_deref() {
        None | Some("false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(invalid),
    }
}

fn optional_cookie_secure_bool() -> Result<Option<bool>, DashboardOidcConfigError> {
    match env_trim("HERMES_DASHBOARD_COOKIE_SECURE").as_deref() {
        None => Ok(None),
        Some("false") => Ok(Some(false)),
        Some("true") => Ok(Some(true)),
        Some(_) => Err(DashboardOidcConfigError::InvalidCookieSecureBoolean),
    }
}

fn http_timeout_from_env() -> Result<u64, DashboardOidcConfigError> {
    match env_trim("HERMES_DASHBOARD_OIDC_HTTP_TIMEOUT_MS") {
        None => Ok(DEFAULT_HTTP_TIMEOUT_MS),
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|timeout| (MIN_HTTP_TIMEOUT_MS..=MAX_HTTP_TIMEOUT_MS).contains(timeout))
            .ok_or(DashboardOidcConfigError::InvalidHttpTimeout),
    }
}

fn session_ttl_from_env() -> Result<i64, DashboardOidcConfigError> {
    match env_trim("HERMES_DASHBOARD_SESSION_TTL_SECONDS") {
        None => Ok(DEFAULT_SESSION_TTL_SECONDS),
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|ttl| (1..=MAX_SESSION_TTL_SECONDS).contains(ttl))
            .ok_or(DashboardOidcConfigError::InvalidSessionTtl),
    }
}

fn callback_path_allowlist_from_env() -> Result<HashSet<String>, DashboardOidcConfigError> {
    let Some(paths) = parse_list_env("HERMES_DASHBOARD_OIDC_ALLOWED_CALLBACK_PATHS") else {
        return Ok(HashSet::new());
    };
    let mut allowed = HashSet::with_capacity(paths.len());
    for path in paths {
        if !is_safe_callback_path(&path) {
            return Err(DashboardOidcConfigError::InvalidCallbackPathAllowlist);
        }
        allowed.insert(path);
    }
    Ok(allowed)
}

fn is_safe_callback_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("//")
        && !path.contains('\\')
        && !path.contains(['?', '#', '{', '}', '*'])
        && !path.chars().any(char::is_control)
        && !path.starts_with("/v1/")
        && !matches!(
            path,
            "/health"
                | "/metrics"
                | "/auth/status"
                | "/auth/logout"
                | "/auth/oidc/login"
                | "/auth/oidc/refresh"
        )
}

fn validate_session_secret(secret: &[u8]) -> Result<(), DashboardOidcConfigError> {
    let all_same = secret
        .first()
        .is_some_and(|first| secret.iter().all(|byte| byte == first));
    let repeated_half =
        secret.len() % 2 == 0 && secret[..secret.len() / 2] == secret[secret.len() / 2..];
    if secret.len() < MIN_SESSION_SECRET_BYTES || all_same || repeated_half {
        return Err(DashboardOidcConfigError::WeakSessionSecret);
    }
    let lower = String::from_utf8_lossy(secret).to_ascii_lowercase();
    if ["password", "change-me", "replace-me", "default", "secret"]
        .iter()
        .any(|weak| lower == *weak || lower.starts_with(weak))
    {
        return Err(DashboardOidcConfigError::WeakSessionSecret);
    }
    Ok(())
}

fn normalize_configured_issuer(input: &str) -> Result<String, DashboardOidcConfigError> {
    normalize_runtime_issuer(input).ok_or(DashboardOidcConfigError::InvalidIssuer)
}

fn normalize_runtime_issuer(input: &str) -> Option<String> {
    let mut url = parse_safe_http_url(input).ok()?;
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(if path.is_empty() { "/" } else { &path });
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn validate_redirect_uri(
    input: &str,
    callback_allowlist: &HashSet<String>,
) -> Result<(String, String, bool), DashboardOidcConfigError> {
    let url =
        parse_safe_http_url(input).map_err(|_| DashboardOidcConfigError::InvalidRedirectUri)?;
    if url.fragment().is_some() {
        return Err(DashboardOidcConfigError::InvalidRedirectUri);
    }
    let callback_path = url.path().to_string();
    if callback_path != CALLBACK_PATH && !callback_allowlist.contains(&callback_path) {
        return Err(DashboardOidcConfigError::CallbackPathNotAllowed);
    }
    Ok((
        url.to_string(),
        callback_path,
        url.scheme().eq_ignore_ascii_case("https"),
    ))
}

fn parse_safe_http_url(input: &str) -> Result<Url, ()> {
    let url = Url::parse(input).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    if url.scheme() == "http" && !url_host_is_loopback(&url) {
        return Err(());
    }
    Ok(url)
}

fn url_host_is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::COOKIE;
    use jsonwebtoken::{encode, EncodingKey, Header};

    fn test_config() -> DashboardOidcConfig {
        DashboardOidcConfig {
            issuer: "https://issuer.example".to_string(),
            client_id: "hermes-dashboard".to_string(),
            client_secret: None,
            redirect_uri: "http://127.0.0.1:8787/auth/oidc/callback".to_string(),
            callback_path: CALLBACK_PATH.to_string(),
            scopes: vec!["openid".to_string(), "email".to_string()],
            allowed_emails: HashSet::new(),
            allowed_domains: HashSet::new(),
            session_ttl_seconds: 3600,
            authorization_endpoint: Some("https://issuer.example/authorize".to_string()),
            token_endpoint: Some("https://issuer.example/token".to_string()),
            jwks_uri: Some("https://issuer.example/jwks".to_string()),
            cookie_secure: false,
            allow_machine_bearer: false,
            enable_provider_logout: false,
            http_timeout: Duration::from_secs(1),
            session_secret: b"unit-test-session-signing-key-7a9c3e1d".to_vec(),
        }
    }

    #[test]
    fn signed_session_cookie_round_trips_and_rejects_tamper_and_expiry() {
        let cfg = test_config();
        let session = DashboardSessionClaims {
            sid: Uuid::new_v4().to_string(),
            sub: "user-1".to_string(),
            email: Some("user@example.com".to_string()),
            name: Some("User".to_string()),
            iss: cfg.issuer.clone(),
            exp: Utc::now().timestamp() + 60,
        };
        GLOBAL_SESSION_STORE.insert(
            session.sid.clone(),
            DashboardSessionRecord {
                issuer: session.iss.clone(),
                sub: session.sub.clone(),
                exp: session.exp,
                refresh_token: None,
            },
        );
        let signed = sign_json(&session, cfg.session_secret()).expect("sign");
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={signed}")).expect("header"),
        );
        let verified = verify_session_from_headers(&headers, &cfg).expect("verify");
        assert_eq!(verified.sub, "user-1");

        let tampered = format!(
            "{SESSION_COOKIE}={}.bad",
            signed.split('.').next().expect("body")
        );
        let mut bad = HeaderMap::new();
        bad.insert(COOKIE, HeaderValue::from_str(&tampered).expect("header"));
        assert!(matches!(
            verify_session_from_headers(&bad, &cfg),
            Err(DashboardSessionError::InvalidCookie)
        ));

        let expired = DashboardSessionClaims {
            exp: Utc::now().timestamp() - 1,
            ..session
        };
        let expired = sign_json(&expired, cfg.session_secret()).expect("sign");
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={expired}")).expect("header"),
        );
        assert!(matches!(
            verify_session_from_headers(&headers, &cfg),
            Err(DashboardSessionError::Expired)
        ));
    }

    #[test]
    fn server_held_refresh_tokens_are_aead_encrypted_and_key_bound() {
        let secret = b"unit-test-session-signing-key-7a9c3e1d";
        let raw = "refresh-fixture-value";
        let mut encrypted = encrypt_refresh_token(Some(raw.to_string()), secret)
            .expect("encryption")
            .expect("nonempty token");
        assert_ne!(encrypted.ciphertext.as_slice(), raw.as_bytes());
        assert_eq!(
            decrypt_refresh_token(&encrypted, secret).as_deref(),
            Some(raw)
        );
        assert!(decrypt_refresh_token(&encrypted, b"different-session-secret").is_none());
        encrypted.ciphertext[0] ^= 1;
        assert!(decrypt_refresh_token(&encrypted, secret).is_none());
    }

    #[test]
    fn refreshed_identity_must_match_the_consumed_session_principal() {
        let cfg = test_config();
        let record = DashboardSessionRecord {
            issuer: cfg.issuer.clone(),
            sub: "user-1".to_string(),
            exp: 4_102_444_800,
            refresh_token: None,
        };
        let mut claims = DashboardIdTokenClaims {
            iss: cfg.issuer.clone(),
            sub: record.sub.clone(),
            aud: Audience::One(cfg.client_id.clone()),
            azp: None,
            exp: 4_102_444_800,
            nonce: None,
            email: Some("user@example.com".to_string()),
            email_verified: Some(true),
            name: None,
        };
        assert!(refreshed_identity_matches_session(&claims, &record));
        claims.sub = "other-user".to_string();
        assert!(!refreshed_identity_matches_session(&claims, &record));
        claims.sub = record.sub.clone();
        claims.iss = "https://other-issuer.example".to_string();
        assert!(!refreshed_identity_matches_session(&claims, &record));
    }

    #[test]
    fn authorization_url_uses_pkce_nonce_and_exact_redirect_uri() {
        let cfg = test_config();
        let metadata = OidcProviderMetadata {
            issuer: cfg.issuer.clone(),
            authorization_endpoint: "https://issuer.example/authorize".to_string(),
            token_endpoint: "https://issuer.example/token".to_string(),
            jwks_uri: "https://issuer.example/jwks".to_string(),
            end_session_endpoint: None,
            revocation_endpoint: None,
        };
        let url = build_authorization_url(&cfg, &metadata, "state-1", "nonce-1", "challenge-1")
            .expect("url");
        let url = Url::parse(&url).expect("parse");
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query.get("response_type"), Some(&"code".to_string()));
        assert_eq!(query.get("redirect_uri"), Some(&cfg.redirect_uri));
        assert_eq!(
            query.get("code_challenge_method"),
            Some(&"S256".to_string())
        );
        assert_eq!(query.get("nonce"), Some(&"nonce-1".to_string()));
    }

    #[test]
    fn allowlist_and_open_redirect_protection_fail_closed() {
        let mut cfg = test_config();
        cfg.allowed_domains.insert("example.com".to_string());
        let claims = DashboardIdTokenClaims {
            iss: cfg.issuer.clone(),
            sub: "user-1".to_string(),
            aud: Audience::One(cfg.client_id.clone()),
            azp: None,
            exp: 4_102_444_800,
            nonce: Some("nonce".to_string()),
            email: Some("user@example.com".to_string()),
            email_verified: Some(true),
            name: None,
        };
        assert!(claim_allowed(&claims, &cfg));
        let mut unverified = claims.clone();
        unverified.email_verified = Some(false);
        assert!(!claim_allowed(&unverified, &cfg));
        cfg.allowed_domains.clear();
        cfg.allowed_domains.insert("other.example".to_string());
        assert!(!claim_allowed(&claims, &cfg));
        assert_eq!(
            sanitize_return_to(Some("/dashboard?tab=1")),
            "/dashboard?tab=1"
        );
        assert_eq!(sanitize_return_to(Some("https://evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("//evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("/\\evil.example")), "/");
    }

    #[test]
    fn multiple_audiences_require_authorized_party() {
        let cfg = test_config();
        let mut claims = DashboardIdTokenClaims {
            iss: cfg.issuer.clone(),
            sub: "user-1".to_string(),
            aud: Audience::Many(vec![cfg.client_id.clone(), "other-client".to_string()]),
            azp: None,
            exp: 4_102_444_800,
            nonce: None,
            email: None,
            email_verified: None,
            name: None,
        };
        assert!(!audience_is_acceptable(&claims, &cfg.client_id));
        claims.azp = Some(cfg.client_id.clone());
        assert!(audience_is_acceptable(&claims, &cfg.client_id));
    }

    #[test]
    fn dashboard_session_lifetime_never_exceeds_id_token_expiry() {
        let cfg = test_config();
        let token_exp = (Utc::now().timestamp() + 15) as u64;
        let claims = DashboardIdTokenClaims {
            iss: cfg.issuer.clone(),
            sub: "user-1".to_string(),
            aud: Audience::One(cfg.client_id.clone()),
            azp: None,
            exp: token_exp,
            nonce: None,
            email: Some("user@example.com".to_string()),
            email_verified: Some(true),
            name: None,
        };
        let session = new_dashboard_session(&claims, 3600).expect("usable token expiration");
        assert!(session.exp <= token_exp as i64);
        assert!(session.exp > Utc::now().timestamp());
    }

    #[test]
    fn jwt_verifier_rejects_symmetric_and_ambiguous_keys() {
        let cfg = test_config();
        let token = encode(
            &Header::new(Algorithm::HS256),
            &json!({
                "iss": cfg.issuer,
                "sub": "user-1",
                "aud": cfg.client_id,
                "exp": 4_102_444_800_u64,
                "nonce": "nonce-1"
            }),
            &EncodingKey::from_secret(b"secret"),
        )
        .expect("encode");
        let jwks: JwkSet = serde_json::from_value(json!({
            "keys": [{
                "kty": "oct",
                "alg": "HS256",
                "kid": "test",
                "k": URL_SAFE_NO_PAD.encode(b"secret")
            }]
        }))
        .expect("jwks");
        assert!(matches!(
            verify_id_token_with_jwks(&token, &jwks, &test_config(), Some("nonce-1")),
            Err(OidcTokenVerificationError::UnsupportedAlgorithm)
        ));

        let ambiguous: JwkSet = serde_json::from_value(json!({
            "keys": [
                {"kty":"RSA","kid":"same","n":"AQAB","e":"AQAB"},
                {"kty":"RSA","kid":"same","n":"AQAB","e":"AQAB"}
            ]
        }))
        .expect("jwks");
        assert!(matches!(
            select_jwk(&ambiguous, Some("same"), Algorithm::RS256),
            Err(JwkSelectionError::Ambiguous)
        ));

        let wrong_algorithm: JwkSet = serde_json::from_value(json!({
            "keys": [{"kty":"RSA","kid":"rsa","alg":"HS256","n":"AQAB","e":"AQAB"}]
        }))
        .expect("jwks");
        assert!(matches!(
            select_jwk(&wrong_algorithm, Some("rsa"), Algorithm::RS256),
            Err(JwkSelectionError::AlgorithmMismatch)
        ));

        let encryption_key: JwkSet = serde_json::from_value(json!({
            "keys": [{"kty":"RSA","kid":"rsa","use":"enc","n":"AQAB","e":"AQAB"}]
        }))
        .expect("jwks");
        assert!(matches!(
            select_jwk(&encryption_key, Some("rsa"), Algorithm::RS256),
            Err(JwkSelectionError::KeyUseMismatch)
        ));
    }

    #[test]
    fn redirect_validation_requires_exact_loopback_or_https_callback() {
        assert!(
            validate_redirect_uri("http://127.0.0.1:8787/auth/oidc/callback", &HashSet::new())
                .is_ok()
        );
        assert!(validate_redirect_uri(
            "http://dashboard.example/auth/oidc/callback",
            &HashSet::new()
        )
        .is_err());
        assert!(validate_redirect_uri(
            "https://dashboard.example/auth/oidc/callback",
            &HashSet::new()
        )
        .is_ok());
        let allowed = HashSet::from(["/auth/custom/callback".to_string(), "/callback".to_string()]);
        assert!(
            validate_redirect_uri("https://dashboard.example/auth/custom/callback", &allowed)
                .is_ok()
        );
        assert!(validate_redirect_uri("https://dashboard.example/callback", &allowed).is_ok());
        assert!(!is_safe_callback_path("/v1/rpc"));
        assert!(!is_safe_callback_path("/health"));
        assert!(!is_safe_callback_path("/auth/oidc/login"));
        assert!(!is_safe_callback_path("/auth/oidc/refresh"));
    }

    #[test]
    fn weak_session_secrets_and_noncanonical_issuers_fail_closed() {
        assert_eq!(
            validate_session_secret(b"too-short"),
            Err(DashboardOidcConfigError::WeakSessionSecret)
        );
        assert_eq!(
            validate_session_secret(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            Err(DashboardOidcConfigError::WeakSessionSecret)
        );
        assert!(validate_session_secret(b"unit-test-session-signing-key-7a9c3e1d").is_ok());
        assert_eq!(
            normalize_runtime_issuer("https://issuer.example/"),
            Some("https://issuer.example".to_string())
        );
        assert!(normalize_runtime_issuer("https://issuer.example/?query=value").is_none());
    }

    #[test]
    fn jwks_cache_is_bounded_and_honors_no_cache() {
        let cache = JwksCache::new(1, Duration::from_secs(1), Duration::from_secs(1));
        let jwks: JwkSet = serde_json::from_value(json!({"keys": []})).expect("jwks");
        cache.insert(
            "one".to_string(),
            Arc::new(jwks.clone()),
            Duration::from_secs(1),
        );
        cache.insert("two".to_string(), Arc::new(jwks), Duration::from_secs(1));
        assert_eq!(cache.entry_count(), 1);
        assert!(cache.get("one").is_none());
        assert!(cache.get("two").is_some());
        let uncached: JwkSet = serde_json::from_value(json!({"keys": []})).expect("jwks");
        cache.insert("two".to_string(), Arc::new(uncached), Duration::ZERO);
        assert!(cache.get("two").is_none());
        let headers = HeaderMap::from_iter([(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        )]);
        assert_eq!(
            jwks_cache_ttl(&headers, Duration::from_secs(30), Duration::from_secs(60)),
            Duration::ZERO
        );
    }
}
