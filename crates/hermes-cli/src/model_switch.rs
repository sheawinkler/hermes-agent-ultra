use std::collections::HashSet;

use hermes_core::AgentError;
use hermes_intelligence::models_dev::{default_client, ModelsDevClient};
use serde_json::Value;

// Providers where models.dev entries are merged on top of the curated list.
// Keep openrouter/nous out of this set per upstream behavior.
const MODELS_DEV_PREFERRED: &[&str] = &[
    "opencode-go",
    "opencode-zen",
    "deepseek",
    "kilocode",
    "fireworks",
    "mistral",
    "togetherai",
    "cohere",
    "perplexity",
    "groq",
    "nvidia",
    "huggingface",
    "zai",
    "gemini",
    "google",
];
const NOUS_DEFAULT_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";

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
    let provider = provider.trim().to_ascii_lowercase();
    MODELS_DEV_PREFERRED
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&provider))
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

fn resolve_nous_catalog_endpoint_and_token() -> Option<(String, String)> {
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
    let Some((base_url, token)) = resolve_nous_catalog_endpoint_and_token() else {
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
    if normalized == "nous" {
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
            return curated.iter().map(|model| model.to_string()).collect();
        }
        return merged;
    }

    if !is_models_dev_preferred_provider(&normalized) {
        return curated.iter().map(|model| model.to_string()).collect();
    }

    // Best-effort refresh: if fetch/list fails or returns empty, curated stays as fallback.
    client.fetch(false).await;
    let models_dev = client.list_agentic_models(&normalized);
    if models_dev.is_empty() {
        return curated.iter().map(|model| model.to_string()).collect();
    }

    merge_with_models_dev(&models_dev, curated)
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use hermes_intelligence::models_dev::ModelsDevClient;
    use serde_json::json;

    use super::{
        is_models_dev_preferred_provider, merge_with_models_dev, provider_catalog_entries,
        provider_curated_models, provider_model_ids_with_client,
    };

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
        assert_eq!(out, expected);
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
}
