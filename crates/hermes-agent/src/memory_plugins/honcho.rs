//! Honcho memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Honcho's AI-native cross-session
//! user modeling. Provides context recall, peer-card access, semantic search,
//! and persistent conclusions via the Honcho API.
//!
//! Mirrors the Python `plugins/memory/honcho/__init__.py` at the capability
//! level, while using direct HTTP calls instead of the Python SDK.
//!
//! Configuration chain:
//!   1. `$HERMES_HOME/honcho.json`
//!   2. Environment variables (`HONCHO_API_KEY`, `HONCHO_BASE_URL`)
//!   3. Defaults

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
    MemoryVisibility,
};
use hermes_intelligence::redact_sensitive_text;
use parking_lot::Mutex;
use reqwest::Method;
use serde_json::{json, Map, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const HOST: &str = "hermes";
const DEFAULT_BASE_URL: &str = "https://api.honcho.dev";
const DEFAULT_TIMEOUT_SECS: f64 = 30.0;
const OAUTH_ACCESS_TOKEN_PREFIX: &str = "hch-at-";
const OAUTH_REFRESH_SKEW_SECONDS: f64 = 120.0;
const OAUTH_REFRESH_TIMEOUT_SECS: f64 = 15.0;
const EXPORT_PAGE_SIZE: u32 = 100;

enum HonchoExportError {
    Unsupported,
    Failed(&'static str),
}

struct HonchoStablePage {
    snapshot: String,
    revision: String,
    next_cursor: Option<String>,
    records: Vec<Value>,
}

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn profile_schema() -> Value {
    json!({
        "name": "honcho_profile",
        "description": "Retrieve the authenticated subject's Honcho peer card.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "honcho_search",
        "description": "Search only the authenticated subject's Honcho memory.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "max_tokens": {"type": "integer"}
            },
            "required": ["query"]
        }
    })
}

fn context_schema() -> Value {
    json!({
        "name": "honcho_context",
        "description": "Ask Honcho only about the authenticated subject.",
        "parameters": {
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        }
    })
}

