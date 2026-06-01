//! Real browser backend: CDP (Chrome DevTools Protocol) via WebSocket.
//!
//! This backend connects to a running Chrome/Chromium instance via CDP
//! and provides browser automation capabilities.

use async_trait::async_trait;
use hermes_config::{
    cli_config_path, config_path, managed_nous_tools_enabled, resolve_managed_tool_gateway,
    ManagedToolGatewayConfig, ResolveOptions,
};
use regex::Regex;
use serde_json::{json, Value};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::OnceLock;
use tokio::sync::Mutex;
use url::Url;
use uuid::Uuid;

use crate::tools::browser::BrowserBackend;
use hermes_core::ToolError;

const BROWSERBASE_BASE_URL_DEFAULT: &str = "https://api.browserbase.com";
const BROWSERBASE_MAX_SESSION_TIMEOUT_SECS: u64 = 21_600;
const BROWSER_USE_BASE_URL_DEFAULT: &str = "https://api.browser-use.com/api/v3";
const BROWSER_USE_MANAGED_TIMEOUT_MINUTES: u64 = 5;
const BROWSER_USE_MANAGED_PROXY_COUNTRY_CODE: &str = "us";
#[cfg(test)]
const CAMOFOX_STATE_DIR_NAME: &str = "browser_auth";
#[cfg(test)]
const CAMOFOX_STATE_SUBDIR: &str = "camofox";

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct CamofoxIdentity {
    user_id: String,
    session_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CamofoxLoopbackRewrite {
    from: String,
    to: String,
    original_url: String,
    rewritten_url: String,
}

/// Resolve the default browser backend from environment.
///
/// Selection order:
/// - `HERMES_BROWSER_BACKEND=browser-use` / `BROWSER_CLOUD_PROVIDER=browser-use`
/// - `HERMES_BROWSER_BACKEND=browserbase` / `BROWSER_CLOUD_PROVIDER=browserbase`
/// - Browserbase credentials (`BROWSERBASE_API_KEY` + `BROWSERBASE_PROJECT_ID`)
/// - Browser Use direct or managed gateway configuration
/// - `HERMES_BROWSER_BACKEND=camofox`
/// - local CDP (`CHROME_CDP_URL` or localhost default)
pub fn browser_backend_from_env() -> Arc<dyn BrowserBackend> {
    match browser_backend_choice_from_env() {
        "browser-use" => match BrowserUseBrowserBackend::from_env() {
            Ok(backend) => Arc::new(CloudFallbackBrowserBackend::new(
                "BrowserUseProvider",
                Arc::new(backend),
            )),
            Err(err) if explicit_browser_use_requested_from_env_or_config() => {
                Arc::new(UnavailableBrowserBackend::new(err.to_string()))
            }
            Err(_) => Arc::new(CdpBrowserBackend::from_env()),
        },
        "browserbase" => match BrowserbaseBrowserBackend::from_env() {
            Ok(backend) => Arc::new(CloudFallbackBrowserBackend::new(
                "BrowserbaseProvider",
                Arc::new(backend),
            )),
            Err(err) if explicit_browserbase_requested_from_env_or_config() => {
                Arc::new(UnavailableBrowserBackend::new(err.to_string()))
            }
            Err(_) => Arc::new(CdpBrowserBackend::from_env()),
        },
        "camofox" => Arc::new(CamoFoxBrowserBackend::from_env()),
        _ => Arc::new(CdpBrowserBackend::from_env()),
    }
}

fn explicit_browser_use_requested_from_env_or_config() -> bool {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if normalize_browser_provider(&value) == Some("browser-use") {
                return true;
            }
        }
    }
    matches!(configured_browser_cloud_provider(), Some("browser-use"))
}

fn explicit_browserbase_requested_from_env_or_config() -> bool {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if normalize_browser_provider(&value) == Some("browserbase") {
                return true;
            }
        }
    }
    matches!(configured_browser_cloud_provider(), Some("browserbase"))
}

fn browser_backend_choice_from_env() -> &'static str {
    for key in [
        "HERMES_BROWSER_BACKEND",
        "BROWSER_CLOUD_PROVIDER",
        "BROWSER_PROVIDER",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if let Some(provider) = normalize_browser_provider(&value) {
                return provider;
            }
        }
    }

    if cdp_override_from_env() {
        return "cdp";
    }

    if let Some(provider) = configured_browser_cloud_provider() {
        return provider;
    }

    if BrowserbaseConfig::is_configured_from_env() {
        "browserbase"
    } else if BrowserUseConfig::is_configured_from_env_or_managed() {
        "browser-use"
    } else if camofox_mode_enabled_from_env() {
        "camofox"
    } else {
        "cdp"
    }
}

fn normalize_browser_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "browser-use" | "browser_use" | "browseruse" | "managed-browser" | "managed_browser" => {
            Some("browser-use")
        }
        "browserbase" | "browser-base" => Some("browserbase"),
        "camofox" | "camo" => Some("camofox"),
        "cdp" | "chrome" | "chromium" | "local" => Some("cdp"),
        _ => None,
    }
}

fn env_optional_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(name: &str, default: bool) -> bool {
    env_optional_nonempty(name)
        .map(|v| {
            !matches!(
                v.to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(default)
}

fn cdp_override_from_env() -> bool {
    env_optional_nonempty("CHROME_CDP_URL")
        .or_else(|| env_optional_nonempty("BROWSER_CDP_URL"))
        .is_some()
}

fn camofox_mode_enabled_from_env() -> bool {
    env_optional_nonempty("CAMOFOX_URL").is_some() && !cdp_override_from_env()
}

#[cfg(test)]
fn camofox_state_dir_for_home(home: &Path) -> std::path::PathBuf {
    home.join(CAMOFOX_STATE_DIR_NAME).join(CAMOFOX_STATE_SUBDIR)
}

#[cfg(test)]
fn camofox_identity_for_home(home: &Path, task_id: Option<&str>) -> CamofoxIdentity {
    use sha2::{Digest, Sha256};

    let state_dir = camofox_state_dir_for_home(home);
    let scope_root = state_dir.to_string_lossy();
    let logical_scope = task_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("default");
    let user_digest =
        hex::encode(Sha256::digest(format!("camofox-user:{scope_root}")))[..10].to_string();
    let session_digest = hex::encode(Sha256::digest(format!(
        "camofox-session:{scope_root}:{logical_scope}"
    )))[..16]
        .to_string();
    CamofoxIdentity {
        user_id: format!("hermes_{user_digest}"),
        session_key: format!("task_{session_digest}"),
    }
}

fn camofox_loopback_rewrite_enabled_from_env() -> bool {
    env_bool("CAMOFOX_REWRITE_LOOPBACK_URLS", false)
}

fn camofox_loopback_alias_from_env() -> String {
    env_optional_nonempty("CAMOFOX_LOOPBACK_HOST_ALIAS")
        .unwrap_or_else(|| "host.docker.internal".to_string())
}

fn is_loopback_hostname(host: &str) -> bool {
    let normalized = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    matches!(normalized.as_str(), "localhost" | "localhost.localdomain")
        || normalized
            .parse::<IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false)
}

fn rewrite_loopback_url_for_camofox(
    input: &str,
    enabled: bool,
    alias: &str,
) -> (String, Option<CamofoxLoopbackRewrite>) {
    if !enabled || alias.trim().is_empty() {
        return (input.to_string(), None);
    }

    let Ok(mut parsed) = Url::parse(input) else {
        return (input.to_string(), None);
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return (input.to_string(), None);
    }
    let Some(host) = parsed
        .host_str()
        .map(|host| host.trim_matches(['[', ']']).to_string())
    else {
        return (input.to_string(), None);
    };
    if !is_loopback_hostname(&host) {
        return (input.to_string(), None);
    }
    if parsed.set_host(Some(alias.trim())).is_err() {
        return (input.to_string(), None);
    }
    let rewritten = parsed.to_string();
    (
        rewritten.clone(),
        Some(CamofoxLoopbackRewrite {
            from: host,
            to: alias.trim().to_string(),
            original_url: input.to_string(),
            rewritten_url: rewritten,
        }),
    )
}

fn secret_url_param(key: &str) -> bool {
    let key = key
        .trim()
        .trim_matches(|c: char| c == '-' || c == '_' || c.is_ascii_whitespace())
        .to_ascii_lowercase();
    key.contains("token")
        || key.contains("secret")
        || key.contains("password")
        || key.contains("authorization")
        || key.contains("credential")
        || (key.contains("api") && key.contains("key"))
        || key == "key"
}

fn validate_url_does_not_exfiltrate_secret(input: &str) -> Result<(), ToolError> {
    let Ok(parsed) = Url::parse(input) else {
        return Ok(());
    };
    for (key, value) in parsed.query_pairs() {
        if secret_url_param(&key) && !value.trim().is_empty() {
            return Err(ToolError::InvalidParams(format!(
                "Blocked URL: query parameter '{key}' looks like an API key or token; pass secrets via local env/vault, not browser or web URLs."
            )));
        }
    }
    Ok(())
}

fn secret_assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|token|secret|password|authorization|credential)\b\s*[:=]\s*["']?([A-Za-z0-9_\-./]{8,})"#,
        )
        .expect("secret assignment regex")
    })
}

