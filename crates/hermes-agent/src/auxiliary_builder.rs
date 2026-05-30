//! Wires the abstract [`AuxiliaryClient`] (defined in `hermes-intelligence`)
//! to concrete [`LlmProvider`] implementations from this crate.

use std::collections::HashMap;
use std::sync::Arc;

use hermes_config::config::LlmProviderConfig;
use hermes_core::LlmProvider;
use hermes_intelligence::auxiliary::{
    AuxiliaryClient, AuxiliaryConfig, AuxiliarySource, AuxiliaryTask, FallbackChainEntry,
    ProviderCandidate, ProviderChain,
};

use crate::provider::{AnthropicProvider, GenericProvider, OpenRouterProvider};

mod default_models {
    pub const OPENROUTER: &str = "google/gemini-3-flash-preview";
    pub const ANTHROPIC: &str = "claude-haiku-4-5-20251001";
    pub const OPENAI: &str = "gpt-4o-mini";
    pub const ZAI: &str = "glm-4.5-flash";
    pub const KIMI: &str = "kimi-k2-turbo-preview";
    pub const MINIMAX: &str = "MiniMax-M2.7";
    pub const GEMINI: &str = "gemini-3-flash-preview";
}

#[derive(Debug, Clone)]
pub struct AuxiliaryWiringSummary {
    pub registered: Vec<String>,
    pub skipped: Vec<String>,
}

/// Inputs for [`build_auxiliary_client`] — env chain plus optional config primary bridge.
#[derive(Debug, Clone, Default)]
pub struct AuxiliaryBuildParams {
    pub config: AuxiliaryConfig,
    pub primary_provider: Option<String>,
    pub primary_model: Option<String>,
    pub llm_providers: HashMap<String, LlmProviderConfig>,
}

pub fn build_default_auxiliary_client(
    config: AuxiliaryConfig,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    build_auxiliary_client(AuxiliaryBuildParams {
        config,
        ..Default::default()
    })
}

/// Build an [`AuxiliaryClient`] from environment variables and optional config primary bridge.
pub fn build_auxiliary_client(
    params: AuxiliaryBuildParams,
) -> (AuxiliaryClient, AuxiliaryWiringSummary) {
    let mut summary = AuxiliaryWiringSummary {
        registered: Vec::new(),
        skipped: Vec::new(),
    };
    let mut chain_candidates: Vec<ProviderCandidate> = Vec::new();

    if let Some(primary) = maybe_primary_vision_candidate(&params, &mut summary) {
        chain_candidates.push(primary);
    }

    let (env_candidates, env_summary) = collect_env_candidates(params.config.clone());
    summary.registered.extend(env_summary.registered);
    summary.skipped.extend(env_summary.skipped);
    chain_candidates.extend(env_candidates);

    let mut builder = AuxiliaryClient::builder()
        .config(params.config.clone())
        .primary_context(params.primary_provider.clone(), params.primary_model.clone());
    builder = builder.extend_candidates(chain_candidates);

    // Wire per-task fallback_chain entries from config into the builder.
    for (task_key, task_override) in &params.config.tasks {
        if task_override.fallback_chain.is_empty() {
            continue;
        }
        let mut fb_chain = ProviderChain::new();
        for entry in &task_override.fallback_chain {
            if let Some(candidate) = build_fallback_chain_candidate(entry, &params.llm_providers) {
                fb_chain.push(candidate);
            }
        }
        if !fb_chain.is_empty() {
            builder = builder.add_task_fallback_chain(task_key.clone(), fb_chain);
        }
    }

    (builder.build(), summary)
}

/// Resolve a single `FallbackChainEntry` into a `ProviderCandidate`.
fn build_fallback_chain_candidate(
    entry: &FallbackChainEntry,
    llm_providers: &HashMap<String, LlmProviderConfig>,
) -> Option<ProviderCandidate> {
    let provider_name = entry.provider.trim().to_lowercase();
    if provider_name.is_empty() {
        return None;
    }

    // api_key: explicit in entry > llm_providers config > env
    let api_key = entry
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            llm_providers
                .get(&provider_name)
                .and_then(|cfg| resolve_provider_api_key(cfg, &provider_name))
        })
        .or_else(|| provider_api_key_from_env(&provider_name))?;

    let base_url = entry
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            llm_providers
                .get(&provider_name)
                .and_then(|cfg| cfg.base_url.clone())
        })
        .or_else(|| default_base_url(&provider_name))?;

    let model = entry
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| provider_name.clone());

    let source = match provider_name.as_str() {
        "openrouter" => AuxiliarySource::OpenRouter,
        "anthropic" => AuxiliarySource::Anthropic,
        "custom" => AuxiliarySource::Custom,
        other => AuxiliarySource::DirectKey(other.to_string()),
    };

    let llm: Arc<dyn LlmProvider> = Arc::new(GenericProvider::new(base_url, api_key, model.clone()));
    Some(ProviderCandidate::new(source, model, llm))
}

