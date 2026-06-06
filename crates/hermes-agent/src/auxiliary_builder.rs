//! Wires the abstract [`AuxiliaryClient`] (defined in `hermes-intelligence`)
//! to concrete [`LlmProvider`] implementations from this crate.
//!
//! The intelligence crate cannot depend on `hermes-agent` (would create a
//! cycle), so the binary layer owns the provider construction. This module
//! reads standard environment variables and assembles the auto-detect chain
//! that mirrors the Python `_get_provider_chain` ordering.

use std::sync::Arc;

use hermes_core::LlmProvider;
use hermes_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryConfig, AuxiliarySource, ProviderCandidate,
};

use crate::provider::{
    openai_codex_provider, AnthropicProvider, GenericProvider, OpenRouterProvider,
};
use crate::provider_profiles;
use crate::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};

/// Default auxiliary models per source. Mirrors the Python
/// `_API_KEY_PROVIDER_AUX_MODELS` table — chosen to be cheap and fast.
mod default_models {
    pub const OPENROUTER: &str = "google/gemini-3-flash-preview";
    pub const ANTHROPIC: &str = "claude-haiku-4-5-20251001";
    pub const OPENAI: &str = "gpt-4o-mini";
    pub const ZAI: &str = "glm-4.5-flash";
    pub const KIMI: &str = "kimi-k2-turbo-preview";
    pub const MINIMAX: &str = "MiniMax-M3";
    pub const GEMINI: &str = "gemini-3.5-flash";
    pub const GMI: &str = "google/gemini-3.1-flash-lite-preview";
    pub const TENCENT_TOKENHUB: &str = "hy3-preview";
}

mod default_base_urls {
    pub const OPENAI: &str = "https://api.openai.com/v1";
    pub const OPENAI_CODEX: &str = "https://chatgpt.com/backend-api/codex";
    pub const DEEPSEEK: &str = "https://api.deepseek.com/v1";
    pub const GEMINI: &str = "https://generativelanguage.googleapis.com/v1beta/openai";
    pub const XAI: &str = "https://api.x.ai/v1";
    pub const XIAOMI: &str = "https://api.xiaomimimo.com/v1";
    pub const GMI: &str = "https://api.gmi-serving.com/v1";
    pub const HUGGINGFACE: &str = "https://router.huggingface.co/v1";
    pub const ZAI: &str = "https://api.z.ai/api/paas/v4";
    pub const MINIMAX: &str = "https://api.minimax.io/v1";
    pub const MINIMAX_CN: &str = "https://api.minimaxi.com/v1";
    pub const NOVITA: &str = "https://api.novita.ai/openai/v1";
    pub const NVIDIA: &str = "https://integrate.api.nvidia.com/v1";
    pub const ARCEE: &str = "https://api.arcee.ai/api/v1";
    pub const TENCENT_TOKENHUB: &str = "https://tokenhub.tencentmaas.com/v1";
    pub const OLLAMA_LOCAL: &str = "http://127.0.0.1:11434/v1";
    pub const LLAMA_CPP: &str = "http://127.0.0.1:8080/v1";
    pub const VLLM: &str = "http://127.0.0.1:8000/v1";
    pub const MLX: &str = "http://127.0.0.1:8080/v1";
    pub const APPLE_ANE: &str = "http://127.0.0.1:8081/v1";
    pub const SGLANG: &str = "http://127.0.0.1:30000/v1";
    pub const TGI: &str = "http://127.0.0.1:8082/v1";
}

/// Returned by [`build_default_auxiliary_client`] alongside the client so
/// callers can introspect what was wired (e.g. for `hermes status`).
#[derive(Debug, Clone)]
pub struct AuxiliaryWiringSummary {
    pub registered: Vec<String>,
    pub skipped: Vec<String>,
}

