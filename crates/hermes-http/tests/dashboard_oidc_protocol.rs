use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::extract::{Form, Query, State};
use axum::http::header::{CACHE_CONTROL, COOKIE, LOCATION, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use sha2::{Digest, Sha256};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use url::Url;

static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

const CLIENT_ID: &str = "mock-dashboard-client";
const SESSION_SECRET: &str = "unit-test-session-signing-key-7a9c3e1d";
const MACHINE_BEARER: &str = "mock-machine-bearer";
const RSA_A_DER_B64: &str = "MIIEowIBAAKCAQEA7byghFJNIzE9eWLfUHICzX3c12Vvch3iWtMY6GYmPBhtqCoxfpvyTeZaQvzp7KScqo/e401f+Vcz8/wgCXWb1OJnIM4Fhes+XYy/sQI0+iv4CkiyCW6SwRwIvnoxjYUj5r85tuS1vk+kVbUyyo9QXJI/WtCUcn5KlDPr4Da8QJZDeuW5IriorvzdGedrOsQogY/K9JGNzvlb7zIgPAtjWgIWJ/c8WUi7ohKYp4UdY4aa0/cA7A+U0bqJLZZ8ORVOl+xUV9iqUjsS5Y+zCcZ3TAary6GGHFJulojgC5beBvgtkY2OHTSnkaqP41O7+eu9rX5UlLUOjupii2iCDPhU3wIDAQABAoIBAGdsJE58OVeCf9JTW3/1IVgBFbZUx+9WpdbKN+8+JQsGu4mpVV/Vess9122F9kP+9rLSpO2whvyDXbQaLlu/ILwzhBbG2atBRB/Rb8ETCzX8M8tRhbZJtqkwx3MJ2I3O6mfjL7nrdE8m+YBZtA9f2l9zNgGNdM78NviTP92KIkpb3bi4lB35Wgrkw8KAPiu23/0Lv0fb+m5NfYNcykIPzq6vOF7Ck7v1REB6JVN1Ul5/WGL55VLVmNdOKPZbZvXX0ndQJVPzGRFFilRUOKujMm9YQluOtVKTzQjqJ6PRvTx0mosRDX6pnGktP3Q6tmwZmhCDFnmqM5SfPu1i1cAUW20CgYEA91tyNudtz6u7EHq6Cva+seg8P5TNFpRtOllAbWnF2NuI5aJiLCVt6zVtiFMlRSati4cq643PJViAzyF+nsdD54HgZry4QaYsy0T6g+a74cC2n8Tg5hOHHa5/LEk8k50bCciV86IriH2Itb6t8bilGXQ7GdmRtQ6XYuwebZCe/s0CgYEA9gsg8mhawQ3c9kvOS1cWkRVpXF8tTuMpvB9nN25mhhFAfX7oISMM3dUi09czAkQmh0WhGiJP5oP1xk7geWtPX3PfidjGCuZHtHy16cLBMB2IKQxy+XtoTU3HZ3aumGww3ILyaty8nk+CidV3znV7AaRHey0z/qgEoe/ltuD+ylsCgYEAiL9GYjIJVEDFVvPyPDNy8xnHjQLvaqc9x38mDhbmqN+dTA48pdk/bkQIPNByj0s1sChWwwiP1z6ZX2lN/et9j9QOQd3dI3YB15JXyGnbvo9yptsJ1kg/b1pTtiD620SEeSEqPRmPZZK47Hjw4uxIYc0PC1hrpCPf73FH1RyZuZUCgYBh9cpXWT8Q8CESmtn22ySr/Rdm5KoYZ49dTC9/PWBqboH7tURctSTRSaPFLkAr3dBtOUbMLe8b+1wmaY8sKJKAkZTlwD4RZuATtzeCa70ZGBj3JvK76+t6I8aJphIbs6+8jBJSg7JBYay6jhhWRiYEYFDpdPTnpiO6Cdau+mPXUwKBgG+SYBdJEc/K6NU77XPFWmdCXu+z4+5dPz9o34Sj8YIhD8m6CmmJ1ybTcmRLoOAt5oJ/asCDAyIGV9Nk0uBa5Vb5eeys0DkWOy9g7HSCoecDt2Map46+yEmRPrnQlE5WVk0M9+tSCe1QYXdJpdXz338z1vBECZ0QE69iV2kXayX/";
const RSA_B_DER_B64: &str = "MIIEowIBAAKCAQEAm//q4mCut/POpTvKGIyoOdfeDfYraxlowx4wzCeN/QEYkIKcOdW/0dxCKOsc51ay+w5mXw9qTUWiKkcI88PTdenJic/pzVucPRTxTBDmBf/qliTLPipkQLrba1AfLZbWio7k5WtAAmgh7uyn7mMtbLuTPd70EHYjAA7+NsfjE+90ztDmsatNESHHhfHsSUqskARQx49oyBebu4JehVo5X1FwWkyyE8Yy2H1pWwubVfIqCsp44SbXG4TtcuTm+vBFly/JKVEoLli0iTWZFoKnsqQKRq8Eija0DbEbkp3JNQ4tTQbRoa8KsoKPmar1zEuN5DUmbu1JRqfcZ0La6cG3+QIDAQABAoIBAEKN1G/wP5pLcgYclAQVPlAvQJVESRHQLVnRL5Bp77HZHdnJX01tXAD0r2U1qYco/gMOCa7UrM16v8y5saAOE9FihJE/Gsh6JRYKMUf6jahRnouf9vX2r/7NuX73F9YjXDSQ8ibOsuhT9OYyIRMbTO3uuMrUq9u6CwSjYOGONdryy3FN8uEsEFB7cUFWJSMDLgGesHadBs4Aj9shpE03y8cToDOFFKwLyQG43yJBu7O+mCw5HyU/O2n56b56pjOMmuzL8UYmIIMf0XWvOO8+2RfPMO5vZX5Ht+nUDRoV2uch68hAtO88g0UcHkUcM2/cVCOi0z31OLzMWs6dBO1NweECgYEA0e9VcU+/5vO5aIfwRms9XjIFIFYvwDmJVpgjvp/qonkit+NVvoPSIE5uKyWEaVpffhmSDlp37Wi5xP62/1k7r1QdxVHn1+9mUvsQLJevXRkn9RitkBidaD/Tfq/YmVUSEl5UjxnyWN/O7Q8R/v5e9QMqyXiJHIn/EgojmnnJ8CsCgYEAvjrh5LoLwrs4volUI3nrdNn2O+Tob2VlfabDDVajlgfO/iGCfHOaOYegC6PB9VtcEP995auKpjn1rkX+QEcSk9T9YHr6gJPpLuQPK1G09r5TWbAXGaNcASaa96gSat3jwqY2pNQxuJ7GWvej8jnR5gJN6FhkaAyGlmdzmbRuAmsCgYBBu1ZdbEMB5T4gZK/u3JUScUb5oQLChUFjQ6+teBTXWmZsfVnTlSKtyuDj0ZNsCUftC8SXiewTAb9o1vgla6MseoH3znVxPxwiz0l5LOVTmRTRwTBW9d4TTijCefyvxdsqtS50wAMJoMoBn75awPqqbJtxCbSkovCijVoofAmmkQKBgQCDRBByi6rn4ddDK3JC1LOUO389TzTRAY/qB7PatWlGcvbzx93J0N+gSSU4QvTpQPSjyhlu7VNMZvxQGH5cjm6UUknRVKsdJHOo5rhM9SHBjg3n+qnGCELJjIEs2e5zjc/HlnhofwAcXHuVdALOd/WOGRL3fF80qUgJ6zMK4kVzJwKBgHpXAzYIvKlrqR+pU+zE7U8IqxeR/t8og133DFQsLOFXpt5DFjv7U7jPWSvqddLtcv0MaISgGh9xc0puBEFAYMOvBsnXmd4p/dtMvEjOVcn1L2Jd0v2bizBUhdchnN2gcRFWMjTJNK6HhOK3tG/lqsSty/s8POR2D1SHsoiPQ09J";
const EC_DER_B64: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgv6uQKxafVtYht4hn7vtoBJmPS6TtcA5zs/aH5fTH4BahRANCAAR1f5C+V28pJX2wZByYsQMyK/3NyhJzb4YT+rkgNBbc9fQnlHSHX0Sc/4/JGIkVp51d3+K3XcCrT+zi8tCpxQ2Y";

const RSA_A_JWK_N: &str = "7byghFJNIzE9eWLfUHICzX3c12Vvch3iWtMY6GYmPBhtqCoxfpvyTeZaQvzp7KScqo_e401f-Vcz8_wgCXWb1OJnIM4Fhes-XYy_sQI0-iv4CkiyCW6SwRwIvnoxjYUj5r85tuS1vk-kVbUyyo9QXJI_WtCUcn5KlDPr4Da8QJZDeuW5IriorvzdGedrOsQogY_K9JGNzvlb7zIgPAtjWgIWJ_c8WUi7ohKYp4UdY4aa0_cA7A-U0bqJLZZ8ORVOl-xUV9iqUjsS5Y-zCcZ3TAary6GGHFJulojgC5beBvgtkY2OHTSnkaqP41O7-eu9rX5UlLUOjupii2iCDPhU3w";
const RSA_B_JWK_N: &str = "m__q4mCut_POpTvKGIyoOdfeDfYraxlowx4wzCeN_QEYkIKcOdW_0dxCKOsc51ay-w5mXw9qTUWiKkcI88PTdenJic_pzVucPRTxTBDmBf_qliTLPipkQLrba1AfLZbWio7k5WtAAmgh7uyn7mMtbLuTPd70EHYjAA7-NsfjE-90ztDmsatNESHHhfHsSUqskARQx49oyBebu4JehVo5X1FwWkyyE8Yy2H1pWwubVfIqCsp44SbXG4TtcuTm-vBFly_JKVEoLli0iTWZFoKnsqQKRq8Eija0DbEbkp3JNQ4tTQbRoa8KsoKPmar1zEuN5DUmbu1JRqfcZ0La6cG3-Q";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoveryMode {
    Normal,
    IssuerMismatch,
    Malformed,
    Delayed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TokenMode {
    RsaA,
    RsaB,
    Es,
    IssuerMismatch,
    AudienceMismatch,
    NonceMismatch,
    Expired,
    FutureNbf,
    Denied,
    UnverifiedEmail,
    ShortLived,
    ProviderError,
    Malformed,
    Delayed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RefreshMode {
    RsaB,
    Expired,
    Rejected,
    Malformed,
    Delayed,
    CrossSubject,
    CrossIssuer,
}

#[derive(Clone, Debug)]
struct AuthorizeRecord {
    state: String,
    nonce: String,
    code_challenge: String,
    code_challenge_method: String,
    redirect_uri: String,
    client_id: String,
    response_type: String,
}

#[derive(Clone, Debug)]
struct TokenRecord {
    code_verifier: String,
    redirect_uri: String,
    client_id: String,
}

#[derive(Clone, Debug)]
struct RefreshRecord {
    refresh_token: String,
    client_id: String,
}

#[derive(Clone, Debug)]
struct RevocationRecord {
    refresh_token: String,
    client_id: String,
}

struct MockIdp {
    issuer: String,
    discovery_mode: Mutex<DiscoveryMode>,
    token_mode: Mutex<TokenMode>,
    refresh_mode: Mutex<RefreshMode>,
    jwks_phase: AtomicU8,
    cache_control: Mutex<String>,
    jwks_requests: AtomicUsize,
    end_session_requests: AtomicUsize,
    authorization: Mutex<Vec<AuthorizeRecord>>,
    token_requests: Mutex<Vec<TokenRecord>>,
    refresh_requests: Mutex<Vec<RefreshRecord>>,
    revocation_requests: Mutex<Vec<RevocationRecord>>,
}

impl MockIdp {
    fn new(issuer: String) -> Self {
        Self {
            issuer,
            discovery_mode: Mutex::new(DiscoveryMode::Normal),
            token_mode: Mutex::new(TokenMode::RsaA),
            refresh_mode: Mutex::new(RefreshMode::RsaB),
            jwks_phase: AtomicU8::new(0),
            jwks_requests: AtomicUsize::new(0),
            end_session_requests: AtomicUsize::new(0),
            cache_control: Mutex::new("max-age=3600".to_string()),
            authorization: Mutex::new(Vec::new()),
            token_requests: Mutex::new(Vec::new()),
            refresh_requests: Mutex::new(Vec::new()),
            revocation_requests: Mutex::new(Vec::new()),
        }
    }

    async fn set_discovery_mode(&self, mode: DiscoveryMode) {
        *self.discovery_mode.lock().await = mode;
    }

    async fn set_token_mode(&self, mode: TokenMode) {
        *self.token_mode.lock().await = mode;
        let phase = match mode {
            TokenMode::RsaA
            | TokenMode::IssuerMismatch
            | TokenMode::AudienceMismatch
            | TokenMode::NonceMismatch
            | TokenMode::Expired
            | TokenMode::FutureNbf
            | TokenMode::Denied
            | TokenMode::UnverifiedEmail
            | TokenMode::ShortLived => 0,
            TokenMode::RsaB => 1,
            TokenMode::Es => 2,
            TokenMode::ProviderError | TokenMode::Malformed | TokenMode::Delayed => {
                self.jwks_phase.load(Ordering::SeqCst)
            }
        };
        self.jwks_phase.store(phase, Ordering::SeqCst);
    }

    async fn set_refresh_mode(&self, mode: RefreshMode) {
        *self.refresh_mode.lock().await = mode;
        if matches!(
            mode,
            RefreshMode::RsaB
                | RefreshMode::Expired
                | RefreshMode::CrossSubject
                | RefreshMode::CrossIssuer
        ) {
            self.jwks_phase.store(1, Ordering::SeqCst);
        }
    }

    async fn set_cache_control(&self, cache_control: &str) {
        *self.cache_control.lock().await = cache_control.to_string();
    }

    fn set_jwks_malformed(&self) {
        self.jwks_phase.store(3, Ordering::SeqCst);
    }
}

struct RunningServer {
    base_url: String,
    task: JoinHandle<()>,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct EnvGuard {
    original: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn apply(values: Vec<(&'static str, Option<String>)>) -> Self {
        let mut original = Vec::with_capacity(values.len());
        for (key, value) in values {
            original.push((key, std::env::var_os(key)));
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self { original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.original.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

async fn spawn_server(app: Router) -> RunningServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind listener");
    let address = listener.local_addr().expect("listener address");
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test listener");
    });
    RunningServer {
        base_url: format!("http://{address}"),
        task,
    }
}

async fn spawn_mock_idp() -> (Arc<MockIdp>, RunningServer) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock IdP");
    let address = listener.local_addr().expect("mock IdP address");
    let issuer = format!("http://{address}");
    let idp = Arc::new(MockIdp::new(issuer));
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(mock_discovery))
        .route("/authorize", get(mock_authorize))
        .route("/token", post(mock_token))
        .route("/jwks", get(mock_jwks))
        .route("/end-session", get(mock_end_session))
        .route("/revoke", post(mock_revoke))
        .with_state(idp.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve mock IdP");
    });
    (
        idp,
        RunningServer {
            base_url: format!("http://{address}"),
            task,
        },
    )
}

async fn spawn_dashboard() -> RunningServer {
    let state = hermes_http::HttpServerState::build(hermes_config::GatewayConfig::default())
        .await
        .expect("build dashboard state");
    spawn_server(hermes_http::router(state)).await
}

fn oidc_env(
    issuer: &str,
    redirect_uri: &str,
    machine_bearer: &str,
    provider_logout: &str,
) -> EnvGuard {
    EnvGuard::apply(vec![
        ("HERMES_DASHBOARD_AUTH_PROVIDER", Some("oidc".to_string())),
        ("HERMES_DASHBOARD_AUTH_MODE", None),
        ("HERMES_DASHBOARD_OIDC_ISSUER", Some(issuer.to_string())),
        ("HERMES_DASHBOARD_OIDC_CLIENT_ID", Some(CLIENT_ID.to_string())),
        (
            "HERMES_DASHBOARD_OIDC_REDIRECT_URI",
            Some(redirect_uri.to_string()),
        ),
        (
            "HERMES_DASHBOARD_SESSION_SECRET",
            Some(SESSION_SECRET.to_string()),
        ),
        ("HERMES_DASHBOARD_OIDC_SESSION_SECRET", None),
        ("HERMES_DASHBOARD_SESSION_TTL_SECONDS", None),
        (
            "HERMES_DASHBOARD_OIDC_ALLOWED_EMAILS",
            Some("allowed@example.test".to_string()),
        ),
        ("HERMES_DASHBOARD_OIDC_ALLOWED_DOMAINS", None),
        ("HERMES_DASHBOARD_OIDC_SCOPES", None),
        ("HERMES_DASHBOARD_OIDC_AUTHORIZATION_ENDPOINT", None),
        ("HERMES_DASHBOARD_OIDC_TOKEN_ENDPOINT", None),
        ("HERMES_DASHBOARD_OIDC_JWKS_URI", None),
        ("HERMES_DASHBOARD_OIDC_ALLOWED_CALLBACK_PATHS", None),
        ("HERMES_DASHBOARD_COOKIE_SECURE", None),
        (
            "HERMES_DASHBOARD_OIDC_ALLOW_MACHINE_BEARER",
            Some(machine_bearer.to_string()),
        ),
        (
            "HERMES_DASHBOARD_OIDC_ENABLE_PROVIDER_LOGOUT",
            Some(provider_logout.to_string()),
        ),
        (
            "HERMES_DASHBOARD_OIDC_HTTP_TIMEOUT_MS",
            Some("50".to_string()),
        ),
        ("HERMES_HTTP_API_KEY", Some(MACHINE_BEARER.to_string())),
    ])
}

async fn mock_discovery(State(idp): State<Arc<MockIdp>>) -> Response {
    let mode = *idp.discovery_mode.lock().await;
    match mode {
        DiscoveryMode::Normal => discovery_response(&idp.issuer),
        DiscoveryMode::IssuerMismatch => discovery_response("http://127.0.0.1:1"),
        DiscoveryMode::Malformed => (StatusCode::OK, "{").into_response(),
        DiscoveryMode::Delayed => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            discovery_response(&idp.issuer)
        }
    }
}

fn discovery_response(issuer: &str) -> Response {
    Json(json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "end_session_endpoint": format!("{issuer}/end-session"),
        "revocation_endpoint": format!("{issuer}/revoke"),
    }))
    .into_response()
}

