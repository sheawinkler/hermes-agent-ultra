use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use hermes_core::AgentError;
use hermes_intelligence::models_dev::{default_client, ModelsDevClient};
use hmac::{Hmac, Mac};
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::providers::provider_capability_for;
const NOUS_DEFAULT_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
const PROVIDER_CATALOG_CACHE_VERSION: u32 = 1;

const CURATED_PROVIDER_MODELS: &[(&str, &[&str])] = &[
    (
        "openrouter",
        &[
            "openai/gpt-5.5",
            "openai/gpt-5.5-pro",
            "moonshotai/kimi-k2.6",
            "anthropic/claude-opus-4.7",
            "openai/gpt-5.4",
        ],
    ),
    (
        "nous",
        &[
            "nousresearch/hermes-3-llama-3.1-405b",
            "nousresearch/hermes-4-405b",
            "nousresearch/hermes-4-70b",
            "moonshotai/kimi-k2.6",
            "xiaomi/mimo-v2.5-pro",
            "anthropic/claude-sonnet-4.5",
        ],
    ),
    (
        "openai",
        &[
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-5.4",
            "gpt-5.4-mini",
        ],
    ),
    (
        "anthropic",
        &[
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
        ],
    ),
    (
        "opencode-go",
        &[
            "kimi-k2.6",
            "kimi-k2.5",
            "glm-5.1",
            "mimo-v2-pro",
            "mimo-v2-omni",
            "qwen3.6-plus",
            "qwen3.5-plus",
        ],
    ),
    (
        "opencode-zen",
        &[
            "kimi-k2.5",
            "gpt-5.4-pro",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "gemini-3-pro",
            "kimi-k2-thinking",
        ],
    ),
    (
        "deepseek",
        &[
            "deepseek-chat",
            "deepseek-reasoner",
            "deepseek-v4-pro",
            "deepseek-v4-flash",
            "deepseek-v3.2",
            "deepseek-ai/deepseek-v3-2",
        ],
    ),
    (
        "kilocode",
        &[
            "anthropic/claude-opus-4.6",
            "openai/gpt-5.4",
            "google/gemini-3-flash-preview",
        ],
    ),
    ("fireworks", &["accounts/fireworks-ai/qwq-32b"]),
    (
        "mistral",
        &["mistral-small", "mistral-large", "pixtral-large-latest"],
    ),
    ("togetherai", &["meta-llama/Llama-4-Maverick-17B"]),
    ("cohere", &["command-a", "command-r7b"]),
    (
        "perplexity",
        &["sonar-large-online", "pplx-70b-online", "pplx-7b-online"],
    ),
    ("groq", &["llama-3.3-70b-versatile", "llama-3.1-8b-instant"]),
    (
        "nvidia",
        &[
            "nvidia/llama-3.3-nemotron-super-49b-v1.5",
            "nvidia/nemotron-3-super-120b-a12b",
        ],
    ),
    (
        "huggingface",
        &[
            "moonshotai/Kimi-K2.5",
            "Qwen/Qwen3.5-397B-A17B",
            "deepseek-ai/DeepSeek-V3.2",
        ],
    ),
    ("zai", &["glm-5.1", "glm-5.0", "glm-4.5-flash"]),
    (
        "gemini",
        &[
            "gemini-3.1-pro-preview",
            "gemini-3-flash-preview",
            "gemini-3.1-flash-lite-preview",
        ],
    ),
    (
        "google",
        &["gemini-3.1-pro-preview", "gemini-3-flash-preview"],
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCatalogEntry {
    pub provider: String,
    pub models: Vec<String>,
    pub total_models: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogCacheStatus {
    pub verified: bool,
    pub age_secs: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderCatalogCacheRecord {
    version: u32,
    provider: String,
    generated_at: String,
    models: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ProviderCatalogCacheSignature {
    version: u32,
    algorithm: String,
    key_id: String,
    payload_sha256: String,
    signature_hex: String,
    signed_at: String,
}

fn catalog_cache_ttl_secs() -> i64 {
    std::env::var("HERMES_PROVIDER_MODEL_CACHE_TTL_SECS")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(30 * 60)
}

fn provider_catalog_cache_dir() -> PathBuf {
    hermes_config::hermes_home()
        .join("cache")
        .join("provider-model-catalog")
}

fn provider_catalog_cache_path(provider: &str) -> PathBuf {
    provider_catalog_cache_dir().join(format!("{}.json", provider.trim().to_ascii_lowercase()))
}

fn provider_catalog_signature_path(provider: &str) -> PathBuf {
    provider_catalog_cache_dir().join(format!("{}.sig.json", provider.trim().to_ascii_lowercase()))
}

fn parse_hex_key(raw: &str) -> Option<Vec<u8>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let decoded = hex::decode(trimmed).ok()?;
    if decoded.len() < 16 {
        return None;
    }
    Some(decoded)
}

fn ensure_provenance_key() -> Option<Vec<u8>> {
    if let Ok(raw) = std::env::var("HERMES_PROVENANCE_SIGNING_KEY") {
        if let Some(key) = parse_hex_key(&raw) {
            return Some(key);
        }
    }
    let key_path = hermes_config::hermes_home()
        .join("auth")
        .join("provenance.key");
    if let Ok(raw) = std::fs::read_to_string(&key_path) {
        if let Some(key) = parse_hex_key(&raw) {
            return Some(key);
        }
    }

    let parent = key_path.parent()?;
    if std::fs::create_dir_all(parent).is_err() {
        return None;
    }
    let mut key = [0u8; 32];
    OsRng.fill_bytes(&mut key);
    let encoded = hex::encode(key);
    if std::fs::write(&key_path, format!("{encoded}\n")).is_err() {
        return None;
    }
    Some(key.to_vec())
}

fn cache_key_id(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    let hexed = hex::encode(digest);
    format!("k-{}", &hexed[..16])
}

fn sign_cache_payload(bytes: &[u8]) -> Option<ProviderCatalogCacheSignature> {
    let key = ensure_provenance_key()?;
    let payload_sha = hex::encode(Sha256::digest(bytes));
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(payload_sha.as_bytes());
    let signature_hex = hex::encode(mac.finalize().into_bytes());
    Some(ProviderCatalogCacheSignature {
        version: PROVIDER_CATALOG_CACHE_VERSION,
        algorithm: "hmac-sha256".to_string(),
        key_id: cache_key_id(&key),
        payload_sha256: payload_sha,
        signature_hex,
        signed_at: Utc::now().to_rfc3339(),
    })
}

fn verify_cache_payload(bytes: &[u8], signature: &ProviderCatalogCacheSignature) -> Option<bool> {
    let key = ensure_provenance_key()?;
    let payload_sha = hex::encode(Sha256::digest(bytes));
    if payload_sha != signature.payload_sha256 {
        return Some(false);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(payload_sha.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());
    Some(expected == signature.signature_hex)
}

fn provider_catalog_cache_status(provider: &str) -> Option<CatalogCacheStatus> {
    let path = provider_catalog_cache_path(provider);
    let sig_path = provider_catalog_signature_path(provider);
    let payload_bytes = std::fs::read(path).ok()?;
    let payload: ProviderCatalogCacheRecord = serde_json::from_slice(&payload_bytes).ok()?;
    let sig_raw = std::fs::read_to_string(sig_path).ok()?;
    let signature: ProviderCatalogCacheSignature = serde_json::from_str(&sig_raw).ok()?;
    let verified = verify_cache_payload(&payload_bytes, &signature).unwrap_or(false);
    let age_secs = DateTime::parse_from_rfc3339(&payload.generated_at)
        .ok()
        .map(|ts| Utc::now().signed_duration_since(ts.with_timezone(&Utc)))
        .and_then(|delta| u64::try_from(delta.num_seconds().max(0)).ok());
    Some(CatalogCacheStatus { verified, age_secs })
}

pub fn cached_provider_catalog_status(provider: &str) -> Option<CatalogCacheStatus> {
    provider_catalog_cache_status(provider)
}

fn load_provider_catalog_cache(provider: &str) -> Option<Vec<String>> {
    let ttl = catalog_cache_ttl_secs();
    let status = provider_catalog_cache_status(provider)?;
    if !status.verified {
        return None;
    }
    if let Some(age) = status.age_secs {
        if age > ttl as u64 {
            return None;
        }
    }
    let path = provider_catalog_cache_path(provider);
    let payload_raw = std::fs::read_to_string(path).ok()?;
    let payload: ProviderCatalogCacheRecord = serde_json::from_str(&payload_raw).ok()?;
    if payload.version != PROVIDER_CATALOG_CACHE_VERSION {
        return None;
    }
    let normalized = provider.trim().to_ascii_lowercase();
    if payload.provider.trim().to_ascii_lowercase() != normalized {
        return None;
    }
    Some(payload.models)
}

fn persist_provider_catalog_cache(provider: &str, models: &[String]) {
    let record = ProviderCatalogCacheRecord {
        version: PROVIDER_CATALOG_CACHE_VERSION,
        provider: provider.trim().to_ascii_lowercase(),
        generated_at: Utc::now().to_rfc3339(),
        models: models.to_vec(),
    };
    let Ok(payload_bytes) = serde_json::to_vec_pretty(&record) else {
        return;
    };
    let Some(signature) = sign_cache_payload(&payload_bytes) else {
        return;
    };
    let Ok(sig_bytes) = serde_json::to_vec_pretty(&signature) else {
        return;
    };
    let cache_path = provider_catalog_cache_path(provider);
    let sig_path = provider_catalog_signature_path(provider);
    if let Some(parent) = cache_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp_payload = cache_path.with_extension("json.tmp");
    let tmp_sig = sig_path.with_extension("sig.json.tmp");
    if let Ok(mut file) = std::fs::File::create(&tmp_payload) {
        let _ = file.write_all(&payload_bytes);
        let _ = file.flush();
        let _ = std::fs::rename(&tmp_payload, &cache_path);
    }
    if let Ok(mut file) = std::fs::File::create(&tmp_sig) {
        let _ = file.write_all(&sig_bytes);
        let _ = file.flush();
        let _ = std::fs::rename(&tmp_sig, &sig_path);
    }
}

pub fn normalize_provider_model(input: &str) -> Result<String, AgentError> {
    if input.trim().is_empty() {
        return Err(AgentError::Config("Model cannot be empty".to_string()));
    }
    if input.contains(':') {
        Ok(input.to_string())
    } else {
        Ok(format!("openai:{input}"))
    }
}

pub fn curated_provider_slugs() -> Vec<&'static str> {
    CURATED_PROVIDER_MODELS
        .iter()
        .map(|(provider, _)| *provider)
        .collect()
}

pub fn is_models_dev_preferred_provider(provider: &str) -> bool {
    provider_capability_for(provider)
        .map(|cap| cap.models_dev_merged)
        .unwrap_or(false)
}

pub fn merge_with_models_dev(models_dev: &[String], curated: &[&str]) -> Vec<String> {
    if models_dev.is_empty() {
        return curated.iter().map(|m| m.to_string()).collect();
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut merged = Vec::with_capacity(curated.len() + models_dev.len());

    for item in models_dev {
        let key = item.to_ascii_lowercase();
        if seen.insert(key) {
            merged.push(item.clone());
        }
    }

    for item in curated {
        let key = item.to_ascii_lowercase();
        if seen.insert(key) {
            merged.push((*item).to_string());
        }
    }

    merged
}

pub fn provider_curated_models(provider: &str) -> &'static [&'static str] {
    let normalized = provider.trim().to_ascii_lowercase();
    for (slug, models) in CURATED_PROVIDER_MODELS {
        if slug.eq_ignore_ascii_case(&normalized) {
            return models;
        }
    }
    &[]
}

async fn resolve_nous_catalog_endpoint_and_token() -> Option<(String, String)> {
    if let Ok(creds) = crate::auth::resolve_nous_runtime_credentials(
        false,
        true,
        crate::auth::NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        crate::auth::DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await
    {
        if !creds.api_key.trim().is_empty() {
            return Some((creds.base_url, creds.api_key));
        }
    }
    let auth_state = crate::auth::read_provider_auth_state("nous").ok().flatten();
    let token = std::env::var("NOUS_API_KEY")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            auth_state.as_ref().and_then(|state| {
                state
                    .get("agent_key")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
        })
        .or_else(|| {
            auth_state.as_ref().and_then(|state| {
                state
                    .get("access_token")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
        })?;

    let base_url = std::env::var("NOUS_INFERENCE_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            auth_state.as_ref().and_then(|state| {
                state
                    .get("inference_base_url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
        })
        .unwrap_or_else(|| NOUS_DEFAULT_INFERENCE_BASE_URL.to_string());
    Some((base_url, token))
}

async fn fetch_nous_live_models() -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let Some((base_url, token)) = resolve_nous_catalog_endpoint_and_token().await else {
        return Vec::new();
    };
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let response = match reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
    {
        Ok(resp) => resp,
        Err(_) => return Vec::new(),
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    let payload: Value = match response.json().await {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let ids = payload
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return ids;
    }
    let mut seen: HashSet<String> = HashSet::new();
    let mut dedup = Vec::with_capacity(ids.len());
    for id in ids {
        let key = id.to_ascii_lowercase();
        if seen.insert(key) {
            dedup.push(id);
        }
    }
    dedup
}

pub async fn provider_model_ids_with_client(
    provider: &str,
    client: &ModelsDevClient,
) -> Vec<String> {
    let curated = provider_curated_models(provider);
    if curated.is_empty() {
        return Vec::new();
    }
    let normalized = provider.trim().to_ascii_lowercase();
    if let Some(cached) = load_provider_catalog_cache(&normalized) {
        if !cached.is_empty() {
            return cached;
        }
    }

    let computed = if normalized == "nous" {
        // Nous model picker should always include curated compatibility picks
        // (including kimi-k2.6), then append live/models.dev discoveries.
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        for model in curated {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push((*model).to_string());
            }
        }

        let live = fetch_nous_live_models().await;
        for model in live {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push(model);
            }
        }

        // Nous Portal fronts a large OpenRouter-compatible catalog.
        // Keep curated picks first, then append dynamic agentic models.
        client.fetch(false).await;
        let models_dev = client.list_agentic_models("openrouter");
        for model in models_dev {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push(model);
            }
        }

        if merged.is_empty() {
            curated.iter().map(|model| model.to_string()).collect()
        } else {
            merged
        }
    } else if !is_models_dev_preferred_provider(&normalized) {
        curated.iter().map(|model| model.to_string()).collect()
    } else {
        // Best-effort refresh: if fetch/list fails or returns empty, curated stays as fallback.
        client.fetch(false).await;
        let models_dev = client.list_agentic_models(&normalized);
        if models_dev.is_empty() {
            curated.iter().map(|model| model.to_string()).collect()
        } else {
            merge_with_models_dev(&models_dev, curated)
        }
    };
    persist_provider_catalog_cache(&normalized, &computed);
    computed
}

pub async fn provider_model_ids(provider: &str) -> Vec<String> {
    provider_model_ids_with_client(provider, default_client()).await
}

pub async fn provider_catalog_entries(
    providers: &[&str],
    max_models: usize,
) -> Vec<ProviderCatalogEntry> {
    let client = default_client();
    let mut entries = Vec::new();

    for provider in providers {
        let models = provider_model_ids_with_client(provider, client).await;
        if models.is_empty() {
            continue;
        }
        let total_models = models.len();
        let models = models.into_iter().take(max_models).collect();
        entries.push(ProviderCatalogEntry {
            provider: (*provider).to_string(),
            models,
            total_models,
        });
    }

    entries
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use std::time::{SystemTime, UNIX_EPOCH};

    use hermes_intelligence::models_dev::ModelsDevClient;
    use serde_json::json;

    use super::{
        cached_provider_catalog_status, is_models_dev_preferred_provider,
        load_provider_catalog_cache, merge_with_models_dev, persist_provider_catalog_cache,
        provider_catalog_cache_path, provider_catalog_entries, provider_curated_models,
        provider_model_ids_with_client,
    };

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    fn cache_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("hermes-cli-model-switch-{label}-{nanos}.json"))
    }

    fn seeded_client(seed: serde_json::Value) -> ModelsDevClient {
        let client = ModelsDevClient::new("http://127.0.0.1:9/unreachable", cache_path("seeded"));
        client.seed_cache(seed);
        client
    }

    #[test]
    fn merge_returns_curated_when_models_dev_empty() {
        let curated = ["mimo-v2-pro", "kimi-k2.6"];
        let merged = merge_with_models_dev(&[], &curated);
        assert_eq!(merged, vec!["mimo-v2-pro", "kimi-k2.6"]);
    }

    #[test]
    fn merge_is_models_dev_first_with_case_insensitive_dedup() {
        let models_dev = vec![
            "MiniMax-M2.7".to_string(),
            "mimo-v2.5-pro".to_string(),
            "mimo-v2.5-pro".to_string(),
        ];
        let curated = ["minimax-m2.7", "mimo-v2-pro"];
        let merged = merge_with_models_dev(&models_dev, &curated);
        assert_eq!(
            merged,
            vec![
                "MiniMax-M2.7".to_string(),
                "mimo-v2.5-pro".to_string(),
                "mimo-v2-pro".to_string()
            ]
        );
    }

    #[test]
    fn preferred_provider_set_excludes_openrouter_and_nous() {
        assert!(is_models_dev_preferred_provider("opencode-go"));
        assert!(is_models_dev_preferred_provider("google"));
        assert!(!is_models_dev_preferred_provider("openrouter"));
        assert!(!is_models_dev_preferred_provider("nous"));
    }

    #[test]
    fn deepseek_curated_models_include_v4_variants() {
        let models = provider_curated_models("deepseek");
        assert!(models.contains(&"deepseek-v4-pro"));
        assert!(models.contains(&"deepseek-v4-flash"));
    }

    #[test]
    fn openrouter_curated_models_include_gpt55_variants() {
        let models = provider_curated_models("openrouter");
        assert!(models.contains(&"openai/gpt-5.5"));
        assert!(models.contains(&"openai/gpt-5.5-pro"));
    }

    #[tokio::test]
    async fn preferred_provider_merges_models_dev_with_curated() {
        let client = seeded_client(json!({
            "opencode-go": {
                "models": {
                    "mimo-v2.5-pro": {"tool_call": true},
                    "mimo-v2-pro": {"tool_call": true},
                    "ignore-non-agentic": {"tool_call": false}
                }
            }
        }));
        let merged = provider_model_ids_with_client("opencode-go", &client).await;
        assert!(merged.iter().any(|m| m == "mimo-v2.5-pro"));
        let mimo25 = merged
            .iter()
            .position(|m| m == "mimo-v2.5-pro")
            .expect("missing mimo-v2.5-pro");
        let mimo2 = merged
            .iter()
            .position(|m| m == "mimo-v2-pro")
            .expect("missing mimo-v2-pro");
        let qwen = merged
            .iter()
            .position(|m| m == "qwen3.6-plus")
            .expect("missing curated fallback qwen3.6-plus");
        assert!(mimo25 < qwen);
        assert!(mimo2 < qwen);
        assert!(merged.iter().any(|m| m == "qwen3.6-plus"));
    }

    #[tokio::test]
    async fn preferred_provider_falls_back_to_curated_when_models_dev_has_no_provider_data() {
        let client = seeded_client(json!({
            "placeholder": {"models": {}}
        }));
        let out = provider_model_ids_with_client("opencode-go", &client).await;
        let expected: Vec<String> = provider_curated_models("opencode-go")
            .iter()
            .map(|m| (*m).to_string())
            .collect();
        assert!(out.len() >= expected.len());
        for model in &expected {
            assert!(
                out.iter().any(|m| m == model),
                "missing curated model: {model}"
            );
        }
    }

    #[tokio::test]
    async fn openrouter_never_merges_models_dev_entries() {
        let client = seeded_client(json!({
            "openrouter": {
                "models": {
                    "this-should-not-appear": {"tool_call": true}
                }
            }
        }));
        let out = provider_model_ids_with_client("openrouter", &client).await;
        assert!(!out.iter().any(|m| m == "this-should-not-appear"));
        let expected: Vec<String> = provider_curated_models("openrouter")
            .iter()
            .map(|m| (*m).to_string())
            .collect();
        assert_eq!(out, expected);
    }

    #[tokio::test]
    async fn nous_provider_uses_curated_plus_openrouter_agentic_catalog() {
        let client = seeded_client(json!({
            "openrouter": {
                "models": {
                    "moonshotai/kimi-k2.6": {"tool_call": true},
                    "openai/gpt-5.5": {"tool_call": true},
                    "anthropic/claude-opus-4.7": {"tool_call": true}
                }
            }
        }));
        let out = provider_model_ids_with_client("nous", &client).await;
        assert!(
            out.starts_with(&[
                "nousresearch/hermes-3-llama-3.1-405b".to_string(),
                "nousresearch/hermes-4-405b".to_string(),
                "nousresearch/hermes-4-70b".to_string(),
                "moonshotai/kimi-k2.6".to_string(),
                "xiaomi/mimo-v2.5-pro".to_string(),
                "anthropic/claude-sonnet-4.5".to_string()
            ]),
            "nous list should keep curated models first"
        );
        assert!(
            out.iter().any(|m| m == "openai/gpt-5.5"),
            "expected openrouter-derived models in nous catalog"
        );
    }

    #[tokio::test]
    async fn provider_catalog_entries_truncates_but_keeps_total() {
        let client = seeded_client(json!({
            "opencode-go": {
                "models": {
                    "mimo-v2.5-pro": {"tool_call": true},
                    "mimo-v2.5": {"tool_call": true},
                    "mimo-v2-pro": {"tool_call": true},
                    "kimi-k2.6": {"tool_call": true}
                }
            },
            "openrouter": {"models": {}}
        }));

        let providers = ["opencode-go", "openrouter"];
        let mut entries = Vec::new();
        for provider in providers {
            let models = provider_model_ids_with_client(provider, &client).await;
            let total_models = models.len();
            entries.push(super::ProviderCatalogEntry {
                provider: provider.to_string(),
                models: models.into_iter().take(2).collect(),
                total_models,
            });
        }

        let opencode = entries
            .into_iter()
            .find(|entry| entry.provider == "opencode-go")
            .expect("missing opencode-go catalog entry");
        assert_eq!(opencode.models.len(), 2);
        assert!(opencode.total_models >= 4);
    }

    #[tokio::test]
    async fn provider_catalog_entries_uses_global_client_shape() {
        // Smoke-test the function shape with unknown providers only, avoiding network use.
        let entries = provider_catalog_entries(&["unknown-provider"], 2).await;
        assert!(entries.is_empty());
    }

    #[test]
    fn signed_provider_catalog_cache_round_trip_verifies() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prior_home = std::env::var("HERMES_HOME").ok();
        let prior_signing = std::env::var("HERMES_PROVENANCE_SIGNING_KEY").ok();
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::remove_var("HERMES_PROVENANCE_SIGNING_KEY");

        let models = vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()];
        persist_provider_catalog_cache("openai", &models);
        let loaded = load_provider_catalog_cache("openai").expect("load cache");
        assert_eq!(loaded, models);

        let status = cached_provider_catalog_status("openai").expect("cache status");
        assert!(status.verified);
        if let Some(value) = prior_home {
            std::env::set_var("HERMES_HOME", value);
        } else {
            std::env::remove_var("HERMES_HOME");
        }
        if let Some(value) = prior_signing {
            std::env::set_var("HERMES_PROVENANCE_SIGNING_KEY", value);
        } else {
            std::env::remove_var("HERMES_PROVENANCE_SIGNING_KEY");
        }
    }

    #[test]
    fn signed_provider_catalog_cache_detects_tamper() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prior_home = std::env::var("HERMES_HOME").ok();
        let prior_signing = std::env::var("HERMES_PROVENANCE_SIGNING_KEY").ok();
        std::env::set_var("HERMES_HOME", tmp.path());
        std::env::remove_var("HERMES_PROVENANCE_SIGNING_KEY");

        let models = vec!["gpt-4o".to_string()];
        persist_provider_catalog_cache("openai", &models);
        let path = provider_catalog_cache_path("openai");
        let mut raw = std::fs::read_to_string(&path).expect("read cache");
        raw = raw.replace("gpt-4o", "gpt-4o-tampered");
        std::fs::write(&path, raw).expect("write tampered cache");

        assert!(
            load_provider_catalog_cache("openai").is_none(),
            "tampered payload must fail signature verification"
        );
        let status = cached_provider_catalog_status("openai").expect("cache status");
        assert!(!status.verified);
        if let Some(value) = prior_home {
            std::env::set_var("HERMES_HOME", value);
        } else {
            std::env::remove_var("HERMES_HOME");
        }
        if let Some(value) = prior_signing {
            std::env::set_var("HERMES_PROVENANCE_SIGNING_KEY", value);
        } else {
            std::env::remove_var("HERMES_PROVENANCE_SIGNING_KEY");
        }
    }
}
