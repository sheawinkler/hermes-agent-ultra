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

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Method;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const HOST: &str = "hermes";
const DEFAULT_BASE_URL: &str = "https://api.honcho.dev";
const DEFAULT_TIMEOUT_SECS: f64 = 30.0;
const PEER_ID_HASH_ESCALATION_LENGTHS: &[usize] = &[8, 12, 16, 24, 32, 64];
const OAUTH_ACCESS_TOKEN_PREFIX: &str = "hch-at-";
const OAUTH_REFRESH_SKEW_SECONDS: f64 = 120.0;
const OAUTH_REFRESH_TIMEOUT_SECS: f64 = 15.0;

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn profile_schema() -> Value {
    json!({
        "name": "honcho_profile",
        "description": "Retrieve the user's peer card from Honcho — a curated list of key facts about them. Fast, no LLM reasoning.",
        "parameters": {"type": "object", "properties": {"peer": {"type":"string"}}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "honcho_search",
        "description": "Semantic search over Honcho's stored context about the user. Returns raw excerpts ranked by relevance.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for in Honcho's memory."},
                "max_tokens": {"type": "integer", "description": "Token budget for returned context (default 800, max 2000)."},
                "peer": {"type":"string", "description": "Peer alias or peer id (default user)."}
            },
            "required": ["query"]
        }
    })
}

fn context_schema() -> Value {
    json!({
        "name": "honcho_context",
        "description": "Ask Honcho a natural language question and get a synthesized answer using dialectic reasoning.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "A natural language question."},
                "peer": {"type": "string", "description": "Which peer to query about: 'user' (default) or 'ai'."}
            },
            "required": ["query"]
        }
    })
}

