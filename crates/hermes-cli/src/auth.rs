use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use hermes_core::AgentError;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const DEFAULT_NOUS_PORTAL_URL: &str = "https://portal.nousresearch.com";
pub const DEFAULT_NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const DEFAULT_NOUS_CLIENT_ID: &str = "hermes-cli";
pub const DEFAULT_NOUS_SCOPE: &str = "inference:mint_agent_key";
pub const DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS: u32 = 30 * 60;

pub const DEFAULT_CODEX_ISSUER: &str = "https://auth.openai.com";
pub const DEFAULT_CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
pub const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
pub const CODEX_OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const CODEX_OAUTH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
pub const ANTHROPIC_OAUTH_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const ANTHROPIC_OAUTH_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
pub const ANTHROPIC_OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const ANTHROPIC_OAUTH_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
pub const ANTHROPIC_OAUTH_SCOPE: &str = "org:create_api_key user:profile user:inference";
pub const ANTHROPIC_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;
pub const DEFAULT_QWEN_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
pub const QWEN_OAUTH_CLIENT_ID: &str = "f0304373b74a44d2b584a3fb70ca9e56";
pub const QWEN_OAUTH_TOKEN_URL: &str = "https://chat.qwen.ai/api/v1/oauth2/token";
pub const QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 120;
pub const DEFAULT_GEMINI_CLOUDCODE_BASE_URL: &str = "cloudcode-pa://google";
pub const GEMINI_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;
const GEMINI_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GEMINI_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const GEMINI_USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v1/userinfo";
const GEMINI_OAUTH_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile";
const GEMINI_CALLBACK_HOST: &str = "127.0.0.1";
const GEMINI_CALLBACK_PORT: u16 = 8085;
const GEMINI_CALLBACK_PATH: &str = "/oauth2callback";
const GEMINI_CALLBACK_WAIT_SECS: u64 = 300;
const DEFAULT_GEMINI_CLIENT_ID_PROJECT_NUM: &str = "681255809395";
const DEFAULT_GEMINI_CLIENT_ID_HASH: &str = "oo8ft2oprdrnp9e3aqf6av3hmdib135j";
const DEFAULT_GEMINI_CLIENT_SECRET_SUFFIX: &str = "4uHgMPm-1o7Sk-geV6Cu5clXFsxl";

#[derive(Debug, Clone)]
pub struct NousDeviceCodeOptions {
    pub portal_base_url: Option<String>,
    pub inference_base_url: Option<String>,
    pub client_id: Option<String>,
    pub scope: Option<String>,
    pub open_browser: bool,
    pub timeout_seconds: f64,
    pub min_key_ttl_seconds: u32,
}

