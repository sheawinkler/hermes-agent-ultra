//! Rust-native dashboard OpenID Connect auth.
//!
//! This is intentionally scoped to dashboard/API-server auth and keeps machine
//! access via `HERMES_HTTP_API_KEY` working as a bearer-token bypass in the
//! request guard.

use std::collections::HashSet;

use axum::extract::Query;
use axum::http::header::{COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use jsonwebtoken::jwk::{AlgorithmParameters, Jwk, JwkSet};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Sha256;
use url::Url;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

const SESSION_COOKIE: &str = "hermes_dashboard_session";
const LOGIN_COOKIE: &str = "hermes_dashboard_oidc_login";
const DEFAULT_SESSION_TTL_SECONDS: i64 = 8 * 60 * 60;

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
    pub config_error: Option<String>,
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
            _ => Self {
                mode: DashboardAuthMode::Bearer,
                oidc: None,
                config_error: None,
            },
        }
    }

    pub fn oidc_enabled(&self) -> bool {
        self.mode == DashboardAuthMode::Oidc
    }
}

#[derive(Debug, Clone)]
pub struct DashboardOidcConfig {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
    pub allowed_emails: HashSet<String>,
    pub allowed_domains: HashSet<String>,
    pub session_ttl_seconds: i64,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub jwks_uri: Option<String>,
    pub cookie_secure: bool,
    session_secret: Vec<u8>,
}

impl DashboardOidcConfig {
    pub fn from_env() -> Result<Self, String> {
        let issuer = required_env("HERMES_DASHBOARD_OIDC_ISSUER")?;
        let client_id = required_env("HERMES_DASHBOARD_OIDC_CLIENT_ID")?;
        let redirect_uri = env_trim("HERMES_DASHBOARD_OIDC_REDIRECT_URI")
            .unwrap_or_else(|| "http://127.0.0.1:8787/auth/oidc/callback".to_string());
        let session_secret = required_env("HERMES_DASHBOARD_SESSION_SECRET")
            .or_else(|_| required_env("HERMES_DASHBOARD_OIDC_SESSION_SECRET"))?;
        if session_secret.len() < 16 {
            return Err("HERMES_DASHBOARD_SESSION_SECRET must be at least 16 bytes".to_string());
        }
        let cookie_secure = env_bool("HERMES_DASHBOARD_COOKIE_SECURE")
            .unwrap_or_else(|| redirect_uri.starts_with("https://"));
        Ok(Self {
            issuer: issuer.trim_end_matches('/').to_string(),
            client_id,
            client_secret: env_trim("HERMES_DASHBOARD_OIDC_CLIENT_SECRET"),
            redirect_uri,
            scopes: parse_list_env("HERMES_DASHBOARD_OIDC_SCOPES")
                .unwrap_or_else(|| vec!["openid".into(), "email".into(), "profile".into()]),
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
            session_ttl_seconds: env_trim("HERMES_DASHBOARD_SESSION_TTL_SECONDS")
                .and_then(|value| value.parse::<i64>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(DEFAULT_SESSION_TTL_SECONDS),
            authorization_endpoint: env_trim("HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT"),
            token_endpoint: env_trim("HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT"),
            jwks_uri: env_trim("HERMES_DASHBOARD_OIDC_JWKS_URI"),
            cookie_secure,
            session_secret: session_secret.into_bytes(),
        })
    }

    fn session_secret(&self) -> &[u8] {
        &self.session_secret
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OidcProviderMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OidcTokenResponse {
    pub id_token: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_token: Option<String>,
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
    pub exp: u64,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DashboardSessionClaims {
    pub sub: String,
    pub email: Option<String>,
    pub name: Option<String>,
    pub iss: String,
    pub exp: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    error_description: Option<String>,
}

pub fn oidc_exempt_path(path: &str) -> bool {
    path == "/auth/status"
        || path == "/auth/logout"
        || path == "/auth/oidc/login"
        || path == "/auth/oidc/callback"
}

pub fn verify_session_from_headers(
    headers: &HeaderMap,
    config: &DashboardOidcConfig,
) -> Result<DashboardSessionClaims, String> {
    let raw = cookie_value(headers, SESSION_COOKIE)
        .ok_or_else(|| "session cookie missing".to_string())?;
    let session: DashboardSessionClaims = verify_signed_json(raw, config.session_secret())?;
    if session.exp <= Utc::now().timestamp() {
        return Err("session expired".to_string());
    }
    Ok(session)
}

pub async fn auth_status(headers: HeaderMap) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let mut authenticated = false;
    let mut identity = Value::Null;
    if let Some(ref oidc) = auth.oidc {
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
            "configured": auth.config_error.is_none(),
            "config_error": auth.config_error,
            "authenticated": authenticated,
            "identity": identity,
            "oidc": auth.oidc.as_ref().map(oidc_public_status),
        }),
    )
}

pub async fn auth_logout() -> Response {
    let mut response = json_response(StatusCode::OK, json!({"status": "ok", "logged_out": true}));
    clear_cookie(&mut response, SESSION_COOKIE);
    clear_cookie(&mut response, LOGIN_COOKIE);
    response
}

pub async fn oidc_login(Query(params): Query<OidcLoginParams>) -> Response {
    let auth = DashboardAuthConfig::from_env();
    let Some(mut oidc) = auth.oidc else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": auth.config_error.unwrap_or_else(|| "dashboard OIDC is not configured".to_string())}),
        );
    };

    let client = reqwest::Client::new();
    let metadata = match resolve_metadata(&client, &mut oidc).await {
        Ok(metadata) => metadata,
        Err(err) => return json_response(StatusCode::SERVICE_UNAVAILABLE, json!({"error": err})),
    };

    let pkce = hermes_auth::generate_pkce_pair();
    let state = Uuid::new_v4().to_string();
    let nonce = Uuid::new_v4().to_string();
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
        Err(err) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": err})),
    };
    let auth_url = match build_authorization_url(&oidc, &metadata, &state, &nonce, &pkce.challenge)
    {
        Ok(url) => url,
        Err(err) => return json_response(StatusCode::SERVICE_UNAVAILABLE, json!({"error": err})),
    };

    redirect_with_cookie(&auth_url, &login_cookie)
}