fn bearer_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\bBearer\s+[A-Za-z0-9_\-./]{12,}"#).expect("bearer token regex")
    })
}

fn provider_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\b(?:sk|ghp|github_pat|or)-[A-Za-z0-9_\-]{12,}"#)
            .expect("provider token regex")
    })
}

fn redact_browser_observation(input: &str) -> String {
    let redacted = secret_assignment_re().replace_all(input, "$1=[REDACTED]");
    let redacted = bearer_token_re().replace_all(&redacted, "Bearer [REDACTED]");
    provider_token_re()
        .replace_all(&redacted, "[REDACTED_SECRET]")
        .to_string()
}

fn add_fallback_metadata(mut value: Value, provider: &str, reason: &str) -> Value {
    if let Some(obj) = value.as_object_mut() {
        obj.insert("fallback_from_cloud".into(), Value::Bool(true));
        obj.insert("fallback_provider".into(), provider.into());
        obj.insert("fallback_reason".into(), reason.into());
        return value;
    }
    json!({
        "fallback_from_cloud": true,
        "fallback_provider": provider,
        "fallback_reason": reason,
        "local_result": value,
    })
}

fn browser_fallback_response(local_result: String, provider: &str, reason: &str) -> String {
    let value = serde_json::from_str::<Value>(&local_result).unwrap_or(Value::String(local_result));
    add_fallback_metadata(value, provider, reason).to_string()
}

fn normalize_base_url(raw: &str) -> String {
    normalize_base_url_with_default(raw, BROWSERBASE_BASE_URL_DEFAULT)
}

fn normalize_base_url_with_default(raw: &str, default: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

fn configured_browser_cloud_provider() -> Option<&'static str> {
    for path in [cli_config_path(), config_path()] {
        if let Some(provider) = browser_provider_from_yaml_path(&path) {
            return Some(provider);
        }
    }
    None
}

fn browser_provider_from_yaml_path(path: &Path) -> Option<&'static str> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let provider = value
        .get("browser")
        .and_then(|browser| browser.get("cloud_provider"))
        .and_then(yaml_scalar_as_string)?;
    normalize_browser_provider(&provider)
}

fn browser_use_prefers_gateway_from_config() -> bool {
    for path in [cli_config_path(), config_path()] {
        if let Some(prefer) = browser_use_gateway_preference_from_yaml_path(&path) {
            return prefer;
        }
    }
    false
}

fn browser_use_gateway_preference_from_yaml_path(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;

    if let Some(raw) = value
        .get("browser")
        .and_then(|browser| browser.get("use_gateway"))
    {
        return Some(yaml_truthy(raw));
    }
    if let Some(raw) = value
        .get("tool_gateway")
        .and_then(|gateway| gateway.get("browser"))
    {
        return Some(yaml_gateway_route(raw));
    }
    None
}

fn yaml_scalar_as_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(s) => Some(s.clone()),
        serde_yaml::Value::Bool(b) => Some(b.to_string()),
        serde_yaml::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn yaml_truthy(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::Bool(b) => *b,
        serde_yaml::Value::Number(n) => n.as_i64().map(|v| v != 0).unwrap_or(false),
        serde_yaml::Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "gateway" | "managed"
        ),
        _ => false,
    }
}

fn yaml_gateway_route(value: &serde_yaml::Value) -> bool {
    match value {
        serde_yaml::Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "gateway" | "managed" | "nous" | "true" | "1" | "yes" | "on"
        ),
        _ => yaml_truthy(value),
    }
}

fn browser_vision_payload(instruction: &str, screenshot: Value) -> String {
    json!({
        "status": "vision_analysis",
        "instruction": instruction,
        "screenshot": screenshot,
        "note": "Screenshot captured; vision analysis requires LLM integration"
    })
    .to_string()
}

/// Browser backend using Chrome DevTools Protocol.
/// Connects to Chrome via WebSocket for automation.
pub struct CdpBrowserBackend {
    /// CDP WebSocket endpoint URL (e.g., ws://localhost:9222/devtools/page/...)
    endpoint: String,
    client: reqwest::Client,
}

/// CamoFox anti-detection browser backend (compat layer).
///
/// Currently routes through CDP endpoint while exposing a dedicated type so
/// higher layers can opt into anti-detection profile selection.
pub struct CamoFoxBrowserBackend {
    inner: CdpBrowserBackend,
    profile: String,
}

struct CloudFallbackBrowserBackend {
    provider: &'static str,
    cloud: Arc<dyn BrowserBackend>,
    local: CdpBrowserBackend,
}