/// Runtime-selected main provider/model to try before the cheap auxiliary chain.
///
/// This is the Rust equivalent of upstream's `main_runtime` path: `auto`
/// auxiliary routing first uses the active chat provider and model when a
/// working client can be built. Explicit per-task provider/model overrides are
/// still resolved inside `AuxiliaryClient` and bypass the auto chain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuxiliaryMainRuntime {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub supports_vision: bool,
}

impl AuxiliaryMainRuntime {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: Some(provider.into()),
            model: Some(model.into()),
            supports_vision: true,
            ..Default::default()
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(api_key.into());
        self
    }

    pub fn with_api_key_env(mut self, env_var: impl Into<String>) -> Self {
        self.api_key_env = Some(env_var.into());
        self
    }

    pub fn with_supports_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }
}

/// Build an [`AuxiliaryClient`] from environment variables.
///
/// Resolution rules:
///
/// * `OPENROUTER_API_KEY` → registers an `openrouter` candidate
/// * `ANTHROPIC_API_KEY` → registers an `anthropic` candidate
/// * `HERMES_OPENAI_API_KEY` (+ optional `OPENAI_BASE_URL`) → registers a
///   `custom` candidate (`OPENAI_API_KEY` kept as legacy fallback)
/// * `ZAI_API_KEY`, `KIMI_API_KEY`, `MINIMAX_API_KEY`, `GEMINI_API_KEY` →
///   register direct-key candidates (OpenAI-compatible base URLs)
///
/// Order matches Python: OpenRouter > Custom > Anthropic > direct keys.
pub fn build_default_auxiliary_client(
    config: AuxiliaryConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client_with_main_runtime(config, None)
}

pub fn build_auxiliary_client_with_main_runtime(
    config: AuxiliaryConfig,
    main_runtime: Option<AuxiliaryMainRuntime>,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    let mut summary = AuxiliaryWiringSummary {
        registered: Vec::new(),
        skipped: Vec::new(),
    };
    let mut builder = AuxiliaryClient::builder().config(config);
    let main_label = main_runtime
        .as_ref()
        .and_then(|main| register_main_runtime(&mut builder, &mut summary, main));

    if main_label.as_deref() == Some("openrouter") {
        summary
            .skipped
            .push("openrouter (covered by main runtime)".into());
    } else if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> = Arc::new(
                OpenRouterProvider::new(key.trim())
                    .with_model(default_models::OPENROUTER)
                    .with_http_referer("https://hermes-agent.nousresearch.com")
                    .with_x_title("Hermes Agent"),
            );
            add_candidate(
                &mut builder,
                ProviderCandidate::new(
                    AuxiliarySource::OpenRouter,
                    default_models::OPENROUTER,
                    provider,
                ),
            );
            summary.registered.push("openrouter".into());
        } else {
            summary.skipped.push("openrouter (empty)".into());
        }
    } else {
        summary.skipped.push("openrouter (no key)".into());
    }

    // Custom OpenAI-compatible endpoint (covers HERMES_OPENAI_API_KEY and
    // legacy OPENAI_API_KEY + custom base URLs). We mark it `Custom` rather
    // than the OpenAI source so that the chain dedup logic doesn't collide
    // with explicitly-named providers.
    if main_label.as_deref() == Some("custom") {
        summary
            .skipped
            .push("custom (covered by main runtime)".into());
    } else if let Some(key) = std::env::var("HERMES_OPENAI_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok())
    {
        if !key.trim().is_empty() {
            let base_url = std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            let model = std::env::var("OPENAI_AUXILIARY_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| default_models::OPENAI.into());
            let provider: Arc<dyn LlmProvider> =
                Arc::new(GenericProvider::new(base_url, key.trim(), model.clone()));
            add_candidate(
                &mut builder,
                ProviderCandidate::new(AuxiliarySource::Custom, model, provider),
            );
            summary.registered.push("custom".into());
        } else {
            summary.skipped.push("custom (empty key)".into());
        }
    } else {
        summary.skipped.push("custom (no key)".into());
    }

    if main_label.as_deref() == Some("anthropic") {
        summary
            .skipped
            .push("anthropic (covered by main runtime)".into());
    } else if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> =
                Arc::new(AnthropicProvider::new(key.trim()).with_model(default_models::ANTHROPIC));
            add_candidate(
                &mut builder,
                ProviderCandidate::new(
                    AuxiliarySource::Anthropic,
                    default_models::ANTHROPIC,
                    provider,
                ),
            );
            summary.registered.push("anthropic".into());
        }
    } else {
        summary.skipped.push("anthropic (no key)".into());
    }

    register_direct_key(
        &mut builder,
        &mut summary,
        "ZAI_API_KEY",
        "zai",
        "https://api.z.ai/api/coding/paas/v4",
        default_models::ZAI,
        main_label.as_deref(),
    );
    register_kimi_direct_key(&mut builder, &mut summary, main_label.as_deref());
    register_direct_key(
        &mut builder,
        &mut summary,
        "MINIMAX_API_KEY",
        "minimax",
        default_base_urls::MINIMAX,
        default_models::MINIMAX,
        main_label.as_deref(),
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "MINIMAX_CN_API_KEY",
        "minimax-cn",
        default_base_urls::MINIMAX_CN,
        default_models::MINIMAX,
        main_label.as_deref(),
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "GEMINI_API_KEY",
        "gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        default_models::GEMINI,
        main_label.as_deref(),
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "GMI_API_KEY",
        "gmi",
        default_base_urls::GMI,
        default_models::GMI,
        main_label.as_deref(),
    );
    register_direct_key(
        &mut builder,
        &mut summary,
        "TOKENHUB_API_KEY",
        "tencent-tokenhub",
        default_base_urls::TENCENT_TOKENHUB,
        default_models::TENCENT_TOKENHUB,
        main_label.as_deref(),
    );

    let client = builder.build();
    (client, summary)
}

