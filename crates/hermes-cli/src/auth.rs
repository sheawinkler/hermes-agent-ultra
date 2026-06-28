use std::collections::{BTreeMap, HashSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use base64::engine::general_purpose::{
    URL_SAFE as BASE64_URL_SAFE, URL_SAFE_NO_PAD as BASE64_URL_SAFE_NO_PAD,
};
use base64::Engine as _;
use chrono::Utc;
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

include!("auth/types.rs");

fn auth_json_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_AUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
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

fn write_owner_only_atomic(path: &Path, raw: &str) -> Result<(), AgentError> {
    let parent = path.parent().ok_or_else(|| {
        AgentError::Io(format!(
            "credential path {} has no parent directory",
            path.display()
        ))
    })?;
    std::fs::create_dir_all(parent)
        .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials.json");
    let nonce = rand::random::<u64>();
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}.{}", std::process::id(), nonce));

    let result = (|| -> Result<(), AgentError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&tmp_path)
            .map_err(|e| AgentError::Io(format!("create {}: {}", tmp_path.display(), e)))?;
        file.write_all(raw.as_bytes())
            .map_err(|e| AgentError::Io(format!("write {}: {}", tmp_path.display(), e)))?;
        file.sync_all()
            .map_err(|e| AgentError::Io(format!("fsync {}: {}", tmp_path.display(), e)))?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| AgentError::Io(format!("set permissions on {}: {}", tmp_path.display(), e)),
            )?;
        }
        std::fs::rename(&tmp_path, path).map_err(|e| {
            AgentError::Io(format!(
                "rename {} -> {}: {}",
                tmp_path.display(),
                path.display(),
                e
            ))
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(
                |e| AgentError::Io(format!("set permissions on {}: {}", path.display(), e)),
            )?;
        }
        let _ = std::fs::File::open(parent).and_then(|dir| dir.sync_all());
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&tmp_path);
    }
    result
}

fn save_auth_store(path: &Path, store: &AuthStore) -> Result<(), AgentError> {
    let mut raw = serde_json::to_string_pretty(store)
        .map_err(|e| AgentError::Config(format!("serialize auth store: {}", e)))?;
    raw.push('\n');
    write_owner_only_atomic(path, &raw)
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
    if let Some(found) = store.providers.get(&provider).cloned() {
        return Ok(Some(found));
    }

    // Compatibility with upstream profile behavior: if the active auth store
    // does not include the provider, scan global/root auth stores before
    // reporting "missing".
    for candidate in hermes_auth_store_discovery_paths() {
        if candidate == path {
            continue;
        }
        if let Some(found) = read_provider_auth_state_from_store_path(&candidate, &provider) {
            return Ok(Some(found));
        }
    }
    Ok(None)
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

fn existing_unique_paths(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|path| path.is_file())
        .filter(|path| seen.insert(path.clone()))
        .collect()
}

fn openai_oauth_discovery_paths(extra_env_vars: &[&str]) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut env_names = vec![
        "HERMES_OPENAI_OAUTH_FILE",
        "HERMES_CODEX_AUTH_FILE",
        "CODEX_AUTH_FILE",
    ];
    env_names.extend_from_slice(extra_env_vars);
    for env_name in env_names {
        if let Ok(path) = std::env::var(env_name) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".codex").join("auth.json"));
        candidates.push(home.join(".pi").join("agent").join("auth.json"));
    }
    existing_unique_paths(candidates)
}

fn hermes_auth_store_discovery_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for env_name in ["HERMES_AUTH_FILE"] {
        if let Ok(path) = std::env::var(env_name) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    candidates.push(auth_json_path());
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".hermes").join("auth.json"));
        candidates.push(home.join(".hermes-agent-ultra").join("auth.json"));
    }
    existing_unique_paths(candidates)
}

fn decode_jwt_exp_seconds(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let decoded = BASE64_URL_SAFE_NO_PAD.decode(payload).ok()?;
    let json: Value = serde_json::from_slice(&decoded).ok()?;
    json.get("exp").and_then(value_as_i64)
}