struct UnavailableBrowserBackend {
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserbaseConfig {
    api_key: String,
    project_id: String,
    base_url: String,
    proxies: bool,
    advanced_stealth: bool,
    keep_alive: bool,
    session_timeout_secs: Option<u64>,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserbaseFeatures {
    basic_stealth: bool,
    proxies: bool,
    advanced_stealth: bool,
    keep_alive: bool,
    custom_timeout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserbaseSession {
    session_name: String,
    bb_session_id: String,
    cdp_url: String,
    features: BrowserbaseFeatures,
}

/// Browserbase cloud browser backend.
///
/// Direct Browserbase credentials only. Nous-managed browser routing belongs to
/// Browser Use, matching the current provider contract.
pub struct BrowserbaseBrowserBackend {
    config: BrowserbaseConfig,
    client: reqwest::Client,
    session: Mutex<Option<BrowserbaseSession>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserUseConfig {
    api_key: String,
    base_url: String,
    managed_mode: bool,
    task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserUseSession {
    session_name: String,
    bb_session_id: String,
    cdp_url: String,
    external_call_id: Option<String>,
}

/// Browser Use cloud browser backend.
///
/// Supports both direct `BROWSER_USE_API_KEY` and the Nous-managed Browser Use
/// gateway. Managed mode preserves upstream's idempotency-key behavior for
/// in-flight or retryable session creation.
pub struct BrowserUseBrowserBackend {
    config: BrowserUseConfig,
    client: reqwest::Client,
    session: Mutex<Option<BrowserUseSession>>,
    pending_create_key: Mutex<Option<String>>,
}

impl UnavailableBrowserBackend {
    fn new(message: String) -> Self {
        Self { message }
    }

    fn unavailable(&self) -> ToolError {
        ToolError::ExecutionFailed(self.message.clone())
    }
}

impl CloudFallbackBrowserBackend {
    fn new(provider: &'static str, cloud: Arc<dyn BrowserBackend>) -> Self {
        Self {
            provider,
            cloud,
            local: CdpBrowserBackend::from_env(),
        }
    }

    fn fallback_error(&self, cloud_err: &ToolError, local_err: ToolError) -> ToolError {
        ToolError::ExecutionFailed(format!(
            "{} failed: {}; local CDP fallback failed: {}",
            self.provider, cloud_err, local_err
        ))
    }

    fn mark_fallback(&self, local_result: String, cloud_err: &ToolError) -> String {
        browser_fallback_response(local_result, self.provider, &cloud_err.to_string())
    }
}

impl BrowserbaseConfig {
    pub fn new(api_key: String, project_id: String) -> Self {
        Self {
            api_key,
            project_id,
            base_url: BROWSERBASE_BASE_URL_DEFAULT.to_string(),
            proxies: true,
            advanced_stealth: false,
            keep_alive: true,
            session_timeout_secs: None,
            task_id: "rust".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let api_key = env_optional_nonempty("BROWSERBASE_API_KEY").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browserbase requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID".into(),
            )
        })?;
        let project_id = env_optional_nonempty("BROWSERBASE_PROJECT_ID").ok_or_else(|| {
            ToolError::ExecutionFailed(
                "Browserbase requires BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID".into(),
            )
        })?;
        let mut cfg = Self::new(api_key, project_id);
        if let Some(base_url) = env_optional_nonempty("BROWSERBASE_BASE_URL") {
            cfg.base_url = normalize_base_url(&base_url);
        }
        cfg.proxies = env_bool("BROWSERBASE_PROXIES", true);
        cfg.advanced_stealth = env_bool("BROWSERBASE_ADVANCED_STEALTH", false);
        cfg.keep_alive = env_bool("BROWSERBASE_KEEP_ALIVE", true);
        cfg.session_timeout_secs = env_optional_nonempty("BROWSERBASE_SESSION_TIMEOUT")
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .map(|v| v.min(BROWSERBASE_MAX_SESSION_TIMEOUT_SECS));
        if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
            cfg.task_id = task_id;
        }
        Ok(cfg)
    }

    pub fn is_configured_from_env() -> bool {
        env_optional_nonempty("BROWSERBASE_API_KEY").is_some()
            && env_optional_nonempty("BROWSERBASE_PROJECT_ID").is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }
}

impl BrowserUseConfig {
    pub fn new_direct(api_key: String) -> Self {
        Self {
            api_key,
            base_url: BROWSER_USE_BASE_URL_DEFAULT.to_string(),
            managed_mode: false,
            task_id: "rust".to_string(),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        let direct_api_key = env_optional_nonempty("BROWSER_USE_API_KEY");
        if let Some(api_key) = direct_api_key {
            if !browser_use_prefers_gateway_from_config() {
                let mut cfg = Self::new_direct(api_key);
                if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
                    cfg.task_id = task_id;
                }
                return Ok(cfg);
            }
        }

        if let Some(managed) =
            resolve_managed_tool_gateway("browser-use", ResolveOptions::default())
        {
            let mut cfg = Self::from_managed(&managed);
            if let Some(task_id) = env_optional_nonempty("HERMES_TASK_ID") {
                cfg.task_id = task_id;
            }
            return Ok(cfg);
        }

        let message = if managed_nous_tools_enabled() {
            "Browser Use requires either a direct BROWSER_USE_API_KEY credential or a managed Browser Use gateway configuration."
        } else {
            "Browser Use requires a direct BROWSER_USE_API_KEY credential."
        };
        Err(ToolError::ExecutionFailed(message.into()))
    }

    pub fn from_managed(cfg: &ManagedToolGatewayConfig) -> Self {
        Self {
            api_key: cfg.nous_user_token.clone(),
            base_url: normalize_base_url_with_default(
                &cfg.gateway_origin,
                BROWSER_USE_BASE_URL_DEFAULT,
            ),
            managed_mode: true,
            task_id: "rust".to_string(),
        }
    }

    pub fn is_configured_from_env_or_managed() -> bool {
        env_optional_nonempty("BROWSER_USE_API_KEY").is_some()
            || resolve_managed_tool_gateway("browser-use", ResolveOptions::default()).is_some()
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn managed_mode(&self) -> bool {
        self.managed_mode
    }
}

impl BrowserbaseBrowserBackend {
    pub fn new(config: BrowserbaseConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(BrowserbaseConfig::from_env()?))
    }

    pub fn config(&self) -> &BrowserbaseConfig {
        &self.config
    }

    async fn ensure_session(&self) -> Result<BrowserbaseSession, ToolError> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let session = self.create_session().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn create_session(&self) -> Result<BrowserbaseSession, ToolError> {
        let mut omit_keep_alive = false;
        let mut omit_proxies = false;
        let mut keepalive_fallback = false;
        let mut proxies_fallback = false;

        let mut response = self.post_session(omit_keep_alive, omit_proxies).await?;
        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED && self.config.keep_alive {
            keepalive_fallback = true;
            omit_keep_alive = true;
            response = self.post_session(omit_keep_alive, omit_proxies).await?;
        }
        if response.status() == reqwest::StatusCode::PAYMENT_REQUIRED && self.config.proxies {
            proxies_fallback = true;
            omit_proxies = true;
            response = self.post_session(omit_keep_alive, omit_proxies).await?;
        }

        let status = response.status();
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Browserbase response: {e}"))
        })?;
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browserbase session: {status} {text}"
            )));
        }
        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Browserbase response: {e}"))
        })?;
        let bb_session_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browserbase response missing session id".into())
            })?
            .to_string();
        let cdp_url = data
            .get("connectUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browserbase response missing connectUrl".into())
            })?
            .to_string();
        let suffix = Uuid::new_v4().simple().to_string();
        let session_name = format!("hermes_{}_{}", self.config.task_id, &suffix[..8]);
        let features = BrowserbaseFeatures {
            basic_stealth: true,
            proxies: self.config.proxies && !proxies_fallback,
            advanced_stealth: self.config.advanced_stealth,
            keep_alive: self.config.keep_alive && !keepalive_fallback,
            custom_timeout: self.config.session_timeout_secs.is_some(),
        };
        tracing::info!(
            session_id = %bb_session_id,
            session_name = %session_name,
            "created Browserbase session"
        );
        Ok(BrowserbaseSession {
            session_name,
            bb_session_id,
            cdp_url,
            features,
        })
    }

    async fn post_session(
        &self,
        omit_keep_alive: bool,
        omit_proxies: bool,
    ) -> Result<reqwest::Response, ToolError> {
        self.client
            .post(format!("{}/v1/sessions", self.config.base_url))
            .header("Content-Type", "application/json")
            .header("X-BB-API-Key", &self.config.api_key)
            .json(&browserbase_session_payload(
                &self.config,
                omit_keep_alive,
                omit_proxies,
            ))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Browserbase API connection failed: {e}"))
            })
    }

    pub async fn close_active_session(&self) -> Result<bool, ToolError> {
        let mut guard = self.session.lock().await;
        let Some(session) = guard.take() else {
            return Ok(false);
        };
        self.close_session(&session.bb_session_id).await
    }

    async fn close_session(&self, session_id: &str) -> Result<bool, ToolError> {
        let resp = self
            .client
            .post(format!("{}/v1/sessions/{session_id}", self.config.base_url))
            .header("Content-Type", "application/json")
            .header("X-BB-API-Key", &self.config.api_key)
            .json(&json!({
                "projectId": self.config.project_id,
                "status": "REQUEST_RELEASE",
            }))
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Browserbase close failed: {e}")))?;
        Ok(matches!(
            resp.status(),
            reqwest::StatusCode::OK
                | reqwest::StatusCode::CREATED
                | reqwest::StatusCode::NO_CONTENT
        ))
    }

    async fn browserbase_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let session = self.ensure_session().await?;
        Ok(json!({
            "method": method,
            "params": params,
            "target": session.cdp_url,
            "status": "sent",
            "browserbase": {
                "session_name": session.session_name,
                "bb_session_id": session.bb_session_id,
                "features": {
                    "basic_stealth": session.features.basic_stealth,
                    "proxies": session.features.proxies,
                    "advanced_stealth": session.features.advanced_stealth,
                    "keep_alive": session.features.keep_alive,
                    "custom_timeout": session.features.custom_timeout,
                }
            }
        }))
    }
}