fn add_candidate(
    builder: &mut hermes_intelligence::auxiliary::AuxiliaryClientBuilder,
    candidate: ProviderCandidate,
) {
    let temp_builder = std::mem::take(builder);
    *builder = temp_builder.add_candidate(candidate);
}

fn register_main_runtime(
    builder: &mut hermes_intelligence::auxiliary::AuxiliaryClientBuilder,
    summary: &mut AuxiliaryWiringSummary,
    main: &AuxiliaryMainRuntime,
) -> Option<String> {
    let provider = main.provider.as_deref().map(str::trim).unwrap_or_default();
    let model = main.model.as_deref().map(str::trim).unwrap_or_default();
    if provider.is_empty() || model.is_empty() {
        summary
            .skipped
            .push("main runtime (missing provider/model)".into());
        return None;
    }

    let label = canonical_provider_label(provider);
    let Some(candidate) = build_main_runtime_candidate(&label, model, main) else {
        summary
            .skipped
            .push(format!("main:{label} (no working client)"));
        return None;
    };

    add_candidate(builder, candidate);
    summary.registered.push(format!("main:{label}"));
    Some(label)
}

fn build_main_runtime_candidate(
    label: &str,
    model: &str,
    main: &AuxiliaryMainRuntime,
) -> Option<ProviderCandidate> {
    let base_url = main
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let api_key = resolve_main_api_key(main, label).or_else(|| {
        provider_allows_no_api_key(label, base_url.as_deref()).then(|| "local-no-key".to_string())
    })?;
    let normalized_model = crate::model_normalize::normalize_model_for_provider(model, label);
    let model = normalized_model.as_str();

    let provider: Arc<dyn LlmProvider> = match label {
        "openrouter" => Arc::new(
            OpenRouterProvider::new(api_key)
                .with_model(model)
                .with_http_referer("https://hermes-agent.nousresearch.com")
                .with_x_title("Hermes Agent"),
        ),
        "openai-codex" => Arc::new(openai_codex_provider(api_key, model, base_url.as_deref())),
        "anthropic" => {
            let mut provider = AnthropicProvider::new(api_key).with_model(model);
            if let Some(url) = base_url {
                provider = provider.with_base_url(url);
            }
            Arc::new(provider)
        }
        "nous" => {
            let mut provider = NousProvider::new(api_key).with_model(model);
            if let Some(url) = base_url {
                provider = provider.with_base_url(url);
            }
            Arc::new(provider)
        }
        "qwen" | "qwen-oauth" => {
            let mut provider = QwenProvider::new(api_key).with_model(model);
            if let Some(url) = base_url {
                provider = provider.with_base_url(url);
            }
            Arc::new(provider)
        }
        "kimi" | "moonshot" => {
            let mut provider = KimiProvider::new(api_key).with_model(model);
            if let Some(url) = base_url {
                provider = provider.with_base_url(url);
            }
            Arc::new(provider)
        }
        "minimax" => {
            let mut provider = MiniMaxProvider::new(api_key).with_model(model);
            if let Some(url) = base_url {
                provider = provider.with_base_url(url);
            }
            Arc::new(provider)
        }
        "copilot" | "copilot-acp" => Arc::new(
            CopilotProvider::new(
                base_url.unwrap_or_else(|| "https://api.githubcopilot.com".to_string()),
                api_key,
            )
            .with_model(model),
        ),
        _ => {
            let url = base_url.or_else(|| default_base_url(label).map(str::to_string))?;
            Arc::new(GenericProvider::new(url, api_key, model))
        }
    };

    Some(
        ProviderCandidate::new(source_for_label(label), model, provider)
            .with_supports_vision(main.supports_vision),
    )
}

