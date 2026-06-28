use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use hermes_agent::bedrock::{
    curated_bedrock_models_for_region, discover_bedrock_model_ids, has_aws_credentials,
    resolve_bedrock_region,
};
use hermes_config::{GatewayConfig, LlmProviderConfig, StaleAuxiliaryAssignment};
use hermes_core::AgentError;
use hermes_intelligence::models_dev::{default_client, ModelsDevClient};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::providers::{canonical_provider_id, provider_capability_for};
const NOUS_DEFAULT_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
const PROVIDER_CATALOG_CACHE_VERSION: u32 = 2;
pub const MOA_PROVIDER: &str = "moa";
pub const MOA_DEFAULT_PRESET: &str = "default";
const OLLAMA_LOCAL_DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";
const LLAMA_CPP_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const VLLM_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000/v1";
const MLX_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const APPLE_ANE_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8081/v1";
const SGLANG_DEFAULT_BASE_URL: &str = "http://127.0.0.1:30000/v1";
const TGI_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8082/v1";
const LMSTUDIO_DEFAULT_BASE_URL: &str = "http://127.0.0.1:1234/v1";
const LMDEPLOY_DEFAULT_BASE_URL: &str = "http://127.0.0.1:23333/v1";
const LOCALAI_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const KOBOLDCPP_DEFAULT_BASE_URL: &str = "http://127.0.0.1:5001/v1";
const TEXT_GENERATION_WEBUI_DEFAULT_BASE_URL: &str = "http://127.0.0.1:5000/v1";
const TABBYAPI_DEFAULT_BASE_URL: &str = "http://127.0.0.1:5000/v1";
const HUGGINGFACE_ROUTER_DEFAULT_BASE_URL: &str = "https://router.huggingface.co/v1";

include!("model_switch/curated_models.rs");

include!("model_switch/catalog_cache.rs");

pub fn normalize_provider_model(input: &str) -> Result<String, AgentError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(AgentError::Config("Model cannot be empty".to_string()));
    }
    if trimmed.eq_ignore_ascii_case("list") || trimmed.eq_ignore_ascii_case("ls") {
        return Err(AgentError::Config(
            "`hermes model list` is not a model setter. Run `hermes model` to list providers."
                .to_string(),
        ));
    }
    let canonical_provider = canonical_provider_id(trimmed);
    if !trimmed.contains(':')
        && curated_provider_slugs().iter().any(|provider| {
            provider.eq_ignore_ascii_case(trimmed)
                || provider.eq_ignore_ascii_case(canonical_provider.as_str())
        })
    {
        return Err(AgentError::Config(format!(
            "`{trimmed}` is a provider, not a model. Use `{canonical_provider}:<model-id>`."
        )));
    }
    if trimmed.contains(':') {
        Ok(trimmed.to_string())
    } else {
        Ok(format!("openai:{trimmed}"))
    }
}

pub fn provider_slug_from_provider_model(provider_model: &str) -> &str {
    provider_model
        .split_once(':')
        .map(|(provider, _)| provider.trim())
        .filter(|provider| !provider.is_empty())
        .unwrap_or("openai")
}