impl BrowserUseBrowserBackend {
    pub fn new(config: BrowserUseConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            session: Mutex::new(None),
            pending_create_key: Mutex::new(None),
        }
    }

    pub fn from_env() -> Result<Self, ToolError> {
        Ok(Self::new(BrowserUseConfig::from_env()?))
    }

    pub fn config(&self) -> &BrowserUseConfig {
        &self.config
    }

    async fn ensure_session(&self) -> Result<BrowserUseSession, ToolError> {
        let mut guard = self.session.lock().await;
        if let Some(session) = guard.as_ref() {
            return Ok(session.clone());
        }
        let session = self.create_session().await?;
        *guard = Some(session.clone());
        Ok(session)
    }

    async fn create_session(&self) -> Result<BrowserUseSession, ToolError> {
        let idempotency_key = if self.config.managed_mode {
            Some(self.get_or_create_pending_create_key().await)
        } else {
            None
        };

        let response = self.post_session(idempotency_key.as_deref()).await?;
        let status = response.status();
        let external_call_id = if self.config.managed_mode {
            response
                .headers()
                .get("x-external-call-id")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_string())
        } else {
            None
        };
        let text = response.text().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to read Browser Use response: {e}"))
        })?;

        if !status.is_success() {
            if self.config.managed_mode
                && !browser_use_should_preserve_pending_create_key(status, &text)
            {
                self.clear_pending_create_key().await;
            }
            return Err(ToolError::ExecutionFailed(format!(
                "Failed to create Browser Use session: {status} {text}"
            )));
        }

        let data: Value = serde_json::from_str(&text).map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse Browser Use response: {e}"))
        })?;
        if self.config.managed_mode {
            self.clear_pending_create_key().await;
        }
        let bb_session_id = data
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                ToolError::ExecutionFailed("Browser Use response missing session id".into())
            })?
            .to_string();
        let cdp_url = data
            .get("cdpUrl")
            .or_else(|| data.get("connectUrl"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let suffix = Uuid::new_v4().simple().to_string();
        let session_name = format!("hermes_{}_{}", self.config.task_id, &suffix[..8]);
        tracing::info!(
            session_id = %bb_session_id,
            session_name = %session_name,
            managed = self.config.managed_mode,
            "created Browser Use session"
        );
        Ok(BrowserUseSession {
            session_name,
            bb_session_id,
            cdp_url,
            external_call_id,
        })
    }

    async fn post_session(
        &self,
        idempotency_key: Option<&str>,
    ) -> Result<reqwest::Response, ToolError> {
        let mut request = self
            .client
            .post(format!("{}/browsers", self.config.base_url))
            .json(&browser_use_session_payload(self.config.managed_mode));
        for (name, value) in browser_use_headers(&self.config, idempotency_key) {
            request = request.header(name, value);
        }
        request.send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Browser Use API connection failed: {e}"))
        })
    }

    async fn get_or_create_pending_create_key(&self) -> String {
        let mut guard = self.pending_create_key.lock().await;
        if let Some(existing) = guard.as_ref() {
            return existing.clone();
        }
        let created = format!("browser-use-session-create:{}", Uuid::new_v4().simple());
        *guard = Some(created.clone());
        created
    }

    async fn clear_pending_create_key(&self) {
        let mut guard = self.pending_create_key.lock().await;
        *guard = None;
    }

    pub async fn close_active_session(&self) -> Result<bool, ToolError> {
        let mut guard = self.session.lock().await;
        let Some(session) = guard.take() else {
            return Ok(false);
        };
        self.close_session(&session.bb_session_id).await
    }

    async fn close_session(&self, session_id: &str) -> Result<bool, ToolError> {
        let mut request = self
            .client
            .patch(format!("{}/browsers/{session_id}", self.config.base_url))
            .json(&json!({"action": "stop"}));
        for (name, value) in browser_use_headers(&self.config, None) {
            request = request.header(name, value);
        }
        let resp = request
            .send()
            .await
            .map_err(|e| ToolError::ExecutionFailed(format!("Browser Use close failed: {e}")))?;
        Ok(matches!(
            resp.status(),
            reqwest::StatusCode::OK
                | reqwest::StatusCode::CREATED
                | reqwest::StatusCode::NO_CONTENT
        ))
    }

    async fn browser_use_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        let session = self.ensure_session().await?;
        Ok(json!({
            "method": method,
            "params": params,
            "target": session.cdp_url,
            "status": "sent",
            "browser_use": {
                "session_name": session.session_name,
                "bb_session_id": session.bb_session_id,
                "features": {"browser_use": true},
                "managed_mode": self.config.managed_mode,
                "external_call_id": session.external_call_id,
            }
        }))
    }
}

fn browser_use_headers(
    config: &BrowserUseConfig,
    idempotency_key: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        ("Content-Type", "application/json".to_string()),
        ("X-Browser-Use-API-Key", config.api_key.clone()),
    ];
    if let Some(key) = idempotency_key {
        headers.push(("X-Idempotency-Key", key.to_string()));
    }
    headers
}

fn browser_use_session_payload(managed_mode: bool) -> Value {
    if managed_mode {
        json!({
            "timeout": BROWSER_USE_MANAGED_TIMEOUT_MINUTES,
            "proxyCountryCode": BROWSER_USE_MANAGED_PROXY_COUNTRY_CODE,
        })
    } else {
        json!({})
    }
}

fn browser_use_should_preserve_pending_create_key(status: reqwest::StatusCode, body: &str) -> bool {
    if status.as_u16() >= 500 {
        return true;
    }
    if status != reqwest::StatusCode::CONFLICT {
        return false;
    }
    let Ok(payload) = serde_json::from_str::<Value>(body) else {
        return false;
    };
    payload
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(|message| message.as_str())
        .map(|message| message.to_ascii_lowercase().contains("already in progress"))
        .unwrap_or(false)
}