fn canonical_provider_label(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => "anthropic".to_string(),
        "github" | "github-copilot" | "github-models" => "copilot".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "openai-codex" | "codex" => "openai-codex".to_string(),
        other => other.to_string(),
    }
}

fn source_for_label(label: &str) -> AuxiliarySource {
    match label {
        "openrouter" => AuxiliarySource::OpenRouter,
        "nous" => AuxiliarySource::Nous,
        "custom" => AuxiliarySource::Custom,
        "anthropic" => AuxiliarySource::Anthropic,
        other => AuxiliarySource::DirectKey(other.to_string()),
    }
}

fn resolve_main_api_key(main: &AuxiliaryMainRuntime, label: &str) -> Option<String> {
    main.api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            main.api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .and_then(|env_var| std::env::var(env_var).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| provider_api_key_from_env(label))
}

fn provider_api_key_from_env(label: &str) -> Option<String> {
    let env_vars: &[&str] = match label {
        "openai" => &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openai-codex" => &["HERMES_OPENAI_CODEX_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "anthropic" => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        "nous" => &["NOUS_API_KEY"],
        "qwen" | "qwen-oauth" => &["DASHSCOPE_API_KEY"],
        "kimi" | "moonshot" => &[
            "KIMI_API_KEY",
            "KIMI_CODING_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ],
        "minimax" => &["MINIMAX_API_KEY"],
        "minimax-cn" => &["MINIMAX_CN_API_KEY", "MINIMAX_API_KEY"],
        "copilot" | "copilot-acp" => &["GITHUB_COPILOT_TOKEN", "COPILOT_GITHUB_TOKEN"],
        "deepseek" => &["DEEPSEEK_API_KEY"],
        "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "gmi" => &["GMI_API_KEY"],
        "tencent-tokenhub" => &["TOKENHUB_API_KEY"],
        "huggingface" => &["HF_TOKEN", "HUGGINGFACE_API_KEY"],
        "zai" => &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"],
        "novita" => &["NOVITA_API_KEY"],
        "nvidia" => &["NVIDIA_API_KEY"],
        "arcee" => &["ARCEEAI_API_KEY", "ARCEE_API_KEY"],
        "ollama-cloud" => &["OLLAMA_API_KEY"],
        "ollama-local" => &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"],
        "llama-cpp" => &["LLAMA_CPP_API_KEY"],
        "vllm" => &["VLLM_API_KEY"],
        "mlx" => &["MLX_API_KEY"],
        "apple-ane" => &["APPLE_ANE_API_KEY"],
        "sglang" => &["SGLANG_API_KEY"],
        "tgi" => &["TGI_API_KEY", "HUGGINGFACE_API_KEY"],
        _ => &[],
    };
    env_vars.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

fn default_base_url(label: &str) -> Option<&'static str> {
    match label {
        "openai" | "custom" => Some(default_base_urls::OPENAI),
        "openai-codex" => Some(default_base_urls::OPENAI_CODEX),
        "deepseek" => Some(default_base_urls::DEEPSEEK),
        "gemini" | "google" => Some(default_base_urls::GEMINI),
        "xai" => Some(default_base_urls::XAI),
        "xiaomi" => Some(default_base_urls::XIAOMI),
        "gmi" => Some(default_base_urls::GMI),
        "tencent-tokenhub" => Some(default_base_urls::TENCENT_TOKENHUB),
        "huggingface" => Some(default_base_urls::HUGGINGFACE),
        "zai" => Some(default_base_urls::ZAI),
        "minimax" => Some(default_base_urls::MINIMAX),
        "minimax-cn" => Some(default_base_urls::MINIMAX_CN),
        "novita" => Some(default_base_urls::NOVITA),
        "nvidia" => Some(default_base_urls::NVIDIA),
        "arcee" => Some(default_base_urls::ARCEE),
        "ollama-local" => Some(default_base_urls::OLLAMA_LOCAL),
        "llama-cpp" => Some(default_base_urls::LLAMA_CPP),
        "vllm" => Some(default_base_urls::VLLM),
        "mlx" => Some(default_base_urls::MLX),
        "apple-ane" => Some(default_base_urls::APPLE_ANE),
        "sglang" => Some(default_base_urls::SGLANG),
        "tgi" => Some(default_base_urls::TGI),
        _ => None,
    }
}

fn kimi_base_url_from_env_or_key(api_key: &str) -> String {
    std::env::var("KIMI_BASE_URL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            provider_profiles::kimi_base_url_for_api_key(
                api_key,
                provider_profiles::KIMI_LEGACY_BASE_URL,
            )
            .to_string()
        })
}

