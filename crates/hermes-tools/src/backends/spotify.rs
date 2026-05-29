//! Rust-native Spotify Web API backend.

use async_trait::async_trait;
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

use crate::tools::spotify::{SpotifyApiRequest, SpotifyBackend, SpotifyHttpMethod};
use hermes_core::ToolError;

const DEFAULT_SPOTIFY_API_BASE_URL: &str = "https://api.spotify.com/v1";
const SPOTIFY_AUTH_ERROR: &str =
    "Spotify authentication failed or expired. Run `hermes auth spotify` again.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyRuntimeCredentials {
    pub access_token: String,
    pub base_url: String,
    pub source: String,
}

#[derive(Debug)]
pub struct SpotifyWebApiBackend {
    client: Client,
    credentials: Option<SpotifyRuntimeCredentials>,
}

impl SpotifyWebApiBackend {
    pub fn new(credentials: SpotifyRuntimeCredentials) -> Self {
        Self {
            client: spotify_http_client(),
            credentials: Some(credentials),
        }
    }

    pub fn unconfigured() -> Self {
        Self {
            client: spotify_http_client(),
            credentials: None,
        }
    }

    pub fn from_env_or_auth_store() -> Result<Self, ToolError> {
        Ok(Self::new(resolve_spotify_runtime_credentials()?))
    }

    pub fn credentials(&self) -> Option<&SpotifyRuntimeCredentials> {
        self.credentials.as_ref()
    }
}

#[async_trait]
impl SpotifyBackend for SpotifyWebApiBackend {
    async fn call(&self, request: SpotifyApiRequest) -> Result<Value, ToolError> {
        let credentials = self
            .credentials
            .as_ref()
            .ok_or_else(|| ToolError::ExecutionFailed(SPOTIFY_AUTH_ERROR.into()))?;
        let url = build_url(&credentials.base_url, &request)?;
        let response = self
            .client
            .request(method(request.method), url)
            .bearer_auth(&credentials.access_token)
            .header("Content-Type", "application/json")
            .json_if_some(request.body.as_ref())
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Spotify API request failed: {e}")))?;

        read_spotify_response(response, &request.path, request.empty_response).await
    }
}

fn spotify_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| Client::new())
}

trait JsonIfSome {
    fn json_if_some(self, body: Option<&Value>) -> Self;
}

impl JsonIfSome for reqwest::RequestBuilder {
    fn json_if_some(self, body: Option<&Value>) -> Self {
        match body {
            Some(body) => self.json(body),
            None => self,
        }
    }
}

fn method(method: SpotifyHttpMethod) -> Method {
    match method {
        SpotifyHttpMethod::Get => Method::GET,
        SpotifyHttpMethod::Post => Method::POST,
        SpotifyHttpMethod::Put => Method::PUT,
        SpotifyHttpMethod::Delete => Method::DELETE,
    }
}

fn build_url(base_url: &str, request: &SpotifyApiRequest) -> Result<reqwest::Url, ToolError> {
    let base = base_url.trim_end_matches('/');
    let path = request.path.trim_start_matches('/');
    let mut url = reqwest::Url::parse(&format!("{base}/{path}")).map_err(|e| {
        ToolError::ExecutionFailed(format!("invalid Spotify API URL for {}: {e}", request.path))
    })?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in &request.query {
            pairs.append_pair(key, value);
        }
    }
    Ok(url)
}

async fn read_spotify_response(
    response: reqwest::Response,
    path: &str,
    empty_response: Option<Value>,
) -> Result<Value, ToolError> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = response
        .text()
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read Spotify response: {e}")))?;

    if !status.is_success() {
        let detail = extract_spotify_error_detail(&text);
        return Err(ToolError::ExecutionFailed(friendly_spotify_error_message(
            status,
            &detail,
            path,
            retry_after.as_deref(),
        )));
    }

    if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
        return Ok(empty_response.unwrap_or_else(|| {
            json!({
                "success": true,
                "status_code": status.as_u16(),
                "empty": true
            })
        }));
    }

    if content_type.contains("application/json") {
        serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Spotify response: {e}"))
        })
    } else {
        Ok(json!({"success": true, "text": text}))
    }
}

fn extract_spotify_error_detail(raw: &str) -> String {
    let fallback = raw.trim().to_string();
    let Ok(payload) = serde_json::from_str::<Value>(raw) else {
        return fallback;
    };
    payload
        .get("error")
        .and_then(|error| {
            error
                .as_object()
                .and_then(|object| object.get("message").and_then(Value::as_str))
                .or_else(|| error.as_str())
        })
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or(fallback)
}

fn friendly_spotify_error_message(
    status: StatusCode,
    detail: &str,
    path: &str,
    retry_after: Option<&str>,
) -> String {
    let normalized_detail = detail.to_ascii_lowercase();
    let is_playback_path = path.starts_with("/me/player");

    match status.as_u16() {
        401 => SPOTIFY_AUTH_ERROR.to_string(),
        403 if is_playback_path => {
            "Spotify rejected this playback request. Playback control usually requires a Spotify Premium account and an active Spotify Connect device.".to_string()
        }
        403 if normalized_detail.contains("scope") || normalized_detail.contains("permission") => {
            "Spotify rejected the request because the current auth scope is insufficient. Re-run `hermes auth spotify` to refresh permissions.".to_string()
        }
        403 => {
            "Spotify rejected the request. The account may not have permission for this action."
                .to_string()
        }
        404 if is_playback_path => {
            "Spotify could not find an active playback device or player session for this request."
                .to_string()
        }
        404 => "Spotify resource not found.".to_string(),
        429 => {
            let mut message = "Spotify rate limit exceeded.".to_string();
            if let Some(retry_after) = retry_after.filter(|value| !value.trim().is_empty()) {
                message.push_str(&format!(" Retry after {retry_after} seconds."));
            }
            message
        }
        _ if !detail.trim().is_empty() => detail.trim().to_string(),
        _ => format!("Spotify API request failed with status {status}."),
    }
}