fn vision_task_provider_is_auto(config: &AuxiliaryConfig) -> bool {
    let Some(override_) = config.task_override(&AuxiliaryTask::Vision) else {
        return true;
    };
    let provider = override_
        .provider
        .as_deref()
        .map(str::trim)
        .unwrap_or("auto")
        .to_ascii_lowercase();
    provider.is_empty() || provider == "auto"
}

fn maybe_primary_vision_candidate(
    params: &AuxiliaryBuildParams,
    summary: &mut AuxiliaryWiringSummary,
) -> Option<ProviderCandidate> {
    if !vision_task_provider_is_auto(&params.config) {
        return None;
    }
    let provider_name = params.primary_provider.as_deref()?.trim();
    if provider_name.is_empty() || provider_name.eq_ignore_ascii_case("auto") {
        return None;
    }
    let model = params
        .primary_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let cfg = params
        .llm_providers
        .get(provider_name)
        .or_else(|| {
            params
                .llm_providers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(provider_name))
                .map(|(_, v)| v)
        })?;
    let api_key = resolve_provider_api_key(cfg, provider_name)?;
    let base_url = cfg
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| default_base_url(provider_name))?;
    let llm: Arc<dyn LlmProvider> = Arc::new(GenericProvider::new(
        base_url.clone(),
        api_key,
        model.clone(),
    ));
    let label = if provider_name.starts_with("custom") || cfg.base_url.is_some() {
        "custom".to_string()
    } else {
        provider_name.to_string()
    };
    tracing::debug!(
        primary_provider = %provider_name,
        primary_model = %model,
        auxiliary_label = %label,
        base_url = %base_url,
        "auxiliary vision: prepending primary llm_providers candidate"
    );
    summary
        .registered
        .insert(0, format!("primary:{label}"));
    Some(ProviderCandidate::new(
        if label == "custom" {
            AuxiliarySource::Custom
        } else {
            AuxiliarySource::DirectKey(label.clone())
        },
        model,
        llm,
    ))
}

fn resolve_provider_api_key(cfg: &LlmProviderConfig, provider_name: &str) -> Option<String> {
    if let Some(key) = cfg
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if key.starts_with("${") && key.ends_with('}') {
            let env_name = key.trim_start_matches("${").trim_end_matches('}');
            return std::env::var(env_name).ok().filter(|v| !v.trim().is_empty());
        }
        return Some(key.to_string());
    }
    if let Some(env_name) = cfg
        .api_key_env
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Ok(v) = std::env::var(env_name) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    provider_api_key_from_env(provider_name)
}

fn provider_api_key_from_env(provider_name: &str) -> Option<String> {
    let upper = provider_name.to_ascii_uppercase().replace('-', "_");
    for key in [
        format!("{upper}_API_KEY"),
        "HERMES_OPENAI_API_KEY".to_string(),
        "OPENAI_API_KEY".to_string(),
        "OPENROUTER_API_KEY".to_string(),
    ] {
        if let Ok(v) = std::env::var(&key) {
            if !v.trim().is_empty() {
                return Some(v);
            }
        }
    }
    None
}

fn default_base_url(provider_name: &str) -> Option<String> {
    match provider_name.to_ascii_lowercase().as_str() {
        "openrouter" => Some("https://openrouter.ai/api/v1".into()),
        "anthropic" => Some("https://api.anthropic.com".into()),
        "openai" => Some("https://api.openai.com/v1".into()),
        "custom" => std::env::var("OPENAI_BASE_URL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or(Some("https://api.openai.com/v1".into())),
        _ => std::env::var("OPENAI_BASE_URL").ok().filter(|s| !s.trim().is_empty()),
    }
}

fn collect_env_candidates(_config: AuxiliaryConfig) -> (Vec<ProviderCandidate>, AuxiliaryWiringSummary) {
    let mut summary = AuxiliaryWiringSummary {
        registered: Vec::new(),
        skipped: Vec::new(),
    };
    let mut out = Vec::new();

    if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> = Arc::new(
                OpenRouterProvider::new(key.trim())
                    .with_model(default_models::OPENROUTER)
                    .with_http_referer("https://hermes-agent.nousresearch.com")
                    .with_x_title("Hermes Agent"),
            );
            out.push(ProviderCandidate::new(
                AuxiliarySource::OpenRouter,
                default_models::OPENROUTER,
                provider,
            ));
            summary.registered.push("openrouter".into());
        } else {
            summary.skipped.push("openrouter (empty)".into());
        }
    } else {
        summary.skipped.push("openrouter (no key)".into());
    }

    if let Some(key) = std::env::var("HERMES_OPENAI_API_KEY")
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
            out.push(ProviderCandidate::new(
                AuxiliarySource::Custom,
                model,
                provider,
            ));
            summary.registered.push("custom".into());
        } else {
            summary.skipped.push("custom (empty key)".into());
        }
    } else {
        summary.skipped.push("custom (no key)".into());
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            let provider: Arc<dyn LlmProvider> =
                Arc::new(AnthropicProvider::new(key.trim()).with_model(default_models::ANTHROPIC));
            out.push(ProviderCandidate::new(
                AuxiliarySource::Anthropic,
                default_models::ANTHROPIC,
                provider,
            ));
            summary.registered.push("anthropic".into());
        }
    } else {
        summary.skipped.push("anthropic (no key)".into());
    }

    register_direct_key_env(
        &mut out,
        &mut summary,
        "ZAI_API_KEY",
        "zai",
        "https://api.z.ai/api/coding/paas/v4",
        default_models::ZAI,
    );
    register_direct_key_env(
        &mut out,
        &mut summary,
        "KIMI_API_KEY",
        "kimi",
        "https://api.moonshot.ai/v1",
        default_models::KIMI,
    );
    if let Ok(key) = std::env::var("MINIMAX_API_KEY") {
        if !key.trim().is_empty() {
            let base_url = std::env::var("MINIMAX_BASE_URL")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "https://api.minimax.io/v1".to_string());
            let provider: Arc<dyn LlmProvider> = Arc::new(GenericProvider::new(
                base_url,
                key.trim(),
                default_models::MINIMAX,
            ));
            out.push(ProviderCandidate::new(
                AuxiliarySource::DirectKey("minimax".to_string()),
                default_models::MINIMAX,
                provider,
            ));
            summary.registered.push("minimax".into());
        } else {
            summary.skipped.push("minimax (empty)".into());
        }
    } else {
        summary.skipped.push("minimax (no key)".into());
    }
    register_direct_key_env(
        &mut out,
        &mut summary,
        "GEMINI_API_KEY",
        "gemini",
        "https://generativelanguage.googleapis.com/v1beta/openai",
        default_models::GEMINI,
    );

    (out, summary)
}