fn conclude_schema() -> Value {
    json!({
        "name": "honcho_conclude",
        "description": "Persist or delete a conclusion only for the authenticated subject.",
        "parameters": {
            "type": "object",
            "properties": {
                "conclusion": {"type": "string"},
                "delete_id": {"type": "string"}
            },
            "required": []
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct HonchoConfig {
    api_key: String,
    base_url: String,
    enabled: bool,
    recall_mode: String,
    context_tokens: Option<usize>,
    // Provider endpoints and OAuth profile selection only. Subject identity is
    // never configurable and is always the opaque bound filter.
    timeout_secs: f64,
    endpoints: HashMap<String, String>,
    host_had_explicit_api_key: bool,
    host: String,
    config_path: PathBuf,
    oauth: Option<HonchoOAuthCredential>,
}

#[derive(Debug, Clone, PartialEq)]
struct HonchoOAuthCredential {
    access_token: String,
    refresh_token: String,
    expires_at: f64,
    client_id: String,
    token_endpoint: String,
    scope: String,
    token_type: String,
}

impl HonchoOAuthCredential {
    fn from_host_block(block: &Value) -> Option<Self> {
        let oauth = block.get("oauth").and_then(Value::as_object)?;
        let access_token = block.get("apiKey").and_then(Value::as_str)?;
        if !is_honcho_oauth_access_token(access_token) {
            return None;
        }
        let refresh_token = oauth.get("refreshToken").and_then(Value::as_str)?;
        let token_endpoint = oauth.get("tokenEndpoint").and_then(Value::as_str)?;
        let client_id = oauth.get("clientId").and_then(Value::as_str)?;
        if refresh_token.trim().is_empty()
            || token_endpoint.trim().is_empty()
            || client_id.trim().is_empty()
        {
            return None;
        }

        Some(Self {
            access_token: access_token.to_string(),
            refresh_token: refresh_token.to_string(),
            expires_at: json_number_or_string_as_f64(oauth.get("expiresAt")).unwrap_or(0.0),
            client_id: client_id.to_string(),
            token_endpoint: token_endpoint.to_string(),
            scope: oauth
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("write")
                .to_string(),
            token_type: oauth
                .get("tokenType")
                .and_then(Value::as_str)
                .unwrap_or("Bearer")
                .to_string(),
        })
    }

    fn oauth_block(&self) -> Map<String, Value> {
        let mut block = Map::new();
        block.insert(
            "refreshToken".to_string(),
            Value::String(self.refresh_token.clone()),
        );
        block.insert("expiresAt".to_string(), json!(self.expires_at as i64));
        block.insert(
            "clientId".to_string(),
            Value::String(self.client_id.clone()),
        );
        block.insert(
            "tokenEndpoint".to_string(),
            Value::String(self.token_endpoint.clone()),
        );
        block.insert("scope".to_string(), Value::String(self.scope.clone()));
        block.insert(
            "tokenType".to_string(),
            Value::String(self.token_type.clone()),
        );
        block
    }

    fn is_expired(&self, now: f64) -> bool {
        now >= self.expires_at - OAUTH_REFRESH_SKEW_SECONDS
    }
}

impl HonchoConfig {
    fn config_path(hermes_home: &str) -> std::path::PathBuf {
        std::path::Path::new(hermes_home).join("honcho.json")
    }

    fn default_config_path() -> std::path::PathBuf {
        config_io::default_hermes_home().join("honcho.json")
    }

    fn from_env() -> Self {
        let api_key = std::env::var("HONCHO_API_KEY").unwrap_or_default();
        let base_url = std::env::var("HONCHO_BASE_URL").unwrap_or_default();
        let timeout_secs = std::env::var("HONCHO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1.0, 60.0);
        let mut endpoints = HashMap::new();
        for (key, env) in [
            ("profile", "HONCHO_ENDPOINT_PROFILE"),
            ("search", "HONCHO_ENDPOINT_SEARCH"),
            ("context", "HONCHO_ENDPOINT_CONTEXT"),
            ("conclude", "HONCHO_ENDPOINT_CONCLUDE"),
            ("messages", "HONCHO_ENDPOINT_MESSAGES"),
            ("flush", "HONCHO_ENDPOINT_FLUSH"),
        ] {
            if let Ok(value) = std::env::var(env) {
                if !value.trim().is_empty() {
                    endpoints.insert(key.to_string(), value);
                }
            }
        }
        Self {
            enabled: !api_key.trim().is_empty() || !base_url.trim().is_empty(),
            api_key,
            base_url,
            recall_mode: "hybrid".to_string(),
            context_tokens: Some(800),
            // `workspace_id` is deliberately not configurable: each request
            // uses the bound opaque subject key instead.
            timeout_secs,
            endpoints,
            host_had_explicit_api_key: false,
            host: active_host(),
            config_path: Self::default_config_path(),
            oauth: None,
        }
    }

    fn from_config_file(hermes_home: &str) -> Self {
        let mut config = Self::from_env();
        let host = active_host();
        config.host = host.clone();
        config.config_path = Self::config_path(hermes_home);
        for config_path in honcho_config_load_paths(hermes_home) {
            let Ok(content) = std::fs::read_to_string(&config_path) else {
                continue;
            };
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                Self::apply_config_value(&mut config, &raw);
                if let Some(credential) = HonchoOAuthCredential::from_host_block(&raw) {
                    config.oauth = Some(credential);
                    config.config_path = config_path.clone();
                }
                if let Some(host_block) = honcho_host_block(&raw, &host) {
                    config.host_had_explicit_api_key = value_has_nonempty_api_key(host_block);
                    Self::apply_config_value(&mut config, host_block);
                    if let Some(credential) = HonchoOAuthCredential::from_host_block(host_block) {
                        config.oauth = Some(credential);
                        config.config_path = config_path.clone();
                    }
                }
            }
        }
        config.base_url = normalize_honcho_base_url(&config.base_url);
        if config.base_url.is_empty() {
            config.base_url = DEFAULT_BASE_URL.to_string();
        }
        if honcho_base_url_is_loopback(&config.base_url) && !config.host_had_explicit_api_key {
            // Top-level API keys are usually cloud credentials. Do not send
            // them to a no-auth loopback Honcho unless hosts.<host>.apiKey
            // opted into local JWT/bearer auth.
            config.api_key.clear();
        }
        config
    }

    fn endpoint<'a>(&'a self, key: &str, default: &'a str) -> String {
        self.endpoints
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    fn refresh_oauth_if_needed(&mut self) -> Result<bool, String> {
        let Some(mut current) = self.oauth.clone() else {
            return Ok(false);
        };
        if !is_honcho_oauth_access_token(&self.api_key) {
            return Ok(false);
        }

        let now = epoch_seconds();
        let cache_key = oauth_cache_key(&self.config_path, &self.host);
        if let Some((expires_at, token)) =
            honcho_oauth_expiry_cache().lock().get(&cache_key).cloned()
        {
            if now < expires_at - OAUTH_REFRESH_SKEW_SECONDS {
                self.api_key = token;
                return Ok(false);
            }
        }

        if let Some(on_disk) = read_oauth_credential(&self.config_path, &self.host) {
            current = on_disk;
        }
        seed_oauth_cache(&self.config_path, &self.host, &current);
        if !current.is_expired(now) {
            self.api_key = current.access_token.clone();
            self.oauth = Some(current);
            return Ok(false);
        }

        let _process_guard = honcho_oauth_refresh_lock().lock();
        let _file_guard = ConfigRefreshLock::acquire(&self.config_path);

        if let Some(on_disk) = read_oauth_credential(&self.config_path, &self.host) {
            current = on_disk;
        }
        if !current.is_expired(now) {
            self.api_key = current.access_token.clone();
            self.oauth = Some(current);
            seed_oauth_cache(&self.config_path, &self.host, self.oauth.as_ref().unwrap());
            return Ok(false);
        }

        match exchange_oauth_refresh_token(&current, now) {
            Ok(rotated) => {
                persist_oauth_credential(&self.config_path, &self.host, &rotated)?;
                seed_oauth_cache(&self.config_path, &self.host, &rotated);
                self.api_key = rotated.access_token.clone();
                self.oauth = Some(rotated);
                tracing::info!("Honcho OAuth token refreshed for host {}", self.host);
                Ok(true)
            }
            Err(err) => {
                tracing::warn!(
                    "Honcho OAuth refresh failed for host {}: {}",
                    self.host,
                    err
                );
                self.api_key = current.access_token.clone();
                self.oauth = Some(current);
                Ok(false)
            }
        }
    }

    fn apply_config_value(config: &mut Self, raw: &Value) {
        if let Some(key) = raw
            .get("apiKey")
            .or_else(|| raw.get("api_key"))
            .and_then(|v| v.as_str())
        {
            if !key.is_empty() {
                config.api_key = key.to_string();
            }
        }
        if let Some(url) = raw
            .get("baseUrl")
            .or_else(|| raw.get("base_url"))
            .and_then(|v| v.as_str())
        {
            if !url.is_empty() {
                config.base_url = url.to_string();
            }
        }
        if let Some(mode) = raw.get("recallMode").and_then(|v| v.as_str()) {
            config.recall_mode = match mode {
                "context" | "tools" | "hybrid" => mode.to_string(),
                "auto" => "hybrid".to_string(),
                _ => "hybrid".to_string(),
            };
        }
        if let Some(tokens) = raw.get("contextTokens").and_then(|v| v.as_u64()) {
            config.context_tokens = Some(tokens.clamp(32, 4000) as usize);
        }
        if let Some(timeout) = raw
            .get("timeout")
            .or_else(|| raw.get("requestTimeout"))
            .and_then(|v| v.as_f64())
        {
            config.timeout_secs = timeout.clamp(1.0, 60.0);
        }
        if let Some(enabled) = raw.get("enabled").and_then(|v| v.as_bool()) {
            config.enabled = enabled;
        } else {
            config.enabled =
                !config.api_key.trim().is_empty() || !config.base_url.trim().is_empty();
        }
        if let Some(map) = raw.get("endpoints").and_then(|v| v.as_object()) {
            for (key, value) in map {
                if let Some(path) = value.as_str() {
                    if !path.trim().is_empty() {
                        config.endpoints.insert(key.to_string(), path.to_string());
                    }
                }
            }
        }
    }
}

fn active_host() -> String {
    if let Ok(explicit) = std::env::var("HERMES_HONCHO_HOST") {
        let explicit = explicit.trim();
        if !explicit.is_empty() {
            return explicit.to_string();
        }
    }
    let profile = std::env::var("HERMES_PROFILE").unwrap_or_default();
    profile_host_key(Some(profile.trim()))
}

fn profile_host_key(profile: Option<&str>) -> String {
    let Some(profile) = profile.map(str::trim).filter(|profile| !profile.is_empty()) else {
        return HOST.to_string();
    };
    if matches!(profile, "default" | "custom" | HOST) {
        return HOST.to_string();
    }
    let sanitized = profile
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    format!(
        "{HOST}_{}",
        if sanitized.is_empty() {
            "profile"
        } else {
            &sanitized
        }
    )
}

fn legacy_profile_host_key(host: &str) -> Option<String> {
    let suffix = host.strip_prefix(&format!("{HOST}_"))?;
    if suffix.trim().is_empty() {
        None
    } else {
        Some(format!("{HOST}.{suffix}"))
    }
}

fn honcho_host_block<'a>(raw: &'a Value, host: &str) -> Option<&'a Value> {
    let hosts = raw.get("hosts").and_then(Value::as_object)?;
    if let Some(block) = hosts.get(host) {
        return Some(block);
    }
    legacy_profile_host_key(host).and_then(|legacy| hosts.get(&legacy))
}

fn honcho_config_load_paths(hermes_home: &str) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = dirs::home_dir() {
        paths.push(home.join(".honcho").join("config.json"));
        paths.push(home.join(".hermes").join("honcho.json"));
    }
    paths.push(HonchoConfig::config_path(hermes_home));

    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    deduped
}

fn value_has_nonempty_api_key(raw: &Value) -> bool {
    raw.get("apiKey")
        .or_else(|| raw.get("api_key"))
        .and_then(Value::as_str)
        .is_some_and(|key| !key.trim().is_empty())
}

fn normalize_honcho_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return strip_honcho_base_url_version(trimmed);
    }
    if trimmed.contains("://") {
        return String::new();
    }
    strip_honcho_base_url_version(&format!("https://{trimmed}"))
}