fn provider_allows_no_api_key(label: &str, base_url: Option<&str>) -> bool {
    matches!(
        label,
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    ) || base_url.is_some_and(is_loopback_base_url)
}

fn is_loopback_base_url(base_url: &str) -> bool {
    let trimmed = base_url.trim().to_ascii_lowercase();
    trimmed.starts_with("http://127.")
        || trimmed.starts_with("http://localhost")
        || trimmed.starts_with("http://[::1]")
}

fn register_direct_key(
    builder: &mut hermes_intelligence::auxiliary::AuxiliaryClientBuilder,
    summary: &mut AuxiliaryWiringSummary,
    env_var: &str,
    label: &str,
    base_url: &str,
    default_model: &str,
    skip_label: Option<&str>,
) {
    if skip_label == Some(label) {
        summary
            .skipped
            .push(format!("{label} (covered by main runtime)"));
        return;
    }

    let owned;
    let key = match std::env::var(env_var) {
        Ok(v) if !v.trim().is_empty() => {
            owned = v;
            owned.trim()
        }
        _ => {
            summary.skipped.push(format!("{label} (no key)"));
            return;
        }
    };
    let provider: Arc<dyn LlmProvider> =
        Arc::new(GenericProvider::new(base_url, key, default_model));
    add_candidate(
        builder,
        ProviderCandidate::new(
            AuxiliarySource::DirectKey(label.to_string()),
            default_model,
            provider,
        ),
    );
    summary.registered.push(label.to_string());
}