async fn mock_authorize(
    State(idp): State<Arc<MockIdp>>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let record = AuthorizeRecord {
        state: query.get("state").cloned().unwrap_or_default(),
        nonce: query.get("nonce").cloned().unwrap_or_default(),
        code_challenge: query.get("code_challenge").cloned().unwrap_or_default(),
        code_challenge_method: query
            .get("code_challenge_method")
            .cloned()
            .unwrap_or_default(),
        redirect_uri: query.get("redirect_uri").cloned().unwrap_or_default(),
        client_id: query.get("client_id").cloned().unwrap_or_default(),
        response_type: query.get("response_type").cloned().unwrap_or_default(),
    };
    let mut callback = match Url::parse(&record.redirect_uri) {
        Ok(url) => url,
        Err(_) => return (StatusCode::BAD_REQUEST, "missing redirect URI").into_response(),
    };
    callback
        .query_pairs_mut()
        .append_pair("code", "mock-authorization-code")
        .append_pair("state", &record.state);
    idp.authorization.lock().await.push(record);
    redirect(callback.as_str())
}

async fn mock_token(
    State(idp): State<Arc<MockIdp>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    if form.get("grant_type").map(String::as_str) == Some("refresh_token") {
        return mock_refresh(idp, form).await;
    }
    let record = TokenRecord {
        code_verifier: form.get("code_verifier").cloned().unwrap_or_default(),
        redirect_uri: form.get("redirect_uri").cloned().unwrap_or_default(),
        client_id: form.get("client_id").cloned().unwrap_or_default(),
    };
    idp.token_requests.lock().await.push(record);
    let mode = *idp.token_mode.lock().await;
    if mode == TokenMode::ProviderError {
        return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"}))).into_response();
    }
    if mode == TokenMode::Malformed {
        return (StatusCode::OK, "not-json").into_response();
    }
    if mode == TokenMode::Delayed {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let nonce = idp
        .authorization
        .lock()
        .await
        .last()
        .map(|record| record.nonce.clone())
        .unwrap_or_default();
    let token = signed_id_token(mode, &idp.issuer, &nonce);
    Json(json!({
        "id_token": token,
        "access_token": "ignored-test-access-token",
        "refresh_token": "mock-refresh-initial",
        "token_type": "Bearer",
    }))
    .into_response()
}

async fn mock_refresh(idp: Arc<MockIdp>, form: HashMap<String, String>) -> Response {
    let record = RefreshRecord {
        refresh_token: form.get("refresh_token").cloned().unwrap_or_default(),
        client_id: form.get("client_id").cloned().unwrap_or_default(),
    };
    idp.refresh_requests.lock().await.push(record);
    let mode = *idp.refresh_mode.lock().await;
    match mode {
        RefreshMode::Rejected => {
            return (StatusCode::UNAUTHORIZED, Json(json!({"error":"invalid_grant"}))).into_response();
        }
        RefreshMode::Malformed => return (StatusCode::OK, "not-json").into_response(),
        RefreshMode::Delayed => tokio::time::sleep(Duration::from_millis(200)).await,
        RefreshMode::RsaB | RefreshMode::Expired => {}
    }
    let token = signed_refresh_id_token(mode, &idp.issuer);
    Json(json!({
        "id_token": token,
        "refresh_token": "mock-refresh-rotated",
        "token_type": "Bearer",
    }))
    .into_response()
}

async fn mock_revoke(
    State(idp): State<Arc<MockIdp>>,
    Form(form): Form<HashMap<String, String>>,
) -> Response {
    idp.revocation_requests.lock().await.push(RevocationRecord {
        refresh_token: form.get("token").cloned().unwrap_or_default(),
        client_id: form.get("client_id").cloned().unwrap_or_default(),
    });
    StatusCode::OK.into_response()
}

async fn mock_jwks(State(idp): State<Arc<MockIdp>>) -> Response {
    idp.jwks_requests.fetch_add(1, Ordering::SeqCst);
    if idp.jwks_phase.load(Ordering::SeqCst) == 3 {
        return (StatusCode::OK, "not-json").into_response();
    }
    let key = match idp.jwks_phase.load(Ordering::SeqCst) {
        0 => json!({
            "kty":"RSA", "alg":"RS256", "kid":"rsa-a", "n":RSA_A_JWK_N, "e":"AQAB"
        }),
        1 => json!({
            "kty":"RSA", "alg":"RS256", "kid":"rsa-b", "n":RSA_B_JWK_N, "e":"AQAB"
        }),
        _ => json!({
            "kty":"EC", "alg":"ES256", "kid":"ec-a", "crv":"P-256",
            "x":"dX-QvldvKSV9sGQcmLEDMiv9zcoSc2-GE_q5IDQW3PU",
            "y":"9CeUdIdfRJz_j8kYiRWnnV3f4rddwKtP7OLy0KnFDZg"
        }),
    };
    let cache_control = idp.cache_control.lock().await.clone();
    (
        [(
            CACHE_CONTROL,
            HeaderValue::from_str(&cache_control).expect("cache-control header"),
        )],
        Json(json!({"keys":[key]})),
    )
        .into_response()
}

async fn mock_end_session(State(idp): State<Arc<MockIdp>>) -> Response {
    idp.end_session_requests.fetch_add(1, Ordering::SeqCst);
    Json(json!({"provider_logout":"received"})).into_response()
}

fn signed_id_token(mode: TokenMode, issuer: &str, nonce: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut claims = match mode {
        TokenMode::IssuerMismatch => json!({
            "iss":"http://127.0.0.1:1", "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nonce":nonce, "email":"allowed@example.test"
        }),
        TokenMode::AudienceMismatch => json!({
            "iss":issuer, "sub":"mock-user", "aud":"wrong-client",
            "exp": now + 3600, "nonce":nonce, "email":"allowed@example.test"
        }),
        TokenMode::NonceMismatch => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nonce":"wrong-nonce", "email":"allowed@example.test"
        }),
        TokenMode::Expired => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now - 3600, "nonce":nonce, "email":"allowed@example.test"
        }),
        TokenMode::FutureNbf => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nbf": now + 3600, "nonce":nonce, "email":"allowed@example.test"
        }),
        TokenMode::Denied => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nonce":nonce, "email":"not-allowed@example.test"
        }),
        TokenMode::UnverifiedEmail => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nonce":nonce, "email":"allowed@example.test",
            "email_verified":false
        }),
        TokenMode::ShortLived => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 30, "nonce":nonce, "email":"allowed@example.test"
        }),
        _ => json!({
            "iss":issuer, "sub":"mock-user", "aud":CLIENT_ID,
            "exp": now + 3600, "nonce":nonce, "email":"allowed@example.test", "name":"Mock User"
        }),
    };
    if let Some(claims) = claims.as_object_mut() {
        claims
            .entry("email_verified")
            .or_insert(Value::Bool(true));
    }
    match mode {
        TokenMode::RsaB => sign_rsa(&claims, RSA_B_DER_B64, "rsa-b"),
        TokenMode::Es => sign_ec(&claims, "ec-a"),
        _ => sign_rsa(&claims, RSA_A_DER_B64, "rsa-a"),
    }
}