fn load_openai_oauth_import_from_path(path: &Path, base_url: &str) -> Option<OpenAiOAuthImport> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: ExternalOpenAiAuthFile = serde_json::from_str(&raw).ok()?;
    let tokens = parsed.tokens?;
    let access_token = tokens
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let now_secs = Utc::now().timestamp();
    let expires_in = tokens
        .id_token
        .as_deref()
        .and_then(|jwt| decode_jwt_exp_seconds(jwt))
        .map(|exp| exp - now_secs)
        .filter(|remaining| *remaining > 0);
    let state = CodexAuthState {
        tokens: CodexTokens {
            access_token,
            refresh_token,
            expires_in,
        },
        base_url: base_url.to_string(),
        last_refresh: parsed
            .last_refresh
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Utc::now().to_rfc3339()),
        auth_mode: parsed.auth_mode.or_else(|| Some("chatgpt".to_string())),
        source: Some("discovered_external".to_string()),
    };
    Some(OpenAiOAuthImport {
        state,
        source_path: path.to_path_buf(),
    })
}

pub fn discover_existing_openai_oauth() -> Result<Option<OpenAiOAuthImport>, AgentError> {
    for path in openai_oauth_discovery_paths(&[]) {
        if let Some(imported) = load_openai_oauth_import_from_path(&path, DEFAULT_CODEX_BASE_URL) {
            return Ok(Some(imported));
        }
    }
    Ok(None)
}

fn read_provider_auth_state_from_store_path(path: &Path, provider: &str) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    parsed
        .get("providers")
        .and_then(Value::as_object)
        .and_then(|providers| providers.get(provider))
        .cloned()
}

fn load_codex_oauth_import_from_store(path: &Path) -> Option<OpenAiOAuthImport> {
    let value = read_provider_auth_state_from_store_path(path, "openai-codex")?;
    let mut state: CodexAuthState = serde_json::from_value(value).ok()?;
    if state.tokens.access_token.trim().is_empty() {
        return None;
    }
    if state.base_url.trim().is_empty() {
        state.base_url = DEFAULT_CODEX_BASE_URL.to_string();
    }
    if state.last_refresh.trim().is_empty() {
        state.last_refresh = Utc::now().to_rfc3339();
    }
    if state
        .auth_mode
        .as_deref()
        .map(str::trim)
        .is_none_or(|v| v.is_empty())
    {
        state.auth_mode = Some("chatgpt".to_string());
    }
    if state
        .source
        .as_deref()
        .map(str::trim)
        .is_none_or(|v| v.is_empty())
    {
        state.source = Some("discovered_auth_store".to_string());
    }
    Some(OpenAiOAuthImport {
        state,
        source_path: path.to_path_buf(),
    })
}

pub fn discover_existing_openai_codex_oauth() -> Result<Option<OpenAiOAuthImport>, AgentError> {
    for path in openai_oauth_discovery_paths(&["HERMES_OPENAI_CODEX_OAUTH_FILE"]) {
        if let Some(imported) = load_openai_oauth_import_from_path(&path, DEFAULT_CODEX_BASE_URL) {
            return Ok(Some(imported));
        }
    }
    for path in hermes_auth_store_discovery_paths() {
        if let Some(imported) = load_codex_oauth_import_from_store(&path) {
            return Ok(Some(imported));
        }
    }
    Ok(None)
}