impl Default for NousDeviceCodeOptions {
    fn default() -> Self {
        Self {
            portal_base_url: None,
            inference_base_url: None,
            client_id: None,
            scope: None,
            open_browser: true,
            timeout_seconds: 15.0,
            min_key_ttl_seconds: DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CodexDeviceCodeOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for CodexDeviceCodeOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 15.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NousAuthState {
    pub portal_base_url: String,
    pub inference_base_url: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub token_type: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    pub obtained_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_expires_in: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_reused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_key_obtained_at: Option<String>,
}

impl NousAuthState {
    pub fn runtime_api_key(&self) -> Option<String> {
        if let Some(agent_key) = self
            .agent_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(agent_key.to_string());
        }
        let access = self.access_token.trim();
        if access.is_empty() {
            None
        } else {
            Some(access.to_string())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexAuthState {
    pub tokens: CodexTokens,
    pub base_url: String,
    pub last_refresh: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthStore {
    #[serde(default = "default_auth_store_version")]
    version: u32,
    #[serde(default)]
    providers: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    updated_at: Option<String>,
}

const fn default_auth_store_version() -> u32 {
    1
}

impl Default for AuthStore {
    fn default() -> Self {
        Self {
            version: default_auth_store_version(),
            providers: BTreeMap::new(),
            active_provider: None,
            updated_at: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct NousDeviceCodeResponse {
    device_code: Option<String>,
    user_code: Option<String>,
    verification_uri: Option<String>,
    verification_uri_complete: Option<String>,
    expires_in: Option<i64>,
    interval: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct NousTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    scope: Option<String>,
    expires_in: Option<i64>,
    inference_base_url: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NousAgentKeyResponse {
    api_key: Option<String>,
    key_id: Option<String>,
    expires_at: Option<String>,
    expires_in: Option<i64>,
    reused: Option<bool>,
    inference_base_url: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexDeviceUserCodeResponse {
    user_code: Option<String>,
    device_auth_id: Option<String>,
    interval: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CodexDevicePollResponse {
    authorization_code: Option<String>,
    code_verifier: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CodexTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QwenCliTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub resource_url: String,
    pub expiry_date: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct QwenRuntimeCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
    pub expires_at_ms: Option<i64>,
    pub auth_file: PathBuf,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub tokens: QwenCliTokens,
}

#[derive(Debug, Clone)]
pub struct QwenAuthStatus {
    pub logged_in: bool,
    pub auth_file: PathBuf,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GeminiOAuthLoginOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for GeminiOAuthLoginOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 20.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiOAuthFileState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnthropicOAuthLoginOptions {
    pub open_browser: bool,
    pub timeout_seconds: f64,
}

impl Default for AnthropicOAuthLoginOptions {
    fn default() -> Self {
        Self {
            open_browser: true,
            timeout_seconds: 20.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicOAuthState {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct GeminiRuntimeCredentials {
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub source: String,
    pub expires_at_ms: Option<i64>,
    pub auth_file: PathBuf,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub refresh_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct GeminiOAuthStatus {
    pub logged_in: bool,
    pub auth_file: PathBuf,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AnthropicOAuthStatus {
    pub logged_in: bool,
    pub source: Option<String>,
    pub api_key: Option<String>,
    pub expires_at_ms: Option<i64>,
    pub error: Option<String>,
}

fn auth_json_path() -> PathBuf {
    hermes_config::paths::auth_json_path()
}

fn load_auth_store(path: &Path) -> Result<AuthStore, AgentError> {
    if !path.exists() {
        return Ok(AuthStore::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    if raw.trim().is_empty() {
        return Ok(AuthStore::default());
    }
    serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn save_auth_store(path: &Path, store: &AuthStore) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let mut raw = serde_json::to_string_pretty(store)
        .map_err(|e| AgentError::Config(format!("serialize auth store: {}", e)))?;
    raw.push('\n');
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

pub fn save_provider_auth_state(provider: &str, state: Value) -> Result<PathBuf, AgentError> {
    let provider = provider.trim().to_ascii_lowercase();
    let path = auth_json_path();
    let mut store = load_auth_store(&path)?;
    store.providers.insert(provider.clone(), state);
    store.active_provider = Some(provider);
    store.updated_at = Some(Utc::now().to_rfc3339());
    save_auth_store(&path, &store)?;
    Ok(path)
}

pub fn read_provider_auth_state(provider: &str) -> Result<Option<Value>, AgentError> {
    let provider = provider.trim().to_ascii_lowercase();
    let path = auth_json_path();
    let store = load_auth_store(&path)?;
    Ok(store.providers.get(&provider).cloned())
}

pub fn clear_provider_auth_state(provider: &str) -> Result<bool, AgentError> {
    let provider = provider.trim().to_ascii_lowercase();
    let path = auth_json_path();
    let mut store = load_auth_store(&path)?;
    let removed = store.providers.remove(&provider).is_some();
    if store.active_provider.as_deref() == Some(provider.as_str()) {
        store.active_provider = None;
    }
    if removed {
        store.updated_at = Some(Utc::now().to_rfc3339());
        save_auth_store(&path, &store)?;
    }
    Ok(removed)
}

pub fn save_nous_auth_state(state: &NousAuthState) -> Result<PathBuf, AgentError> {
    let value = serde_json::to_value(state)
        .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
    save_provider_auth_state("nous", value)
}

pub fn save_codex_auth_state(state: &CodexAuthState) -> Result<PathBuf, AgentError> {
    let value = serde_json::to_value(state)
        .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
    save_provider_auth_state("openai-codex", value)
}

pub fn save_openai_auth_state(state: &CodexAuthState) -> Result<PathBuf, AgentError> {
    let value = serde_json::to_value(state)
        .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
    save_provider_auth_state("openai", value)
}

fn qwen_cli_auth_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_QWEN_CLI_AUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".qwen")
        .join("oauth_creds.json")
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|v| v.trim().parse::<i64>().ok()))
}

fn read_qwen_cli_tokens() -> Result<QwenCliTokens, AgentError> {
    let auth_path = qwen_cli_auth_path();
    if !auth_path.exists() {
        return Err(AgentError::AuthFailed(
            "Qwen CLI credentials not found. Run `qwen auth qwen-oauth` first.".into(),
        ));
    }
    let raw = std::fs::read_to_string(&auth_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", auth_path.display(), e)))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", auth_path.display(), e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::Config(format!(
            "invalid Qwen CLI credentials in {}",
            auth_path.display()
        ))
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(format!(
                "Qwen OAuth access_token missing in {}",
                auth_path.display()
            ))
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let resource_url = object
        .get("resource_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("portal.qwen.ai")
        .to_string();
    let expiry_date = object.get("expiry_date").and_then(value_as_i64);
    Ok(QwenCliTokens {
        access_token,
        refresh_token,
        token_type,
        resource_url,
        expiry_date,
    })
}

fn save_qwen_cli_tokens(tokens: &QwenCliTokens) -> Result<PathBuf, AgentError> {
    let auth_path = qwen_cli_auth_path();
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let tmp_path = auth_path.with_extension("tmp");
    let mut raw = serde_json::to_string_pretty(tokens)
        .map_err(|e| AgentError::Config(format!("serialize Qwen tokens: {}", e)))?;
    raw.push('\n');
    std::fs::write(&tmp_path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", tmp_path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp_path, perms).map_err(|e| {
            AgentError::Io(format!("set permissions on {}: {}", tmp_path.display(), e))
        })?;
    }
    std::fs::rename(&tmp_path, &auth_path).map_err(|e| {
        AgentError::Io(format!(
            "rename {} -> {}: {}",
            tmp_path.display(),
            auth_path.display(),
            e
        ))
    })?;
    Ok(auth_path)
}

fn qwen_access_token_is_expiring(expiry_date_ms: Option<i64>, skew_seconds: i64) -> bool {
    let Some(expiry_ms) = expiry_date_ms else {
        return true;
    };
    let skew = skew_seconds.max(0);
    Utc::now().timestamp_millis() + skew.saturating_mul(1000) >= expiry_ms
}

async fn refresh_qwen_cli_tokens(
    tokens: &QwenCliTokens,
    timeout_seconds: f64,
) -> Result<QwenCliTokens, AgentError> {
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Qwen OAuth refresh token missing. Re-run `qwen auth qwen-oauth`.".into(),
            )
        })?
        .to_string();
    let token_url = std::env::var("HERMES_QWEN_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| QWEN_OAUTH_TOKEN_URL.to_string());
    let client_id = std::env::var("HERMES_QWEN_OAUTH_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| QWEN_OAUTH_CLIENT_ID.to_string());
    let timeout = if timeout_seconds.is_finite() {
        timeout_seconds.clamp(5.0, 120.0)
    } else {
        20.0
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build qwen oauth client: {}", e)))?;
    let response = client
        .post(&token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Qwen OAuth refresh failed: {}", e)))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Qwen OAuth refresh read failed: {}", e)))?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Qwen OAuth refresh failed ({}). Re-run `qwen auth qwen-oauth`. {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|e| {
        AgentError::AuthFailed(format!("Qwen OAuth refresh JSON parse failed: {}", e))
    })?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Qwen OAuth refresh response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Qwen OAuth refresh response missing access_token".into())
        })?
        .to_string();
    let refreshed_refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(refresh_token);
    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(tokens.token_type.as_str())
        .to_string();
    let resource_url = object
        .get("resource_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(tokens.resource_url.as_str())
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(6 * 60 * 60)
        .max(1);
    let refreshed = QwenCliTokens {
        access_token,
        refresh_token: Some(refreshed_refresh_token),
        token_type,
        resource_url,
        expiry_date: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
    };
    let _ = save_qwen_cli_tokens(&refreshed)?;
    Ok(refreshed)
}

pub async fn resolve_qwen_runtime_credentials(
    force_refresh: bool,
    refresh_if_expiring: bool,
    refresh_skew_seconds: i64,
) -> Result<QwenRuntimeCredentials, AgentError> {
    let mut tokens = read_qwen_cli_tokens()?;
    let should_refresh = force_refresh
        || (refresh_if_expiring
            && qwen_access_token_is_expiring(tokens.expiry_date, refresh_skew_seconds));
    if should_refresh {
        tokens = refresh_qwen_cli_tokens(&tokens, 20.0).await?;
    }
    if tokens.access_token.trim().is_empty() {
        return Err(AgentError::AuthFailed(
            "Qwen OAuth access token missing. Re-run `qwen auth qwen-oauth`.".into(),
        ));
    }
    let base_url = std::env::var("HERMES_QWEN_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_QWEN_BASE_URL.to_string());
    Ok(QwenRuntimeCredentials {
        provider: "qwen-oauth".to_string(),
        base_url,
        api_key: tokens.access_token.clone(),
        source: "qwen-cli".to_string(),
        expires_at_ms: tokens.expiry_date,
        auth_file: qwen_cli_auth_path(),
        refresh_token: tokens.refresh_token.clone(),
        token_type: tokens.token_type.clone(),
        tokens,
    })
}

pub async fn get_qwen_auth_status() -> QwenAuthStatus {
    let auth_file = qwen_cli_auth_path();
    match resolve_qwen_runtime_credentials(false, false, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)
        .await
    {
        Ok(creds) => QwenAuthStatus {
            logged_in: true,
            auth_file,
            source: Some(creds.source),
            api_key: Some(creds.api_key),
            expires_at_ms: creds.expires_at_ms,
            error: None,
        },
        Err(err) => QwenAuthStatus {
            logged_in: false,
            auth_file,
            source: None,
            api_key: None,
            expires_at_ms: None,
            error: Some(err.to_string()),
        },
    }
}

fn gemini_cli_auth_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_GEMINI_OAUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    hermes_config::paths::hermes_home()
        .join("auth")
        .join("google_oauth.json")
}

fn parse_packed_gemini_refresh(
    raw_refresh: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(raw) = raw_refresh.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None, None);
    };
    let mut parts = raw.splitn(3, '|');
    let refresh_token = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let project_id = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let managed_project_id = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (refresh_token, project_id, managed_project_id)
}

fn pack_gemini_refresh(
    refresh_token: Option<&str>,
    project_id: Option<&str>,
    managed_project_id: Option<&str>,
) -> Option<String> {
    let refresh_token = refresh_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    let project_id = project_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let managed_project_id = managed_project_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if project_id.is_empty() && managed_project_id.is_empty() {
        Some(refresh_token)
    } else {
        Some(format!(
            "{}|{}|{}",
            refresh_token, project_id, managed_project_id
        ))
    }
}

fn read_gemini_cli_state() -> Result<GeminiOAuthFileState, AgentError> {
    let auth_path = gemini_cli_auth_path();
    if !auth_path.exists() {
        return Err(AgentError::AuthFailed(
            "Google OAuth credentials not found. Run `hermes auth google-gemini-cli` first.".into(),
        ));
    }
    let raw = std::fs::read_to_string(&auth_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", auth_path.display(), e)))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", auth_path.display(), e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::Config(format!(
            "invalid Google OAuth credentials in {}",
            auth_path.display()
        ))
    })?;

    let packed_refresh = object
        .get("refresh")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let refresh_value = packed_refresh.as_deref().or_else(|| {
        object
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });
    let (refresh_token, packed_project_id, packed_managed_project_id) =
        parse_packed_gemini_refresh(refresh_value);

    let project_id = object
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(packed_project_id);
    let managed_project_id = object
        .get("managed_project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(packed_managed_project_id);
    let packed = pack_gemini_refresh(
        refresh_token.as_deref(),
        project_id.as_deref(),
        managed_project_id.as_deref(),
    );
    let access = object
        .get("access")
        .or_else(|| object.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let email = object
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires = object
        .get("expires")
        .or_else(|| object.get("expires_at_ms"))
        .and_then(value_as_i64);

    Ok(GeminiOAuthFileState {
        refresh: packed,
        access,
        expires,
        email,
        project_id,
        managed_project_id,
    })
}

fn save_gemini_cli_state(state: &GeminiOAuthFileState) -> Result<PathBuf, AgentError> {
    let auth_path = gemini_cli_auth_path();
    if let Some(parent) = auth_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let tmp_path = auth_path.with_extension("tmp");
    let mut raw = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Config(format!("serialize Google OAuth credentials: {}", e)))?;
    raw.push('\n');
    std::fs::write(&tmp_path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", tmp_path.display(), e)))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        std::fs::set_permissions(&tmp_path, perms).map_err(|e| {
            AgentError::Io(format!("set permissions on {}: {}", tmp_path.display(), e))
        })?;
    }
    std::fs::rename(&tmp_path, &auth_path).map_err(|e| {
        AgentError::Io(format!(
            "rename {} -> {}: {}",
            tmp_path.display(),
            auth_path.display(),
            e
        ))
    })?;
    Ok(auth_path)
}

fn gemini_access_token_is_expiring(expiry_ms: Option<i64>, skew_seconds: i64) -> bool {
    let Some(expiry_ms) = expiry_ms else {
        return true;
    };
    let skew = skew_seconds.max(0);
    Utc::now().timestamp_millis() + skew.saturating_mul(1000) >= expiry_ms
}

fn default_http_timeout_seconds(timeout_seconds: f64, fallback: f64) -> f64 {
    if timeout_seconds.is_finite() {
        timeout_seconds.clamp(5.0, 120.0)
    } else {
        fallback
    }
}

fn default_gemini_client_id() -> String {
    format!(
        "{}-{}.apps.googleusercontent.com",
        DEFAULT_GEMINI_CLIENT_ID_PROJECT_NUM, DEFAULT_GEMINI_CLIENT_ID_HASH
    )
}

fn default_gemini_client_secret() -> String {
    format!("GOCSPX-{}", DEFAULT_GEMINI_CLIENT_SECRET_SUFFIX)
}

fn build_oauth_pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut verifier_bytes);
    let verifier = BASE64_URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = BASE64_URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn build_oauth_state_token() -> String {
    let mut state_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut state_bytes);
    BASE64_URL_SAFE_NO_PAD.encode(state_bytes)
}

fn maybe_open_browser(url: &str, enabled: bool) {
    if !enabled {
        return;
    }
    match try_open_url(url) {
        Ok(_) => println!("  (Opened browser for authorization)"),
        Err(err) => println!("  Could not open browser automatically: {}", err),
    }
}

fn parse_code_from_manual_input(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let parsed = reqwest::Url::parse(trimmed).ok()?;
        for (k, v) in parsed.query_pairs() {
            if k == "code" {
                let code = v.trim().to_string();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
        return None;
    }
    if let Some(query) = trimmed.strip_prefix('?') {
        let parsed = reqwest::Url::parse(&format!("http://localhost/?{}", query)).ok()?;
        for (k, v) in parsed.query_pairs() {
            if k == "code" {
                let code = v.trim().to_string();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
        return None;
    }
    Some(trimmed.to_string())
}

fn prompt_line_blocking(prompt: &str) -> Result<String, AgentError> {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| AgentError::Io(format!("stdin: {}", e)))?;
    Ok(buf.trim().to_string())
}

fn respond_oauth_callback(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn wait_for_gemini_oauth_callback(
    listener: &TcpListener,
    expected_state: &str,
    wait_secs: u64,
) -> Result<Option<String>, AgentError> {
    let listener = listener
        .try_clone()
        .map_err(|e| AgentError::Io(format!("clone OAuth callback listener: {}", e)))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| AgentError::Io(format!("set nonblocking callback listener: {}", e)))?;
    let deadline = Instant::now() + Duration::from_secs(wait_secs.max(1));
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 8192];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]);
                let request_line = request.lines().next().unwrap_or("");
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let parsed = reqwest::Url::parse(&format!("http://localhost{}", path)).ok();
                let Some(url) = parsed else {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        "<html><body><h1>Invalid callback</h1></body></html>",
                    );
                    continue;
                };
                if url.path() != GEMINI_CALLBACK_PATH {
                    respond_oauth_callback(
                        &mut stream,
                        "404 Not Found",
                        "<html><body><h1>Not found</h1></body></html>",
                    );
                    continue;
                }

                let mut code = None;
                let mut state = None;
                let mut error = None;
                for (k, v) in url.query_pairs() {
                    if k == "code" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            code = Some(value);
                        }
                    } else if k == "state" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            state = Some(value);
                        }
                    } else if k == "error" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            error = Some(value);
                        }
                    }
                }
                if let Some(err) = error {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        &format!(
                            "<html><body><h1>Google sign-in failed</h1><p>{}</p></body></html>",
                            err
                        ),
                    );
                    return Err(AgentError::AuthFailed(format!(
                        "Google OAuth authorization failed: {}",
                        err
                    )));
                }
                if state.as_deref() != Some(expected_state) {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        "<html><body><h1>State mismatch</h1></body></html>",
                    );
                    return Err(AgentError::AuthFailed(
                        "Google OAuth callback state mismatch".into(),
                    ));
                }
                if let Some(code) = code {
                    respond_oauth_callback(
                        &mut stream,
                        "200 OK",
                        "<html><body><h1>Signed in to Google</h1><p>You can close this tab and return to Hermes.</p></body></html>",
                    );
                    return Ok(Some(code));
                }
                respond_oauth_callback(
                    &mut stream,
                    "400 Bad Request",
                    "<html><body><h1>Missing authorization code</h1></body></html>",
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "listen for Google OAuth callback failed: {}",
                    err
                )));
            }
        }
    }
    Ok(None)
}