pub async fn oidc_callback(
    Query(params): Query<OidcCallbackParams>,
    headers: HeaderMap,
) -> Response {
    if let Some(error) = params.error {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": error, "error_description": params.error_description}),
        );
    }
    let Some(code) = params
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "missing code"}));
    };
    let Some(state) = params
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return json_response(StatusCode::BAD_REQUEST, json!({"error": "missing state"}));
    };

    let auth = DashboardAuthConfig::from_env();
    let Some(mut oidc) = auth.oidc else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": auth.config_error.unwrap_or_else(|| "dashboard OIDC is not configured".to_string())}),
        );
    };
    let Some(raw_login) = cookie_value(&headers, LOGIN_COOKIE) else {
        return json_response(
            StatusCode::UNAUTHORIZED,
            json!({"error": "missing OIDC login cookie"}),
        );
    };
    let login: LoginCookie = match verify_signed_json(raw_login, oidc.session_secret()) {
        Ok(login) => login,
        Err(err) => return json_response(StatusCode::UNAUTHORIZED, json!({"error": err})),
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

    let client = reqwest::Client::new();
    let metadata = match resolve_metadata(&client, &mut oidc).await {
        Ok(metadata) => metadata,
        Err(err) => return json_response(StatusCode::SERVICE_UNAVAILABLE, json!({"error": err})),
    };
    let token = match exchange_code(&client, &oidc, &metadata, code, &login.code_verifier).await {
        Ok(token) => token,
        Err(err) => return json_response(StatusCode::BAD_GATEWAY, json!({"error": err})),
    };
    let claims = match verify_id_token(
        &client,
        &token.id_token,
        &oidc,
        &metadata,
        Some(&login.nonce),
    )
    .await
    {
        Ok(claims) => claims,
        Err(err) => return json_response(StatusCode::UNAUTHORIZED, json!({"error": err})),
    };
    if !claim_allowed(&claims, &oidc) {
        return json_response(
            StatusCode::FORBIDDEN,
            json!({"error": "OIDC identity is not allowlisted"}),
        );
    }

    let session = DashboardSessionClaims {
        sub: claims.sub,
        email: claims.email,
        name: claims.name,
        iss: claims.iss,
        exp: Utc::now().timestamp() + oidc.session_ttl_seconds,
    };
    let session_cookie = match sign_cookie(
        SESSION_COOKIE,
        &session,
        oidc.session_secret(),
        oidc.session_ttl_seconds,
        oidc.cookie_secure,
    ) {
        Ok(cookie) => cookie,
        Err(err) => return json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error": err})),
    };
    let mut response = redirect_with_cookie(&login.return_to, &session_cookie);
    clear_cookie(&mut response, LOGIN_COOKIE);
    response
}