pub fn format_stale_auxiliary_warning(
    main_provider: &str,
    stale: &[StaleAuxiliaryAssignment],
) -> Option<String> {
    if stale.is_empty() {
        return None;
    }

    let slots = stale
        .iter()
        .map(|entry| {
            let model = entry.model.trim();
            if model.is_empty() {
                format!("{}={}", entry.task, entry.provider)
            } else {
                format!("{}={}/{}", entry.task, entry.provider, model)
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let plural = if stale.len() == 1 { "" } else { "s" };

    Some(format!(
        "Warning: {} auxiliary task{} ({}) still run on providers other than main '{}'. They will not follow this model switch. Reset with `hermes config set auxiliary.<task>.provider auto`.",
        stale.len(),
        plural,
        slots,
        main_provider
    ))
}

pub fn curated_provider_slugs() -> Vec<&'static str> {
    CURATED_PROVIDER_MODELS
        .iter()
        .map(|(provider, _)| *provider)
        .collect()
}

pub fn provider_picker_description(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "alibaba-coding-plan" => {
            "Alibaba Coding Plan (coding models via DashScope Coding Plan API)"
        }
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            "Google Gemini via OAuth + Code Assist (Code Assist OAuth flow)"
        }
        "kimi-coding" => "Kimi Coding Plan (api.kimi.com & Moonshot API)",
        "kimi-coding-cn" => "Kimi / Moonshot China (Domestic direct API)",
        "lmstudio" | "lm-studio" | "lm_studio" => {
            "LM Studio (local desktop OpenAI-compatible model server)"
        }
        "lmdeploy" | "lm-deploy" | "lm_deploy" => {
            "LMDeploy (self-host OpenAI-compatible inference server)"
        }
        "localai" | "local-ai" | "local_ai" => "LocalAI (local/self-host OpenAI-compatible server)",
        "koboldcpp" | "kobold-cpp" | "kobold" => {
            "KoboldCpp (single-binary local OpenAI-compatible server)"
        }
        "text-generation-webui" | "text-generation-web-ui" | "textgen-webui" | "oobabooga" => {
            "text-generation-webui / oobabooga (local OpenAI-compatible server)"
        }
        "tabbyapi" | "tabby-api" | "exllama" | "exllamav2" => {
            "TabbyAPI / ExLlamaV2 (local OpenAI-compatible server)"
        }
        "minimax-cn" | "minimax_cn" => "MiniMax China (Domestic direct API)",
        "xai-oauth" => "xAI Grok OAuth (SuperGrok / Premium+ subscription)",
        "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => {
            "Nous Portal API key (direct API key access to Nous inference)"
        }
        raw => match canonical_provider_id(raw).as_str() {
            "nous" => {
                "Nous Portal (Everything your agent needs, 300+ models with bundled tool use)"
            }
            "openrouter" => "OpenRouter (Pay-per-use API aggregator)",
            "moa" => "Mixture of Agents (virtual provider backed by Rust quorum fan-out)",
            "novita" => "NovitaAI (Cloud: Model API, Agent Sandbox, GPU Cloud)",
            "anthropic" => "Anthropic (Claude models via API key or Claude Code)",
            "openai-codex" => "OpenAI Codex (Codex CLI via ChatGPT subscription or API key)",
            "openai" => "OpenAI API (api.openai.com, API key)",
            "qwen" => "Qwen Cloud / DashScope (Qwen + multi-provider)",
            "qwen-oauth" => "Qwen OAuth (Reuses local Qwen CLI login)",
            "xiaomi" => "Xiaomi MiMo (MiMo-V2.5 and V2 models: pro, omni, flash)",
            "tencent-tokenhub" => "Tencent TokenHub (Hy3 Preview via tokenhub.tencentmaas.com)",
            "nvidia" => "NVIDIA NIM (Nemotron models via build.nvidia.com or local NIM)",
            "copilot" => "GitHub Copilot (Uses GITHUB_TOKEN or gh auth token)",
            "copilot-acp" => "GitHub Copilot ACP (Spawns copilot --acp --stdio)",
            "huggingface" => "Hugging Face Inference Providers",
            "gemini" => "Google AI Studio (Native Gemini API)",
            "deepseek" => "DeepSeek (V3, R1, coder, direct API)",
            "xai" => "xAI Grok (Direct API)",
            "zai" => "Z.AI / GLM (Zhipu direct API)",
            "kimi" => "Kimi Coding Plan (api.kimi.com & Moonshot API)",
            "stepfun" => "StepFun Step Plan (Agent / coding models via Step Plan API)",
            "minimax" => "MiniMax (Global direct API)",
            "ollama-cloud" => "Ollama Cloud (Cloud-hosted open models, ollama.com)",
            "arcee" => "Arcee AI (Trinity models, direct API)",
            "gmi" => "GMI Cloud (Multi-model direct API)",
            "kilocode" => "Kilo Code (Kilo Gateway API)",
            "opencode-zen" => "OpenCode Zen (Curated models, pay-as-you-go)",
            "opencode-go" => "OpenCode Go (Open models subscription)",
            "bedrock" => "AWS Bedrock (Claude, Nova, Llama, DeepSeek; IAM or API key)",
            "azure-foundry" => {
                "Azure Foundry (OpenAI-style or Anthropic-style endpoint, your Azure AI deployment)"
            }
            "ai-gateway" => "Vercel AI Gateway (OpenAI-compatible gateway)",
            "ollama-local" => "Ollama Local (local OpenAI-compatible server)",
            "llama-cpp" => "llama.cpp Server (local OpenAI-compatible endpoint)",
            "vllm" => "vLLM Server (local/self-host OpenAI-compatible endpoint)",
            "mlx" => "MLX Server (Apple Silicon local endpoint)",
            "apple-ane" => "Apple ANE Endpoint (private local endpoint)",
            "sglang" => "SGLang Server (local/self-host OpenAI-compatible endpoint)",
            "tgi" => "Text Generation Inference (local/self-host endpoint)",
            "lmstudio" => "LM Studio (local desktop OpenAI-compatible model server)",
            "lmdeploy" => "LMDeploy (self-host OpenAI-compatible inference server)",
            "localai" => "LocalAI (local/self-host OpenAI-compatible server)",
            "koboldcpp" => "KoboldCpp (single-binary local OpenAI-compatible server)",
            "text-generation-webui" => {
                "text-generation-webui / oobabooga (local OpenAI-compatible server)"
            }
            "tabbyapi" => "TabbyAPI / ExLlamaV2 (local OpenAI-compatible server)",
            _ => "Provider catalog entry",
        },
    }
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
    let normalized = canonical_provider_id(provider);
    let normalized = if normalized == "nous-api" {
        "nous".to_string()
    } else {
        normalized
    };
    for (slug, models) in CURATED_PROVIDER_MODELS {
        if slug.eq_ignore_ascii_case(&normalized) {
            return models;
        }
    }
    &[]
}