async fn fetch_gemini_user_email(access_token: &str, timeout_seconds: f64) -> Option<String> {
    let timeout = default_http_timeout_seconds(timeout_seconds, 15.0);
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}?alt=json", GEMINI_USERINFO_ENDPOINT))
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let payload: Value = serde_json::from_str(&body).ok()?;
    payload
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn refresh_gemini_cli_state(
    state: &GeminiOAuthFileState,
    timeout_seconds: f64,
) -> Result<GeminiOAuthFileState, AgentError> {
    let (refresh_token, _, _) = parse_packed_gemini_refresh(state.refresh.as_deref());
    let refresh_token = refresh_token.ok_or_else(|| {
        AgentError::AuthFailed(
            "Google OAuth refresh token missing. Re-run `hermes auth google-gemini-cli`.".into(),
        )
    })?;
    let token_url = std::env::var("HERMES_GEMINI_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| GEMINI_TOKEN_ENDPOINT.to_string());
    let client_id = std::env::var("HERMES_GEMINI_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_id);
    let client_secret = std::env::var("HERMES_GEMINI_CLIENT_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_secret);

    let timeout = default_http_timeout_seconds(timeout_seconds, 20.0);
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Google OAuth client: {}", e)))?;

    let mut form: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    form.insert("grant_type".to_string(), "refresh_token".to_string());
    form.insert("refresh_token".to_string(), refresh_token.clone());
    form.insert("client_id".to_string(), client_id);
    if !client_secret.is_empty() {
        form.insert("client_secret".to_string(), client_secret);
    }
    let response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Google OAuth refresh failed: {}", e)))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Google OAuth refresh read failed: {}", e)))?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Google OAuth refresh failed ({}): {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|e| {
        AgentError::AuthFailed(format!("Google OAuth refresh JSON parse failed: {}", e))
    })?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Google OAuth refresh response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Google OAuth refresh response missing access_token".into())
        })?
        .to_string();
    let refreshed_refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(refresh_token.as_str())
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(3600)
        .max(60);
    let email = if state.email.is_some() {
        state.email.clone()
    } else {
        fetch_gemini_user_email(&access_token, timeout).await
    };
    Ok(GeminiOAuthFileState {
        refresh: pack_gemini_refresh(
            Some(refreshed_refresh_token.as_str()),
            state.project_id.as_deref(),
            state.managed_project_id.as_deref(),
        ),
        access: Some(access_token),
        expires: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
        email,
        project_id: state.project_id.clone(),
        managed_project_id: state.managed_project_id.clone(),
    })
}