fn strip_honcho_base_url_version(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let Some((prefix, tail)) = trimmed.rsplit_once('/') else {
        return trimmed.to_string();
    };
    let Some(digits) = tail.strip_prefix('v') else {
        return trimmed.to_string();
    };
    if !digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()) {
        prefix.trim_end_matches('/').to_string()
    } else {
        trimmed.to_string()
    }
}

fn honcho_base_url_is_loopback(base_url: &str) -> bool {
    let normalized = base_url.trim().to_ascii_lowercase();
    normalized.contains("localhost")
        || normalized.contains("127.0.0.1")
        || normalized.contains("[::1]")
        || normalized.contains("://::1")
}

fn is_honcho_oauth_access_token(value: &str) -> bool {
    value.starts_with(OAUTH_ACCESS_TOKEN_PREFIX)
}

fn json_number_or_string_as_f64(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) => raw.parse::<f64>().ok(),
        _ => None,
    }
}

fn epoch_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}

type OAuthCacheKey = (String, String);
type OAuthExpiryCache = HashMap<OAuthCacheKey, (f64, String)>;

fn oauth_cache_key(path: &Path, host: &str) -> OAuthCacheKey {
    (path.to_string_lossy().to_string(), host.to_string())
}

fn honcho_oauth_expiry_cache() -> &'static Mutex<OAuthExpiryCache> {
    static CACHE: OnceLock<Mutex<OAuthExpiryCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn honcho_oauth_refresh_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn seed_oauth_cache(path: &Path, host: &str, credential: &HonchoOAuthCredential) {
    honcho_oauth_expiry_cache().lock().insert(
        oauth_cache_key(path, host),
        (credential.expires_at, credential.access_token.clone()),
    );
}

fn read_oauth_credential(path: &Path, host: &str) -> Option<HonchoOAuthCredential> {
    let raw = Value::Object(config_io::read_json_object(path));
    honcho_host_block(&raw, host).and_then(HonchoOAuthCredential::from_host_block)
}