fn signed_refresh_id_token(mode: RefreshMode, issuer: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let exp = if mode == RefreshMode::Expired {
        now - 1
    } else {
        now + 3600
    };
    let token_issuer = if mode == RefreshMode::CrossIssuer {
        "http://127.0.0.1:1"
    } else {
        issuer
    };
    let subject = if mode == RefreshMode::CrossSubject {
        "other-allowlisted-user"
    } else {
        "mock-user"
    };
    let claims = json!({
        "iss":token_issuer,
        "sub":subject,
        "aud":CLIENT_ID,
        "exp":exp,
        "email":"allowed@example.test",
        "email_verified":true,
        "name":"Refreshed Mock User"
    });
    sign_rsa(&claims, RSA_B_DER_B64, "rsa-b")
}

fn sign_rsa(claims: &Value, key_der_b64: &str, kid: &str) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some(kid.to_string());
    let key = STANDARD.decode(key_der_b64).expect("decode RSA fixture");
    encode(&header, claims, &EncodingKey::from_rsa_der(&key)).expect("sign RS256 token")
}

fn sign_ec(claims: &Value, kid: &str) -> String {
    let mut header = Header::new(Algorithm::ES256);
    header.kid = Some(kid.to_string());
    let key = STANDARD.decode(EC_DER_B64).expect("decode EC fixture");
    encode(&header, claims, &EncodingKey::from_ec_der(&key)).expect("sign ES256 token")
}

