use std::sync::Arc;
use std::time::Instant;

use hermes_agent::provider::{
    AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
};
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_config::GatewayConfig;
use hermes_core::LlmProvider;
use hermes_server_client::ServerLlmProvider;

use super::api_keys::{
    allow_no_api_key, provider_api_key_from_env, resolve_api_key_literal_or_env_ref,
};
use super::cache::{ProviderCacheEntry, provider_cache, provider_cache_key, prune_provider_cache};
use super::names::normalize_runtime_provider_name;
use super::no_backend::NoBackendProvider;
use super::resolve::resolve_provider_and_model;
use super::urls::{
    OPENAI_CODEX_BASE_URL, STEPFUN_BASE_URL, provider_base_url_from_env, provider_default_base_url,
};

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    if config.server.enabled && config.server.api_ready() {
        match ServerLlmProvider::new(config.server.clone(), hermes_config::hermes_home()) {
            Ok(provider) => {
                tracing::info!(
                    base_url = %config.server.effective_llm_base_url(),
                    "using remote Flowy server LLM provider"
                );
                return Arc::new(provider);
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "server.enabled but remote LLM provider init failed; falling back to local provider config"
                );
            }
        }
    }

    let (provider_name, model_name) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(provider_name.as_str());

    let provider_config = config
        .llm_providers
        .get(provider_name.as_str())
        .or_else(|| config.llm_providers.get(runtime_provider.as_str()));
    let provider_config = provider_config.or_else(|| {
        config.llm_providers.iter().find_map(|(name, cfg)| {
            if name.eq_ignore_ascii_case(provider_name.as_str())
                || name.eq_ignore_ascii_case(runtime_provider.as_str())
            {
                Some(cfg)
            } else {
                None
            }
        })
    });

    let default_base_url = provider_default_base_url(provider_name.as_str())
        .or_else(|| provider_default_base_url(runtime_provider.as_str()));
    let base_url = provider_config
        .and_then(|c| c.base_url.clone())
        .or_else(|| provider_base_url_from_env(provider_name.as_str()))
        .or_else(|| provider_base_url_from_env(runtime_provider.as_str()))
        .or_else(|| default_base_url.map(ToString::to_string));

    let api_key = provider_config
        .and_then(|c| c.api_key.as_deref())
        .and_then(resolve_api_key_literal_or_env_ref)
        .or_else(|| {
            provider_config
                .and_then(|c| c.api_key_env.as_deref())
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .and_then(|name| std::env::var(name).ok())
                .filter(|v| !v.trim().is_empty())
        })
        .or_else(|| provider_api_key_from_env(provider_name.as_str()))
        .or_else(|| provider_api_key_from_env(runtime_provider.as_str()));

    let local_no_key_ok = allow_no_api_key(
        provider_name.as_str(),
        runtime_provider.as_str(),
        base_url.as_deref(),
    );

    let api_key = match api_key {
        Some(k) => k,
        None if local_no_key_ok => "local-no-key".to_string(),
        None => {
            tracing::warn!(
                provider = %provider_name,
                runtime_provider = %runtime_provider,
                model = %model,
                impact = "llm requests will fail until a valid API key is configured",
                "No API key for provider; using NoBackendProvider"
            );
            return Arc::new(NoBackendProvider {
                model: model.to_string(),
            });
        }
    };

    let cache_key = provider_cache_key(
        runtime_provider.as_str(),
        model_name.as_str(),
        base_url.as_deref(),
        &api_key,
    );
    {
        let mut guard = provider_cache().lock().unwrap();
        if let Some(entry) = guard.get_mut(&cache_key) {
            entry.last_used = Instant::now();
            return entry.provider.clone();
        }
    }

    let built: Arc<dyn LlmProvider> = match runtime_provider.as_str() {
        "openai" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "openai-codex" | "codex" => {
            let mut p = OpenAiProvider::new(&api_key).with_model(model_name.as_str());
            p = p.with_base_url(
                base_url
                    .clone()
                    .unwrap_or_else(|| OPENAI_CODEX_BASE_URL.to_string()),
            );
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "openrouter" => {
            let p = OpenRouterProvider::new(&api_key).with_model(model_name.as_str());
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "qwen" | "qwen-oauth" => {
            let mut p = QwenProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "kimi" | "moonshot" => {
            let mut p = KimiProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "minimax" => {
            let mut p = MiniMaxProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "stepfun" => {
            let url = base_url
                .clone()
                .unwrap_or_else(|| STEPFUN_BASE_URL.to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
                as Arc<dyn LlmProvider>
        }
        "nous" => {
            let mut p = NousProvider::new(&api_key).with_model(model_name.as_str());
            if let Some(url) = base_url.clone() {
                p = p.with_base_url(url);
            }
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "copilot" => {
            let p = CopilotProvider::new(
                base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.github.com/copilot".to_string()),
                &api_key,
            )
            .with_model(model_name.as_str());
            Arc::new(p) as Arc<dyn LlmProvider>
        }
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi" => {
            let url = base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
                as Arc<dyn LlmProvider>
        }
        _ => {
            let url = base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(GenericProvider::new(url, &api_key, model_name.as_str()))
                as Arc<dyn LlmProvider>
        }
    };
    {
        let mut guard = provider_cache().lock().unwrap();
        guard.insert(
            cache_key,
            ProviderCacheEntry {
                provider: built.clone(),
                last_used: Instant::now(),
            },
        );
        prune_provider_cache(&mut guard);
    }
    built
}