fn browserbase_session_payload(
    config: &BrowserbaseConfig,
    omit_keep_alive: bool,
    omit_proxies: bool,
) -> Value {
    let mut payload = json!({"projectId": &config.project_id});
    if config.keep_alive && !omit_keep_alive {
        payload["keepAlive"] = json!(true);
    }
    if let Some(timeout) = config.session_timeout_secs {
        payload["timeout"] = json!(timeout);
    }
    if config.proxies && !omit_proxies {
        payload["proxies"] = json!(true);
    }
    if config.advanced_stealth {
        payload["browserSettings"] = json!({"advancedStealth": true});
    }
    payload
}

#[async_trait]
impl BrowserBackend for UnavailableBrowserBackend {
    async fn navigate(&self, _url: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn click(&self, _selector: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn r#type(&self, _selector: &str, _text: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn scroll(&self, _direction: &str, _amount: Option<u32>) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn press(&self, _key: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn get_images(&self, _selector: Option<&str>) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn vision(&self, _instruction: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }

    async fn console(&self, _action: &str) -> Result<String, ToolError> {
        Err(self.unavailable())
    }
}

#[async_trait]
impl BrowserBackend for CloudFallbackBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        match self.cloud.navigate(url).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser command failed; trying local CDP fallback");
                let local = self
                    .local
                    .navigate(url)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        match self.cloud.snapshot().await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser snapshot failed; trying local CDP fallback");
                let local = self
                    .local
                    .snapshot()
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        match self.cloud.click(selector).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser click failed; trying local CDP fallback");
                let local = self
                    .local
                    .click(selector)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        match self.cloud.r#type(selector, text).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser type failed; trying local CDP fallback");
                let local = self
                    .local
                    .r#type(selector, text)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        match self.cloud.scroll(direction, amount).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser scroll failed; trying local CDP fallback");
                let local = self
                    .local
                    .scroll(direction, amount)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        match self.cloud.go_back().await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser back failed; trying local CDP fallback");
                let local = self
                    .local
                    .go_back()
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        match self.cloud.press(key).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser key press failed; trying local CDP fallback");
                let local = self
                    .local
                    .press(key)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        match self.cloud.get_images(selector).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser images failed; trying local CDP fallback");
                let local = self
                    .local
                    .get_images(selector)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        match self.cloud.vision(instruction).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser vision failed; trying local CDP fallback");
                let local = self
                    .local
                    .vision(instruction)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match self.cloud.console(action).await {
            Ok(result) => Ok(result),
            Err(cloud_err) => {
                tracing::warn!(provider = self.provider, error = %cloud_err, "cloud browser console failed; trying local CDP fallback");
                let local = self
                    .local
                    .console(action)
                    .await
                    .map_err(|local_err| self.fallback_error(&cloud_err, local_err))?;
                Ok(self.mark_fallback(local, &cloud_err))
            }
        }
    }
}

#[async_trait]
impl BrowserBackend for BrowserbaseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .browserbase_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .browserbase_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let js = format!(
            "let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }}",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "typed", "selector": selector, "text": text, "cdp": result})
                .to_string(),
        )
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self
            .browserbase_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .browserbase_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .browserbase_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({"status": "key_pressed", "key": key, "cdp": result}).to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .browserbase_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        let result = self
            .browserbase_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self
                    .browserbase_command("Runtime.evaluate", json!({
                        "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                    }))
                    .await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .browserbase_command(
                        "Runtime.evaluate",
                        json!({"expression": "console.clear(); 'cleared'"}),
                    )
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
    }
}

#[async_trait]
impl BrowserBackend for BrowserUseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .browser_use_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .browser_use_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let js = format!(
            "let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }}",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "typed", "selector": selector, "text": text, "cdp": result})
                .to_string(),
        )
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self
            .browser_use_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .browser_use_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .browser_use_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({"status": "key_pressed", "key": key, "cdp": result}).to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .browser_use_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        let result = self
            .browser_use_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self
                    .browser_use_command("Runtime.evaluate", json!({
                        "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                    }))
                    .await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .browser_use_command(
                        "Runtime.evaluate",
                        json!({"expression": "console.clear(); 'cleared'"}),
                    )
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
    }
}

impl CamoFoxBrowserBackend {
    pub fn new(endpoint: String, profile: String) -> Self {
        Self {
            inner: CdpBrowserBackend::new(endpoint),
            profile,
        }
    }

    pub fn from_env() -> Self {
        let endpoint = std::env::var("CAMOFOX_CDP_URL")
            .or_else(|_| std::env::var("CHROME_CDP_URL"))
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        let profile = std::env::var("CAMOFOX_PROFILE").unwrap_or_else(|_| "default".to_string());
        Self::new(endpoint, profile)
    }
}

impl CdpBrowserBackend {
    pub fn new(endpoint: String) -> Self {
        Self {
            endpoint,
            client: reqwest::Client::new(),
        }
    }

    /// Create from environment variable `CHROME_CDP_URL` or default localhost.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("CHROME_CDP_URL")
            .or_else(|_| std::env::var("BROWSER_CDP_URL"))
            .unwrap_or_else(|_| "http://localhost:9222".to_string());
        Self::new(endpoint)
    }

    /// Send a CDP command via HTTP (simplified - real impl would use WebSocket).
    async fn cdp_command(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        // Get the first available page target
        let targets_resp = self
            .client
            .get(format!("{}/json", self.endpoint))
            .send()
            .await
            .map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Failed to connect to Chrome CDP at {}: {}",
                    self.endpoint, e
                ))
            })?;

        let targets: Vec<Value> = targets_resp.json().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Failed to parse CDP targets: {}", e))
        })?;

        let ws_url = targets.first()
            .and_then(|t| t.get("webSocketDebuggerUrl"))
            .and_then(|u| u.as_str())
            .ok_or_else(|| ToolError::ExecutionFailed("No Chrome page target found. Is Chrome running with --remote-debugging-port=9222?".into()))?;

        // For a full implementation, we'd use tokio-tungstenite to connect
        // to the WebSocket and send CDP commands. For now, return a structured
        // response indicating the command that would be sent.
        Ok(json!({
            "method": method,
            "params": params,
            "target": ws_url,
            "status": "sent",
        }))
    }
}