pub async fn resolve_gemini_oauth_runtime_credentials(
    force_refresh: bool,
) -> Result<GeminiRuntimeCredentials, AgentError> {
    let mut state = read_gemini_cli_state()?;
    if force_refresh
        || gemini_access_token_is_expiring(
            state.expires,
            GEMINI_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        )
    {
        state = refresh_gemini_cli_state(&state, 20.0).await?;
        let _ = save_gemini_cli_state(&state)?;
    }
    let api_key = state
        .access
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Google OAuth access token missing. Re-run `hermes auth google-gemini-cli`.".into(),
            )
        })?
        .to_string();
    Ok(GeminiRuntimeCredentials {
        provider: "google-gemini-cli".to_string(),
        base_url: DEFAULT_GEMINI_CLOUDCODE_BASE_URL.to_string(),
        api_key,
        source: "google-oauth".to_string(),
        expires_at_ms: state.expires,
        auth_file: gemini_cli_auth_path(),
        email: state.email,
        project_id: state.project_id,
        refresh_token: parse_packed_gemini_refresh(state.refresh.as_deref()).0,
    })
}

pub async fn get_gemini_oauth_auth_status() -> GeminiOAuthStatus {
    let auth_file = gemini_cli_auth_path();
    match read_gemini_cli_state() {
        Ok(state) => {
            let api_key = state
                .access
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            GeminiOAuthStatus {
                logged_in: api_key.is_some(),
                auth_file,
                source: Some("google-oauth".to_string()),
                api_key,
                expires_at_ms: state.expires,
                email: state.email,
                project_id: state.project_id,
                error: None,
            }
        }
        Err(err) => GeminiOAuthStatus {
            logged_in: false,
            auth_file,
            source: None,
            api_key: None,
            expires_at_ms: None,
            email: None,
            project_id: None,
            error: Some(err.to_string()),
        },
    }
}