pub async fn discover_oidc_metadata(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<OidcProviderMetadata, String> {
    let issuer = issuer.trim_end_matches('/');
    let url = format!("{issuer}/.well-known/openid-configuration");
    let metadata = client
        .get(&url)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .send()
        .await
        .map_err(|err| format!("OIDC discovery failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("OIDC discovery rejected: {err}"))?
        .json::<OidcProviderMetadata>()
        .await
        .map_err(|err| format!("OIDC discovery parse failed: {err}"))?;
    if metadata.issuer.trim_end_matches('/') != issuer {
        return Err("OIDC discovery issuer mismatch".to_string());
    }
    Ok(metadata)
}

async fn resolve_metadata(
    client: &reqwest::Client,
    config: &mut DashboardOidcConfig,
) -> Result<OidcProviderMetadata, String> {
    match (
        config.authorization_endpoint.clone(),
        config.token_endpoint.clone(),
        config.jwks_uri.clone(),
    ) {
        (Some(authorization_endpoint), Some(token_endpoint), Some(jwks_uri)) => {
            Ok(OidcProviderMetadata {
                issuer: config.issuer.clone(),
                authorization_endpoint,
                token_endpoint,
                jwks_uri,
            })
        }
        _ => {
            let metadata = discover_oidc_metadata(client, &config.issuer).await?;
            config.authorization_endpoint = Some(metadata.authorization_endpoint.clone());
            config.token_endpoint = Some(metadata.token_endpoint.clone());
            config.jwks_uri = Some(metadata.jwks_uri.clone());
            Ok(metadata)
        }
    }
}

fn build_authorization_url(
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    state: &str,
    nonce: &str,
    code_challenge: &str,
) -> Result<String, String> {
    let mut url = Url::parse(&metadata.authorization_endpoint)
        .map_err(|err| format!("invalid authorization_endpoint: {err}"))?;
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
) -> Result<OidcTokenResponse, String> {
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("client_id", config.client_id.clone()),
        ("code", code.to_string()),
        ("redirect_uri", config.redirect_uri.clone()),
        ("code_verifier", code_verifier.to_string()),
    ];
    if let Some(secret) = config.client_secret.as_deref() {
        form.push(("client_secret", secret.to_string()));
    }
    client
        .post(&metadata.token_endpoint)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .form(&form)
        .send()
        .await
        .map_err(|err| format!("OIDC token exchange failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("OIDC token endpoint rejected: {err}"))?
        .json::<OidcTokenResponse>()
        .await
        .map_err(|err| format!("OIDC token response parse failed: {err}"))
}

async fn verify_id_token(
    client: &reqwest::Client,
    token: &str,
    config: &DashboardOidcConfig,
    metadata: &OidcProviderMetadata,
    expected_nonce: Option<&str>,
) -> Result<DashboardIdTokenClaims, String> {
    let jwks = client
        .get(&metadata.jwks_uri)
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .send()
        .await
        .map_err(|err| format!("OIDC JWKS fetch failed: {err}"))?
        .error_for_status()
        .map_err(|err| format!("OIDC JWKS rejected: {err}"))?
        .json::<JwkSet>()
        .await
        .map_err(|err| format!("OIDC JWKS parse failed: {err}"))?;
    verify_id_token_with_jwks(token, &jwks, config, expected_nonce)
}

pub fn verify_id_token_with_jwks(
    token: &str,
    jwks: &JwkSet,
    config: &DashboardOidcConfig,
    expected_nonce: Option<&str>,
) -> Result<DashboardIdTokenClaims, String> {
    let header = decode_header(token).map_err(|err| format!("OIDC token header invalid: {err}"))?;
    if !matches!(
        header.alg,
        Algorithm::RS256
            | Algorithm::RS384
            | Algorithm::RS512
            | Algorithm::ES256
            | Algorithm::ES384
    ) {
        return Err("OIDC token uses an unsupported signing algorithm".to_string());
    }
    let jwk = select_jwk(jwks, &header.kid)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|err| format!("OIDC JWK invalid: {err}"))?;
    let mut validation = Validation::new(header.alg);
    validation.validate_nbf = true;
    validation.set_audience(&[config.client_id.as_str()]);
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.required_spec_claims.insert("aud".to_string());
    validation.required_spec_claims.insert("exp".to_string());
    validation.required_spec_claims.insert("iss".to_string());
    validation.required_spec_claims.insert("sub".to_string());
    let claims = decode::<DashboardIdTokenClaims>(token, &key, &validation)
        .map_err(|err| format!("OIDC token rejected: {err}"))?
        .claims;
    if !claims.aud.contains(&config.client_id) {
        return Err("OIDC token audience mismatch".to_string());
    }
    if let Some(expected) = expected_nonce {
        if claims.nonce.as_deref() != Some(expected) {
            return Err("OIDC token nonce mismatch".to_string());
        }
    }
    Ok(claims)
}

fn select_jwk<'a>(set: &'a JwkSet, kid: &Option<String>) -> Result<&'a Jwk, String> {
    let jwk = if let Some(kid) = kid {
        set.find(kid)
            .ok_or_else(|| format!("OIDC JWKS did not contain kid {kid}"))?
    } else if set.keys.len() == 1 {
        &set.keys[0]
    } else {
        return Err("OIDC token has no kid and JWKS has multiple keys".to_string());
    };
    if matches!(jwk.algorithm, AlgorithmParameters::OctetKey(_)) {
        return Err("OIDC ID tokens must use asymmetric signing keys".to_string());
    }
    Ok(jwk)
}

fn claim_allowed(claims: &DashboardIdTokenClaims, config: &DashboardOidcConfig) -> bool {
    if config.allowed_emails.is_empty() && config.allowed_domains.is_empty() {
        return true;
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

fn oidc_public_status(config: &DashboardOidcConfig) -> Value {
    json!({
        "issuer": config.issuer,
        "client_id_configured": !config.client_id.is_empty(),
        "redirect_uri": config.redirect_uri,
        "scopes": config.scopes,
        "allowed_email_count": config.allowed_emails.len(),
        "allowed_domain_count": config.allowed_domains.len(),
        "authorization_endpoint_configured": config.authorization_endpoint.is_some(),
        "token_endpoint_configured": config.token_endpoint.is_some(),
        "jwks_uri_configured": config.jwks_uri.is_some(),
        "cookie_secure": config.cookie_secure,
        "session_ttl_seconds": config.session_ttl_seconds,
    })
}

fn sign_cookie<T: Serialize>(
    name: &str,
    payload: &T,
    secret: &[u8],
    max_age_seconds: i64,
    secure: bool,
) -> Result<String, String> {
    let value = sign_json(payload, secret)?;
    let secure_attr = if secure { "; Secure" } else { "" };
    Ok(format!(
        "{name}={value}; Path=/; Max-Age={max_age_seconds}; HttpOnly; SameSite=Lax{secure_attr}"
    ))
}

fn sign_json<T: Serialize>(payload: &T, secret: &[u8]) -> Result<String, String> {
    let body = serde_json::to_vec(payload)
        .map_err(|err| format!("cookie payload encode failed: {err}"))?;
    let body_b64 = URL_SAFE_NO_PAD.encode(body);
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|err| format!("cookie signer init failed: {err}"))?;
    mac.update(body_b64.as_bytes());
    let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{body_b64}.{sig}"))
}

fn verify_signed_json<T: DeserializeOwned>(value: &str, secret: &[u8]) -> Result<T, String> {
    let (body_b64, sig_b64) = value
        .split_once('.')
        .ok_or_else(|| "signed cookie is malformed".to_string())?;
    let expected = URL_SAFE_NO_PAD
        .decode(sig_b64.as_bytes())
        .map_err(|_| "signed cookie signature is malformed".to_string())?;
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|err| format!("cookie verifier init failed: {err}"))?;
    mac.update(body_b64.as_bytes());
    mac.verify_slice(&expected)
        .map_err(|_| "signed cookie signature mismatch".to_string())?;
    let body = URL_SAFE_NO_PAD
        .decode(body_b64.as_bytes())
        .map_err(|_| "signed cookie body is malformed".to_string())?;
    serde_json::from_slice::<T>(&body)
        .map_err(|err| format!("signed cookie payload invalid: {err}"))
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

fn clear_cookie(response: &mut Response, name: &str) {
    let cookie = format!("{name}=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(SET_COOKIE, value);
    }
}

fn redirect_with_cookie(location: &str, cookie: &str) -> Response {
    let mut response = (StatusCode::FOUND, "").into_response();
    if let Ok(value) = HeaderValue::from_str(location) {
        response.headers_mut().insert(LOCATION, value);
    }
    if let Ok(value) = HeaderValue::from_str(cookie) {
        response.headers_mut().append(SET_COOKIE, value);
    }
    response
}

fn json_response(status: StatusCode, payload: Value) -> Response {
    (status, axum::Json(payload)).into_response()
}

fn sanitize_return_to(input: Option<&str>) -> String {
    let value = input.unwrap_or("/").trim();
    if value.starts_with('/') && !value.starts_with("//") {
        value.to_string()
    } else {
        "/".to_string()
    }
}

fn env_trim(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn required_env(key: &str) -> Result<String, String> {
    env_trim(key).ok_or_else(|| format!("{key} is required for dashboard OIDC"))
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

fn env_bool(key: &str) -> Option<bool> {
    match env_trim(key)?.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
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
            scopes: vec!["openid".to_string(), "email".to_string()],
            allowed_emails: HashSet::new(),
            allowed_domains: HashSet::new(),
            session_ttl_seconds: 3600,
            authorization_endpoint: Some("https://issuer.example/authorize".to_string()),
            token_endpoint: Some("https://issuer.example/token".to_string()),
            jwks_uri: Some("https://issuer.example/jwks".to_string()),
            cookie_secure: false,
            session_secret: b"0123456789abcdef".to_vec(),
        }
    }

    #[test]
    fn signed_session_cookie_round_trips_and_rejects_tamper() {
        let cfg = test_config();
        let session = DashboardSessionClaims {
            sub: "user-1".to_string(),
            email: Some("user@example.com".to_string()),
            name: Some("User".to_string()),
            iss: cfg.issuer.clone(),
            exp: Utc::now().timestamp() + 60,
        };
        let signed = sign_json(&session, cfg.session_secret()).expect("sign");
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={signed}")).unwrap(),
        );
        let verified = verify_session_from_headers(&headers, &cfg).expect("verify");
        assert_eq!(verified.sub, "user-1");

        let tampered = format!("{SESSION_COOKIE}={}.bad", signed.split('.').next().unwrap());
        let mut bad = HeaderMap::new();
        bad.insert(COOKIE, HeaderValue::from_str(&tampered).unwrap());
        assert!(verify_session_from_headers(&bad, &cfg).is_err());
    }

    #[test]
    fn authorization_url_uses_pkce_and_nonce() {
        let cfg = test_config();
        let metadata = OidcProviderMetadata {
            issuer: cfg.issuer.clone(),
            authorization_endpoint: "https://issuer.example/authorize".to_string(),
            token_endpoint: "https://issuer.example/token".to_string(),
            jwks_uri: "https://issuer.example/jwks".to_string(),
        };
        let url = build_authorization_url(&cfg, &metadata, "state-1", "nonce-1", "challenge-1")
            .expect("url");
        assert!(url.contains("response_type=code"));
        assert!(url.contains("client_id=hermes-dashboard"));
        assert!(url.contains("code_challenge=challenge-1"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("nonce=nonce-1"));
    }

    #[test]
    fn allowlist_accepts_email_or_domain() {
        let mut cfg = test_config();
        cfg.allowed_domains.insert("example.com".to_string());
        let claims = DashboardIdTokenClaims {
            iss: cfg.issuer.clone(),
            sub: "user-1".to_string(),
            aud: Audience::One(cfg.client_id.clone()),
            exp: 4_102_444_800,
            nonce: Some("nonce".to_string()),
            email: Some("user@example.com".to_string()),
            name: None,
        };
        assert!(claim_allowed(&claims, &cfg));
        cfg.allowed_domains.clear();
        cfg.allowed_domains.insert("other.example".to_string());
        assert!(!claim_allowed(&claims, &cfg));
        cfg.allowed_domains.clear();
        cfg.allowed_emails.insert("user@example.com".to_string());
        assert!(claim_allowed(&claims, &cfg));
    }

    #[test]
    fn jwt_verifier_rejects_symmetric_algorithms() {
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
        let err = verify_id_token_with_jwks(&token, &jwks, &test_config(), Some("nonce-1"))
            .expect_err("symmetric algorithms should be rejected");
        assert!(err.contains("unsupported signing algorithm"));
    }

    #[test]
    fn return_to_rejects_external_redirects() {
        assert_eq!(sanitize_return_to(Some("/dashboard")), "/dashboard");
        assert_eq!(sanitize_return_to(Some("https://evil.example")), "/");
        assert_eq!(sanitize_return_to(Some("//evil.example")), "/");
    }

    #[test]
    fn boolean_env_accepts_explicit_secure_cookie_values() {
        std::env::set_var("HERMES_DASHBOARD_COOKIE_SECURE", "yes");
        assert_eq!(env_bool("HERMES_DASHBOARD_COOKIE_SECURE"), Some(true));
        std::env::set_var("HERMES_DASHBOARD_COOKIE_SECURE", "off");
        assert_eq!(env_bool("HERMES_DASHBOARD_COOKIE_SECURE"), Some(false));
        std::env::remove_var("HERMES_DASHBOARD_COOKIE_SECURE");
    }
}