fn conclude_schema() -> Value {
    json!({
        "name": "honcho_conclude",
        "description": "Write a conclusion about the user back to Honcho's memory. Conclusions are persistent facts that build the user's profile.",
        "parameters": {
            "type": "object",
            "properties": {
                "conclusion": {"type": "string", "description": "A factual statement about the user to persist."},
                "delete_id": {"type": "string", "description": "Optional conclusion id to delete."},
                "peer": {"type":"string", "description": "Peer alias or peer id (default user)."}
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
    workspace_id: String,
    peer_name: Option<String>,
    ai_peer: String,
    pin_user_peer: bool,
    user_peer_aliases: HashMap<String, String>,
    runtime_peer_prefix: String,
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
            workspace_id: "hermes".to_string(),
            peer_name: None,
            ai_peer: "hermes".to_string(),
            pin_user_peer: false,
            user_peer_aliases: HashMap::new(),
            runtime_peer_prefix: String::new(),
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
        config.workspace_id = host.clone();
        config.ai_peer = host.clone();
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
        if let Some((expires_at, token)) = honcho_oauth_expiry_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&cache_key)
            .cloned()
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

        let _process_guard = honcho_oauth_refresh_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
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
        if let Some(ws) = raw.get("workspace").and_then(|v| v.as_str()) {
            config.workspace_id = ws.to_string();
        }
        if let Some(peer) = raw.get("peerName").and_then(|v| v.as_str()) {
            config.peer_name = Some(peer.to_string());
        }
        if let Some(ai) = raw.get("aiPeer").and_then(|v| v.as_str()) {
            config.ai_peer = ai.to_string();
        }
        if let Some(pin) = raw
            .get("pinUserPeer")
            .or_else(|| raw.get("pinPeerName"))
            .and_then(|v| v.as_bool())
        {
            config.pin_user_peer = pin;
        }
        if let Some(map) = raw.get("userPeerAliases").and_then(|v| v.as_object()) {
            config.user_peer_aliases = map
                .iter()
                .filter_map(|(key, value)| {
                    let alias = value.as_str()?.trim();
                    if key.trim().is_empty() || alias.is_empty() {
                        None
                    } else {
                        Some((key.trim().to_string(), alias.to_string()))
                    }
                })
                .collect();
        }
        if let Some(prefix) = raw.get("runtimePeerPrefix").and_then(|v| v.as_str()) {
            config.runtime_peer_prefix = prefix.trim().to_string();
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
    honcho_oauth_expiry_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
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

/// Honcho AI-native memory with dialectic Q&A and persistent user modeling.
pub struct HonchoMemoryPlugin {
    config: Mutex<Option<HonchoConfig>>,
    session_key: Mutex<String>,
    prefetch_result: Arc<Mutex<String>>,
    turn_count: Mutex<u32>,
    recall_mode: Mutex<String>,
}

impl HonchoMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_key: Mutex::new(String::new()),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            turn_count: Mutex::new(0),
            recall_mode: Mutex::new("hybrid".to_string()),
        }
    }

    fn config_snapshot(&self) -> Result<HonchoConfig, String> {
        self.config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "Honcho is not active for this session.".to_string())
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

    fn extract_peer(config: &HonchoConfig, args: &Value) -> String {
        match args.get("peer").and_then(|v| v.as_str()).unwrap_or("user") {
            "ai" => config.ai_peer.clone(),
            "user" => {
                let runtime_ids = runtime_user_ids_from_args(args)
                    .into_iter()
                    .chain(runtime_user_ids_from_env())
                    .collect::<Vec<_>>();
                Self::resolve_user_peer_id(config, "", &runtime_ids)
            }
            other => sanitize_id(other),
        }
    }

    fn resolve_user_peer_id(
        config: &HonchoConfig,
        session_key: &str,
        runtime_ids: &[String],
    ) -> String {
        if config.pin_user_peer {
            if let Some(peer) = config.peer_name.as_deref() {
                if !peer.trim().is_empty() {
                    return sanitize_id(peer.trim());
                }
            }
        }

        for runtime_id in unique_nonempty(runtime_ids) {
            if let Some(alias) = config.user_peer_aliases.get(&runtime_id) {
                if !alias.trim().is_empty() {
                    return sanitize_id(alias.trim());
                }
            }
        }

        if let Some(primary_runtime_id) = unique_nonempty(runtime_ids).into_iter().next() {
            if !config.runtime_peer_prefix.is_empty() {
                return generated_runtime_peer_id(
                    config,
                    &config.runtime_peer_prefix,
                    &primary_runtime_id,
                );
            }
            return sanitize_id(&primary_runtime_id);
        }

        if let Some(peer) = config.peer_name.as_deref() {
            if !peer.trim().is_empty() {
                return sanitize_id(peer.trim());
            }
        }

        session_key_fallback_peer_id(session_key)
    }

    fn client(config: &HonchoConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout_secs))
            .build()
            .map_err(|e| format!("Honcho HTTP client build failed: {e}"))
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
        let client = Self::client(&effective_config)?;
        let url = Self::build_url(&effective_config, path);
        let mut req = client
            .request(method.clone(), &url)
            .header("Content-Type", "application/json");
        if !effective_config.api_key.is_empty() {
            req = req
                .header(
                    "Authorization",
                    format!("Bearer {}", effective_config.api_key),
                )
                .header("X-API-Key", &effective_config.api_key);
        }
        if let Some(items) = query {
            req = req.query(items);
        }
        if let Some(payload) = body {
            req = req.json(payload);
        }
        let resp = req
            .send()
            .map_err(|e| format!("Honcho request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body_text = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Honcho request {} {} returned {}: {}",
                method, url, status, body_text
            ));
        }
        if body_text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str::<Value>(&body_text)
            .map_err(|e| format!("Honcho response parse error: {e}; body={body_text}"))
    }

    fn extract_text_result(v: &Value) -> Option<String> {
        if let Some(s) = v.get("result").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = v.get("context").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = v.get("answer").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(arr) = v.get("results").and_then(|v| v.as_array()) {
            let lines: Vec<String> = arr
                .iter()
                .filter_map(|item| {
                    item.get("memory")
                        .or_else(|| item.get("content"))
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !lines.is_empty() {
                return Some(lines.join("\n"));
            }
        }
        None
    }

    fn context_query(
        config: &HonchoConfig,
        session_id: &str,
        query: &str,
        max_tokens: usize,
        peer: &str,
    ) -> Result<Value, String> {
        let path = config.endpoint("context", "/v1/context/query");
        let payload = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "peer": peer,
            "query": query,
            "max_tokens": max_tokens,
        });
        Self::send_json(config, Method::POST, &path, Some(&payload), None)
    }
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
        let config = HonchoConfig::from_config_file(&hermes_home.to_string_lossy());
        config.enabled
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = HonchoConfig::from_config_file(hermes_home);
        if !config.enabled {
            tracing::debug!("Honcho not configured — plugin inactive");
            return;
        }

        *self.recall_mode.lock().unwrap() = config.recall_mode.clone();
        *self.session_key.lock().unwrap() = session_id.to_string();
        *self.config.lock().unwrap() = Some(config);
        *self.prefetch_result.lock().unwrap() = String::new();

        tracing::info!(
            "Honcho memory plugin initialized for session {}",
            session_id
        );
    }

    fn system_prompt_block(&self) -> String {
        let mode = self.recall_mode.lock().unwrap().clone();
        match mode.as_str() {
            "context" => {
                "# Honcho Memory\n\
                 Active (context-injection mode). Relevant user context is automatically \
                 injected before each turn. No memory tools are available."
                    .to_string()
            }
            "tools" => {
                "# Honcho Memory\n\
                 Active (tools-only mode). Use honcho_profile for a quick factual snapshot, \
                 honcho_search for raw excerpts, honcho_context for synthesized answers, \
                 honcho_conclude to save facts about the user."
                    .to_string()
            }
            _ => {
                "# Honcho Memory\n\
                 Active (hybrid mode). Relevant context is auto-injected AND memory tools are available. \
                 Use honcho_profile, honcho_search, honcho_context, honcho_conclude."
                    .to_string()
            }
        }
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return String::new();
        }

        let result = {
            let mut lock = self.prefetch_result.lock().unwrap();
            let value = lock.clone();
            lock.clear();
            value
        };
        if result.is_empty() {
            return String::new();
        }
        format!("## Honcho Context\n{}", result)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return;
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Honcho prefetch skipped: {}", e);
                return;
            }
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let out = Arc::clone(&self.prefetch_result);
        let query = trimmed.to_string();
        let max_tokens = config.context_tokens.unwrap_or(800).min(2000).max(64);
        let peer = Self::resolve_user_peer_id(&config, &session_id, &runtime_user_ids_from_env());
        std::thread::spawn(move || {
            match Self::context_query(&config, &session_id, &query, max_tokens, &peer) {
                Ok(v) => {
                    if let Some(text) = Self::extract_text_result(&v) {
                        if !text.trim().is_empty() {
                            *out.lock().unwrap() = text;
                        }
                    }
                }
                Err(e) => tracing::debug!("Honcho prefetch failed: {}", e),
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if self.config.lock().unwrap().is_none() {
            return;
        }
        let user_content = user_content.trim();
        let assistant_content = assistant_content.trim();
        if user_content.is_empty() || assistant_content.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Honcho sync skipped: {}", e);
                return;
            }
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let path = config.endpoint("messages", "/v1/sessions/{session}/messages");
        let payload = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "messages": [
                {"role":"user", "content": user_content.chars().take(8000).collect::<String>()},
                {"role":"assistant", "content": assistant_content.chars().take(8000).collect::<String>()}
            ]
        });
        let rendered_path = Self::apply_template(&path, "user", &session_id);
        std::thread::spawn(move || {
            if let Err(e) =
                Self::send_json(&config, Method::POST, &rendered_path, Some(&payload), None)
            {
                tracing::debug!("Honcho sync_turn failed: {}", e);
            }
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "context" {
            return Vec::new();
        }
        vec![
            profile_schema(),
            search_schema(),
            context_schema(),
            conclude_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => return json!({"error": e}).to_string(),
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let peer = Self::extract_peer(&config, args);

        match tool_name {
            "honcho_profile" => {
                let template = config.endpoint("profile", "/v1/peers/{peer}/card");
                let path = Self::apply_template(&template, &peer, &session_id);
                let maybe_card = args.get("card").and_then(|v| v.as_array()).cloned();
                let result = if let Some(card) = maybe_card {
                    let body = json!({
                        "workspace_id": config.workspace_id,
                        "session_id": session_id,
                        "peer": peer,
                        "card": card
                    });
                    Self::send_json(&config, Method::POST, &path, Some(&body), None)
                } else {
                    let query = vec![
                        ("workspace_id", config.workspace_id.clone()),
                        ("session_id", session_id.clone()),
                        ("peer", peer.clone()),
                    ];
                    Self::send_json(&config, Method::GET, &path, None, Some(&query))
                };
                match result {
                    Ok(v) => {
                        if let Some(card) = v
                            .get("card")
                            .or_else(|| v.get("result"))
                            .and_then(|c| c.as_array())
                        {
                            if card.is_empty() {
                                return json!({
                                    "result": card,
                                    "count": 0,
                                    "hint": honcho_empty_profile_hint(&peer)
                                })
                                .to_string();
                            }
                            return json!({"result": card, "count": card.len()}).to_string();
                        }
                        json!({"result": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho profile failed: {e}")}).to_string(),
                }
            }
            "honcho_search" => {
                let query_text = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query_text.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                let max_tokens = args
                    .get("max_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(800)
                    .clamp(64, 2000) as usize;
                let path = config.endpoint("search", "/v1/context/search");
                let body = json!({
                    "workspace_id": config.workspace_id,
                    "session_id": session_id,
                    "peer": peer,
                    "query": query_text,
                    "max_tokens": max_tokens
                });
                match Self::send_json(&config, Method::POST, &path, Some(&body), None) {
                    Ok(v) => {
                        let text = Self::extract_text_result(&v).unwrap_or_default();
                        if text.is_empty() {
                            return json!({"result": "No relevant context found.", "raw": v})
                                .to_string();
                        }
                        json!({"result": text, "raw": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho search failed: {e}")}).to_string(),
                }
            }
            "honcho_context" => {
                let query_text = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query_text.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                let max_tokens = config.context_tokens.unwrap_or(800).min(2000).max(64);
                match Self::context_query(&config, &session_id, query_text, max_tokens, &peer) {
                    Ok(v) => {
                        let text = Self::extract_text_result(&v).unwrap_or_default();
                        if text.is_empty() {
                            return json!({"result": "No result from Honcho.", "raw": v})
                                .to_string();
                        }
                        json!({"result": text, "raw": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho context failed: {e}")}).to_string(),
                }
            }
            "honcho_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let delete_id = args
                    .get("delete_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if conclusion.is_empty() && delete_id.is_empty() {
                    return json!({"error": "Provide conclusion or delete_id"}).to_string();
                }
                if !conclusion.is_empty() && !delete_id.is_empty() {
                    return json!({"error": "Provide either conclusion or delete_id, not both"})
                        .to_string();
                }
                let template = config.endpoint("conclude", "/v1/conclusions");
                if !delete_id.is_empty() {
                    let delete_path = format!("{}/{}", template.trim_end_matches('/'), delete_id);
                    let query = vec![
                        ("workspace_id", config.workspace_id.clone()),
                        ("session_id", session_id.clone()),
                        ("peer", peer.clone()),
                    ];
                    return match Self::send_json(
                        &config,
                        Method::DELETE,
                        &delete_path,
                        None,
                        Some(&query),
                    ) {
                        Ok(v) => json!({"result":"Conclusion deleted", "raw": v}).to_string(),
                        Err(e) => {
                            json!({"error": format!("Honcho delete failed: {e}")}).to_string()
                        }
                    };
                }
                let body = json!({
                    "workspace_id": config.workspace_id,
                    "session_id": session_id,
                    "peer": peer,
                    "conclusion": conclusion
                });
                match Self::send_json(&config, Method::POST, &template, Some(&body), None) {
                    Ok(v) => json!({"result": "Conclusion saved.", "raw": v}).to_string(),
                    Err(e) => json!({"error": format!("Honcho conclude failed: {e}")}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn on_session_end(&self, _messages: &[Value]) {
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let template = config.endpoint("flush", "/v1/sessions/{session}/flush");
        let path = Self::apply_template(&template, "user", &session_id);
        let body = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id
        });
        if let Err(e) = Self::send_json(&config, Method::POST, &path, Some(&body), None) {
            tracing::debug!("Honcho session end flush failed: {}", e);
        }
    }

    fn on_session_switch(&self, new_session_id: &str, parent_session_id: &str, reset: bool) {
        let new_session_id = new_session_id.trim();
        if new_session_id.is_empty() {
            return;
        }
        *self.session_key.lock().unwrap() = new_session_id.to_string();
        *self.turn_count.lock().unwrap() = 0;
        *self.prefetch_result.lock().unwrap() = String::new();
        tracing::debug!(
            "Honcho session switch: new_session={} parent={} reset={}",
            new_session_id,
            parent_session_id,
            reset
        );
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if action != "add" || target != "user" || content.trim().is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let peer = Self::resolve_user_peer_id(&config, &session_id, &runtime_user_ids_from_env());
        let template = config.endpoint("conclude", "/v1/conclusions");
        let body = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "peer": peer,
            "conclusion": content.trim(),
            "source": "memory_write_hook"
        });
        std::thread::spawn(move || {
            if let Err(e) = Self::send_json(&config, Method::POST, &template, Some(&body), None) {
                tracing::debug!("Honcho memory mirror failed: {}", e);
            }
        });
    }

    fn shutdown(&self) {
        tracing::debug!("Honcho memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Honcho API key or OAuth access token", "secret": true, "env_var": "HONCHO_API_KEY", "url": "https://app.honcho.dev"},
            {"key": "oauth", "description": "Honcho OAuth grant metadata stored under hosts.<profile>.oauth", "secret": true},
            {"key": "baseUrl", "description": "Honcho base URL (for self-hosted)"},
            {"key": "timeout", "description": "HTTP timeout seconds", "default": DEFAULT_TIMEOUT_SECS},
            {"key": "pinUserPeer", "description": "Pin gateway runtime users to peerName", "default": false},
            {"key": "userPeerAliases", "description": "Runtime user ID to Honcho peer ID map"},
            {"key": "runtimePeerPrefix", "description": "Prefix for unknown gateway runtime user peers", "default": ""},
            {"key": "endpoints", "description": "Optional endpoint path overrides"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
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
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

fn unique_nonempty(values: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        let candidate = value.trim();
        if !candidate.is_empty() && !out.iter().any(|existing| existing == candidate) {
            out.push(candidate.to_string());
        }
    }
    out
}

fn runtime_user_ids_from_args(args: &Value) -> Vec<String> {
    [
        "runtime_user_id",
        "runtimeUserId",
        "runtime_id",
        "runtimeId",
        "user_id",
        "userId",
        "runtime_user_id_alt",
        "runtimeUserIdAlt",
        "username",
    ]
    .into_iter()
    .filter_map(|key| args.get(key).and_then(Value::as_str))
    .map(ToString::to_string)
    .collect()
}

fn runtime_user_ids_from_env() -> Vec<String> {
    [
        "HERMES_RUNTIME_USER_ID",
        "HERMES_GATEWAY_USER_ID",
        "HERMES_SESSION_USER_ID",
        "HERMES_USER_ID",
        "HERMES_USER",
    ]
    .into_iter()
    .filter_map(|key| std::env::var(key).ok())
    .collect()
}

fn session_key_fallback_peer_id(key: &str) -> String {
    let (channel, chat_id) = key.split_once(':').unwrap_or(("default", key));
    sanitize_id(&format!("user-{channel}-{chat_id}"))
}

fn explicit_user_peer_ids(config: &HonchoConfig) -> Vec<String> {
    let mut explicit = Vec::new();
    if let Some(peer) = config.peer_name.as_deref() {
        if !peer.trim().is_empty() {
            explicit.push(sanitize_id(peer.trim()));
        }
    }
    for alias in config.user_peer_aliases.values() {
        if !alias.trim().is_empty() {
            explicit.push(sanitize_id(alias.trim()));
        }
    }
    unique_nonempty(&explicit)
}

fn generated_runtime_peer_id(config: &HonchoConfig, prefix: &str, runtime_id: &str) -> String {
    let raw_peer_id = format!("{prefix}{runtime_id}");
    let sanitized_peer_id = sanitize_id(&raw_peer_id);
    let explicit_ids = explicit_user_peer_ids(config);
    if sanitized_peer_id != raw_peer_id || explicit_ids.contains(&sanitized_peer_id) {
        let digest = Sha256::digest(raw_peer_id.as_bytes());
        let hex = format!("{digest:x}");
        for len in PEER_ID_HASH_ESCALATION_LENGTHS {
            let candidate = format!("{sanitized_peer_id}-{}", &hex[..*len]);
            if !explicit_ids.contains(&candidate) {
                return candidate;
            }
        }
        return format!("{sanitized_peer_id}-{hex}");
    }
    sanitized_peer_id
}

include!("honcho/profile_tests.rs");