fn resolve_gemini_project_id_from_env() -> Option<String> {
    for name in [
        "HERMES_GEMINI_PROJECT_ID",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_PROJECT_ID",
    ] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub async fn login_google_gemini_cli_oauth(
    options: GeminiOAuthLoginOptions,
) -> Result<GeminiRuntimeCredentials, AgentError> {
    let timeout = default_http_timeout_seconds(options.timeout_seconds, 20.0);
    let client_id = std::env::var("HERMES_GEMINI_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_id);
    let client_secret = std::env::var("HERMES_GEMINI_CLIENT_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_secret);
    let token_url = std::env::var("HERMES_GEMINI_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| GEMINI_TOKEN_ENDPOINT.to_string());

    let (code_verifier, code_challenge) = build_oauth_pkce_pair();
    let state_token = build_oauth_state_token();
    let listener = TcpListener::bind((GEMINI_CALLBACK_HOST, GEMINI_CALLBACK_PORT))
        .or_else(|_| TcpListener::bind((GEMINI_CALLBACK_HOST, 0)))
        .map_err(|e| {
            AgentError::Io(format!("bind Google OAuth callback listener failed: {}", e))
        })?;
    let callback_port = listener
        .local_addr()
        .map_err(|e| AgentError::Io(format!("read callback listener addr failed: {}", e)))?
        .port();
    let redirect_uri = format!(
        "http://{}:{}{}",
        GEMINI_CALLBACK_HOST, callback_port, GEMINI_CALLBACK_PATH
    );

    let mut auth_url = reqwest::Url::parse(GEMINI_AUTH_ENDPOINT)
        .map_err(|e| AgentError::Config(format!("invalid Google OAuth authorize URL: {}", e)))?;
    {
        let mut pairs = auth_url.query_pairs_mut();
        pairs.append_pair("client_id", &client_id);
        pairs.append_pair("redirect_uri", &redirect_uri);
        pairs.append_pair("response_type", "code");
        pairs.append_pair("scope", GEMINI_OAUTH_SCOPE);
        pairs.append_pair("state", &state_token);
        pairs.append_pair("code_challenge", &code_challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("access_type", "offline");
        pairs.append_pair("prompt", "consent");
    }
    let auth_url = auth_url.to_string();

    println!();
    println!("Authorize Hermes with Google (Gemini CLI OAuth).");
    println!("Open this URL:");
    println!("  {}", auth_url);
    maybe_open_browser(&auth_url, options.open_browser);

    let code =
        match wait_for_gemini_oauth_callback(&listener, &state_token, GEMINI_CALLBACK_WAIT_SECS)? {
            Some(code) => code,
            None => {
                println!();
                println!(
                    "OAuth callback timed out. Paste the full callback URL or the code value:"
                );
                let raw = prompt_line_blocking("Callback URL or code: ")?;
                parse_code_from_manual_input(&raw).ok_or_else(|| {
                    AgentError::AuthFailed(
                        "No Google OAuth authorization code provided. Aborting.".into(),
                    )
                })?
            }
        };

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Google OAuth client: {}", e)))?;
    let mut form: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    form.insert("grant_type".to_string(), "authorization_code".to_string());
    form.insert("code".to_string(), code.clone());
    form.insert("code_verifier".to_string(), code_verifier.clone());
    form.insert("client_id".to_string(), client_id.clone());
    form.insert("redirect_uri".to_string(), redirect_uri.clone());
    if !client_secret.is_empty() {
        form.insert("client_secret".to_string(), client_secret.clone());
    }
    let token_response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("Google OAuth token exchange failed: {}", e))
        })?;
    let token_status = token_response.status();
    let token_body = token_response.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("Google OAuth token response read failed: {}", e))
    })?;
    if !token_status.is_success() {
        let detail = extract_error_message(&token_body).unwrap_or(token_body);
        return Err(AgentError::AuthFailed(format!(
            "Google OAuth token exchange failed ({}): {}",
            token_status, detail
        )));
    }
    let token_payload: Value = serde_json::from_str(&token_body).map_err(|e| {
        AgentError::AuthFailed(format!("invalid Google OAuth token response: {}", e))
    })?;
    let object = token_payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Google OAuth token response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Google OAuth token response missing access_token".into())
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Google OAuth token response missing refresh_token; re-run login and grant offline access."
                    .into(),
            )
        })?
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(3600)
        .max(60);
    let email = fetch_gemini_user_email(&access_token, timeout).await;
    let project_id = resolve_gemini_project_id_from_env();
    let state = GeminiOAuthFileState {
        refresh: pack_gemini_refresh(Some(&refresh_token), project_id.as_deref(), None),
        access: Some(access_token.clone()),
        expires: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
        email: email.clone(),
        project_id: project_id.clone(),
        managed_project_id: None,
    };
    let auth_file = save_gemini_cli_state(&state)?;
    Ok(GeminiRuntimeCredentials {
        provider: "google-gemini-cli".to_string(),
        base_url: DEFAULT_GEMINI_CLOUDCODE_BASE_URL.to_string(),
        api_key: access_token,
        source: "google-oauth".to_string(),
        expires_at_ms: state.expires,
        auth_file,
        email,
        project_id,
        refresh_token: Some(refresh_token),
    })
}

pub async fn login_anthropic_oauth(
    options: AnthropicOAuthLoginOptions,
) -> Result<AnthropicOAuthState, AgentError> {
    let timeout = default_http_timeout_seconds(options.timeout_seconds, 20.0);
    let authorize_url = std::env::var("HERMES_ANTHROPIC_OAUTH_AUTHORIZE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_AUTHORIZE_URL.to_string());
    let token_url = std::env::var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_TOKEN_URL.to_string());
    let client_id = std::env::var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_CLIENT_ID.to_string());

    let (code_verifier, code_challenge) = build_oauth_pkce_pair();
    let mut auth_url = reqwest::Url::parse(&authorize_url)
        .map_err(|e| AgentError::Config(format!("invalid Anthropic authorize URL: {}", e)))?;
    {
        let mut pairs = auth_url.query_pairs_mut();
        pairs.append_pair("code", "true");
        pairs.append_pair("client_id", &client_id);
        pairs.append_pair("response_type", "code");
        pairs.append_pair("redirect_uri", ANTHROPIC_OAUTH_REDIRECT_URI);
        pairs.append_pair("scope", ANTHROPIC_OAUTH_SCOPE);
        pairs.append_pair("code_challenge", &code_challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", &code_verifier);
    }
    let auth_url = auth_url.to_string();

    println!();
    println!("Authorize Hermes with Claude Pro/Max.");
    println!("Open this URL:");
    println!("  {}", auth_url);
    maybe_open_browser(&auth_url, options.open_browser);
    println!();
    println!("After authorizing, Claude will show a code. Paste it below.");
    let raw_input = prompt_line_blocking("Authorization code: ")?;
    if raw_input.trim().is_empty() {
        return Err(AgentError::AuthFailed(
            "No authorization code entered for Anthropic OAuth".into(),
        ));
    }
    let mut split = raw_input.splitn(2, '#');
    let code = split.next().unwrap_or("").trim().to_string();
    let state = split.next().unwrap_or("").trim().to_string();
    if code.is_empty() {
        return Err(AgentError::AuthFailed(
            "Anthropic OAuth authorization code is empty".into(),
        ));
    }

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Anthropic OAuth client: {}", e)))?;
    let exchange_payload = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id,
        "code": code,
        "state": state,
        "redirect_uri": ANTHROPIC_OAUTH_REDIRECT_URI,
        "code_verifier": code_verifier,
    });
    let response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&exchange_payload)
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("Anthropic OAuth token exchange failed: {}", e))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("Anthropic OAuth response read failed: {}", e))
    })?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Anthropic OAuth token exchange failed ({}): {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid Anthropic OAuth response: {}", e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Anthropic OAuth response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Anthropic OAuth response missing access_token".into())
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = object
        .get("expires_in")
        .and_then(value_as_i64)
        .filter(|v| *v > 0)
        .map(|secs| Utc::now().timestamp_millis() + secs * 1000);
    Ok(AnthropicOAuthState {
        access_token,
        refresh_token,
        expires_at_ms,
    })
}