fn anthropic_oauth_discovery_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for env_name in [
        "HERMES_ANTHROPIC_OAUTH_FILE",
        "CLAUDE_CODE_CREDENTIALS_FILE",
        "HERMES_CLAUDE_CREDENTIALS_FILE",
    ] {
        if let Ok(path) = std::env::var(env_name) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    if let Ok(claude_config_dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        let trimmed = claude_config_dir.trim();
        if !trimmed.is_empty() {
            candidates.push(PathBuf::from(trimmed).join(".credentials.json"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".claude").join(".credentials.json"));
        candidates.push(home.join(".hermes").join(".anthropic_oauth.json"));
    }
    existing_unique_paths(candidates)
}

fn normalize_unix_millis(timestamp: i64) -> i64 {
    if timestamp > 0 && timestamp < 10_000_000_000 {
        timestamp.saturating_mul(1000)
    } else {
        timestamp
    }
}

fn anthropic_keychain_source_path() -> PathBuf {
    PathBuf::from(format!(
        "macos-keychain://{}",
        ANTHROPIC_CLAUDE_CODE_KEYCHAIN_SERVICE
    ))
}

fn load_anthropic_oauth_import_from_claude_credentials_json(
    raw: &str,
    source_path: PathBuf,
    source: &str,
) -> Option<AnthropicOAuthImport> {
    let parsed = serde_json::from_str::<ExternalClaudeCredentialsFile>(raw).ok()?;
    let oauth = parsed.claude_ai_oauth?;
    let access_token = oauth
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let refresh_token = oauth
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = oauth.expires_at_ms.map(normalize_unix_millis);
    Some(AnthropicOAuthImport {
        state: AnthropicOAuthState {
            access_token,
            refresh_token,
            expires_at_ms,
        },
        source_path,
        source: source.to_string(),
    })
}

fn load_anthropic_oauth_import_from_keychain_payload(raw: &str) -> Option<AnthropicOAuthImport> {
    load_anthropic_oauth_import_from_claude_credentials_json(
        raw,
        anthropic_keychain_source_path(),
        "macos_keychain",
    )
}

fn run_anthropic_keychain_read() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }

    let mut child = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            ANTHROPIC_CLAUDE_CODE_KEYCHAIN_SERVICE,
            "-w",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let started = Instant::now();
    loop {
        if started.elapsed() >= ANTHROPIC_CLAUDE_CODE_KEYCHAIN_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait().ok()? {
            Some(status) => {
                let output = child.wait_with_output().ok()?;
                if !status.success() {
                    return None;
                }
                let stdout = String::from_utf8(output.stdout).ok()?;
                let trimmed = stdout.trim();
                if trimmed.is_empty() {
                    return None;
                }
                return Some(trimmed.to_string());
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn load_anthropic_oauth_import_from_keychain() -> Option<AnthropicOAuthImport> {
    let raw = run_anthropic_keychain_read()?;
    load_anthropic_oauth_import_from_keychain_payload(&raw)
}

fn load_anthropic_oauth_import_from_path(path: &Path) -> Option<AnthropicOAuthImport> {
    let raw = std::fs::read_to_string(path).ok()?;

    if let Some(imported) = load_anthropic_oauth_import_from_claude_credentials_json(
        &raw,
        path.to_path_buf(),
        "claude_code_credentials_file",
    ) {
        return Some(imported);
    }

    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let object = parsed.as_object()?;
    let access_token = object
        .get("access_token")
        .or_else(|| object.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .or_else(|| object.get("refreshToken"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = object
        .get("expires_at_ms")
        .or_else(|| object.get("expiresAt"))
        .or_else(|| object.get("expires"))
        .and_then(value_as_i64)
        .map(normalize_unix_millis);
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("discovered_external")
        .to_string();
    Some(AnthropicOAuthImport {
        state: AnthropicOAuthState {
            access_token,
            refresh_token,
            expires_at_ms,
        },
        source_path: path.to_path_buf(),
        source,
    })
}

fn load_anthropic_oauth_import_from_store(path: &Path) -> Option<AnthropicOAuthImport> {
    let value = read_provider_auth_state_from_store_path(path, "anthropic")?;
    let object = value.as_object()?;
    let access_token = object
        .get("access_token")
        .or_else(|| object.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = object
        .get("expires_at_ms")
        .or_else(|| object.get("expires"))
        .and_then(value_as_i64)
        .map(normalize_unix_millis);
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("hermes_pkce")
        .to_string();
    Some(AnthropicOAuthImport {
        state: AnthropicOAuthState {
            access_token,
            refresh_token,
            expires_at_ms,
        },
        source_path: path.to_path_buf(),
        source,
    })
}

pub fn discover_existing_anthropic_oauth() -> Result<Option<AnthropicOAuthImport>, AgentError> {
    discover_existing_anthropic_oauth_with_keychain(load_anthropic_oauth_import_from_keychain())
}

fn discover_existing_anthropic_oauth_with_keychain(
    keychain_import: Option<AnthropicOAuthImport>,
) -> Result<Option<AnthropicOAuthImport>, AgentError> {
    if let Some(imported) = keychain_import {
        return Ok(Some(imported));
    }
    for path in anthropic_oauth_discovery_paths() {
        if let Some(imported) = load_anthropic_oauth_import_from_path(&path) {
            return Ok(Some(imported));
        }
    }
    for path in hermes_auth_store_discovery_paths() {
        if let Some(imported) = load_anthropic_oauth_import_from_store(&path) {
            return Ok(Some(imported));
        }
    }
    Ok(None)
}

fn nous_oauth_discovery_paths() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for env_name in ["HERMES_NOUS_OAUTH_FILE"] {
        if let Ok(path) = std::env::var(env_name) {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".hermes").join(".nous_oauth.json"));
    }
    existing_unique_paths(candidates)
}

fn parse_nous_oauth_state(value: Value) -> Option<NousAuthState> {
    let state: NousAuthState = serde_json::from_value(value).ok()?;
    if state.runtime_api_key().is_none() {
        return None;
    }
    Some(state)
}

fn nous_auth_state_is_runtime_viable(state: &NousAuthState) -> bool {
    if state
        .refresh_token
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    nous_invoke_jwt_status(
        &state.access_token,
        state.scope.as_deref(),
        state.expires_at.as_deref(),
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
    )
    .is_none()
}

pub fn read_nous_auth_state() -> Result<Option<NousAuthState>, AgentError> {
    if let Some(raw_state) = read_provider_auth_state("nous")? {
        if let Some(state) = parse_nous_oauth_state(raw_state) {
            return Ok(Some(state));
        }
    }
    Ok(discover_existing_nous_oauth()?.map(|imported| imported.state))
}

pub fn read_valid_nous_auth_state() -> Result<Option<NousAuthState>, AgentError> {
    Ok(read_nous_auth_state()?.and_then(|state| {
        if nous_auth_state_is_runtime_viable(&state) {
            Some(state)
        } else {
            None
        }
    }))
}

fn load_nous_oauth_import_from_path(path: &Path) -> Option<NousOAuthImport> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let state = parse_nous_oauth_state(parsed)?;
    Some(NousOAuthImport {
        state,
        source_path: path.to_path_buf(),
    })
}

fn load_nous_oauth_import_from_store(path: &Path) -> Option<NousOAuthImport> {
    let value = read_provider_auth_state_from_store_path(path, "nous")?;
    let state = parse_nous_oauth_state(value)?;
    Some(NousOAuthImport {
        state,
        source_path: path.to_path_buf(),
    })
}

pub fn discover_existing_nous_oauth() -> Result<Option<NousOAuthImport>, AgentError> {
    for path in nous_oauth_discovery_paths() {
        if let Some(imported) = load_nous_oauth_import_from_path(&path) {
            return Ok(Some(imported));
        }
    }
    for path in hermes_auth_store_discovery_paths() {
        if let Some(imported) = load_nous_oauth_import_from_store(&path) {
            return Ok(Some(imported));
        }
    }
    Ok(None)
}

fn parse_iso_timestamp_utc(raw: &str) -> Option<chrono::DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = if trimmed.ends_with('Z') {
        format!("{}+00:00", &trimmed[..trimmed.len() - 1])
    } else {
        trimmed.to_string()
    };
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&normalized) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S") {
        return Some(chrono::DateTime::<Utc>::from_naive_utc_and_offset(
            naive, Utc,
        ));
    }
    None
}

fn timestamp_is_expiring(expires_at: Option<&str>, skew_seconds: i64) -> bool {
    let Some(raw) = expires_at else {
        return true;
    };
    let Some(parsed) = parse_iso_timestamp_utc(raw) else {
        return true;
    };
    let remaining = (parsed - Utc::now()).num_seconds();
    remaining <= skew_seconds.max(0)
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    let payload = token.trim().split('.').nth(1)?;
    let decoded = BASE64_URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| BASE64_URL_SAFE.decode(payload))
        .ok()?;
    serde_json::from_slice(&decoded).ok()
}