fn persist_oauth_credential(
    path: &Path,
    host: &str,
    credential: &HonchoOAuthCredential,
) -> Result<(), String> {
    let mut root = config_io::read_json_object(path);
    let hosts = root
        .entry("hosts".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !hosts.is_object() {
        *hosts = Value::Object(Map::new());
    }
    let hosts = hosts
        .as_object_mut()
        .ok_or_else(|| "honcho hosts block must be an object".to_string())?;
    let block = hosts
        .entry(host.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !block.is_object() {
        *block = Value::Object(Map::new());
    }
    let block = block
        .as_object_mut()
        .ok_or_else(|| "honcho host block must be an object".to_string())?;
    block.insert(
        "apiKey".to_string(),
        Value::String(credential.access_token.clone()),
    );
    block.insert("oauth".to_string(), Value::Object(credential.oauth_block()));
    config_io::write_owner_only_atomic(path, &Value::Object(root))
}

fn exchange_oauth_refresh_token(
    credential: &HonchoOAuthCredential,
    now: f64,
) -> Result<HonchoOAuthCredential, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs_f64(OAUTH_REFRESH_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("Honcho OAuth refresh client build failed: {e}"))?;
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", credential.client_id.as_str()),
        ("refresh_token", credential.refresh_token.as_str()),
    ];
    let resp = client
        .post(&credential.token_endpoint)
        .form(&form)
        .send()
        .map_err(|e| format!("Honcho OAuth refresh request failed: {e}"))?;
    let status = resp.status();
    let body_text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!(
            "Honcho OAuth refresh returned {}: {}",
            status, body_text
        ));
    }
    let body = serde_json::from_str::<Value>(&body_text)
        .map_err(|e| format!("Honcho OAuth refresh response parse failed: {e}"))?;
    let access_token = body
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| is_honcho_oauth_access_token(token))
        .ok_or_else(|| "Honcho OAuth refresh missing OAuth access_token".to_string())?;
    let refresh_token = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| "Honcho OAuth refresh missing refresh_token".to_string())?;
    let expires_in = json_number_or_string_as_f64(body.get("expires_in")).unwrap_or(0.0);
    Ok(HonchoOAuthCredential {
        access_token: access_token.to_string(),
        refresh_token: refresh_token.to_string(),
        expires_at: now + expires_in,
        client_id: credential.client_id.clone(),
        token_endpoint: credential.token_endpoint.clone(),
        scope: body
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or(&credential.scope)
            .to_string(),
        token_type: body
            .get("token_type")
            .or_else(|| body.get("tokenType"))
            .and_then(Value::as_str)
            .unwrap_or(&credential.token_type)
            .to_string(),
    })
}

struct ConfigRefreshLock {
    file: Option<File>,
}

impl ConfigRefreshLock {
    fn acquire(config_path: &Path) -> Self {
        let lock_path = PathBuf::from(format!("{}.lock", config_path.display()));
        let file = (|| -> Result<File, String> {
            if let Some(parent) = lock_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
            }
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&lock_path)
                .map_err(|e| format!("open {}: {e}", lock_path.display()))?;
            lock_file_exclusive(&file)?;
            Ok(file)
        })();
        match file {
            Ok(file) => Self { file: Some(file) },
            Err(err) => {
                tracing::debug!("Honcho OAuth cross-process lock unavailable: {}", err);
                Self { file: None }
            }
        }
    }
}

impl Drop for ConfigRefreshLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.as_ref() {
            let _ = unlock_file(file);
        }
    }
}