fn redirect(location: &str) -> Response {
    (
        StatusCode::FOUND,
        [(LOCATION, HeaderValue::from_str(location).expect("redirect header"))],
        "",
    )
        .into_response()
}

fn no_redirect_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("build test client")
}

fn response_cookie(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|cookie| {
            cookie
                .strip_prefix(&format!("{name}="))
                .and_then(|cookie| cookie.split(';').next())
                .map(|value| format!("{name}={value}"))
        })
        .expect("named cookie")
}

fn all_set_cookies(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(ToString::to_string))
        .collect()
}

fn session_sid(cookie: &str) -> String {
    let value = cookie
        .split_once('=')
        .expect("session cookie name")
        .1
        .split('.')
        .next()
        .expect("signed session body");
    let bytes = URL_SAFE_NO_PAD.decode(value).expect("decode session body");
    serde_json::from_slice::<Value>(&bytes)
        .expect("session JSON")["sid"]
        .as_str()
        .expect("session identifier")
        .to_string()
}

async fn fresh_session(client: &reqwest::Client, dashboard: &str) -> String {
    let (login_cookie, callback_url) = begin_login(client, dashboard, "/dashboard").await;
    let response = complete_callback(client, &login_cookie, &callback_url).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    response_cookie(response.headers(), "hermes_dashboard_session")
}