fn register_kimi_direct_key(
    builder: &mut hermes_intelligence::auxiliary::AuxiliaryClientBuilder,
    summary: &mut AuxiliaryWiringSummary,
    skip_label: Option<&str>,
) {
    const LABEL: &str = "kimi";
    if skip_label == Some(LABEL) {
        summary
            .skipped
            .push(format!("{LABEL} (covered by main runtime)"));
        return;
    }

    let Some(key) = ["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
        .iter()
        .find_map(|env_var| {
            std::env::var(env_var)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    else {
        summary.skipped.push(format!("{LABEL} (no key)"));
        return;
    };
    let base_url = kimi_base_url_from_env_or_key(key.as_str());
    let provider: Arc<dyn LlmProvider> = Arc::new(
        GenericProvider::new(base_url, key, default_models::KIMI)
            .with_provider_profile("kimi-coding"),
    );
    add_candidate(
        builder,
        ProviderCandidate::new(
            AuxiliarySource::DirectKey(LABEL.to_string()),
            default_models::KIMI,
            provider,
        ),
    );
    summary.registered.push(LABEL.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "HERMES_OPENAI_API_KEY",
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_AUXILIARY_MODEL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "NOUS_API_KEY",
        "GMI_API_KEY",
        "TOKENHUB_API_KEY",
        "DEEPSEEK_API_KEY",
        "DASHSCOPE_API_KEY",
        "GITHUB_COPILOT_TOKEN",
        "COPILOT_GITHUB_TOKEN",
        "XIAOMI_API_KEY",
        "XAI_API_KEY",
        "HF_TOKEN",
        "ZAI_API_KEY",
        "GLM_API_KEY",
        "KIMI_API_KEY",
        "KIMI_CODING_API_KEY",
        "MOONSHOT_API_KEY",
        "KIMI_CN_API_KEY",
        "MINIMAX_API_KEY",
        "MINIMAX_CN_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    ];

    /// Save current env, then clear; restored when the guard drops.
    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let mut previous = Vec::new();
            for k in KEYS {
                previous.push((*k, std::env::var(k).ok()));
                std::env::remove_var(k);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.previous.drain(..) {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    /// Run all env-mutating cases serially in a single test to avoid races
    /// with parallel cargo test workers (env is process-global).
    #[test]
    fn build_default_auxiliary_client_scenarios() {
        let _g = EnvGuard::clear();

        // Scenario 1: no keys → empty chain.
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 0, "empty env produced non-empty chain");
            assert!(summary.registered.is_empty());
            assert!(!summary.skipped.is_empty());
        }

        // Scenario 2: only OpenRouter.
        std::env::set_var("OPENROUTER_API_KEY", "sk-test");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 1);
            assert_eq!(client.chain_labels(), vec!["openrouter"]);
            assert_eq!(summary.registered, vec!["openrouter"]);
        }
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 3: main OpenRouter runtime wins over the cheap OpenRouter default.
        std::env::set_var("OPENROUTER_API_KEY", "sk-main-or");
        {
            let (client, summary) = build_auxiliary_client_with_main_runtime(
                AuxiliaryConfig::default(),
                Some(AuxiliaryMainRuntime::new(
                    "openrouter",
                    "anthropic/claude-sonnet-4.6",
                )),
            );
            assert_eq!(client.chain_len(), 1);
            assert_eq!(client.chain_labels(), vec!["openrouter"]);
            assert_eq!(
                client.chain_entries(),
                vec![(
                    "openrouter".to_string(),
                    "anthropic/claude-sonnet-4.6".to_string(),
                    true,
                )]
            );
            assert_eq!(summary.registered, vec!["main:openrouter"]);
            assert!(summary
                .skipped
                .contains(&"openrouter (covered by main runtime)".to_string()));
        }
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 4: if main cannot build a client, the cheap chain is used.
        std::env::set_var("OPENROUTER_API_KEY", "sk-fallback-or");
        {
            let (client, summary) = build_auxiliary_client_with_main_runtime(
                AuxiliaryConfig::default(),
                Some(AuxiliaryMainRuntime::new("anthropic", "claude-opus-4-6")),
            );
            assert_eq!(client.chain_labels(), vec!["openrouter"]);
            assert!(summary
                .skipped
                .contains(&"main:anthropic (no working client)".to_string()));
        }
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 5: non-aggregator main providers also use the main model first.
        std::env::set_var("DEEPSEEK_API_KEY", "sk-deepseek");
        std::env::set_var("OPENROUTER_API_KEY", "sk-or-fallback");
        {
            let (client, summary) = build_auxiliary_client_with_main_runtime(
                AuxiliaryConfig::default(),
                Some(
                    AuxiliaryMainRuntime::new("deepseek", "deepseek-chat")
                        .with_supports_vision(false),
                ),
            );
            assert_eq!(client.chain_labels(), vec!["deepseek", "openrouter"]);
            assert_eq!(
                client.chain_entries()[0],
                ("deepseek".to_string(), "deepseek-chat".to_string(), false,)
            );
            assert_eq!(summary.registered[0], "main:deepseek");
        }
        std::env::remove_var("DEEPSEEK_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");

        // Scenario 6: Gemini direct-key auxiliary default uses the renamed model.
        std::env::set_var("GEMINI_API_KEY", "sk-gemini");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_labels(), vec!["gemini"]);
            assert_eq!(
                client.chain_entries(),
                vec![("gemini".to_string(), "gemini-3.5-flash".to_string(), true,)]
            );
            assert_eq!(summary.registered, vec!["gemini"]);
        }
        std::env::remove_var("GEMINI_API_KEY");

        // Scenario 7: MiniMax direct API defaults to M3 and avoids highspeed.
        std::env::set_var("MINIMAX_API_KEY", "sk-minimax");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_labels(), vec!["minimax"]);
            assert_eq!(
                client.chain_entries(),
                vec![("minimax".to_string(), "MiniMax-M3".to_string(), true,)]
            );
            assert_eq!(summary.registered, vec!["minimax"]);
            assert!(!client.chain_entries()[0]
                .1
                .to_ascii_lowercase()
                .contains("highspeed"));
        }
        std::env::remove_var("MINIMAX_API_KEY");

        // Scenario 8: MiniMax CN is a first-class direct-key auxiliary provider.
        std::env::set_var("MINIMAX_CN_API_KEY", "sk-minimax-cn");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_labels(), vec!["minimax-cn"]);
            assert_eq!(
                client.chain_entries(),
                vec![("minimax-cn".to_string(), "MiniMax-M3".to_string(), true,)]
            );
            assert_eq!(summary.registered, vec!["minimax-cn"]);
            assert!(!client.chain_entries()[0]
                .1
                .to_ascii_lowercase()
                .contains("highspeed"));
        }
        std::env::remove_var("MINIMAX_CN_API_KEY");

        // Scenario 9: Kimi Code direct API keys register the Kimi auxiliary source.
        std::env::set_var("KIMI_CODING_API_KEY", "sk-kimi-aux");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_labels(), vec!["kimi"]);
            assert_eq!(
                client.chain_entries(),
                vec![(
                    "kimi".to_string(),
                    "kimi-k2-turbo-preview".to_string(),
                    true,
                )]
            );
            assert_eq!(summary.registered, vec!["kimi"]);
        }
        std::env::remove_var("KIMI_CODING_API_KEY");

        // Scenario 10: full chain, deterministic order.
        std::env::set_var("OPENROUTER_API_KEY", "sk-or");
        std::env::set_var("HERMES_OPENAI_API_KEY", "sk-hermes-oa");
        std::env::set_var("OPENAI_API_KEY", "sk-oa-legacy");
        std::env::set_var("ANTHROPIC_API_KEY", "sk-an");
        std::env::set_var("ZAI_API_KEY", "z");
        std::env::set_var("GMI_API_KEY", "sk-gmi");
        std::env::set_var("TOKENHUB_API_KEY", "sk-tokenhub");
        {
            let (client, _) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(
                client.chain_labels(),
                vec![
                    "openrouter",
                    "custom",
                    "anthropic",
                    "zai",
                    "gmi",
                    "tencent-tokenhub"
                ]
            );
        }
    }
}