fn collect_scope_values(scopes: &mut HashSet<String>, value: Option<&Value>) {
    match value {
        Some(Value::String(raw)) => {
            for part in raw.replace(',', " ").split_whitespace() {
                let cleaned = part.trim();
                if !cleaned.is_empty() {
                    scopes.insert(cleaned.to_string());
                }
            }
        }
        Some(Value::Array(values)) => {
            for item in values {
                collect_scope_values(scopes, Some(item));
            }
        }
        _ => {}
    }
}

fn collect_scope_string(scopes: &mut HashSet<String>, value: Option<&str>) {
    if let Some(raw) = value {
        collect_scope_values(scopes, Some(&Value::String(raw.to_string())));
    }
}

fn nous_invoke_jwt_status(
    token: &str,
    scope: Option<&str>,
    expires_at: Option<&str>,
    min_ttl_seconds: i64,
) -> Option<&'static str> {
    let Some(claims) = decode_jwt_claims(token) else {
        return Some("access_token_not_jwt");
    };
    let mut scopes = HashSet::new();
    collect_scope_string(&mut scopes, scope);
    collect_scope_values(&mut scopes, claims.get("scope"));
    collect_scope_values(&mut scopes, claims.get("scp"));
    if !scopes.contains(NOUS_INFERENCE_INVOKE_SCOPE) {
        return Some("missing_inference_invoke_scope");
    }

    let skew = min_ttl_seconds.max(0);
    if let Some(exp) = claims.get("exp").and_then(Value::as_f64) {
        if exp <= (Utc::now().timestamp() + skew) as f64 {
            return Some("invoke_jwt_expiring");
        }
        return None;
    }
    if timestamp_is_expiring(expires_at, skew) {
        return Some("invoke_jwt_expiry_unknown_or_expiring");
    }
    None
}