fn register_direct_key_env(
    out: &mut Vec<ProviderCandidate>,
    summary: &mut AuxiliaryWiringSummary,
    env_var: &str,
    label: &str,
    base_url: &str,
    default_model: &str,
) {
    let Ok(key) = std::env::var(env_var) else {
        summary.skipped.push(format!("{label} (no key)"));
        return;
    };
    if key.trim().is_empty() {
        summary.skipped.push(format!("{label} (empty key)"));
        return;
    }
    let provider: Arc<dyn LlmProvider> =
        Arc::new(GenericProvider::new(base_url, key.trim(), default_model));
    out.push(ProviderCandidate::new(
        AuxiliarySource::DirectKey(label.to_string()),
        default_model,
        provider,
    ));
    summary.registered.push(label.to_string());
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
        "ZAI_API_KEY",
        "KIMI_API_KEY",
        "MINIMAX_API_KEY",
        "GEMINI_API_KEY",
    ];

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn clear() -> Self {
            let mut previous = Vec::new();
            for k in KEYS {
                previous.push((*k, std::env::var(k).ok()));
                hermes_core::test_env::remove_var(k);
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in self.previous.drain(..) {
                match v {
                    Some(val) => hermes_core::test_env::set_var(k, val),
                    None => hermes_core::test_env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn build_default_auxiliary_client_scenarios() {
        let _g = EnvGuard::clear();

        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 0);
            assert!(summary.registered.is_empty());
        }

        hermes_core::test_env::set_var("OPENROUTER_API_KEY", "sk-test");
        {
            let (client, summary) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(client.chain_len(), 1);
            assert_eq!(summary.registered, vec!["openrouter"]);
        }
        hermes_core::test_env::remove_var("OPENROUTER_API_KEY");

        hermes_core::test_env::set_var("OPENROUTER_API_KEY", "sk-or");
        hermes_core::test_env::set_var("HERMES_OPENAI_API_KEY", "sk-hermes-oa");
        hermes_core::test_env::set_var("ANTHROPIC_API_KEY", "sk-an");
        hermes_core::test_env::set_var("ZAI_API_KEY", "z");
        {
            let (client, _) = build_default_auxiliary_client(AuxiliaryConfig::default());
            assert_eq!(
                client.chain_labels(),
                vec!["openrouter", "custom", "anthropic", "zai"]
            );
        }
    }

    #[test]
    fn primary_llm_provider_prepended_for_vision_auto() {
        let _g = EnvGuard::clear();
        let mut llm = HashMap::new();
        llm.insert(
            "flowy".to_string(),
            LlmProviderConfig {
                api_key: Some("sk-flowy".into()),
                base_url: Some("https://flowy.example/v1".into()),
                ..Default::default()
            },
        );
        let (client, summary) = build_auxiliary_client(AuxiliaryBuildParams {
            config: AuxiliaryConfig::default(),
            primary_provider: Some("flowy".into()),
            primary_model: Some("DeepSeek-V4-Flash".into()),
            llm_providers: llm,
        });
        assert!(summary.registered.first().is_some_and(|s| s.starts_with("primary:")));
        assert_eq!(client.chain_labels().first().map(String::as_str), Some("flowy"));
    }
}