#[cfg(unix)]
fn lock_file_exclusive(file: &File) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn flock(fd: std::os::raw::c_int, operation: std::os::raw::c_int) -> std::os::raw::c_int;
    }

    const LOCK_EX: std::os::raw::c_int = 2;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) -> Result<(), String> {
    use std::os::fd::AsRawFd;

    unsafe extern "C" {
        fn flock(fd: std::os::raw::c_int, operation: std::os::raw::c_int) -> std::os::raw::c_int;
    }

    const LOCK_UN: std::os::raw::c_int = 8;
    let rc = unsafe { flock(file.as_raw_fd(), LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

#[cfg(not(unix))]
fn lock_file_exclusive(_file: &File) -> Result<(), String> {
    Ok(())
}

#[cfg(not(unix))]
fn unlock_file(_file: &File) -> Result<(), String> {
    Ok(())
}

// ---------------------------------------------------------------------------
// HonchoMemoryPlugin
// ---------------------------------------------------------------------------

/// Honcho AI-native memory with a single opaque peer per authenticated subject.
pub struct HonchoMemoryPlugin {
    config: Mutex<Option<HonchoConfig>>,
    bound_subject: Mutex<Option<MemorySubject>>,
    bound_filter: Mutex<Option<MemorySubjectStorageKey>>,
    prefetch_result: Arc<Mutex<String>>,
    turn_count: Mutex<u32>,
    recall_mode: Mutex<String>,
}

impl HonchoMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            bound_subject: Mutex::new(None),
            bound_filter: Mutex::new(None),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            turn_count: Mutex::new(0),
            recall_mode: Mutex::new("hybrid".to_string()),
        }
    }

    fn config_snapshot(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<HonchoConfig, String> {
        if self.bound_subject.lock().as_ref() != Some(subject)
            || self.bound_filter.lock().as_ref() != Some(filter)
        {
            return Err("Honcho request subject does not match initialized subject".into());
        }
        self.config
            .lock()
            .clone()
            .ok_or_else(|| "Honcho is not active for this subject.".to_string())
    }

    fn build_url(config: &HonchoConfig, path: &str) -> String {
        format!(
            "{}/{}",
            config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn apply_template(path: &str, peer: &str, session_id: &str) -> String {
        path.replace("{peer}", peer)
            .replace("{session}", session_id)
            .replace("{session_id}", session_id)
    }

    fn client(config: &HonchoConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout_secs))
            .build()
            .map_err(|error| format!("Honcho HTTP client build failed: {error}"))
    }

    fn send_json(
        config: &HonchoConfig,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, String> {
        let mut effective_config = config.clone();
        effective_config.refresh_oauth_if_needed()?;
        let url = Self::build_url(&effective_config, path);
        let mut request = Self::client(&effective_config)?
            .request(method.clone(), &url)
            .header("Content-Type", "application/json");
        if !effective_config.api_key.is_empty() {
            request = request
                .header(
                    "Authorization",
                    format!("Bearer {}", effective_config.api_key),
                )
                .header("X-API-Key", &effective_config.api_key);
        }
        if let Some(query) = query {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|error| format!("Honcho request {method} {url} failed: {error}"))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Honcho request {method} {url} returned {status}: {text}"
            ));
        }
        if text.trim().is_empty() {
            Ok(json!({}))
        } else {
            serde_json::from_str(&text)
                .map_err(|error| format!("Honcho response parse error: {error}"))
        }
    }

    fn extract_text_result(value: &Value) -> Option<String> {
        for key in ["result", "context", "answer"] {
            if let Some(text) = value.get(key).and_then(Value::as_str) {
                return Some(text.to_string());
            }
        }
        let records = value.get("results").and_then(Value::as_array)?;
        let lines = records
            .iter()
            .filter_map(|record| {
                record
                    .get("memory")
                    .or_else(|| record.get("content"))
                    .or_else(|| record.get("text"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .map(ToString::to_string)
            })
            .collect::<Vec<_>>();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

fn honcho_visibility_label(visibility: MemoryVisibility) -> &'static str {
    match visibility {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Team => "team",
    }
}

fn honcho_scope(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Value {
    let key = filter.as_str();
    json!({
        "opaque_key": key,
        "tenant_filter": key,
        "principal_filter": key,
        "team_filter": subject.team_id().map(|_| key),
        "session_filter": key,
        "surface": subject.surface().as_str(),
        "visibility": honcho_visibility_label(subject.visibility()),
    })
}

fn honcho_scoped_body(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    mut body: Value,
) -> Result<Value, String> {
    let map = body
        .as_object_mut()
        .ok_or_else(|| "Honcho request body must be an object".to_string())?;
    let key = filter.as_str();
    map.insert("subject".to_string(), honcho_scope(subject, filter));
    map.insert("workspace_id".to_string(), json!(key));
    map.insert("peer".to_string(), json!(key));
    map.insert("subject_key".to_string(), json!(key));
    map.insert("tenant_id".to_string(), json!(key));
    map.insert("principal_id".to_string(), json!(key));
    map.insert(
        "team_id".to_string(),
        subject.team_id().map(|_| json!(key)).unwrap_or(Value::Null),
    );
    map.insert("session_id".to_string(), json!(key));
    Ok(body)
}

fn honcho_scope_query(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
) -> Vec<(&'static str, String)> {
    let key = filter.as_str().to_string();
    vec![
        ("workspace_id", key.clone()),
        ("peer", key.clone()),
        ("subject_key", key.clone()),
        ("tenant_id", key.clone()),
        ("principal_id", key.clone()),
        (
            "team_id",
            subject.team_id().map(|_| key.clone()).unwrap_or_default(),
        ),
        ("session_id", key.clone()),
        ("surface", subject.surface().as_str().to_string()),
        (
            "visibility",
            honcho_visibility_label(subject.visibility()).to_string(),
        ),
    ]
}

fn reject_honcho_subject_override(args: &Value) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "peer",
        "user_id",
        "userId",
        "runtime_user_id",
        "runtimeUserId",
        "runtime_id",
        "runtimeId",
        "tenant_id",
        "tenantId",
        "principal_id",
        "principalId",
        "team_id",
        "teamId",
        "session_id",
        "sessionId",
        "subject_key",
        "subjectKey",
    ];
    if FORBIDDEN.iter().any(|key| args.get(*key).is_some()) {
        return Err("authenticated Honcho subject cannot be overridden by tool arguments".into());
    }
    Ok(())
}

fn honcho_records(value: Value) -> Vec<Value> {
    if let Some(records) = value.as_array() {
        return records.clone();
    }
    for key in ["results", "records", "items", "data"] {
        if let Some(records) = value.get(key).and_then(Value::as_array) {
            return records.clone();
        }
    }
    Vec::new()
}

fn honcho_record_id(record: &Value) -> Option<&str> {
    record
        .get("id")
        .or_else(|| record.get("conclusion_id"))
        .or_else(|| record.get("memory_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

fn honcho_retention_candidates(records: &[Value], policy: &MemoryRetentionPolicy) -> Vec<String> {
    let mut selected = std::collections::HashSet::new();
    if let Some(max_records) = policy.max_records {
        for record in records.iter().skip(max_records as usize) {
            if let Some(id) = honcho_record_id(record) {
                selected.insert(id.to_string());
            }
        }
    }
    if let Some(max_age_days) = policy.max_age_days {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);
        for record in records {
            let created = record
                .get("created_at")
                .or_else(|| record.get("createdAt"))
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&chrono::Utc));
            if created.is_some_and(|created| created < cutoff) {
                if let Some(id) = honcho_record_id(record) {
                    selected.insert(id.to_string());
                }
            }
        }
    }
    selected.into_iter().collect()
}

fn honcho_context_query(
    config: &HonchoConfig,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    query: &str,
    max_tokens: usize,
) -> Result<Value, String> {
    let body = honcho_scoped_body(
        subject,
        filter,
        json!({"query": query, "max_tokens": max_tokens.clamp(64, 2000)}),
    )?;
    HonchoMemoryPlugin::send_json(
        config,
        Method::POST,
        &config.endpoint("context", "/v1/context/query"),
        Some(&body),
        None,
    )
}

fn honcho_search_records(
    config: &HonchoConfig,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    query: &str,
) -> Result<Vec<Value>, String> {
    let body = honcho_scoped_body(
        subject,
        filter,
        json!({"query": query, "max_tokens": 2000u32}),
    )?;
    HonchoMemoryPlugin::send_json(
        config,
        Method::POST,
        &config.endpoint("search", "/v1/context/search"),
        Some(&body),
        None,
    )
    .map(honcho_records)
}

fn honcho_export_marker(response: &Value, keys: &[&str]) -> Option<String> {
    [
        Some(response),
        response.get("meta"),
        response.get("metadata"),
        response.get("pagination"),
        response.get("data"),
    ]
    .into_iter()
    .flatten()
    .find_map(|scope| {
        keys.iter().find_map(|key| {
            let value = scope.get(*key)?;
            value
                .as_str()
                .or_else(|| value.get("id").and_then(Value::as_str))
                .or_else(|| value.get("token").and_then(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
    })
}

fn honcho_export_has_more(response: &Value) -> Option<bool> {
    [
        Some(response),
        response.get("meta"),
        response.get("pagination"),
    ]
    .into_iter()
    .flatten()
    .find_map(|scope| {
        scope
            .get("has_more")
            .or_else(|| scope.get("hasMore"))
            .and_then(Value::as_bool)
    })
}

fn stable_honcho_export_page(
    config: &HonchoConfig,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    snapshot: Option<&str>,
    revision: Option<&str>,
    cursor: Option<&str>,
) -> Result<HonchoStablePage, HonchoExportError> {
    let mut body = honcho_scoped_body(
        subject,
        filter,
        json!({
            "query": "*",
            "max_tokens": 2000u32,
            "limit": EXPORT_PAGE_SIZE,
            "page_size": EXPORT_PAGE_SIZE,
            "export_snapshot": true,
            "consistent_snapshot": true,
        }),
    )
    .map_err(|_| HonchoExportError::Failed("unable to build stable export request"))?;
    if let Some(snapshot) = snapshot {
        body["snapshot"] = json!(snapshot);
        body["snapshot_id"] = json!(snapshot);
    }
    if let Some(revision) = revision {
        body["revision"] = json!(revision);
    }
    if let Some(cursor) = cursor {
        body["cursor"] = json!(cursor);
    }
    let response = HonchoMemoryPlugin::send_json(
        config,
        Method::POST,
        &config.endpoint("search", "/v1/context/search"),
        Some(&body),
        None,
    )
    .map_err(|_| HonchoExportError::Failed("stable export listing is unavailable"))?;
    let snapshot = honcho_export_marker(&response, &["snapshot_id", "snapshotId", "snapshot"])
        .ok_or(HonchoExportError::Unsupported)?;
    let revision = honcho_export_marker(
        &response,
        &["revision_id", "revisionId", "revision", "version"],
    )
    .ok_or(HonchoExportError::Unsupported)?;
    let next_cursor = honcho_export_marker(
        &response,
        &[
            "next_cursor",
            "nextCursor",
            "next_page_token",
            "nextPageToken",
        ],
    );
    if honcho_export_has_more(&response) == Some(true) && next_cursor.is_none() {
        return Err(HonchoExportError::Failed(
            "stable export page declared more records without a cursor",
        ));
    }
    Ok(HonchoStablePage {
        snapshot,
        revision,
        next_cursor,
        records: honcho_records(response),
    })
}

fn stream_honcho_export(
    config: &HonchoConfig,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    sink: &mut dyn MemoryExportSink,
) -> Result<(MemoryExportSnapshot, u64, u64), HonchoExportError> {
    let result = (|| {
        let first = stable_honcho_export_page(config, subject, filter, None, None, None)?;
        let snapshot_raw = first.snapshot.clone();
        let revision_raw = first.revision.clone();
        let snapshot = MemoryExportSnapshot::new(
            MemoryExportEvidenceDigest::sha256_bytes(snapshot_raw.as_bytes()),
            MemoryExportEvidenceDigest::sha256_bytes(revision_raw.as_bytes()),
            None,
        );
        sink.begin_provider(snapshot.clone())
            .map_err(|_| HonchoExportError::Failed("export sink rejected provider snapshot"))?;

        let mut current_cursor: Option<String> = None;
        let mut page = first;
        let mut seen_next_cursors = HashSet::new();
        let mut exported_records = 0_u64;
        let mut page_count = 0_u64;
        loop {
            let HonchoStablePage {
                snapshot: received_snapshot,
                revision: received_revision,
                next_cursor,
                records,
            } = page;
            if received_snapshot != snapshot_raw || received_revision != revision_raw {
                return Err(HonchoExportError::Failed(
                    "stable export snapshot changed while paging",
                ));
            }
            if let Some(next_cursor) = next_cursor.as_ref() {
                if !seen_next_cursors.insert(next_cursor.clone()) {
                    return Err(HonchoExportError::Failed(
                        "stable export cursor repeated while paging",
                    ));
                }
            }
            let record_ids = records
                .iter()
                .map(|record| {
                    honcho_record_id(record)
                        .map(|id| MemoryExportEvidenceDigest::sha256_bytes(id.as_bytes()))
                        .ok_or(HonchoExportError::Failed(
                            "stable export record has no stable identifier",
                        ))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let record_count = records.len() as u64;
            sink.append_page(
                MemoryExportPage::new(
                    current_cursor
                        .as_deref()
                        .map(|cursor| MemoryExportEvidenceDigest::sha256_bytes(cursor.as_bytes())),
                    next_cursor
                        .as_deref()
                        .map(|cursor| MemoryExportEvidenceDigest::sha256_bytes(cursor.as_bytes())),
                    record_ids,
                    records,
                )
                .map_err(|_| HonchoExportError::Failed("export page evidence is invalid"))?,
            )
            .map_err(|_| HonchoExportError::Failed("export sink rejected provider page"))?;
            exported_records =
                exported_records
                    .checked_add(record_count)
                    .ok_or(HonchoExportError::Failed(
                        "stable export record count overflow",
                    ))?;
            page_count = page_count.checked_add(1).ok_or(HonchoExportError::Failed(
                "stable export page count overflow",
            ))?;

            let Some(next_cursor) = next_cursor else {
                break;
            };
            current_cursor = Some(next_cursor);
            page = stable_honcho_export_page(
                config,
                subject,
                filter,
                Some(&snapshot_raw),
                Some(&revision_raw),
                current_cursor.as_deref(),
            )?;
        }
        sink.finish_provider(MemoryExportCompletion::exhausted(
            exported_records,
            page_count,
        ))
        .map_err(|_| HonchoExportError::Failed("export sink rejected provider completion"))?;
        Ok((snapshot, exported_records, page_count))
    })();
    if result.is_err() {
        sink.abort();
    }
    result
}

fn honcho_delete_record(
    config: &HonchoConfig,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    id: &str,
) -> Result<(), String> {
    let id = id.trim();
    if id.is_empty() || id.contains('/') || id.contains('?') || id.contains('#') {
        return Err("Honcho record id must be exact".into());
    }
    let path = format!(
        "{}/{}",
        config
            .endpoint("conclude", "/v1/conclusions")
            .trim_end_matches('/'),
        id
    );
    let body = honcho_scoped_body(subject, filter, json!({"id": id}))?;
    HonchoMemoryPlugin::send_json(
        config,
        Method::DELETE,
        &path,
        Some(&body),
        Some(&honcho_scope_query(subject, filter)),
    )
    .map(|_| ())
}

fn honcho_governance_failure(
    provider: &str,
    operation: MemoryGovernanceOperation,
    filter: &MemorySubjectStorageKey,
    actor: Option<&MemoryAuditActor>,
    error: &str,
) -> MemoryProviderGovernanceError {
    if let Some(actor) = actor {
        tracing::warn!(
            provider,
            operation = operation.as_str(),
            subject_key = filter.as_str(),
            actor = actor.opaque_id().as_str(),
            error = %redact_sensitive_text(error),
            "subject-scoped memory governance failed"
        );
    } else {
        tracing::warn!(
            provider,
            operation = operation.as_str(),
            subject_key = filter.as_str(),
            error = %redact_sensitive_text(error),
            "subject-scoped memory governance failed"
        );
    }
    MemoryProviderGovernanceError::failed(provider, operation)
}

impl MemoryProviderPlugin for HonchoMemoryPlugin {
    fn name(&self) -> &str {
        "honcho"
    }

    fn backup_paths(&self) -> Vec<std::path::PathBuf> {
        dirs::home_dir()
            .map(|home| vec![home.join(".honcho")])
            .unwrap_or_default()
    }

    fn is_available(&self) -> bool {
        let hermes_home = config_io::default_hermes_home();
        HonchoConfig::from_config_file(&hermes_home.to_string_lossy()).enabled
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        let config = HonchoConfig::from_config_file(hermes_home);
        if !config.enabled {
            *self.config.lock() = None;
            *self.bound_subject.lock() = None;
            *self.bound_filter.lock() = None;
            return Ok(());
        }
        *self.recall_mode.lock() = config.recall_mode.clone();
        *self.bound_subject.lock() = Some(subject.clone());
        *self.bound_filter.lock() = Some(filter.clone());
        *self.config.lock() = Some(config);
        *self.prefetch_result.lock() = String::new();
        *self.turn_count.lock() = 0;
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        match self.recall_mode.lock().as_str() {
            "context" => "# Honcho Memory\nActive (context-injection mode) for the authenticated memory subject."
                .to_string(),
            "tools" => "# Honcho Memory\nActive (tools-only mode) for the authenticated memory subject."
                .to_string(),
            _ => "# Honcho Memory\nActive (hybrid mode) for the authenticated memory subject."
                .to_string(),
        }
    }

    fn prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _query: &str,
    ) -> String {
        if self.config_snapshot(subject, filter).is_err()
            || self.recall_mode.lock().as_str() == "tools"
        {
            return String::new();
        }
        let result = std::mem::take(&mut *self.prefetch_result.lock());
        if result.is_empty() {
            String::new()
        } else {
            format!("## Honcho Context\n{result}")
        }
    }

    fn queue_prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) {
        if self.recall_mode.lock().as_str() == "tools" || query.trim().is_empty() {
            return;
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let subject = subject.clone();
        let filter = filter.clone();
        let query = query.trim().to_string();
        let max_tokens = config.context_tokens.unwrap_or(800).clamp(64, 2000);
        let out = Arc::clone(&self.prefetch_result);
        std::thread::spawn(move || {
            if let Ok(value) = honcho_context_query(&config, &subject, &filter, &query, max_tokens)
            {
                if let Some(text) = HonchoMemoryPlugin::extract_text_result(&value) {
                    if !text.trim().is_empty() {
                        *out.lock() = text;
                    }
                }
            }
        });
    }

    fn sync_turn(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        let user_content = user_content.trim();
        let assistant_content = assistant_content.trim();
        if user_content.is_empty() || assistant_content.is_empty() {
            return;
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let path = HonchoMemoryPlugin::apply_template(
            &config.endpoint("messages", "/v1/sessions/{session}/messages"),
            filter.as_str(),
            filter.as_str(),
        );
        let payload = match honcho_scoped_body(
            subject,
            filter,
            json!({
                "messages": [
                    {"role":"user","content":redact_sensitive_text(&user_content.chars().take(8000).collect::<String>())},
                    {"role":"assistant","content":redact_sensitive_text(&assistant_content.chars().take(8000).collect::<String>())}
                ]
            }),
        ) {
            Ok(payload) => payload,
            Err(_) => return,
        };
        std::thread::spawn(move || {
            let _ =
                HonchoMemoryPlugin::send_json(&config, Method::POST, &path, Some(&payload), None);
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        if self.recall_mode.lock().as_str() == "context" {
            Vec::new()
        } else {
            vec![
                profile_schema(),
                search_schema(),
                context_schema(),
                conclude_schema(),
            ]
        }
    }

    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        if let Err(error) = reject_honcho_subject_override(args) {
            return json!({"error": error}).to_string();
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(error) => return json!({"error": error}).to_string(),
        };
        let peer = filter.as_str();
        let result = match tool_name {
            "honcho_profile" => {
                let path = HonchoMemoryPlugin::apply_template(
                    &config.endpoint("profile", "/v1/peers/{peer}/card"),
                    peer,
                    peer,
                );
                if let Some(card) = args.get("card").and_then(Value::as_array) {
                    honcho_scoped_body(subject, filter, json!({"card": card})).and_then(|body| {
                        HonchoMemoryPlugin::send_json(
                            &config,
                            Method::POST,
                            &path,
                            Some(&body),
                            None,
                        )
                    })
                } else {
                    HonchoMemoryPlugin::send_json(
                        &config,
                        Method::GET,
                        &path,
                        None,
                        Some(&honcho_scope_query(subject, filter)),
                    )
                }
            }
            "honcho_search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    Err("query is required".to_string())
                } else {
                    let max_tokens = args
                        .get("max_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(800)
                        .clamp(64, 2000);
                    honcho_scoped_body(
                        subject,
                        filter,
                        json!({"query": query, "max_tokens": max_tokens}),
                    )
                    .and_then(|body| {
                        HonchoMemoryPlugin::send_json(
                            &config,
                            Method::POST,
                            &config.endpoint("search", "/v1/context/search"),
                            Some(&body),
                            None,
                        )
                    })
                }
            }
            "honcho_context" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    Err("query is required".to_string())
                } else {
                    honcho_context_query(
                        &config,
                        subject,
                        filter,
                        query,
                        config.context_tokens.unwrap_or(800).clamp(64, 2000),
                    )
                }
            }
            "honcho_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                let delete_id = args
                    .get("delete_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if conclusion.is_empty() == delete_id.is_empty() {
                    Err("provide exactly one of conclusion or delete_id".to_string())
                } else if !delete_id.is_empty() {
                    honcho_delete_record(&config, subject, filter, delete_id)
                        .map(|_| json!({"result":"Conclusion deleted"}))
                } else {
                    honcho_scoped_body(
                        subject,
                        filter,
                        json!({"conclusion": redact_sensitive_text(conclusion)}),
                    )
                    .and_then(|body| {
                        HonchoMemoryPlugin::send_json(
                            &config,
                            Method::POST,
                            &config.endpoint("conclude", "/v1/conclusions"),
                            Some(&body),
                            None,
                        )
                    })
                }
            }
            _ => Err(format!("Unknown tool: {tool_name}")),
        };
        match result {
            Ok(value) => value.to_string(),
            Err(error) => json!({"error": error}).to_string(),
        }
    }

    fn on_turn_start(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        turn_number: u32,
        _message: &str,
    ) {
        if self.bound_subject.lock().as_ref() == Some(subject)
            && self.bound_filter.lock().as_ref() == Some(filter)
        {
            *self.turn_count.lock() = turn_number;
        }
    }

    fn on_session_end(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _messages: &[Value],
    ) {
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let path = HonchoMemoryPlugin::apply_template(
            &config.endpoint("flush", "/v1/sessions/{session}/flush"),
            filter.as_str(),
            filter.as_str(),
        );
        let body = match honcho_scoped_body(subject, filter, json!({})) {
            Ok(body) => body,
            Err(_) => return,
        };
        let _ = HonchoMemoryPlugin::send_json(&config, Method::POST, &path, Some(&body), None);
    }

    fn on_session_switch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _reset: bool,
    ) {
        if self.bound_subject.lock().as_ref() == Some(subject)
            && self.bound_filter.lock().as_ref() == Some(filter)
        {
            *self.turn_count.lock() = 0;
            *self.prefetch_result.lock() = String::new();
        }
    }

    fn on_memory_write(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        action: &str,
        target: &str,
        content: &str,
    ) {
        if action != "add" || target != "user" || content.trim().is_empty() {
            return;
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let body = match honcho_scoped_body(
            subject,
            filter,
            json!({"conclusion": redact_sensitive_text(content.trim()), "source":"memory_write_hook"}),
        ) {
            Ok(body) => body,
            Err(_) => return,
        };
        std::thread::spawn(move || {
            let _ = HonchoMemoryPlugin::send_json(
                &config,
                Method::POST,
                &config.endpoint("conclude", "/v1/conclusions"),
                Some(&body),
                None,
            );
        });
    }

    fn shutdown(&self) {
        *self.config.lock() = None;
        *self.bound_subject.lock() = None;
        *self.bound_filter.lock() = None;
        *self.prefetch_result.lock() = String::new();
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key":"api_key","description":"Honcho API key or OAuth access token","secret":true,"env_var":"HONCHO_API_KEY","url":"https://app.honcho.dev"},
            {"key":"oauth","description":"Honcho OAuth grant metadata","secret":true},
            {"key":"baseUrl","description":"Honcho base URL"},
            {"key":"timeout","description":"HTTP timeout seconds","default":DEFAULT_TIMEOUT_SECS},
            {"key":"endpoints","description":"Optional endpoint path overrides"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        reject_honcho_subject_override(config)?;
        let mut normalized = config
            .as_object()
            .cloned()
            .ok_or_else(|| "config must be a JSON object".to_string())?;
        if let Some(value) = normalized.remove("api_key") {
            normalized.entry("apiKey".to_string()).or_insert(value);
        }
        config_io::merge_and_write_owner_only(
            &HonchoConfig::default_config_path(),
            &Value::Object(normalized),
        )
    }

    fn export_subject(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(error) => {
                sink.abort();
                return Err(honcho_governance_failure(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                    filter,
                    Some(actor),
                    &error,
                ));
            }
        };
        match stream_honcho_export(&config, subject, filter, sink) {
            Ok((snapshot, exported_records, page_count)) => {
                tracing::info!(
                    provider = self.name(),
                    operation = MemoryGovernanceOperation::Export.as_str(),
                    subject_key = filter.as_str(),
                    actor = actor.opaque_id().as_str(),
                    exported_records,
                    page_count,
                    "subject-scoped memory export exhausted"
                );
                Ok(MemoryExportReceipt::exhausted(
                    self.name(),
                    filter.clone(),
                    snapshot,
                    exported_records,
                    page_count,
                ))
            }
            Err(HonchoExportError::Unsupported) => {
                sink.abort();
                Err(MemoryProviderGovernanceError::unsupported(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                ))
            }
            Err(HonchoExportError::Failed(error)) => Err(honcho_governance_failure(
                self.name(),
                MemoryGovernanceOperation::Export,
                filter,
                Some(actor),
                error,
            )),
        }
    }

    // Honcho's context search is semantic rather than a documented stable,
    // subject-scoped inventory. It cannot certify delete-all coverage.
    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Forget,
        ))
    }

    // The search endpoint has no exhaustive order or durable age contract, so
    // it cannot serve as a retention inventory.
    fn retention_sweep(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::RetentionSweep,
        ))
    }

    // A semantic context-search miss is not an absence proof.
    fn verify_subject_absent(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Readback,
        ))
    }
}

include!("honcho/profile_tests.rs");