pub async fn get_anthropic_oauth_status() -> AnthropicOAuthStatus {
    let auth_state = match read_provider_auth_state("anthropic") {
        Ok(value) => value,
        Err(err) => {
            return AnthropicOAuthStatus {
                logged_in: false,
                source: None,
                api_key: None,
                expires_at_ms: None,
                error: Some(err.to_string()),
            };
        }
    };
    let Some(value) = auth_state else {
        return AnthropicOAuthStatus {
            logged_in: false,
            source: None,
            api_key: None,
            expires_at_ms: None,
            error: Some("not logged in".to_string()),
        };
    };
    let object = match value.as_object() {
        Some(v) => v,
        None => {
            return AnthropicOAuthStatus {
                logged_in: false,
                source: None,
                api_key: None,
                expires_at_ms: None,
                error: Some("invalid stored anthropic oauth state".to_string()),
            };
        }
    };
    let api_key = object
        .get("access_token")
        .or_else(|| object.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(Some("hermes_pkce".to_string()));
    let expires_at_ms = object
        .get("expires_at_ms")
        .or_else(|| object.get("expires"))
        .and_then(value_as_i64);
    let is_expired = expires_at_ms
        .map(|exp| {
            gemini_access_token_is_expiring(
                Some(exp),
                ANTHROPIC_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            )
        })
        .unwrap_or(false);
    AnthropicOAuthStatus {
        logged_in: api_key.is_some() && !is_expired,
        source,
        api_key,
        expires_at_ms,
        error: None,
    }
}

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let err = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let desc = value
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if err.is_empty() && desc.is_empty() {
        None
    } else if err.is_empty() {
        Some(desc.to_string())
    } else if desc.is_empty() {
        Some(err.to_string())
    } else {
        Some(format!("{err}: {desc}"))
    }
}

fn try_open_url(url: &str) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cmd.arg(url);

    let status = cmd
        .status()
        .map_err(|e| AgentError::Io(format!("open browser command failed: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Io(format!(
            "open browser command exited with status {}",
            status
        )))
    }
}

pub async fn login_nous_device_code(
    options: NousDeviceCodeOptions,
) -> Result<NousAuthState, AgentError> {
    let portal_base_url = options
        .portal_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            env_or_default(
                "HERMES_PORTAL_BASE_URL",
                &env_or_default("NOUS_PORTAL_BASE_URL", DEFAULT_NOUS_PORTAL_URL),
            )
            .trim_end_matches('/')
            .to_string()
        });
    let requested_inference_base_url = options
        .inference_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_NOUS_INFERENCE_URL));
    let client_id = options
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOUS_CLIENT_ID)
        .to_string();
    let scope = options
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOUS_SCOPE)
        .to_string();
    let timeout_secs = if options.timeout_seconds.is_finite() {
        options.timeout_seconds.clamp(5.0, 120.0)
    } else {
        15.0
    };
    let min_key_ttl_seconds = options.min_key_ttl_seconds.max(60);

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout_secs))
        .build()
        .map_err(|e| AgentError::Io(format!("build oauth client: {}", e)))?;

    println!("Starting Hermes login via Nous Portal...");
    println!("Portal: {}", portal_base_url);

    let mut device_form: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    device_form.insert("client_id".to_string(), client_id.clone());
    if !scope.is_empty() {
        device_form.insert("scope".to_string(), scope.clone());
    }

    let device_resp = client
        .post(format!("{portal_base_url}/api/oauth/device/code"))
        .form(&device_form)
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device code request failed: {}", e)))?;
    let device_status = device_resp.status();
    let device_body = device_resp
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device code response read failed: {}", e)))?;
    if !device_status.is_success() {
        let detail = extract_error_message(&device_body).unwrap_or(device_body);
        return Err(AgentError::AuthFailed(format!(
            "Nous device code request failed ({}): {}",
            device_status, detail
        )));
    }
    let device_data: NousDeviceCodeResponse = serde_json::from_str(&device_body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid device code response: {}", e)))?;

    let device_code = device_data
        .device_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("device code response missing device_code".into()))?
        .to_string();
    let user_code = device_data
        .user_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("device code response missing user_code".into()))?
        .to_string();
    let verification_uri = device_data
        .verification_uri_complete
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            device_data
                .verification_uri
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| {
            AgentError::AuthFailed("device code response missing verification_uri".into())
        })?
        .to_string();
    let expires_in = device_data.expires_in.unwrap_or(900).max(60) as u64;
    let mut poll_interval = device_data.interval.unwrap_or(5).clamp(1, 30) as u64;

    println!();
    println!("To continue:");
    println!("  1. Open: {}", verification_uri);
    println!("  2. If prompted, enter code: {}", user_code);
    if options.open_browser {
        match try_open_url(&verification_uri) {
            Ok(_) => println!("  (Opened browser for verification)"),
            Err(err) => println!("  Could not open browser automatically: {}", err),
        }
    }
    println!("Waiting for approval (polling every {}s)...", poll_interval);

    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let token_payload = loop {
        if Instant::now() >= deadline {
            return Err(AgentError::AuthFailed(
                "timed out waiting for Nous device authorization".into(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;

        let mut token_form: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        token_form.insert(
            "grant_type".to_string(),
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        );
        token_form.insert("client_id".to_string(), client_id.clone());
        token_form.insert("device_code".to_string(), device_code.clone());

        let token_resp = client
            .post(format!("{portal_base_url}/api/oauth/token"))
            .form(&token_form)
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("token poll request failed: {}", e)))?;
        let status = token_resp.status();
        let body = token_resp.text().await.map_err(|e| {
            AgentError::AuthFailed(format!("token poll response read failed: {}", e))
        })?;
        if status.is_success() {
            let payload: NousTokenResponse = serde_json::from_str(&body)
                .map_err(|e| AgentError::AuthFailed(format!("invalid token response: {}", e)))?;
            let has_access_token = payload
                .access_token
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            if !has_access_token {
                return Err(AgentError::AuthFailed(
                    "token response missing access_token".into(),
                ));
            }
            break payload;
        }
        let payload: NousTokenResponse = serde_json::from_str(&body).unwrap_or(NousTokenResponse {
            access_token: None,
            refresh_token: None,
            token_type: None,
            scope: None,
            expires_in: None,
            inference_base_url: None,
            error: None,
            error_description: extract_error_message(&body),
        });
        match payload.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                poll_interval = (poll_interval + 1).min(30);
                continue;
            }
            _ => {
                let detail = payload
                    .error_description
                    .or(payload.error)
                    .unwrap_or_else(|| format!("status {}: {}", status, body));
                return Err(AgentError::AuthFailed(format!(
                    "Nous token exchange failed: {}",
                    detail
                )));
            }
        }
    };

    let access_token = token_payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("token response missing access_token".into()))?
        .to_string();
    let access_expires_in = token_payload.expires_in.filter(|v| *v > 0);
    let now = Utc::now();
    let access_expires_at = access_expires_in.map(|secs| {
        (now + chrono::Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    let mint_resp = client
        .post(format!("{portal_base_url}/api/oauth/agent-key"))
        .bearer_auth(&access_token)
        .json(&serde_json::json!({
            "min_ttl_seconds": min_key_ttl_seconds,
        }))
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("agent key mint request failed: {}", e)))?;
    let mint_status = mint_resp.status();
    let mint_body = mint_resp.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("agent key mint response read failed: {}", e))
    })?;
    if !mint_status.is_success() {
        let parsed = serde_json::from_str::<NousAgentKeyResponse>(&mint_body).ok();
        let detail = parsed
            .and_then(|payload| payload.error_description.or(payload.error))
            .or_else(|| extract_error_message(&mint_body))
            .unwrap_or(mint_body);
        if detail.contains("subscription_required") {
            return Err(AgentError::AuthFailed(format!(
                "Nous subscription required. Subscribe at {}/billing",
                portal_base_url
            )));
        }
        return Err(AgentError::AuthFailed(format!(
            "Nous agent key mint failed ({}): {}",
            mint_status, detail
        )));
    }
    let mint_payload: NousAgentKeyResponse = serde_json::from_str(&mint_body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid agent key response: {}", e)))?;
    let agent_key = mint_payload
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("agent key mint response missing api_key".into()))?
        .to_string();

    let resolved_inference_url = mint_payload
        .inference_base_url
        .or(token_payload.inference_base_url)
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            requested_inference_base_url
                .trim_end_matches('/')
                .to_string()
        });

    Ok(NousAuthState {
        portal_base_url,
        inference_base_url: resolved_inference_url,
        client_id,
        scope: token_payload.scope.or(Some(scope)),
        token_type: token_payload
            .token_type
            .unwrap_or_else(|| "Bearer".to_string()),
        access_token,
        refresh_token: token_payload.refresh_token,
        obtained_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: access_expires_at,
        expires_in: access_expires_in,
        agent_key: Some(agent_key),
        agent_key_id: mint_payload.key_id,
        agent_key_expires_at: mint_payload.expires_at,
        agent_key_expires_in: mint_payload.expires_in,
        agent_key_reused: mint_payload.reused,
        agent_key_obtained_at: Some(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    })
}

