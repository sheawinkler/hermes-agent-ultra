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
use rand::rngs::OsRng;
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::providers::{canonical_provider_id, provider_capability_for};
const NOUS_DEFAULT_INFERENCE_BASE_URL: &str = "https://inference-api.nousresearch.com/v1";
const PROVIDER_CATALOG_CACHE_VERSION: u32 = 2;
pub const DEFAULT_VISIBLE_MODELS_PER_PROVIDER: usize = 50;
const OLLAMA_LOCAL_DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434/v1";
const LLAMA_CPP_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const VLLM_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000/v1";
const MLX_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";
const APPLE_ANE_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8081/v1";
const SGLANG_DEFAULT_BASE_URL: &str = "http://127.0.0.1:30000/v1";
const TGI_DEFAULT_BASE_URL: &str = "http://127.0.0.1:8082/v1";
const HUGGINGFACE_ROUTER_DEFAULT_BASE_URL: &str = "https://router.huggingface.co/v1";

const CURATED_PROVIDER_MODELS: &[(&str, &[&str])] = &[
    (
        "openrouter",
        &[
            "openai/gpt-5.5",
            "openai/gpt-5.5-pro",
            "moonshotai/kimi-k2.6",
            "anthropic/claude-opus-4.7",
            "openai/gpt-5.4",
            "tencent/hy3-preview:free",
            "tencent/hy3-preview",
        ],
    ),
    (
        "novita",
        &[
            "moonshotai/kimi-k2.5",
            "minimax/minimax-m2.7",
            "zai-org/glm-5",
            "deepseek/deepseek-v3-0324",
            "deepseek/deepseek-r1-0528",
            "qwen/qwen3-235b-a22b-fp8",
        ],
    ),
    (
        "nous",
        &[
            "openai/gpt-5.5-pro-20260423",
            "openai/gpt-5.5-pro",
            "openai/gpt-5.5",
            "anthropic/claude-4.7-opus-fast-20260512",
            "anthropic/claude-opus-4.7",
            "qwen/qwen3.6-max-preview-20260420",
            "qwen/qwen3.6-max-preview",
            "deepseek/deepseek-v4-pro",
            "moonshotai/kimi-k2.6",
            "nousresearch/hermes-3-llama-3.1-405b",
            "nousresearch/hermes-4-405b",
            "nousresearch/hermes-4-70b",
            "xiaomi/mimo-v2.5-pro",
            "tencent/hy3-preview",
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
        "bedrock",
        &[
            "anthropic.claude-sonnet-4-6",
            "us.anthropic.claude-sonnet-4-6",
            "anthropic.claude-haiku-4-5-20251001-v1:0",
            "us.anthropic.claude-haiku-4-5-20251001-v1:0",
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "amazon.nova-pro-v1:0",
            "us.amazon.nova-pro-v1:0",
            "amazon.nova-micro-v1:0",
            "us.amazon.nova-micro-v1:0",
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
        "ollama-local",
        &["qwen3:14b", "llama3.1:8b", "deepseek-r1:14b", "mistral:7b"],
    ),
    (
        "llama-cpp",
        &[
            "local-gguf",
            "qwen3-14b-instruct-q4_k_m.gguf",
            "llama-3.1-8b-instruct-q4_k_m.gguf",
        ],
    ),
    (
        "vllm",
        &[
            "NousResearch/Meta-Llama-3-8B-Instruct",
            "Qwen/Qwen3-14B-Instruct",
            "deepseek-ai/DeepSeek-V3.2",
        ],
    ),
    (
        "mlx",
        &[
            "mlx-community/Qwen3-8B-4bit",
            "mlx-community/Llama-3.1-8B-Instruct-4bit",
            "mlx-community/Mistral-7B-Instruct-v0.3-4bit",
        ],
    ),
    (
        "apple-ane",
        &[
            "ane-default",
            "foundation-model",
            "apple-on-device-openai-compatible",
        ],
    ),
    (
        "sglang",
        &[
            "default",
            "Qwen/Qwen3-14B-Instruct",
            "meta-llama/Llama-3.1-8B-Instruct",
        ],
    ),
    (
        "tgi",
        &[
            "default",
            "meta-llama/Llama-3.1-8B-Instruct",
            "Qwen/Qwen3-14B-Instruct",
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
    (
        "gmi",
        &[
            "zai-org/GLM-5.1-FP8",
            "deepseek-ai/DeepSeek-V3.2",
            "moonshotai/Kimi-K2.5",
            "anthropic/claude-sonnet-4.6",
        ],
    ),
    (
        "arcee",
        &[
            "trinity-large-preview",
            "trinity-large-thinking",
            "trinity-mini",
        ],
    ),
    (
        "xiaomi",
        &[
            "mimo-v2.5-pro",
            "mimo-v2.5",
            "mimo-v2-pro",
            "mimo-v2-omni",
            "mimo-v2-flash",
        ],
    ),
    ("tencent-tokenhub", &["hy3-preview"]),
    ("zai", &["glm-5.1", "glm-5.0", "glm-4.5-flash"]),
    ("minimax", &["MiniMax-M3", "MiniMax-M2.7"]),
    ("minimax-cn", &["MiniMax-M3", "MiniMax-M2.7"]),
    (
        "gemini",
        &[
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3.5-flash",
            "gemini-3.1-flash-lite-preview",
        ],
    ),
    (
        "google-gemini-cli",
        &[
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3.5-flash",
        ],
    ),
    (
        "google",
        &[
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-3.5-flash",
        ],
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
    if !trimmed.contains(':')
        && curated_provider_slugs()
            .iter()
            .any(|provider| provider.eq_ignore_ascii_case(trimmed))
    {
        return Err(AgentError::Config(format!(
            "`{trimmed}` is a provider, not a model. Use `{trimmed}:<model-id>`."
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
        "lmstudio" | "lm-studio" => "LM Studio (Local desktop app with built-in model server)",
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
    max_models: usize,
) -> Vec<ProviderCatalogEntry> {
    let providers = provider_slugs_for_config(config);
    let mut entries = Vec::new();

    for provider in providers {
        let models = provider_model_ids_for_config(&provider, config).await;
        if models.is_empty() {
            continue;
        }
        let total_models = models.len();
        let models = models.into_iter().take(max_models).collect();
        entries.push(ProviderCatalogEntry {
            provider,
            models,
            total_models,
        });
    }

    entries
}

fn is_local_openai_compatible_provider(provider: &str) -> bool {
    matches!(
        provider.trim().to_ascii_lowercase().as_str(),
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    )
}

fn local_provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" => Some(OLLAMA_LOCAL_DEFAULT_BASE_URL),
        "llama-cpp" => Some(LLAMA_CPP_DEFAULT_BASE_URL),
        "vllm" => Some(VLLM_DEFAULT_BASE_URL),
        "mlx" => Some(MLX_DEFAULT_BASE_URL),
        "apple-ane" => Some(APPLE_ANE_DEFAULT_BASE_URL),
        "sglang" => Some(SGLANG_DEFAULT_BASE_URL),
        "tgi" => Some(TGI_DEFAULT_BASE_URL),
        _ => None,
    }
}

fn local_provider_base_url_env_var(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" => Some("OLLAMA_BASE_URL"),
        "llama-cpp" => Some("LLAMA_CPP_BASE_URL"),
        "vllm" => Some("VLLM_BASE_URL"),
        "mlx" => Some("MLX_BASE_URL"),
        "apple-ane" => Some("APPLE_ANE_BASE_URL"),
        "sglang" => Some("SGLANG_BASE_URL"),
        "tgi" => Some("TGI_BASE_URL"),
        _ => None,
    }
}

fn local_provider_api_key(provider: &str) -> Option<String> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" => std::env::var("OLLAMA_LOCAL_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("OLLAMA_API_KEY").ok())
            .filter(|v| !v.trim().is_empty()),
        "llama-cpp" => std::env::var("LLAMA_CPP_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "vllm" => std::env::var("VLLM_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "mlx" => std::env::var("MLX_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "apple-ane" => std::env::var("APPLE_ANE_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "sglang" => std::env::var("SGLANG_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "tgi" => std::env::var("TGI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        _ => None,
    }
}

fn local_provider_resolved_base_url(provider: &str) -> Option<String> {
    local_provider_base_url_env_var(provider)
        .and_then(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| local_provider_default_base_url(provider).map(ToString::to_string))
}

fn parse_boolish_env(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
}

fn huggingface_live_catalog_disabled() -> bool {
    parse_boolish_env("HERMES_HF_CATALOG_DISABLE_LIVE")
}

fn huggingface_catalog_limit() -> usize {
    std::env::var("HERMES_HF_CATALOG_LIMIT")
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .map(|v| v.clamp(10, 500))
        .unwrap_or(120)
}

fn resolve_huggingface_catalog_endpoint_and_token() -> (String, Option<String>) {
    let base_url = std::env::var("HF_BASE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("HUGGINGFACE_BASE_URL").ok())
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| HUGGINGFACE_ROUTER_DEFAULT_BASE_URL.to_string());
    let token = std::env::var("HF_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("HUGGINGFACE_API_KEY").ok())
        .filter(|v| !v.trim().is_empty());
    (base_url, token)
}

fn openai_compatible_catalog_credentials(provider: &str) -> Option<(String, String)> {
    let (base_env, default_base, key_envs): (&str, &str, &[&str]) = match provider {
        "gmi" => (
            "GMI_BASE_URL",
            "https://api.gmi-serving.com/v1",
            &["GMI_API_KEY"],
        ),
        "arcee" => (
            "ARCEE_BASE_URL",
            "https://api.arcee.ai/api/v1",
            &["ARCEEAI_API_KEY", "ARCEE_API_KEY"],
        ),
        "xiaomi" => (
            "XIAOMI_BASE_URL",
            "https://api.xiaomimimo.com/v1",
            &["XIAOMI_API_KEY"],
        ),
        "tencent-tokenhub" => (
            "TOKENHUB_BASE_URL",
            "https://tokenhub.tencentmaas.com/v1",
            &["TOKENHUB_API_KEY"],
        ),
        _ => return None,
    };
    let token = key_envs.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })?;
    let base_url = std::env::var(base_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_base.to_string());
    Some((base_url, token))
}

async fn fetch_openai_compatible_live_models(base_url: &str, api_key: Option<&str>) -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let url = format!(
        "{}/models?output_modalities=all",
        base_url.trim_end_matches('/')
    );
    let client = reqwest::Client::new();
    let mut request = client.get(url);
    if let Some(key) = api_key.map(str::trim).filter(|v| !v.is_empty()) {
        request = request.bearer_auth(key);
    }
    let response = match request.send().await {
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
    let mut models = payload
        .get("data")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("id").and_then(Value::as_str))
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();
    if models.is_empty() {
        return models;
    }
    let mut seen = HashSet::new();
    models.retain(|model| seen.insert(model.to_ascii_lowercase()));
    models
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
    let normalized = canonical_provider_id(provider);
    let curated = provider_curated_models(&normalized);
    if curated.is_empty() {
        return Vec::new();
    }
    if let Some(cached) = load_provider_catalog_cache(&normalized) {
        if !cached.is_empty() {
            return cached;
        }
    }

    let computed = if normalized == "bedrock" {
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
    } else if matches!(normalized.as_str(), "nous" | "nous-api") {
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
    } else if normalized == "huggingface" {
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
        normalized.as_str(),
        "gmi" | "arcee" | "xiaomi" | "tencent-tokenhub"
    ) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        if let Some((base_url, token)) = openai_compatible_catalog_credentials(&normalized) {
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
    } else if is_local_openai_compatible_provider(&normalized) {
        let mut seen: HashSet<String> = HashSet::new();
        let mut merged: Vec<String> = Vec::new();
        if let Some(base_url) = local_provider_resolved_base_url(&normalized) {
            let live = fetch_openai_compatible_live_models(
                base_url.as_str(),
                local_provider_api_key(&normalized).as_deref(),
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
    use crate::test_env_lock;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use hermes_intelligence::models_dev::ModelsDevClient;
    use serde_json::json;

    use super::{
        cached_provider_catalog_status, is_models_dev_preferred_provider,
        load_provider_catalog_cache, merge_with_models_dev, normalize_provider_model,
        persist_provider_catalog_cache, provider_catalog_cache_path, provider_catalog_entries,
        provider_catalog_entries_for_config, provider_curated_models,
        provider_model_ids_for_config, provider_model_ids_with_client, provider_picker_description,
        provider_slug_from_provider_model, provider_slugs_for_config,
        resolve_huggingface_catalog_endpoint_and_token, DEFAULT_VISIBLE_MODELS_PER_PROVIDER,
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
        assert!(provider_curated_models("gmi").contains(&"zai-org/GLM-5.1-FP8"));
        assert!(provider_curated_models("gmicloud").contains(&"deepseek-ai/DeepSeek-V3.2"));
        assert!(provider_curated_models("arcee-ai").contains(&"trinity-mini"));
        assert!(provider_curated_models("mimo").contains(&"mimo-v2.5-pro"));
        assert!(provider_curated_models("tokenhub").contains(&"hy3-preview"));
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

        let entries = provider_catalog_entries_for_config(&cfg, 1).await;
        let entry = entries
            .iter()
            .find(|entry| entry.provider == "baidu-coding")
            .expect("custom provider entry");
        assert_eq!(entry.models, vec!["kimi-k2.5"]);
        assert_eq!(entry.total_models, 2);
    }

    #[tokio::test]
    async fn default_visible_models_per_provider_shows_first_fifty() {
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

        let entries =
            provider_catalog_entries_for_config(&cfg, DEFAULT_VISIBLE_MODELS_PER_PROVIDER).await;
        let entry = entries
            .iter()
            .find(|entry| entry.provider == "wide-provider")
            .expect("wide provider entry");

        assert_eq!(entry.total_models, 60);
        assert_eq!(entry.models.len(), 50);
        assert_eq!(entry.models.first().map(String::as_str), Some("model-00"));
        assert_eq!(entry.models.last().map(String::as_str), Some("model-49"));
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