fn configured_llm_provider<'a>(
    config: &'a GatewayConfig,
    provider: &str,
) -> Option<(&'a String, &'a LlmProviderConfig)> {
    let trimmed = provider.trim();
    if trimmed.is_empty() {
        return None;
    }
    config.llm_providers.get_key_value(trimmed).or_else(|| {
        config
            .llm_providers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(trimmed))
    })
}

fn dedup_model_ids(models: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for model in models {
        let trimmed = model.trim();
        if trimmed.is_empty() {
            continue;
        }
        let key = trimmed.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn resolve_configured_api_key(provider: &LlmProviderConfig) -> Option<String> {
    if let Some(env_name) = provider
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(value) = std::env::var(env_name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    let raw = provider.api_key.as_deref()?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some(env_name) = raw.strip_prefix("${").and_then(|v| v.strip_suffix('}')) {
        return std::env::var(env_name.trim())
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
    }
    Some(raw.to_string())
}

pub fn provider_slugs_for_config(config: &GatewayConfig) -> Vec<String> {
    let mut providers = Vec::new();
    let mut seen = HashSet::new();

    for provider in curated_provider_slugs() {
        let key = provider.to_ascii_lowercase();
        if seen.insert(key) {
            providers.push(provider.to_string());
        }
    }

    let mut configured: Vec<String> = config
        .llm_providers
        .keys()
        .map(|provider| provider.trim().to_string())
        .filter(|provider| !provider.is_empty())
        .collect();
    configured.sort_by_key(|provider| provider.to_ascii_lowercase());
    for provider in configured {
        let key = provider.to_ascii_lowercase();
        if seen.insert(key) {
            providers.push(provider);
        }
    }

    providers
}

pub async fn provider_model_ids_for_config(provider: &str, config: &GatewayConfig) -> Vec<String> {
    let configured = configured_llm_provider(config, provider).map(|(_, cfg)| cfg);
    if let Some(provider_cfg) = configured {
        let configured_models = dedup_model_ids(provider_cfg.models.clone());
        if !provider_cfg.discover_models && !configured_models.is_empty() {
            return configured_models;
        }

        if let Some(base_url) = provider_cfg
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let api_key = resolve_configured_api_key(provider_cfg);
            let should_probe =
                provider_cfg.discover_models && (api_key.is_some() || configured_models.is_empty());
            if should_probe {
                let live = fetch_openai_compatible_live_models(base_url, api_key.as_deref()).await;
                if !live.is_empty() {
                    return live;
                }
            }
        }

        if !configured_models.is_empty() {
            return configured_models;
        }
    }

    provider_model_ids(provider).await
}

pub async fn provider_catalog_entries_for_config(
    config: &GatewayConfig,
) -> Vec<ProviderCatalogEntry> {
    let providers = provider_slugs_for_config(config);
    let mut entries = Vec::new();

    for provider in providers {
        let models = provider_model_ids_for_config(&provider, config).await;
        if models.is_empty() {
            continue;
        }
        let total_models = models.len();
        entries.push(ProviderCatalogEntry {
            provider,
            models,
            total_models,
        });
    }

    entries
}

include!("model_switch/live_catalog.rs");
pub async fn provider_model_ids_with_client(
    provider: &str,
    client: &ModelsDevClient,
) -> Vec<String> {
    let normalized = canonical_provider_id(provider);
    let catalog_provider = if normalized == "nous-api" {
        "nous"
    } else {
        normalized.as_str()
    };
    let curated = provider_curated_models(catalog_provider);
    if curated.is_empty() {
        return Vec::new();
    }
    if catalog_provider == MOA_PROVIDER {
        return curated.iter().map(|model| model.to_string()).collect();
    }
    if let Some(cached) = load_provider_catalog_cache(catalog_provider) {
        if !cached.is_empty() {
            return cached;
        }
    }

    let computed = if catalog_provider == "bedrock" {
        let region = resolve_bedrock_region();
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        if has_aws_credentials() {
            for model in discover_bedrock_model_ids(region.as_str()).await {
                let key = model.to_ascii_lowercase();
                if seen.insert(key) {
                    merged.push(model);
                }
            }
        }
        for model in curated_bedrock_models_for_region(region.as_str()) {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push(model);
            }
        }
        for model in curated {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push((*model).to_string());
            }
        }
        merged
    } else if catalog_provider == "nous" {
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
    } else if catalog_provider == "huggingface" {
        // For Hugging Face, prefer curated stable picks first, then append live
        // router models and models.dev-discovered agentic entries.
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        for model in curated {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push((*model).to_string());
            }
        }

        if !huggingface_live_catalog_disabled() {
            let (base_url, token) = resolve_huggingface_catalog_endpoint_and_token();
            let live =
                fetch_openai_compatible_live_models(base_url.as_str(), token.as_deref()).await;
            for model in live.into_iter().take(huggingface_catalog_limit()) {
                let key = model.to_ascii_lowercase();
                if seen.insert(key) {
                    merged.push(model);
                }
            }
        }

        client.fetch(false).await;
        let models_dev = client.list_agentic_models("huggingface");
        for model in models_dev {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push(model);
            }
        }
        merged
    } else if matches!(
        catalog_provider,
        "gmi" | "arcee" | "xiaomi" | "tencent-tokenhub"
    ) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        if let Some((base_url, token)) = openai_compatible_catalog_credentials(catalog_provider) {
            let live = fetch_openai_compatible_live_models(base_url.as_str(), Some(&token)).await;
            for model in live {
                let key = model.to_ascii_lowercase();
                if seen.insert(key) {
                    merged.push(model);
                }
            }
        }
        for model in curated {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push((*model).to_string());
            }
        }
        merged
    } else if is_local_openai_compatible_provider(catalog_provider) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        if let Some(base_url) = local_provider_resolved_base_url(catalog_provider) {
            let live = fetch_openai_compatible_live_models(
                base_url.as_str(),
                local_provider_api_key(catalog_provider).as_deref(),
            )
            .await;
            for model in live {
                let key = model.to_ascii_lowercase();
                if seen.insert(key) {
                    merged.push(model);
                }
            }
        }
        for model in curated {
            let key = model.to_ascii_lowercase();
            if seen.insert(key) {
                merged.push((*model).to_string());
            }
        }
        merged
    } else if !is_models_dev_preferred_provider(catalog_provider) {
        curated.iter().map(|model| model.to_string()).collect()
    } else {
        // Best-effort refresh: if fetch/list fails or returns empty, curated stays as fallback.
        client.fetch(false).await;
        let models_dev = client.list_agentic_models(catalog_provider);
        if models_dev.is_empty() {
            curated.iter().map(|model| model.to_string()).collect()
        } else {
            merge_with_models_dev(&models_dev, curated)
        }
    };
    persist_provider_catalog_cache(catalog_provider, &computed);
    computed
}