async fn refresh_session(
    client: &reqwest::Client,
    dashboard: &str,
    session_cookie: &str,
) -> reqwest::Response {
    client
        .post(format!("{dashboard}/auth/oidc/refresh"))
        .header(COOKIE, session_cookie)
        .send()
        .await
        .expect("refresh request")
}

async fn begin_login(client: &reqwest::Client, dashboard: &str, return_to: &str) -> (String, String) {
    let response = client
        .get(format!("{dashboard}/auth/oidc/login"))
        .query(&[("return_to", return_to)])
        .send()
        .await
        .expect("login request");
    assert_eq!(response.status(), StatusCode::FOUND);
    let login_cookie = response_cookie(response.headers(), "hermes_dashboard_oidc_login");
    let authorization_url = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("authorization redirect")
        .to_string();
    let response = client
        .get(authorization_url)
        .send()
        .await
        .expect("authorize request");
    assert_eq!(response.status(), StatusCode::FOUND);
    let callback_url = response
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("callback redirect")
        .to_string();
    (login_cookie, callback_url)
}

async fn complete_callback(
    client: &reqwest::Client,
    login_cookie: &str,
    callback_url: &str,
) -> reqwest::Response {
    client
        .get(callback_url)
        .header(COOKIE, login_cookie)
        .send()
        .await
        .expect("callback request")
}

fn callback_with_state(callback_url: &str, state: &str) -> String {
    let mut callback = Url::parse(callback_url).expect("callback URL");
    let code = callback
        .query_pairs()
        .find_map(|(key, value)| (key == "code").then(|| value.into_owned()))
        .expect("callback code");
    callback
        .query_pairs_mut()
        .clear()
        .append_pair("code", &code)
        .append_pair("state", state);
    callback.to_string()
}

fn provider_error_callback(callback_url: &str) -> String {
    let mut callback = Url::parse(callback_url).expect("callback URL");
    let state = callback
        .query_pairs()
        .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
        .expect("callback state");
    callback
        .query_pairs_mut()
        .clear()
        .append_pair("error", "access_denied")
        .append_pair("state", &state);
    callback.to_string()
}