#[cfg(test)]
fn nous_invoke_jwt_is_usable(
    token: &str,
    scope: Option<&str>,
    expires_at: Option<&str>,
    min_ttl_seconds: i64,
) -> bool {
    if token.trim().is_empty() {
        return false;
    }
    nous_invoke_jwt_status(token, scope, expires_at, min_ttl_seconds).is_none()
}

fn nous_jwt_expires_at(token: &str, fallback_expires_at: Option<&str>) -> Option<String> {
    if let Some(claims) = decode_jwt_claims(token) {
        if let Some(exp) = claims.get("exp").and_then(Value::as_i64) {
            if let Some(dt) = chrono::DateTime::<Utc>::from_timestamp(exp, 0) {
                return Some(dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            }
        }
    }
    fallback_expires_at
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

fn set_nous_agent_key_from_invoke_jwt(state: &mut NousAuthState) {
    let access_token = state.access_token.trim().to_string();
    if access_token.is_empty() {
        return;
    }
    let obtained_at = if state.agent_key.as_deref() == Some(access_token.as_str()) {
        state
            .agent_key_obtained_at
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    } else {
        None
    }
    .unwrap_or_else(|| Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));

    let expires_at = nous_jwt_expires_at(&access_token, state.expires_at.as_deref());
    let expires_in = expires_at
        .as_deref()
        .and_then(parse_iso_timestamp_utc)
        .map(|dt| (dt - Utc::now()).num_seconds().max(0))
        .or(state.expires_in);

    if let Some(value) = expires_at.clone() {
        state.expires_at = Some(value);
        state.expires_in = expires_in;
    }
    state.agent_key = Some(access_token);
    state.agent_key_id = None;
    state.agent_key_expires_at = expires_at;
    state.agent_key_expires_in = expires_in;
    state.agent_key_reused = Some(false);
    state.agent_key_obtained_at = Some(obtained_at);
}

pub fn nous_auth_state_from_runtime_token(
    access_token: &str,
    refresh_token: Option<String>,
    token_type: Option<&str>,
    scope: Option<String>,
    expires_at: Option<String>,
) -> Result<NousAuthState, AgentError> {
    let access_token = access_token.trim().to_string();
    let scope = scope
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| DEFAULT_NOUS_SCOPE.to_string());
    let now = Utc::now();
    let mut state = NousAuthState {
        portal_base_url: env_or_default("NOUS_PORTAL_BASE_URL", DEFAULT_NOUS_PORTAL_URL)
            .trim_end_matches('/')
            .to_string(),
        inference_base_url: env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_NOUS_INFERENCE_URL)
            .trim_end_matches('/')
            .to_string(),
        client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
        scope: Some(scope),
        token_type: token_type
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("Bearer")
            .to_string(),
        access_token,
        refresh_token,
        obtained_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at,
        expires_in: None,
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    };
    assert_nous_invoke_jwt_usable(&state, None, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)?;
    set_nous_agent_key_from_invoke_jwt(&mut state);
    Ok(state)
}