#[async_trait]
impl BrowserBackend for CdpBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        validate_url_does_not_exfiltrate_secret(url)?;
        let result = self
            .cdp_command("Page.navigate", json!({"url": url}))
            .await?;
        Ok(json!({"status": "navigated", "url": url, "cdp": result}).to_string())
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command("Accessibility.getFullAXTree", json!({}))
            .await?;
        Ok(redact_browser_observation(
            &json!({"status": "snapshot", "cdp": result}).to_string(),
        ))
    }

    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        // Use Runtime.evaluate to find and click the element
        let js = format!(
            "document.querySelector('{}')?.click(); 'clicked'",
            selector.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(json!({"status": "clicked", "selector": selector, "cdp": result}).to_string())
    }

    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        let js = format!(
            "let el = document.querySelector('{}'); if(el) {{ el.value = '{}'; el.dispatchEvent(new Event('input')); 'typed' }} else {{ 'not found' }}",
            selector.replace('\'', "\\'"),
            text.replace('\'', "\\'")
        );
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "typed", "selector": selector, "text": text, "cdp": result})
                .to_string(),
        )
    }

    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        let px = amount.unwrap_or(500) as i32;
        let (x, y) = match direction {
            "up" => (0, -px),
            "down" => (0, px),
            "left" => (-px, 0),
            "right" => (px, 0),
            _ => (0, px),
        };
        let js = format!("window.scrollBy({}, {}); 'scrolled'", x, y);
        let result = self
            .cdp_command("Runtime.evaluate", json!({"expression": js}))
            .await?;
        Ok(
            json!({"status": "scrolled", "direction": direction, "amount": px, "cdp": result})
                .to_string(),
        )
    }

    async fn go_back(&self) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": "history.back(); 'back'"}),
            )
            .await?;
        Ok(json!({"status": "navigated_back", "cdp": result}).to_string())
    }

    async fn press(&self, key: &str) -> Result<String, ToolError> {
        let result = self
            .cdp_command(
                "Input.dispatchKeyEvent",
                json!({
                    "type": "keyDown",
                    "key": key,
                }),
            )
            .await?;
        Ok(json!({"status": "key_pressed", "key": key, "cdp": result}).to_string())
    }

    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        let sel = selector.unwrap_or("img");
        let js = format!(
            "JSON.stringify(Array.from(document.querySelectorAll('{}')).map(img => ({{src: img.src, alt: img.alt, width: img.width, height: img.height}})))",
            sel.replace('\'', "\\'")
        );
        let result = self
            .cdp_command(
                "Runtime.evaluate",
                json!({"expression": js, "returnByValue": true}),
            )
            .await?;
        Ok(json!({"status": "images_found", "selector": sel, "cdp": result}).to_string())
    }

    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        // Take a screenshot and analyze with vision model
        let result = self
            .cdp_command("Page.captureScreenshot", json!({"format": "png"}))
            .await?;
        Ok(browser_vision_payload(instruction, result))
    }

    async fn console(&self, action: &str) -> Result<String, ToolError> {
        match action {
            "read" => {
                let result = self.cdp_command("Runtime.evaluate", json!({
                    "expression": "'Console messages require Runtime.consoleAPICalled event listener'"
                })).await?;
                Ok(json!({"status": "console_read", "cdp": result}).to_string())
            }
            "clear" => {
                let result = self
                    .cdp_command(
                        "Runtime.evaluate",
                        json!({"expression": "console.clear(); 'cleared'"}),
                    )
                    .await?;
                Ok(json!({"status": "console_cleared", "cdp": result}).to_string())
            }
            other => Err(ToolError::InvalidParams(format!(
                "Unknown console action: {}",
                other
            ))),
        }
    }
}