pub async fn login_openai_codex_device_code(
    options: CodexDeviceCodeOptions,
) -> Result<CodexAuthState, AgentError> {
    let issuer = DEFAULT_CODEX_ISSUER;
    let timeout_secs = if options.timeout_seconds.is_finite() {
        options.timeout_seconds.clamp(5.0, 120.0)
    } else {
        15.0
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout_secs))
        .build()
        .map_err(|e| AgentError::Io(format!("build oauth client: {}", e)))?;

    let usercode_resp = client
        .post(format!("{issuer}/api/accounts/deviceauth/usercode"))
        .json(&serde_json::json!({
            "client_id": CODEX_OAUTH_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("failed to request codex device code: {}", e))
        })?;
    let usercode_status = usercode_resp.status();
    let usercode_body = usercode_resp.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("failed reading codex device code response: {}", e))
    })?;
    if !usercode_status.is_success() {
        let detail = extract_error_message(&usercode_body).unwrap_or(usercode_body);
        return Err(AgentError::AuthFailed(format!(
            "codex device code request failed ({}): {}",
            usercode_status, detail
        )));
    }
    let usercode_payload: CodexDeviceUserCodeResponse = serde_json::from_str(&usercode_body)
        .map_err(|e| {
            AgentError::AuthFailed(format!("invalid codex device code response: {}", e))
        })?;
    let user_code = usercode_payload
        .user_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex device code response missing user_code".into())
        })?
        .to_string();
    let device_auth_id = usercode_payload
        .device_auth_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex device code response missing device_auth_id".into())
        })?
        .to_string();
    let poll_interval = usercode_payload.interval.unwrap_or(5).max(1) as u64;

    let verify_url = format!("{issuer}/codex/device");
    println!("To continue, follow these steps:\n");
    println!("  1. Open this URL in your browser:");
    println!("     {}", verify_url);
    println!("\n  2. Enter this code:");
    println!("     {}", user_code);
    println!("\nWaiting for sign-in... (press Ctrl+C to cancel)");
    if options.open_browser {
        let _ = try_open_url(&verify_url);
    }

    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let mut code_payload: Option<CodexDevicePollResponse> = None;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;
        let poll_resp = client
            .post(format!("{issuer}/api/accounts/deviceauth/token"))
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("codex device poll failed: {}", e)))?;
        match poll_resp.status().as_u16() {
            200 => {
                let body = poll_resp.text().await.map_err(|e| {
                    AgentError::AuthFailed(format!("codex poll response read failed: {}", e))
                })?;
                let payload: CodexDevicePollResponse =
                    serde_json::from_str(&body).map_err(|e| {
                        AgentError::AuthFailed(format!("invalid codex poll response: {}", e))
                    })?;
                code_payload = Some(payload);
                break;
            }
            403 | 404 => continue,
            status => {
                return Err(AgentError::AuthFailed(format!(
                    "codex device poll failed with status {}",
                    status
                )));
            }
        }
    }
    let code_payload = code_payload
        .ok_or_else(|| AgentError::AuthFailed("codex device login timed out".into()))?;
    let authorization_code = code_payload
        .authorization_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex poll response missing authorization_code".into())
        })?
        .to_string();
    let code_verifier = code_payload
        .code_verifier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("codex poll response missing code_verifier".into()))?
        .to_string();

    let token_resp = client
        .post(CODEX_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code.as_str()),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("codex token exchange failed: {}", e)))?;
    let token_status = token_resp.status();
    let token_body = token_resp
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("codex token response read failed: {}", e)))?;
    if !token_status.is_success() {
        let detail = extract_error_message(&token_body).unwrap_or(token_body);
        return Err(AgentError::AuthFailed(format!(
            "codex token exchange failed ({}): {}",
            token_status, detail
        )));
    }
    let token_payload: CodexTokenResponse = serde_json::from_str(&token_body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid codex token response: {}", e)))?;
    let access_token = token_payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("codex token response missing access_token".into()))?
        .to_string();
    let refresh_token = token_payload
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let base_url = std::env::var("HERMES_CODEX_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_BASE_URL.to_string());
    Ok(CodexAuthState {
        tokens: CodexTokens {
            access_token,
            refresh_token,
            expires_in: token_payload.expires_in,
        },
        base_url,
        last_refresh: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        auth_mode: Some("chatgpt".to_string()),
        source: Some("device_code".to_string()),
    })
}