fn resolve_spotify_runtime_credentials() -> Result<SpotifyRuntimeCredentials, ToolError> {
    if let Some(access_token) =
        env_string("HERMES_SPOTIFY_ACCESS_TOKEN").or_else(|| env_string("SPOTIFY_ACCESS_TOKEN"))
    {
        return Ok(SpotifyRuntimeCredentials {
            access_token,
            base_url: env_string("HERMES_SPOTIFY_API_BASE_URL")
                .or_else(|| env_string("SPOTIFY_API_BASE_URL"))
                .unwrap_or_else(|| DEFAULT_SPOTIFY_API_BASE_URL.to_string())
                .trim_end_matches('/')
                .to_string(),
            source: "env".to_string(),
        });
    }

    for path in auth_store_candidates() {
        if let Some(credentials) = read_spotify_credentials_from_auth_store(&path) {
            return Ok(credentials);
        }
    }

    Err(ToolError::ExecutionFailed(SPOTIFY_AUTH_ERROR.into()))
}

fn env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn auth_store_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = env_string("HERMES_AUTH_FILE") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(hermes_config::paths::auth_json_path());
    if let Some(home) = user_home_dir() {
        candidates.push(home.join(".hermes-agent-ultra").join("auth.json"));
        candidates.push(home.join(".hermes").join("auth.json"));
    }

    let mut seen = Vec::<PathBuf>::new();
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| {
            if seen.contains(path) {
                false
            } else {
                seen.push(path.clone());
                true
            }
        })
        .collect()
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn read_spotify_credentials_from_auth_store(path: &PathBuf) -> Option<SpotifyRuntimeCredentials> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    let spotify = value
        .get("providers")
        .and_then(Value::as_object)?
        .get("spotify")?
        .as_object()?;
    let access_token = spotify
        .get("access_token")
        .or_else(|| spotify.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let base_url = spotify
        .get("base_url")
        .or_else(|| spotify.get("api_base_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SPOTIFY_API_BASE_URL)
        .trim_end_matches('/')
        .to_string();
    Some(SpotifyRuntimeCredentials {
        access_token,
        base_url,
        source: path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvGuard {
        _tmp: tempfile::TempDir,
        original: Vec<(&'static str, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = test_lock::lock();
            let tmp = tempfile::tempdir().unwrap();
            let keys = [
                "HERMES_HOME",
                "HERMES_AUTH_FILE",
                "HERMES_SPOTIFY_ACCESS_TOKEN",
                "SPOTIFY_ACCESS_TOKEN",
                "HERMES_SPOTIFY_API_BASE_URL",
                "SPOTIFY_API_BASE_URL",
            ];
            let original = keys
                .iter()
                .map(|key| (*key, std::env::var(key).ok()))
                .collect();
            for key in keys {
                std::env::remove_var(key);
            }
            std::env::set_var("HERMES_HOME", tmp.path());
            Self {
                _tmp: tmp,
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.original {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[test]
    fn resolves_credentials_from_env() {
        let _guard = EnvGuard::new();
        std::env::set_var("HERMES_SPOTIFY_ACCESS_TOKEN", "env-token");
        std::env::set_var("HERMES_SPOTIFY_API_BASE_URL", "https://spotify.test/v1/");

        let backend = SpotifyWebApiBackend::from_env_or_auth_store().unwrap();
        let credentials = backend.credentials().unwrap();
        assert_eq!(credentials.access_token, "env-token");
        assert_eq!(credentials.base_url, "https://spotify.test/v1");
        assert_eq!(credentials.source, "env");
    }

    #[test]
    fn resolves_credentials_from_auth_store() {
        let _guard = EnvGuard::new();
        let path = hermes_config::paths::auth_json_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&json!({
                "providers": {
                    "spotify": {
                        "access_token": "store-token",
                        "api_base_url": "https://store.spotify.test/v1/"
                    }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let backend = SpotifyWebApiBackend::from_env_or_auth_store().unwrap();
        let credentials = backend.credentials().unwrap();
        assert_eq!(credentials.access_token, "store-token");
        assert_eq!(credentials.base_url, "https://store.spotify.test/v1");
        assert_eq!(credentials.source, path.display().to_string());
    }

    #[tokio::test]
    async fn unconfigured_backend_returns_auth_error_without_network() {
        let _guard = EnvGuard::new();
        let backend = SpotifyWebApiBackend::unconfigured();
        let err = backend
            .call(SpotifyApiRequest::new(SpotifyHttpMethod::Get, "/me/player"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Run `hermes auth spotify` again"));
    }

    #[test]
    fn friendly_errors_match_upstream_messages() {
        assert_eq!(
            friendly_spotify_error_message(StatusCode::UNAUTHORIZED, "bad", "/me/player", None),
            SPOTIFY_AUTH_ERROR
        );
        assert!(friendly_spotify_error_message(
            StatusCode::FORBIDDEN,
            "scope missing",
            "/search",
            None
        )
        .contains("scope is insufficient"));
        assert!(friendly_spotify_error_message(
            StatusCode::TOO_MANY_REQUESTS,
            "",
            "/search",
            Some("12")
        )
        .contains("Retry after 12 seconds"));
    }
}