#[tokio::test]
async fn mock_oidc_protocol_exercises_login_callback_claims_rotation_and_logout() {
    let _env_lock = ENV_LOCK.lock().await;
    let (idp, idp_server) = spawn_mock_idp().await;
    let redirect_placeholder = "http://127.0.0.1:8787/auth/oidc/callback";
    let _env = oidc_env(&idp_server.base_url, redirect_placeholder, "false", "false");
    let dashboard = spawn_dashboard().await;
    let actual_redirect = format!("{}/auth/oidc/callback", dashboard.base_url);
    std::env::set_var("HERMES_DASHBOARD_OIDC_REDIRECT_URI", &actual_redirect);
    let client = no_redirect_client();

    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "https://evil.example").await;
    let authorization = idp.authorization.lock().await;
    let first = authorization.last().expect("authorization record");
    assert_eq!(first.response_type, "code");
    assert_eq!(first.client_id, CLIENT_ID);
    assert_eq!(first.redirect_uri, actual_redirect);
    assert_eq!(first.code_challenge_method, "S256");
    assert!(!first.state.is_empty());
    assert!(!first.nonce.is_empty());
    assert_ne!(first.state, first.nonce);
    assert!(first
        .state
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    assert!(first
        .nonce
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    assert!(!first.code_challenge.is_empty());
    let code_challenge = first.code_challenge.clone();
    drop(authorization);

    let response = complete_callback(&client, &login_cookie, &callback_url).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(
        response.headers().get(LOCATION).and_then(|value| value.to_str().ok()),
        Some("/")
    );
    let session_cookie = response_cookie(response.headers(), "hermes_dashboard_session");
    assert!(!session_cookie.contains("refresh-token"));
    assert_eq!(idp.jwks_requests.load(Ordering::SeqCst), 1);
    let token_requests = idp.token_requests.lock().await;
    let token = token_requests.last().expect("token request");
    assert_eq!(token.client_id, CLIENT_ID);
    assert_eq!(token.redirect_uri, actual_redirect);
    assert!(!token.code_verifier.is_empty());
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(token.code_verifier.as_bytes())),
        code_challenge,
        "the callback verifier must match the authorize S256 challenge"
    );
    drop(token_requests);

    let status = client
        .get(format!("{}/auth/status", dashboard.base_url))
        .header(COOKIE, &session_cookie)
        .send()
        .await
        .expect("auth status");
    assert_eq!(status.status(), StatusCode::OK);
    let status: Value = status.json().await.expect("status JSON");
    assert_eq!(status["authenticated"], true);

    let machine = client
        .get(format!("{}/v1/rpc", dashboard.base_url))
        .header("authorization", format!("Bearer {MACHINE_BEARER}"))
        .send()
        .await
        .expect("machine request");
    assert_eq!(machine.status(), StatusCode::UNAUTHORIZED);

    idp.set_token_mode(TokenMode::RsaB).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    let response = complete_callback(&client, &login_cookie, &callback_url).await;
    assert_eq!(response.status(), StatusCode::FOUND);
    assert_eq!(idp.jwks_requests.load(Ordering::SeqCst), 2, "one kid-miss refresh after A→B rotation");

    idp.set_cache_control("no-cache").await;
    idp.set_token_mode(TokenMode::Es).await;
    let before_no_cache = idp.jwks_requests.load(Ordering::SeqCst);
    for _ in 0..2 {
        let (login_cookie, callback_url) =
            begin_login(&client, &dashboard.base_url, "/dashboard").await;
        assert_eq!(
            complete_callback(&client, &login_cookie, &callback_url).await.status(),
            StatusCode::FOUND,
            "ES256 id_token is accepted with an EC JWK"
        );
    }
    assert_eq!(
        idp.jwks_requests.load(Ordering::SeqCst),
        before_no_cache + 2,
        "no-cache expires each JWKS response"
    );
    idp.set_cache_control("max-age=3600").await;

    for mode in [
        TokenMode::IssuerMismatch,
        TokenMode::AudienceMismatch,
        TokenMode::NonceMismatch,
        TokenMode::Expired,
        TokenMode::FutureNbf,
    ] {
        idp.set_token_mode(mode).await;
        let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
        assert_eq!(
            complete_callback(&client, &login_cookie, &callback_url).await.status(),
            StatusCode::UNAUTHORIZED,
            "{mode:?} must fail verification"
        );
    }

    idp.set_token_mode(TokenMode::Denied).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::FORBIDDEN
    );

    idp.set_token_mode(TokenMode::UnverifiedEmail).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::FORBIDDEN,
        "an allowlisted email must still be verified"
    );

    idp.set_token_mode(TokenMode::ShortLived).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    let short_lived = complete_callback(&client, &login_cookie, &callback_url).await;
    assert_eq!(short_lived.status(), StatusCode::FOUND);
    let session_cookie = all_set_cookies(short_lived.headers())
        .into_iter()
        .find(|cookie| cookie.starts_with("hermes_dashboard_session="))
        .expect("short-lived session cookie");
    let max_age = session_cookie
        .split(';')
        .find_map(|attribute| attribute.trim().strip_prefix("Max-Age="))
        .expect("session max age")
        .parse::<i64>()
        .expect("integer session max age");
    assert!((1..=30).contains(&max_age));

    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    let bad_state_url = callback_with_state(&callback_url, "wrong-state");
    assert_eq!(
        complete_callback(&client, &login_cookie, &bad_state_url).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let provider_error_url = provider_error_callback(&callback_url);
    let provider_error = complete_callback(&client, &login_cookie, &provider_error_url).await;
    assert_eq!(provider_error.status(), StatusCode::UNAUTHORIZED);
    assert!(all_set_cookies(provider_error.headers())
        .iter()
        .any(|cookie| cookie.starts_with("hermes_dashboard_oidc_login=") && cookie.contains("Max-Age=0")));

    idp.set_token_mode(TokenMode::ProviderError).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::BAD_GATEWAY
    );
    idp.set_token_mode(TokenMode::Malformed).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::BAD_GATEWAY
    );

    idp.set_token_mode(TokenMode::Delayed).await;
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::BAD_GATEWAY,
        "callback token exchange timeout must fail closed"
    );


    idp.set_token_mode(TokenMode::RsaB).await;
    idp.set_jwks_malformed();
    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert_eq!(
        complete_callback(&client, &login_cookie, &callback_url).await.status(),
        StatusCode::BAD_GATEWAY
    );
    let logout = client
        .get(format!("{}/auth/logout", dashboard.base_url))
        .header(COOKIE, &session_cookie)
        .send()
        .await
        .expect("local logout");
    assert_eq!(logout.status(), StatusCode::OK);
    let logout_body: Value = logout.json().await.expect("logout JSON");
    assert_eq!(logout_body["provider_logout"], "disabled");
}