fn assert_nous_invoke_jwt_usable(
    state: &NousAuthState,
    access_token: Option<&str>,
    min_ttl_seconds: i64,
) -> Result<(), AgentError> {
    let token = access_token.unwrap_or(state.access_token.as_str()).trim();
    let reason = if token.is_empty() {
        Some("access_token_not_jwt")
    } else {
        nous_invoke_jwt_status(
            token,
            state.scope.as_deref(),
            state.expires_at.as_deref(),
            min_ttl_seconds,
        )
    };
    if let Some(reason) = reason {
        return Err(AgentError::AuthFailed(format!(
            "Nous Portal access token is not a usable inference JWT ({reason}). Re-authenticate with: hermes auth add nous"
        )));
    }
    Ok(())
}

async fn refresh_nous_access_token(
    state: &mut NousAuthState,
    client: &reqwest::Client,
) -> Result<(), AgentError> {
    let refresh_token = state
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Session expired and no refresh token is available for Nous OAuth.".into(),
            )
        })?
        .to_string();
    let client_id = if state.client_id.trim().is_empty() {
        DEFAULT_NOUS_CLIENT_ID.to_string()
    } else {
        state.client_id.trim().to_string()
    };
    let portal_base_url = state.portal_base_url.trim_end_matches('/').to_string();

    let response = client
        .post(format!("{portal_base_url}/api/oauth/token"))
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Nous OAuth refresh failed: {}", e)))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Nous OAuth refresh read failed: {}", e)))?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Nous OAuth refresh failed ({}): {}",
            status, detail
        )));
    }
    let payload: NousTokenResponse = serde_json::from_str(&body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid Nous refresh response: {}", e)))?;

    let access_token = payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Nous OAuth refresh response missing access_token".into())
        })?
        .to_string();

    let now = Utc::now();
    let access_expires_in = payload.expires_in.filter(|v| *v > 0);
    let access_expires_at = access_expires_in.map(|secs| {
        (now + chrono::Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    state.access_token = access_token;
    state.refresh_token = payload.refresh_token.or_else(|| Some(refresh_token));
    state.token_type = payload.token_type.unwrap_or_else(|| "Bearer".to_string());
    state.scope = payload.scope.or_else(|| state.scope.clone());
    state.obtained_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    state.expires_in = access_expires_in;
    state.expires_at = access_expires_at;
    if let Some(inference_url) = payload
        .inference_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        state.inference_base_url = inference_url.trim_end_matches('/').to_string();
    }
    state.portal_base_url = portal_base_url;
    state.client_id = client_id;
    Ok(())
}

pub async fn resolve_nous_runtime_credentials(
    force_refresh: bool,
    refresh_if_expiring: bool,
    refresh_skew_seconds: i64,
    _min_key_ttl_seconds: u32,
) -> Result<NousRuntimeCredentials, AgentError> {
    let mut state = read_nous_auth_state()?.ok_or_else(|| {
        AgentError::AuthFailed("Hermes is not logged into Nous Portal. Run `hermes portal`.".into())
    })?;

    if state.portal_base_url.trim().is_empty() {
        state.portal_base_url = env_or_default("NOUS_PORTAL_BASE_URL", DEFAULT_NOUS_PORTAL_URL)
            .trim_end_matches('/')
            .to_string();
    }
    if state.inference_base_url.trim().is_empty() {
        state.inference_base_url =
            env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_NOUS_INFERENCE_URL)
                .trim_end_matches('/')
                .to_string();
    }
    if state.client_id.trim().is_empty() {
        state.client_id = DEFAULT_NOUS_CLIENT_ID.to_string();
    }
    if state
        .scope
        .as_deref()
        .map(str::trim)
        .is_none_or(|v| v.is_empty())
    {
        state.scope = Some(DEFAULT_NOUS_SCOPE.to_string());
    }

    let timeout = default_http_timeout_seconds(15.0, 15.0);
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Nous OAuth client: {}", e)))?;

    let invoke_jwt_status = nous_invoke_jwt_status(
        &state.access_token,
        state.scope.as_deref(),
        state.expires_at.as_deref(),
        refresh_skew_seconds,
    );
    let should_refresh_access =
        force_refresh || (refresh_if_expiring && invoke_jwt_status.is_some());
    if should_refresh_access {
        if state
            .refresh_token
            .as_deref()
            .map(str::trim)
            .is_none_or(|v| v.is_empty())
        {
            let reason = if force_refresh {
                "force_refresh"
            } else {
                invoke_jwt_status.unwrap_or("access_unusable")
            };
            return Err(AgentError::AuthFailed(format!(
                "Nous Portal access token is not a usable inference JWT ({reason}) and no refresh token is available. Re-authenticate with: hermes auth add nous"
            )));
        }
        refresh_nous_access_token(&mut state, &client).await?;
    }
    assert_nous_invoke_jwt_usable(&state, None, refresh_skew_seconds)?;
    set_nous_agent_key_from_invoke_jwt(&mut state);

    state.inference_base_url = state.inference_base_url.trim_end_matches('/').to_string();
    let _ = save_nous_auth_state(&state)?;

    let api_key = state.runtime_api_key().ok_or_else(|| {
        AgentError::AuthFailed(
            "Failed to resolve a Nous runtime API key. Re-run `hermes portal`.".into(),
        )
    })?;
    let expires_at = state
        .agent_key_expires_at
        .clone()
        .or_else(|| state.expires_at.clone());
    let expires_in = expires_at
        .as_deref()
        .and_then(parse_iso_timestamp_utc)
        .map(|dt| (dt - Utc::now()).num_seconds().max(0))
        .or(state.agent_key_expires_in)
        .or(state.expires_in);

    Ok(NousRuntimeCredentials {
        provider: "nous".to_string(),
        base_url: state.inference_base_url,
        api_key,
        key_id: state.agent_key_id,
        expires_at,
        expires_in,
        source: NOUS_AUTH_PATH_INVOKE_JWT.to_string(),
        refresh_token: state.refresh_token,
        token_type: state.token_type,
        scope: state.scope,
    })
}

include!("auth/provider_oauth.rs");

#[cfg(test)]
mod tests;