pub async fn provider_model_ids(provider: &str) -> Vec<String> {
    provider_model_ids_with_client(provider, default_client()).await
}

pub async fn provider_catalog_entries(providers: &[&str]) -> Vec<ProviderCatalogEntry> {
    let client = default_client();
    let mut entries = Vec::new();

    for provider in providers {
        let models = provider_model_ids_with_client(provider, client).await;
        if models.is_empty() {
            continue;
        }
        let total_models = models.len();
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
    use crate::test_env_lock;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hermes_intelligence::models_dev::ModelsDevClient;
    use serde_json::json;

    use super::{
        cached_provider_catalog_status, clear_provider_catalog_cache, curated_provider_slugs,
        is_models_dev_preferred_provider, load_provider_catalog_cache, merge_with_models_dev,
        normalize_provider_model, persist_provider_catalog_cache, provider_catalog_cache_path,
        provider_catalog_entries, provider_catalog_entries_for_config,
        provider_catalog_signature_path, provider_curated_models, provider_model_ids_for_config,
        provider_model_ids_with_client, provider_picker_description,
        provider_slug_from_provider_model, provider_slugs_for_config,
        resolve_huggingface_catalog_endpoint_and_token,
    };

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    const TEST_PROVENANCE_SIGNING_KEY: &str =
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    struct ScopedCatalogEnv {
        prior_home: Option<String>,
        prior_signing: Option<String>,
    }

    impl ScopedCatalogEnv {
        fn new(home: &std::path::Path) -> Self {
            let prior_home = std::env::var("HERMES_HOME").ok();
            let prior_signing = std::env::var("HERMES_PROVENANCE_SIGNING_KEY").ok();
            std::env::set_var("HERMES_HOME", home);
            std::env::set_var("HERMES_PROVENANCE_SIGNING_KEY", TEST_PROVENANCE_SIGNING_KEY);
            Self {
                prior_home,
                prior_signing,
            }
        }
    }

    impl Drop for ScopedCatalogEnv {
        fn drop(&mut self) {
            if let Some(value) = self.prior_home.take() {
                std::env::set_var("HERMES_HOME", value);
            } else {
                std::env::remove_var("HERMES_HOME");
            }
            if let Some(value) = self.prior_signing.take() {
                std::env::set_var("HERMES_PROVENANCE_SIGNING_KEY", value);
            } else {
                std::env::remove_var("HERMES_PROVENANCE_SIGNING_KEY");
            }
        }
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
    fn normalize_provider_model_rejects_list_and_bare_provider_names() {
        assert!(normalize_provider_model("list").is_err());
        assert!(normalize_provider_model("nous").is_err());
        assert!(normalize_provider_model("mixture").is_err());
        assert!(normalize_provider_model("mixture-of-agents").is_err());
        assert_eq!(
            normalize_provider_model("nous:openai/gpt-5.5-pro").expect("valid provider model"),
            "nous:openai/gpt-5.5-pro"
        );
        assert_eq!(
            normalize_provider_model("gpt-4o").expect("bare OpenAI model"),
            "openai:gpt-4o"
        );
    }

    #[test]
    fn provider_slug_from_provider_model_defaults_bare_to_openai() {
        assert_eq!(
            provider_slug_from_provider_model("openrouter:anthropic/claude-opus-4.8"),
            "openrouter"
        );
        assert_eq!(provider_slug_from_provider_model("gpt-4o"), "openai");
        assert_eq!(provider_slug_from_provider_model(":gpt-4o"), "openai");
    }

    #[test]
    fn format_stale_auxiliary_warning_lists_pinned_slots() {
        let stale = vec![
            hermes_config::StaleAuxiliaryAssignment {
                task: "compression".to_string(),
                provider: "nous".to_string(),
                model: "hermes-4".to_string(),
            },
            hermes_config::StaleAuxiliaryAssignment {
                task: "curator".to_string(),
                provider: "openai".to_string(),
                model: String::new(),
            },
        ];
        let warning =
            super::format_stale_auxiliary_warning("openrouter", &stale).expect("stale warning");
        assert!(warning.contains("Warning: 2 auxiliary tasks"));
        assert!(warning.contains("compression=nous/hermes-4"));
        assert!(warning.contains("curator=openai"));
        assert!(warning.contains("main 'openrouter'"));
        assert!(warning.contains("auxiliary.<task>.provider auto"));
        assert!(super::format_stale_auxiliary_warning("openrouter", &[]).is_none());
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
    fn local_backend_curated_models_include_ollama_llamacpp_and_vllm() {
        assert!(provider_curated_models("ollama-local").contains(&"qwen3:14b"));
        assert!(provider_curated_models("llama-cpp").contains(&"local-gguf"));
        assert!(provider_curated_models("vllm").contains(&"NousResearch/Meta-Llama-3-8B-Instruct"));
        assert!(provider_curated_models("mlx").contains(&"mlx-community/Qwen3-8B-4bit"));
        assert!(provider_curated_models("apple-ane").contains(&"ane-default"));
        assert!(provider_curated_models("lmstudio").contains(&"local-model"));
        assert!(provider_curated_models("lmdeploy").contains(&"internlm/internlm2_5-7b-chat"));
        assert!(provider_curated_models("localai").contains(&"local-model"));
        assert!(provider_curated_models("koboldcpp").contains(&"koboldcpp"));
        assert!(provider_curated_models("text-generation-webui").contains(&"oobabooga"));
        assert!(provider_curated_models("tabbyapi").contains(&"exllamav2"));
    }

    #[test]
    fn moa_virtual_provider_is_listed_as_static_preset_catalog() {
        assert_eq!(provider_curated_models("moa"), &["default"]);
        assert_eq!(provider_curated_models("mixture-of-agents"), &["default"]);
        assert!(provider_picker_description("moa").contains("virtual provider"));
        assert!(curated_provider_slugs().contains(&"moa"));
    }

    #[test]
    fn provider_curated_models_accepts_aliases() {
        assert_eq!(
            provider_curated_models("ollama"),
            provider_curated_models("ollama-local")
        );
        assert_eq!(
            provider_curated_models("llama.cpp"),
            provider_curated_models("llama-cpp")
        );
        assert_eq!(
            provider_curated_models("llvm"),
            provider_curated_models("vllm")
        );
        assert_eq!(
            provider_curated_models("lm-studio"),
            provider_curated_models("lmstudio")
        );
        assert_eq!(
            provider_curated_models("oobabooga"),
            provider_curated_models("text-generation-webui")
        );
        assert_eq!(
            provider_curated_models("exllamav2"),
            provider_curated_models("tabbyapi")
        );
        assert_eq!(
            provider_curated_models("vmlx"),
            provider_curated_models("mlx")
        );
    }

    #[test]
    fn openrouter_curated_models_include_gpt55_variants() {
        let models = provider_curated_models("openrouter");
        assert!(models.contains(&"openai/gpt-5.5"));
        assert!(models.contains(&"openai/gpt-5.5-pro"));
        assert!(models.contains(&"tencent/hy3-preview:free"));
        assert!(models.contains(&"tencent/hy3-preview"));
    }

    #[test]
    fn direct_api_provider_curated_models_cover_upstream_provider_tests() {
        assert_eq!(provider_curated_models("xai")[0], "grok-build-0.1");
        assert!(provider_curated_models("xai").contains(&"grok-4.3"));
        assert!(provider_curated_models("gmi").contains(&"zai-org/GLM-5.1-FP8"));
        assert!(provider_curated_models("gmicloud").contains(&"deepseek-ai/DeepSeek-V3.2"));
        assert!(provider_curated_models("arcee-ai").contains(&"trinity-mini"));
        assert!(provider_curated_models("mimo").contains(&"mimo-v2.5-pro"));
        assert!(provider_curated_models("tokenhub").contains(&"hy3-preview"));
        assert_eq!(provider_curated_models("zai")[0], "glm-5.2");
        assert_eq!(provider_curated_models("minimax")[0], "MiniMax-M3");
        assert_eq!(provider_curated_models("minimax-cn")[0], "MiniMax-M3");
    }

    #[test]
    fn minimax_picker_defaults_merge_models_dev_with_m3_fallback() {
        assert!(is_models_dev_preferred_provider("minimax"));
        assert!(is_models_dev_preferred_provider("minimax-cn"));
        for provider in ["minimax", "minimax-cn"] {
            let models = provider_curated_models(provider);
            assert!(models.contains(&"MiniMax-M3"));
            assert!(models.contains(&"MiniMax-M2.7"));
            assert!(!models
                .iter()
                .any(|model| model.to_ascii_lowercase().contains("highspeed")));
        }
    }

    #[test]
    fn gemini_curated_models_expose_35_flash_for_api_key_and_oauth_pickers() {
        for provider in ["gemini", "google-gemini-cli"] {
            let models = provider_curated_models(provider);
            assert!(
                models.contains(&"gemini-3.5-flash"),
                "{provider} picker should include gemini-3.5-flash"
            );
            assert!(
                !models.contains(&"gemini-3-flash-preview"),
                "{provider} picker should not offer retired gemini-3-flash-preview"
            );
        }
    }

    #[test]
    fn provider_picker_descriptions_match_refreshed_upstream_copy() {
        assert!(provider_picker_description("nous").contains("300+ models"));
        assert!(provider_picker_description("nous-api").contains("direct API key"));
        assert!(provider_picker_description("openrouter").contains("Pay-per-use API aggregator"));
        assert!(provider_picker_description("google-gemini-cli").contains("Code Assist OAuth flow"));
        assert!(provider_picker_description("xai").contains("Grok"));
        assert!(provider_picker_description("qwen-oauth").contains("Reuses local Qwen CLI login"));
    }

    #[test]
    fn openai_compatible_live_model_url_uses_resolved_base_url() {
        assert_eq!(
            super::openai_compatible_models_url("https://gateway.example.com/custom/v1/"),
            "https://gateway.example.com/custom/v1/models?output_modalities=all"
        );
    }

    #[tokio::test]
    async fn bedrock_provider_models_include_region_aware_curated_fallback_and_aliases() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
        std::env::set_var("AWS_REGION", "eu-central-1");
        let client = seeded_client(json!({}));
        let out = provider_model_ids_with_client("aws-bedrock", &client).await;
        assert!(out.iter().any(|model| model.starts_with("eu.anthropic.")));
        assert!(out.iter().any(|model| model.contains("amazon.nova")));
        std::env::remove_var("AWS_REGION");
    }

    #[tokio::test]
    async fn preferred_provider_merges_models_dev_with_curated() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
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
    async fn local_provider_catalog_returns_curated_without_network_dependency() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
        let client = seeded_client(json!({}));
        let out = provider_model_ids_with_client("ollama-local", &client).await;
        let expected: Vec<String> = provider_curated_models("ollama-local")
            .iter()
            .map(|m| (*m).to_string())
            .collect();
        assert!(out.len() >= expected.len());
        for model in expected {
            assert!(
                out.iter().any(|item| item == &model),
                "missing model {model}"
            );
        }
    }

    #[tokio::test]
    async fn provider_model_ids_normalizes_provider_aliases() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
        let client = seeded_client(json!({}));
        let aliased = provider_model_ids_with_client("ollama", &client).await;
        let canonical = provider_model_ids_with_client("ollama-local", &client).await;
        assert_eq!(aliased, canonical);
    }

    #[tokio::test]
    async fn nous_provider_uses_curated_plus_openrouter_agentic_catalog() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
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
                "openai/gpt-5.5-pro-20260423".to_string(),
                "openai/gpt-5.5-pro".to_string(),
                "openai/gpt-5.5".to_string(),
                "anthropic/claude-4.7-opus-fast-20260512".to_string(),
                "anthropic/claude-opus-4.7".to_string(),
                "qwen/qwen3.6-max-preview-20260420".to_string(),
                "qwen/qwen3.6-max-preview".to_string(),
                "deepseek/deepseek-v4-pro".to_string(),
                "moonshotai/kimi-k2.6".to_string(),
                "nousresearch/hermes-3-llama-3.1-405b".to_string(),
                "nousresearch/hermes-4-405b".to_string(),
                "nousresearch/hermes-4-70b".to_string(),
                "xiaomi/mimo-v2.5-pro".to_string(),
                "tencent/hy3-preview".to_string(),
                "anthropic/claude-sonnet-4.5".to_string()
            ]),
            "nous list should keep curated models first"
        );
        assert!(
            out.iter().any(|m| m == "openai/gpt-5.5"),
            "expected openrouter-derived models in nous catalog"
        );
        let direct = provider_model_ids_with_client("nous-api", &client).await;
        assert_eq!(
            direct, out,
            "nous-api should reuse the Nous Portal model catalog"
        );
    }

    #[tokio::test]
    async fn provider_catalog_entry_fixture_keeps_total_with_subset_preview() {
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
        let entries = provider_catalog_entries(&["unknown-provider"]).await;
        assert!(entries.is_empty());
    }

    #[tokio::test]
    async fn moa_provider_model_ids_bypass_dynamic_catalog_cache() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());
        let client = seeded_client(json!({
            "moa": {
                "models": {
                    "shadow": {"tool_call": true}
                }
            }
        }));

        let out = provider_model_ids_with_client("mixture", &client).await;

        assert_eq!(out, vec!["default".to_string()]);
        assert!(
            !provider_catalog_cache_path("moa").exists(),
            "virtual provider catalog should not be persisted to dynamic cache"
        );
    }

    #[tokio::test]
    async fn configured_provider_discover_false_keeps_explicit_models() {
        let mut cfg = hermes_config::GatewayConfig::default();
        cfg.llm_providers.insert(
            "qianfan-coding".to_string(),
            hermes_config::LlmProviderConfig {
                base_url: Some("https://qianfan.baidubce.com/v2/coding".to_string()),
                api_key: Some("sk-test".to_string()),
                models: vec![
                    " kimi-k2.5 ".to_string(),
                    "glm-5".to_string(),
                    "KIMI-K2.5".to_string(),
                ],
                discover_models: false,
                ..hermes_config::LlmProviderConfig::default()
            },
        );

        let out = provider_model_ids_for_config("QIANFAN-CODING", &cfg).await;

        assert_eq!(out, vec!["kimi-k2.5", "glm-5"]);
    }

    #[tokio::test]
    async fn configured_provider_falls_back_to_explicit_models_when_probe_empty() {
        let mut cfg = hermes_config::GatewayConfig::default();
        cfg.llm_providers.insert(
            "my-gateway".to_string(),
            hermes_config::LlmProviderConfig {
                base_url: Some("https://gateway.example.com/v1".to_string()),
                api_key: Some("sk-test".to_string()),
                models: vec!["fallback-a".to_string(), "fallback-b".to_string()],
                ..hermes_config::LlmProviderConfig::default()
            },
        );

        let out = provider_model_ids_for_config("my-gateway", &cfg).await;

        assert_eq!(out, vec!["fallback-a", "fallback-b"]);
    }

    #[tokio::test]
    async fn config_catalog_entries_include_custom_provider_models() {
        let mut cfg = hermes_config::GatewayConfig::default();
        cfg.llm_providers.insert(
            "baidu-coding".to_string(),
            hermes_config::LlmProviderConfig {
                models: vec!["kimi-k2.5".to_string(), "glm-5".to_string()],
                discover_models: false,
                ..hermes_config::LlmProviderConfig::default()
            },
        );

        let providers = provider_slugs_for_config(&cfg);
        assert!(providers.iter().any(|provider| provider == "baidu-coding"));

        let entries = provider_catalog_entries_for_config(&cfg).await;
        let entry = entries
            .iter()
            .find(|entry| entry.provider == "baidu-coding")
            .expect("custom provider entry");
        assert_eq!(entry.models, vec!["kimi-k2.5", "glm-5"]);
        assert_eq!(entry.total_models, 2);
    }

    #[tokio::test]
    async fn provider_catalog_entries_do_not_truncate_model_picker_results() {
        let mut cfg = hermes_config::GatewayConfig::default();
        let models = (0..60)
            .map(|idx| format!("model-{idx:02}"))
            .collect::<Vec<_>>();
        cfg.llm_providers.insert(
            "wide-provider".to_string(),
            hermes_config::LlmProviderConfig {
                models,
                discover_models: false,
                ..hermes_config::LlmProviderConfig::default()
            },
        );

        let entries = provider_catalog_entries_for_config(&cfg).await;
        let entry = entries
            .iter()
            .find(|entry| entry.provider == "wide-provider")
            .expect("wide provider entry");

        assert_eq!(entry.total_models, 60);
        assert_eq!(entry.models.len(), 60);
        assert_eq!(entry.models.first().map(String::as_str), Some("model-00"));
        assert_eq!(entry.models.last().map(String::as_str), Some("model-59"));
    }

    #[tokio::test]
    async fn huggingface_catalog_keeps_curated_then_models_dev_agentic_entries() {
        let _guard = env_guard();
        std::env::set_var("HERMES_HF_CATALOG_DISABLE_LIVE", "1");
        let client = seeded_client(json!({
            "huggingface": {
                "models": {
                    "meta-llama/Llama-3.3-70B-Instruct": {"tool_call": true},
                    "Qwen/Qwen3.5-Coder-30B-A3B-Instruct": {"tool_call": true}
                }
            }
        }));
        let out = provider_model_ids_with_client("huggingface", &client).await;
        assert!(
            out.starts_with(&[
                "moonshotai/Kimi-K2.5".to_string(),
                "Qwen/Qwen3.5-397B-A17B".to_string(),
                "deepseek-ai/DeepSeek-V3.2".to_string()
            ]),
            "curated huggingface picks should remain first"
        );
        assert!(
            out.iter().any(|m| m == "meta-llama/Llama-3.3-70B-Instruct"),
            "models.dev agentic entries should be appended"
        );
        std::env::remove_var("HERMES_HF_CATALOG_DISABLE_LIVE");
    }

    #[test]
    fn huggingface_catalog_endpoint_prefers_hf_base_url_and_token() {
        let _guard = env_guard();
        std::env::set_var("HF_BASE_URL", "https://example-hf-router.test/v1");
        std::env::set_var("HF_TOKEN", "hf_test_token");
        let (base_url, token) = resolve_huggingface_catalog_endpoint_and_token();
        assert_eq!(base_url, "https://example-hf-router.test/v1");
        assert_eq!(token.as_deref(), Some("hf_test_token"));
        std::env::remove_var("HF_BASE_URL");
        std::env::remove_var("HF_TOKEN");
    }

    #[test]
    fn signed_provider_catalog_cache_round_trip_verifies() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());

        let models = vec!["gpt-4o".to_string(), "gpt-4o-mini".to_string()];
        persist_provider_catalog_cache("openai", &models);
        let loaded = load_provider_catalog_cache("openai").expect("load cache");
        assert_eq!(loaded, models);

        let status = cached_provider_catalog_status("openai").expect("cache status");
        assert!(status.verified);
    }

    #[test]
    fn clearing_provider_catalog_cache_removes_payload_and_signature() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());

        let models = vec!["gpt-4o".to_string()];
        persist_provider_catalog_cache("openai", &models);
        assert!(cached_provider_catalog_status("openai").is_some());

        assert!(clear_provider_catalog_cache("openai").expect("clear cache"));
        assert!(cached_provider_catalog_status("openai").is_none());
        assert!(!provider_catalog_cache_path("openai").exists());
        assert!(!provider_catalog_signature_path("openai").exists());
        assert!(!clear_provider_catalog_cache("openai").expect("idempotent clear"));
    }

    #[test]
    fn signed_provider_catalog_cache_detects_tamper() {
        let _guard = env_guard();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _env = ScopedCatalogEnv::new(tmp.path());

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
    }
}