#[tokio::test]
async fn mock_discovery_rejects_wrong_issuer_malformed_json_and_timeout() {
    let _env_lock = ENV_LOCK.lock().await;
    let (idp, idp_server) = spawn_mock_idp().await;
    let _env = oidc_env(
        &idp_server.base_url,
        "http://127.0.0.1:8787/auth/oidc/callback",
        "false",
        "false",
    );
    let dashboard = spawn_dashboard().await;
    let client = no_redirect_client();

    idp.set_discovery_mode(DiscoveryMode::IssuerMismatch).await;
    assert_eq!(
        client
            .get(format!("{}/auth/oidc/login", dashboard.base_url))
            .send()
            .await
            .expect("issuer mismatch login")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    idp.set_discovery_mode(DiscoveryMode::Malformed).await;
    assert_eq!(
        client
            .get(format!("{}/auth/oidc/login", dashboard.base_url))
            .send()
            .await
            .expect("malformed discovery login")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );

    idp.set_discovery_mode(DiscoveryMode::Delayed).await;
    assert_eq!(
        client
            .get(format!("{}/auth/oidc/login", dashboard.base_url))
            .send()
            .await
            .expect("timed out discovery login")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn configured_callback_allowlist_provider_logout_and_machine_bearer_are_explicit() {
    let _env_lock = ENV_LOCK.lock().await;
    let (idp, idp_server) = spawn_mock_idp().await;
    let _env = oidc_env(
        &idp_server.base_url,
        "http://127.0.0.1:8787/callback",
        "true",
        "true",
    );
    std::env::set_var("HERMES_DASHBOARD_OIDC_ALLOWED_CALLBACK_PATHS", "/callback");
    let dashboard = spawn_dashboard().await;
    let custom_redirect = format!("{}/callback", dashboard.base_url);
    std::env::set_var("HERMES_DASHBOARD_OIDC_REDIRECT_URI", &custom_redirect);
    let client = no_redirect_client();

    let machine = client
        .get(format!("{}/v1/rpc", dashboard.base_url))
        .header("authorization", format!("Bearer {MACHINE_BEARER}"))
        .send()
        .await
        .expect("machine opt-in request");
    assert_ne!(machine.status(), StatusCode::UNAUTHORIZED);
    assert_ne!(machine.status(), StatusCode::SERVICE_UNAVAILABLE);

    let (login_cookie, callback_url) = begin_login(&client, &dashboard.base_url, "/dashboard").await;
    assert!(callback_url.starts_with(&custom_redirect));
    let callback = complete_callback(&client, &login_cookie, &callback_url).await;
    assert_eq!(callback.status(), StatusCode::FOUND);
    let session_cookie = response_cookie(callback.headers(), "hermes_dashboard_session");

    let logout = client
        .get(format!("{}/auth/logout", dashboard.base_url))
        .header(COOKIE, &session_cookie)
        .send()
        .await
        .expect("provider logout");
    assert_eq!(logout.status(), StatusCode::FOUND);
    let location = logout
        .headers()
        .get(LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("provider logout redirect")
        .to_string();
    let provider_url = Url::parse(&location).expect("provider logout URL");
    assert_eq!(provider_url.path(), "/end-session");
    assert!(provider_url
        .query_pairs()
        .any(|(key, value)| key == "post_logout_redirect_uri" && value.starts_with(&dashboard.base_url)));
    assert!(all_set_cookies(logout.headers())
        .iter()
        .filter(|cookie| cookie.contains("Max-Age=0"))
        .count()
        >= 2);
    let revocations = idp.revocation_requests.lock().await;
    let revocation = revocations.last().expect("refresh token revocation");
    assert_eq!(revocation.client_id, CLIENT_ID);
    assert!(!revocation.refresh_token.is_empty());
    drop(revocations);
    assert_eq!(
        client
            .get(&location)
            .send()
            .await
            .expect("provider logout endpoint")
            .status(),
        StatusCode::OK
    );
    assert_eq!(idp.end_session_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn mock_oidc_refresh_rotates_session_and_fails_closed() {
    let _env_lock = ENV_LOCK.lock().await;
    let (idp, idp_server) = spawn_mock_idp().await;
    let _env = oidc_env(
        &idp_server.base_url,
        "http://127.0.0.1:8787/auth/oidc/callback",
        "false",
        "false",
    );
    let dashboard = spawn_dashboard().await;
    std::env::set_var(
        "HERMES_DASHBOARD_OIDC_REDIRECT_URI",
        format!("{}/auth/oidc/callback", dashboard.base_url),
    );
    let client = no_redirect_client();

    let initial_session = fresh_session(&client, &dashboard.base_url).await;
    let initial_sid = session_sid(&initial_session);
    assert!(!initial_session.contains("mock-refresh"));
    assert_eq!(idp.jwks_requests.load(Ordering::SeqCst), 1);

    idp.set_refresh_mode(RefreshMode::RsaB).await;
    let response = refresh_session(&client, &dashboard.base_url, &initial_session).await;
    assert_eq!(response.status(), StatusCode::OK);
    let rotated_session = response_cookie(response.headers(), "hermes_dashboard_session");
    assert_ne!(session_sid(&rotated_session), initial_sid);
    assert_eq!(
        idp.jwks_requests.load(Ordering::SeqCst),
        2,
        "the rotated kid causes exactly one JWKS kid-miss refetch"
    );
    let refresh_requests = idp.refresh_requests.lock().await;
    let first_refresh = refresh_requests.last().expect("refresh request");
    assert_eq!(first_refresh.client_id, CLIENT_ID);
    assert!(!first_refresh.refresh_token.is_empty());
    drop(refresh_requests);

    let stale_status: Value = client
        .get(format!("{}/auth/status", dashboard.base_url))
        .header(COOKIE, &initial_session)
        .send()
        .await
        .expect("stale session status")
        .json()
        .await
        .expect("stale status JSON");
    assert_eq!(stale_status["authenticated"], false);

    idp.set_refresh_mode(RefreshMode::Rejected).await;
    let rejected = refresh_session(&client, &dashboard.base_url, &rotated_session).await;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert!(all_set_cookies(rejected.headers())
        .iter()
        .any(|cookie| cookie.starts_with("hermes_dashboard_session=") && cookie.contains("Max-Age=0")));

    idp.set_token_mode(TokenMode::RsaA).await;
    let malformed_source = fresh_session(&client, &dashboard.base_url).await;
    idp.set_refresh_mode(RefreshMode::Malformed).await;
    assert_eq!(
        refresh_session(&client, &dashboard.base_url, &malformed_source)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );

    idp.set_token_mode(TokenMode::RsaA).await;
    let delayed_source = fresh_session(&client, &dashboard.base_url).await;
    idp.set_refresh_mode(RefreshMode::Delayed).await;
    assert_eq!(
        refresh_session(&client, &dashboard.base_url, &delayed_source)
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "refresh transport timeout must consume the old session and fail closed"
    );

    idp.set_token_mode(TokenMode::RsaA).await;
    let expired_source = fresh_session(&client, &dashboard.base_url).await;
    idp.set_refresh_mode(RefreshMode::Expired).await;
    assert_eq!(
        refresh_session(&client, &dashboard.base_url, &expired_source)
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "expired replacement id_token must not produce a new session"
    );

    idp.set_token_mode(TokenMode::RsaA).await;
    let cross_subject_source = fresh_session(&client, &dashboard.base_url).await;
    idp.set_refresh_mode(RefreshMode::CrossSubject).await;
    let cross_subject = refresh_session(&client, &dashboard.base_url, &cross_subject_source).await;
    assert_eq!(
        cross_subject.status(),
        StatusCode::UNAUTHORIZED,
        "a provider refresh may not switch a browser session to another allowlisted subject"
    );
    assert!(all_set_cookies(cross_subject.headers())
        .iter()
        .any(|cookie| cookie.starts_with("hermes_dashboard_session=") && cookie.contains("Max-Age=0")));
    let cross_subject_status: Value = client
        .get(format!("{}/auth/status", dashboard.base_url))
        .header(COOKIE, &cross_subject_source)
        .send()
        .await
        .expect("cross-subject stale status")
        .json()
        .await
        .expect("cross-subject stale status JSON");
    assert_eq!(cross_subject_status["authenticated"], false);

    idp.set_token_mode(TokenMode::RsaA).await;
    let cross_issuer_source = fresh_session(&client, &dashboard.base_url).await;
    idp.set_refresh_mode(RefreshMode::CrossIssuer).await;
    assert_eq!(
        refresh_session(&client, &dashboard.base_url, &cross_issuer_source)
            .await
            .status(),
        StatusCode::UNAUTHORIZED,
        "a refresh id_token from another issuer must not produce a browser session"
    );
}

#[tokio::test]
async fn oidc_session_cookie_expiry_is_enforced_by_the_dashboard_guard() {
    let _env_lock = ENV_LOCK.lock().await;
    let (_idp, idp_server) = spawn_mock_idp().await;
    let _env = oidc_env(
        &idp_server.base_url,
        "http://127.0.0.1:8787/auth/oidc/callback",
        "false",
        "false",
    );
    std::env::set_var("HERMES_DASHBOARD_SESSION_TTL_SECONDS", "1");
    let dashboard = spawn_dashboard().await;
    std::env::set_var(
        "HERMES_DASHBOARD_OIDC_REDIRECT_URI",
        format!("{}/auth/oidc/callback", dashboard.base_url),
    );
    let client = no_redirect_client();
    let session = fresh_session(&client, &dashboard.base_url).await;

    tokio::time::sleep(Duration::from_millis(1_200)).await;
    let status: Value = client
        .get(format!("{}/auth/status", dashboard.base_url))
        .header(COOKIE, &session)
        .send()
        .await
        .expect("expired session status")
        .json()
        .await
        .expect("expired status JSON");
    assert_eq!(status["authenticated"], false);
    assert_eq!(
        client
            .get(format!("{}/v1/rpc", dashboard.base_url))
            .header(COOKIE, &session)
            .send()
            .await
            .expect("expired guarded request")
            .status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn misconfigured_oidc_is_503_and_invalid_machine_bearer_boolean_fails_closed() {
    let _env_lock = ENV_LOCK.lock().await;
    let _env = EnvGuard::apply(vec![
        ("HERMES_DASHBOARD_AUTH_PROVIDER", Some("oidc".to_string())),
        ("HERMES_DASHBOARD_OIDC_ISSUER", Some("http://127.0.0.1:8787".to_string())),
        ("HERMES_DASHBOARD_OIDC_CLIENT_ID", Some(CLIENT_ID.to_string())),
        (
            "HERMES_DASHBOARD_OIDC_REDIRECT_URI",
            Some("http://127.0.0.1:8787/auth/oidc/callback".to_string()),
        ),
        ("HERMES_DASHBOARD_SESSION_SECRET", None),
        ("HERMES_DASHBOARD_OIDC_SESSION_SECRET", None),
        ("HERMES_DASHBOARD_OIDC_ALLOW_MACHINE_BEARER", Some("false".to_string())),
        ("HERMES_HTTP_API_KEY", Some(MACHINE_BEARER.to_string())),
    ]);
    let dashboard = spawn_dashboard().await;
    let client = no_redirect_client();
    let protected = client
        .get(format!("{}/v1/rpc", dashboard.base_url))
        .header("authorization", format!("Bearer {MACHINE_BEARER}"))
        .send()
        .await
        .expect("misconfigured protected request");
    assert_eq!(protected.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        client
            .get(format!("{}/auth/status", dashboard.base_url))
            .send()
            .await
            .expect("misconfigured status")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    std::env::set_var("HERMES_DASHBOARD_SESSION_SECRET", "weak");
    let weak_secret_dashboard = spawn_dashboard().await;
    assert_eq!(
        client
            .get(format!("{}/v1/rpc", weak_secret_dashboard.base_url))
            .header("authorization", format!("Bearer {MACHINE_BEARER}"))
            .send()
            .await
            .expect("weak-secret protected request")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );


    std::env::set_var("HERMES_DASHBOARD_SESSION_SECRET", SESSION_SECRET);
    std::env::set_var("HERMES_DASHBOARD_OIDC_ALLOW_MACHINE_BEARER", "TRUE");
    let invalid_boolean_dashboard = spawn_dashboard().await;
    assert_eq!(
        client
            .get(format!("{}/v1/rpc", invalid_boolean_dashboard.base_url))
            .header("authorization", format!("Bearer {MACHINE_BEARER}"))
            .send()
            .await
            .expect("invalid boolean protected request")
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}