#[async_trait]
impl BrowserBackend for CamoFoxBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        let alias = camofox_loopback_alias_from_env();
        let (browser_url, rewrite) = rewrite_loopback_url_for_camofox(
            url,
            camofox_loopback_rewrite_enabled_from_env(),
            &alias,
        );
        let result = self.inner.navigate(&browser_url).await?;
        let mut value =
            serde_json::from_str::<Value>(&result).unwrap_or_else(|_| json!({"result": result}));
        if let Some(obj) = value.as_object_mut() {
            obj.insert("camofox_profile".into(), self.profile.clone().into());
            if let Some(rewrite) = rewrite {
                obj.insert("requested_url".into(), rewrite.original_url.clone().into());
                obj.insert(
                    "url_rewrite".into(),
                    json!({
                        "from": rewrite.from,
                        "to": rewrite.to,
                        "original_url": rewrite.original_url,
                        "rewritten_url": rewrite.rewritten_url,
                    }),
                );
                obj.insert(
                    "warning".into(),
                    "Rewrote loopback URL for Docker-hosted Camofox".into(),
                );
            }
            Ok(value.to_string())
        } else {
            Ok(value.to_string())
        }
    }

    async fn snapshot(&self) -> Result<String, ToolError> {
        self.inner.snapshot().await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        self.inner.click(selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        self.inner.r#type(selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        self.inner.scroll(direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        self.inner.go_back().await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        self.inner.press(key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        self.inner.get_images(selector).await
    }
    async fn vision(&self, instruction: &str) -> Result<String, ToolError> {
        self.inner.vision(instruction).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        self.inner.console(action).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::managed_gateway::test_lock;

    struct EnvScope {
        original: Vec<(&'static str, Option<String>)>,
        _g: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvScope {
        fn new() -> Self {
            let g = test_lock::lock();
            let keys = [
                "HERMES_BROWSER_BACKEND",
                "BROWSER_CLOUD_PROVIDER",
                "BROWSER_PROVIDER",
                "BROWSERBASE_API_KEY",
                "BROWSERBASE_PROJECT_ID",
                "BROWSERBASE_BASE_URL",
                "BROWSERBASE_PROXIES",
                "BROWSERBASE_ADVANCED_STEALTH",
                "BROWSERBASE_KEEP_ALIVE",
                "BROWSERBASE_SESSION_TIMEOUT",
                "BROWSER_USE_API_KEY",
                "BROWSER_USE_GATEWAY_URL",
                "HERMES_ENABLE_NOUS_MANAGED_TOOLS",
                "HERMES_TASK_ID",
                "HERMES_HOME",
                "HERMES_AGENT_ULTRA_HOME",
                "TOOL_GATEWAY_USER_TOKEN",
                "TOOL_GATEWAY_DOMAIN",
                "TOOL_GATEWAY_SCHEME",
                "CAMOFOX_URL",
                "CAMOFOX_CDP_URL",
                "CHROME_CDP_URL",
                "BROWSER_CDP_URL",
                "CAMOFOX_REWRITE_LOOPBACK_URLS",
                "CAMOFOX_LOOPBACK_HOST_ALIAS",
            ];
            let original = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            for k in &keys {
                std::env::remove_var(k);
            }
            Self { original, _g: g }
        }
    }

    impl Drop for EnvScope {
        fn drop(&mut self) {
            for (k, v) in &self.original {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn camofox_mode_env_honors_cdp_override() {
        let _scope = EnvScope::new();
        std::env::set_var("CAMOFOX_URL", "http://localhost:9377");
        assert!(camofox_mode_enabled_from_env());
        assert_eq!(browser_backend_choice_from_env(), "camofox");

        std::env::set_var(
            "BROWSER_CDP_URL",
            "ws://127.0.0.1:9222/devtools/browser/abc",
        );
        assert!(!camofox_mode_enabled_from_env());
        assert_eq!(browser_backend_choice_from_env(), "cdp");

        std::env::set_var("BROWSER_CDP_URL", "  ");
        assert!(camofox_mode_enabled_from_env());
    }

    #[test]
    fn camofox_identity_is_profile_scoped_and_task_stable() {
        let profile_a = tempfile::tempdir().expect("profile a");
        let profile_b = tempfile::tempdir().expect("profile b");

        assert_eq!(
            camofox_state_dir_for_home(profile_a.path()),
            profile_a.path().join("browser_auth").join("camofox")
        );
        let first = camofox_identity_for_home(profile_a.path(), Some("task-1"));
        let second = camofox_identity_for_home(profile_a.path(), Some("task-1"));
        let other_task = camofox_identity_for_home(profile_a.path(), Some("task-2"));
        let other_profile = camofox_identity_for_home(profile_b.path(), Some("task-1"));

        assert_eq!(first, second);
        assert!(first.user_id.starts_with("hermes_"));
        assert!(first.session_key.starts_with("task_"));
        assert_eq!(first.user_id, other_task.user_id);
        assert_ne!(first.session_key, other_task.session_key);
        assert_ne!(first.user_id, other_profile.user_id);
    }

    #[test]
    fn camofox_loopback_rewrite_is_opt_in_and_preserves_url_parts() {
        let (unchanged, metadata) = rewrite_loopback_url_for_camofox(
            "http://127.0.0.1:8766/#settings",
            false,
            "host.docker.internal",
        );
        assert_eq!(unchanged, "http://127.0.0.1:8766/#settings");
        assert!(metadata.is_none());

        let (rewritten, metadata) = rewrite_loopback_url_for_camofox(
            "http://127.0.0.1:8766/path?q=1#settings",
            true,
            "host.docker.internal",
        );
        let metadata = metadata.expect("rewrite metadata");
        assert_eq!(
            rewritten,
            "http://host.docker.internal:8766/path?q=1#settings"
        );
        assert_eq!(metadata.from, "127.0.0.1");
        assert_eq!(metadata.to, "host.docker.internal");
        assert_eq!(
            metadata.original_url,
            "http://127.0.0.1:8766/path?q=1#settings"
        );
        assert_eq!(metadata.rewritten_url, rewritten);

        let (rewritten_v6, metadata_v6) =
            rewrite_loopback_url_for_camofox("http://[::1]:8080/path", true, "192.168.1.10");
        assert_eq!(rewritten_v6, "http://192.168.1.10:8080/path");
        assert_eq!(metadata_v6.expect("v6 rewrite").from, "::1");

        let (public_url, public_metadata) = rewrite_loopback_url_for_camofox(
            "https://example.com:8443/path?q=1#top",
            true,
            "host.docker.internal",
        );
        assert_eq!(public_url, "https://example.com:8443/path?q=1#top");
        assert!(public_metadata.is_none());
    }

    #[test]
    fn browser_url_secret_exfiltration_guard_blocks_sensitive_query_params() {
        let err = validate_url_does_not_exfiltrate_secret(
            "https://example.com/callback?api_key=sk-abcdef1234567890",
        )
        .expect_err("api key should be blocked");
        assert!(err.to_string().contains("api_key"));
        assert!(err.to_string().contains("API key or token"));

        let err = validate_url_does_not_exfiltrate_secret(
            "https://openrouter.ai/callback?token=or-abcdef1234567890",
        )
        .expect_err("token should be blocked");
        assert!(err.to_string().contains("token"));

        validate_url_does_not_exfiltrate_secret("https://example.com/search?q=api_key docs")
            .expect("normal search URL should be allowed");
    }

    #[test]
    fn browser_observation_redaction_removes_secret_values() {
        let redacted = redact_browser_observation(
            "Dashboard api_key = FAKESECRETVALUE1234567890 token: ghp_fakeToken1234567890 Authorization: Bearer abcdefghijklmnop",
        );
        assert!(!redacted.contains("FAKESECRETVALUE1234567890"));
        assert!(!redacted.contains("ghp_fakeToken1234567890"));
        assert!(!redacted.contains("abcdefghijklmnop"));
        assert!(redacted.contains("Dashboard"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn browser_cloud_fallback_response_preserves_local_success_with_metadata() {
        let local = json!({
            "status": "navigated",
            "url": "https://example.com",
            "features": {"local": true}
        })
        .to_string();
        let rendered = browser_fallback_response(local, "BrowserUseProvider", "401 Unauthorized");
        let value: Value = serde_json::from_str(&rendered).expect("fallback json");

        assert_eq!(value["status"], "navigated");
        assert_eq!(value["fallback_from_cloud"], true);
        assert_eq!(value["fallback_provider"], "BrowserUseProvider");
        assert_eq!(value["fallback_reason"], "401 Unauthorized");
        assert_eq!(value["features"]["local"], true);
    }

    #[test]
    fn browser_cdp_override_bypasses_auto_cloud_provider_detection() {
        let _scope = EnvScope::new();
        std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
        assert_eq!(browser_backend_choice_from_env(), "browser-use");

        std::env::set_var("CHROME_CDP_URL", "ws://host:9222/devtools/browser/abc");
        assert_eq!(browser_backend_choice_from_env(), "cdp");

        std::env::set_var("HERMES_BROWSER_BACKEND", "browser-use");
        assert_eq!(browser_backend_choice_from_env(), "browser-use");
    }

    #[test]
    fn browser_use_config_prefers_direct_key_unless_gateway_is_requested() {
        let _scope = EnvScope::new();
        std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-token");
        std::env::set_var("BROWSER_USE_GATEWAY_URL", "http://127.0.0.1:3009/");
        std::env::set_var("HERMES_TASK_ID", "task-browser-use");

        let cfg = BrowserUseConfig::from_env().expect("browser use direct config");

        assert_eq!(cfg.api_key, "direct-key");
        assert_eq!(cfg.base_url(), BROWSER_USE_BASE_URL_DEFAULT);
        assert!(!cfg.managed_mode());
        assert_eq!(cfg.task_id, "task-browser-use");
    }

    #[test]
    fn browser_use_config_honors_browser_use_gateway_preference() {
        let _scope = EnvScope::new();
        let home = tempfile::tempdir().expect("temp hermes home");
        std::fs::write(
            home.path().join("config.yaml"),
            "browser:\n  cloud_provider: browser-use\n  use_gateway: true\n",
        )
        .expect("write config");
        std::env::set_var("HERMES_HOME", home.path());
        std::env::set_var("BROWSER_USE_API_KEY", "direct-key");
        std::env::set_var("HERMES_ENABLE_NOUS_MANAGED_TOOLS", "1");
        std::env::set_var("TOOL_GATEWAY_USER_TOKEN", "nous-token");
        std::env::set_var("BROWSER_USE_GATEWAY_URL", "http://127.0.0.1:3009/");

        let cfg = BrowserUseConfig::from_env().expect("browser use managed config");

        assert_eq!(cfg.api_key, "nous-token");
        assert_eq!(cfg.base_url(), "http://127.0.0.1:3009");
        assert!(cfg.managed_mode());
    }

    #[test]
    fn browser_use_payload_and_idempotency_rules_match_provider_contract() {
        let cfg = BrowserUseConfig {
            api_key: "key".into(),
            base_url: BROWSER_USE_BASE_URL_DEFAULT.into(),
            managed_mode: true,
            task_id: "task".into(),
        };

        assert_eq!(browser_use_session_payload(false), json!({}));
        assert_eq!(
            browser_use_session_payload(true),
            json!({"timeout": 5, "proxyCountryCode": "us"})
        );
        assert_eq!(
            browser_use_headers(&cfg, Some("browser-use-session-create:abc")),
            vec![
                ("Content-Type", "application/json".to_string()),
                ("X-Browser-Use-API-Key", "key".to_string()),
                (
                    "X-Idempotency-Key",
                    "browser-use-session-create:abc".to_string()
                ),
            ]
        );
        assert!(browser_use_should_preserve_pending_create_key(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            ""
        ));
        assert!(browser_use_should_preserve_pending_create_key(
            reqwest::StatusCode::CONFLICT,
            r#"{"error":{"message":"Managed Browser Use session creation is already in progress"}}"#
        ));
        assert!(!browser_use_should_preserve_pending_create_key(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"bad request"}}"#
        ));
    }

    #[tokio::test]
    async fn browser_use_create_session_sends_managed_gateway_contract() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0_u8; 1024];
            loop {
                let n = stream.read(&mut tmp).await.expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf);
                if text.contains("\r\n\r\n") && text.contains(r#""proxyCountryCode":"us""#) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf).to_string();
            let _ = tx.send(request);
            let body =
                r#"{"id":"bu_local_session_1","connectUrl":"wss://browser-use.example/session"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nx-external-call-id: call-browser-use-1\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let cfg = BrowserUseConfig {
            api_key: "nous-token".into(),
            base_url: format!("http://{addr}"),
            managed_mode: true,
            task_id: "task-browser-use-managed".into(),
        };
        let backend = BrowserUseBrowserBackend::new(cfg);
        let session = backend.create_session().await.expect("create session");
        let request = rx.await.expect("captured request").to_ascii_lowercase();

        assert!(request.starts_with("post /browsers "));
        assert!(request.contains("x-browser-use-api-key: nous-token"));
        assert!(request.contains("x-idempotency-key: browser-use-session-create:"));
        assert!(request.contains(r#""timeout":5"#));
        assert!(request.contains(r#""proxycountrycode":"us""#));
        assert_eq!(session.bb_session_id, "bu_local_session_1");
        assert_eq!(session.cdp_url, "wss://browser-use.example/session");
        assert_eq!(
            session.external_call_id.as_deref(),
            Some("call-browser-use-1")
        );
    }

    #[tokio::test]
    async fn browser_use_close_session_sends_stop_patch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;
        use tokio::sync::oneshot;

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind server");
        let addr = listener.local_addr().expect("server addr");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            let mut tmp = [0_u8; 1024];
            loop {
                let n = stream.read(&mut tmp).await.expect("read request");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&buf);
                if text.contains("\r\n\r\n") && text.contains(r#""action":"stop""#) {
                    break;
                }
            }
            let request = String::from_utf8_lossy(&buf).to_string();
            let _ = tx.send(request);
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .await
                .expect("write response");
        });

        let cfg = BrowserUseConfig {
            api_key: "direct-key".into(),
            base_url: format!("http://{addr}"),
            managed_mode: false,
            task_id: "task-browser-use-close".into(),
        };
        let backend = BrowserUseBrowserBackend::new(cfg);

        assert!(backend
            .close_session("bu_local_session_close")
            .await
            .expect("close session"));
        let request = rx.await.expect("captured request").to_ascii_lowercase();
        assert!(request.starts_with("patch /browsers/bu_local_session_close "));
        assert!(request.contains("x-browser-use-api-key: direct-key"));
        assert!(request.contains(r#""action":"stop""#));
    }

    #[test]
    fn browser_backend_choice_accepts_browser_use_env_and_config() {
        let _scope = EnvScope::new();
        std::env::set_var("BROWSER_CLOUD_PROVIDER", "browser_use");
        assert_eq!(browser_backend_choice_from_env(), "browser-use");

        std::env::remove_var("BROWSER_CLOUD_PROVIDER");
        let home = tempfile::tempdir().expect("temp hermes home");
        std::fs::write(
            home.path().join("config.yaml"),
            "browser:\n  cloud_provider: managed-browser\n",
        )
        .expect("write config");
        std::env::set_var("HERMES_HOME", home.path());
        assert_eq!(browser_backend_choice_from_env(), "browser-use");
    }

    #[test]
    fn browserbase_config_from_env_normalizes_base_url_and_timeout() {
        let _scope = EnvScope::new();
        std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
        std::env::set_var("BROWSERBASE_PROJECT_ID", "proj");
        std::env::set_var("BROWSERBASE_BASE_URL", "https://proxy.example.com/");
        std::env::set_var("BROWSERBASE_SESSION_TIMEOUT", "30000");
        std::env::set_var("HERMES_TASK_ID", "task-42");

        let cfg = BrowserbaseConfig::from_env().expect("browserbase config");

        assert_eq!(cfg.api_key, "bb-key");
        assert_eq!(cfg.project_id, "proj");
        assert_eq!(cfg.base_url(), "https://proxy.example.com");
        assert_eq!(
            cfg.session_timeout_secs,
            Some(BROWSERBASE_MAX_SESSION_TIMEOUT_SECS)
        );
        assert_eq!(cfg.task_id, "task-42");
    }

    #[test]
    fn browserbase_payload_matches_provider_feature_knobs() {
        let mut cfg = BrowserbaseConfig::new("key".into(), "proj".into());
        cfg.session_timeout_secs = Some(120);
        cfg.advanced_stealth = true;

        assert_eq!(
            browserbase_session_payload(&cfg, false, false),
            json!({
                "projectId": "proj",
                "keepAlive": true,
                "timeout": 120,
                "proxies": true,
                "browserSettings": {"advancedStealth": true},
            })
        );
        assert_eq!(
            browserbase_session_payload(&cfg, true, true),
            json!({
                "projectId": "proj",
                "timeout": 120,
                "browserSettings": {"advancedStealth": true},
            })
        );
    }

    #[test]
    fn browser_vision_payload_is_llm_content_independent() {
        let raw = browser_vision_payload("inspect", json!({"data": "png-bytes"}));
        let value: Value = serde_json::from_str(&raw).expect("vision payload json");

        assert_eq!(value["status"], "vision_analysis");
        assert_eq!(value["instruction"], "inspect");
        assert_eq!(value["screenshot"]["data"], "png-bytes");
        assert_eq!(
            value["note"],
            "Screenshot captured; vision analysis requires LLM integration"
        );
    }

    #[test]
    fn browser_backend_choice_prefers_explicit_provider_then_browserbase_creds() {
        let _scope = EnvScope::new();
        assert_eq!(browser_backend_choice_from_env(), "cdp");

        std::env::set_var("BROWSERBASE_API_KEY", "bb-key");
        std::env::set_var("BROWSERBASE_PROJECT_ID", "proj");
        assert_eq!(browser_backend_choice_from_env(), "browserbase");

        std::env::set_var("HERMES_BROWSER_BACKEND", "camofox");
        assert_eq!(browser_backend_choice_from_env(), "camofox");

        std::env::set_var("BROWSER_CLOUD_PROVIDER", "browserbase");
        assert_eq!(browser_backend_choice_from_env(), "camofox");
    }

    #[test]
    fn browser_backend_choice_accepts_browser_cloud_provider() {
        let _scope = EnvScope::new();
        std::env::set_var("BROWSER_CLOUD_PROVIDER", "browserbase");
        assert_eq!(browser_backend_choice_from_env(), "browserbase");
    }

    #[tokio::test]
    async fn explicit_browserbase_without_credentials_fails_at_runtime() {
        let _scope = EnvScope::new();
        std::env::set_var("HERMES_BROWSER_BACKEND", "browserbase");
        let backend = browser_backend_from_env();
        let err = backend.navigate("https://example.com").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID"));
    }

    #[tokio::test]
    async fn configured_browserbase_without_credentials_fails_at_runtime() {
        let _scope = EnvScope::new();
        let home = tempfile::tempdir().expect("temp hermes home");
        std::fs::write(
            home.path().join("config.yaml"),
            "browser:\n  cloud_provider: browserbase\n",
        )
        .expect("write config");
        std::env::set_var("HERMES_HOME", home.path());

        let backend = browser_backend_from_env();
        let err = backend.navigate("https://example.com").await.unwrap_err();
        assert!(err
            .to_string()
            .contains("BROWSERBASE_API_KEY and BROWSERBASE_PROJECT_ID"));
    }
}