pub async fn login_openai_device_code(
    options: CodexDeviceCodeOptions,
) -> Result<CodexAuthState, AgentError> {
    let mut state = login_openai_codex_device_code(options).await?;
    state.base_url = std::env::var("HERMES_OPENAI_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string());
    state.auth_mode = Some("openai".to_string());
    state.source = Some("device_code".to_string());
    Ok(state)
}

/// Human-readable line after a successful non-OAuth LLM login (API key stored in token store).
pub async fn login(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "LLM API key stored for provider '{}'.",
        provider.trim()
    ))
}

pub async fn logout(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "Removed stored credential for provider '{}'.",
        provider.trim()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static QWEN_ENV_LOCK: Mutex<()> = Mutex::new(());
    static GEMINI_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn nous_runtime_api_key_prefers_agent_key() {
        let state = NousAuthState {
            portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some(DEFAULT_NOUS_SCOPE.to_string()),
            token_type: "Bearer".to_string(),
            access_token: "portal-access".to_string(),
            refresh_token: Some("refresh".to_string()),
            obtained_at: Utc::now().to_rfc3339(),
            expires_at: None,
            expires_in: None,
            agent_key: Some("agent-key".to_string()),
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        };
        assert_eq!(state.runtime_api_key().as_deref(), Some("agent-key"));
    }

    #[test]
    fn clear_provider_auth_state_is_noop_when_missing() {
        let provider = format!("missing-{}", uuid::Uuid::new_v4().simple());
        let removed = clear_provider_auth_state(&provider).expect("clear");
        assert!(!removed);
    }

    #[test]
    fn qwen_access_token_is_expiring_honors_skew() {
        let now_ms = Utc::now().timestamp_millis();
        assert!(qwen_access_token_is_expiring(None, 120));
        assert!(qwen_access_token_is_expiring(Some(now_ms + 30_000), 120));
        assert!(!qwen_access_token_is_expiring(Some(now_ms + 300_000), 120));
    }

    #[test]
    fn gemini_packed_refresh_roundtrip() {
        let packed =
            pack_gemini_refresh(Some("r1"), Some("proj"), Some("managed")).expect("packed refresh");
        assert_eq!(packed, "r1|proj|managed");
        let parsed = parse_packed_gemini_refresh(Some(&packed));
        assert_eq!(parsed.0.as_deref(), Some("r1"));
        assert_eq!(parsed.1.as_deref(), Some("proj"));
        assert_eq!(parsed.2.as_deref(), Some("managed"));
    }

    #[test]
    fn gemini_access_token_is_expiring_honors_skew() {
        let now_ms = Utc::now().timestamp_millis();
        assert!(gemini_access_token_is_expiring(None, 60));
        assert!(gemini_access_token_is_expiring(Some(now_ms + 1_000), 60));
        assert!(!gemini_access_token_is_expiring(Some(now_ms + 120_000), 60));
    }

    #[test]
    fn gemini_state_read_write_roundtrip() {
        let _guard = GEMINI_ENV_LOCK.lock().expect("lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth_path = tmp.path().join("google_oauth.json");
        std::env::set_var(
            "HERMES_GEMINI_OAUTH_FILE",
            auth_path.to_string_lossy().to_string(),
        );
        let state = GeminiOAuthFileState {
            refresh: Some("refresh-token|proj-1|managed-1".to_string()),
            access: Some("access-token".to_string()),
            expires: Some(Utc::now().timestamp_millis() + 5 * 60 * 1000),
            email: Some("dev@example.com".to_string()),
            project_id: Some("proj-1".to_string()),
            managed_project_id: Some("managed-1".to_string()),
        };
        save_gemini_cli_state(&state).expect("save");
        let loaded = read_gemini_cli_state().expect("read");
        assert_eq!(loaded.access.as_deref(), Some("access-token"));
        assert_eq!(
            parse_packed_gemini_refresh(loaded.refresh.as_deref())
                .0
                .as_deref(),
            Some("refresh-token")
        );
        assert_eq!(loaded.project_id.as_deref(), Some("proj-1"));
        assert_eq!(loaded.managed_project_id.as_deref(), Some("managed-1"));
        std::env::remove_var("HERMES_GEMINI_OAUTH_FILE");
    }

    #[tokio::test]
    async fn resolve_qwen_runtime_credentials_reads_qwen_cli_auth_file() {
        let _guard = QWEN_ENV_LOCK.lock().expect("lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth_path = tmp.path().join("oauth_creds.json");
        let expiry_date = Utc::now().timestamp_millis() + 5 * 60 * 1000;
        let payload = serde_json::json!({
            "access_token": "qwen-access-token",
            "refresh_token": "qwen-refresh-token",
            "token_type": "Bearer",
            "resource_url": "portal.qwen.ai",
            "expiry_date": expiry_date,
        });
        std::fs::write(&auth_path, serde_json::to_string_pretty(&payload).unwrap())
            .expect("write auth file");
        std::env::set_var(
            "HERMES_QWEN_CLI_AUTH_FILE",
            auth_path.to_string_lossy().to_string(),
        );
        std::env::set_var("HERMES_QWEN_BASE_URL", "https://portal.qwen.ai/v1");

        let resolved = resolve_qwen_runtime_credentials(false, false, 120)
            .await
            .expect("resolve");
        assert_eq!(resolved.provider, "qwen-oauth");
        assert_eq!(resolved.api_key, "qwen-access-token");
        assert_eq!(resolved.base_url, "https://portal.qwen.ai/v1".to_string());
        assert_eq!(resolved.expires_at_ms, Some(expiry_date));
        assert_eq!(
            resolved.refresh_token.as_deref(),
            Some("qwen-refresh-token")
        );

        std::env::remove_var("HERMES_QWEN_CLI_AUTH_FILE");
        std::env::remove_var("HERMES_QWEN_BASE_URL");
    }
}
