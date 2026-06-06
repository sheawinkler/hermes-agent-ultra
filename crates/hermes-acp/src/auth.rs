//! ACP authentication helpers.
//!
//! Mirrors the Python `acp_adapter/auth.py` contract: advertise a terminal
//! setup method for first-run clients and, when Hermes can resolve configured
//! runtime credentials, advertise that provider as an agent-managed auth method.

use hermes_config::{load_config, GatewayConfig};

use crate::protocol::AuthMethod;

pub const TERMINAL_SETUP_AUTH_METHOD_ID: &str = "hermes-setup";

/// Return true if Hermes can resolve a runtime provider with credentials.
pub fn has_provider() -> bool {
    detect_provider().is_some()
}

/// Resolve the active Hermes runtime provider, or `None` if unavailable.
pub fn detect_provider() -> Option<String> {
    let config = load_config(None).ok()?;
    detect_provider_from_config(&config)
}

/// Build ACP auth methods for the currently configured runtime provider.
pub fn build_auth_methods() -> Vec<AuthMethod> {
    build_auth_methods_for_provider(detect_provider().as_deref())
}

pub(crate) fn build_auth_methods_for_provider(provider: Option<&str>) -> Vec<AuthMethod> {
    let mut methods = Vec::new();
    if let Some(provider) = provider.and_then(normalize_provider_with_credentials_marker) {
        methods.push(AuthMethod {
            id: provider.clone(),
            name: format!("{provider} runtime credentials"),
            description: Some(format!(
                "Authenticate Hermes using the currently configured {provider} runtime credentials."
            )),
            method_type: None,
            args: Vec::new(),
        });
    }

    methods.push(AuthMethod {
        id: TERMINAL_SETUP_AUTH_METHOD_ID.to_string(),
        name: "Configure Hermes provider".to_string(),
        description: Some(
            "Open Hermes' interactive model/provider setup in a terminal. Use this when Hermes has not been configured on this machine yet."
                .to_string(),
        ),
        method_type: Some("terminal".to_string()),
        args: vec!["--setup".to_string()],
    });
    methods
}

pub(crate) fn detect_provider_from_config(config: &GatewayConfig) -> Option<String> {
    let provider = active_provider(config)?;
    provider_has_credentials(config, &provider).then_some(provider)
}

fn normalize_provider_with_credentials_marker(provider: &str) -> Option<String> {
    let provider = normalize_provider_name(provider);
    (!provider.is_empty()).then_some(provider)
}

fn normalize_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "novita-ai" | "novitaai" => "novita".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        _ => normalized,
    }
}

fn active_provider(config: &GatewayConfig) -> Option<String> {
    if let Some(provider) = env_trimmed("HERMES_INFERENCE_PROVIDER") {
        return normalize_provider_with_credentials_marker(&provider);
    }

    let model = env_trimmed("HERMES_INFERENCE_MODEL")
        .or_else(|| env_trimmed("HERMES_MODEL"))
        .or_else(|| config.model.as_deref().map(str::to_string))
        .unwrap_or_default();
    let model = model.trim();

    if let Some((provider, _model_name)) = model.split_once(':') {
        return normalize_provider_with_credentials_marker(provider);
    }

    if !model.is_empty() {
        if let Some((provider, _)) = config.llm_providers.iter().find(|(_, cfg)| {
            cfg.model
                .as_deref()
                .map(str::trim)
                .filter(|candidate| !candidate.is_empty())
                .is_some_and(|candidate| candidate == model)
        }) {
            return normalize_provider_with_credentials_marker(provider);
        }
    }

    if config.llm_providers.len() == 1 {
        if let Some((provider, _)) = config.llm_providers.iter().next() {
            return normalize_provider_with_credentials_marker(provider);
        }
    }

    None
}

fn provider_has_credentials(config: &GatewayConfig, provider: &str) -> bool {
    let provider_config = provider_config(config, provider);
    let direct_config_key = provider_config
        .and_then(|cfg| cfg.api_key.as_deref())
        .is_some_and(non_empty);
    if direct_config_key {
        return true;
    }

    let config_env_key = provider_config
        .and_then(|cfg| cfg.api_key_env.as_deref())
        .and_then(env_trimmed)
        .is_some();
    if config_env_key {
        return true;
    }

    provider_env_key_names(provider)
        .iter()
        .any(|key| env_trimmed(key).is_some())
}

fn provider_config<'a>(
    config: &'a GatewayConfig,
    provider: &str,
) -> Option<&'a hermes_config::LlmProviderConfig> {
    config.llm_providers.get(provider).or_else(|| {
        config
            .llm_providers
            .iter()
            .find(|(name, _)| normalize_provider_name(name) == provider)
            .map(|(_, cfg)| cfg)
    })
}

fn provider_env_key_names(provider: &str) -> &'static [&'static str] {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai" => &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"],
        "openai-codex" => &["HERMES_OPENAI_CODEX_API_KEY"],
        "anthropic" => &[
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
        ],
        "google-gemini-cli" => &[
            "HERMES_GEMINI_OAUTH_API_KEY",
            "GOOGLE_API_KEY",
            "GEMINI_API_KEY",
        ],
        "openrouter" => &["OPENROUTER_API_KEY"],
        "qwen" => &["DASHSCOPE_API_KEY"],
        "qwen-oauth" => &["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"],
        "kimi" => &["MOONSHOT_API_KEY"],
        "minimax" => &["MINIMAX_API_KEY"],
        "nous" => &["NOUS_API_KEY", "HERMES_NOUS_API_KEY"],
        "copilot" | "copilot-acp" => &[
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_COPILOT_TOKEN",
        ],
        "stepfun" => &["STEPFUN_API_KEY"],
        "ai-gateway" => &["AI_GATEWAY_API_KEY"],
        "novita" => &["NOVITA_API_KEY"],
        "xai" => &["XAI_API_KEY"],
        "nvidia" => &["NVIDIA_API_KEY"],
        "kilocode" => &["KILOCODE_API_KEY"],
        "gmi" => &["GMI_API_KEY"],
        "huggingface" => &["HF_TOKEN", "HUGGINGFACE_API_KEY"],
        "zai" => &["ZAI_API_KEY"],
        "arcee" => &["ARCEEAI_API_KEY", "ARCEE_API_KEY"],
        "xiaomi" => &["XIAOMI_API_KEY"],
        "tencent-tokenhub" => &["TOKENHUB_API_KEY"],
        "ollama-cloud" => &["OLLAMA_API_KEY"],
        _ => &[],
    }
}

fn env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use hermes_config::{GatewayConfig, LlmProviderConfig};
    use serde_json::json;

    use super::*;

    #[test]
    fn build_auth_methods_returns_provider_and_terminal_when_configured() {
        let methods = build_auth_methods_for_provider(Some(" OpenRouter "));
        let payloads: Vec<_> = methods
            .iter()
            .map(|method| serde_json::to_value(method).unwrap())
            .collect();

        assert_eq!(payloads[0]["id"], "openrouter");
        assert_eq!(payloads[0]["name"], "openrouter runtime credentials");
        let terminal = payloads
            .iter()
            .find(|payload| payload["id"] == TERMINAL_SETUP_AUTH_METHOD_ID)
            .expect("terminal setup auth method");
        assert_eq!(terminal["type"], "terminal");
        assert_eq!(terminal["args"], json!(["--setup"]));
    }

    #[test]
    fn build_auth_methods_returns_terminal_setup_when_unconfigured() {
        let payloads: Vec<_> = build_auth_methods_for_provider(None)
            .iter()
            .map(|method| serde_json::to_value(method).unwrap())
            .collect();

        assert_eq!(
            payloads,
            vec![json!({
                "args": ["--setup"],
                "description": "Open Hermes' interactive model/provider setup in a terminal. Use this when Hermes has not been configured on this machine yet.",
                "id": TERMINAL_SETUP_AUTH_METHOD_ID,
                "name": "Configure Hermes provider",
                "type": "terminal",
            })]
        );
    }

    #[test]
    fn detect_provider_from_config_requires_non_empty_key() {
        let mut config = GatewayConfig {
            model: Some("openrouter:anthropic/claude-sonnet".to_string()),
            llm_providers: HashMap::from([(
                "openrouter".to_string(),
                LlmProviderConfig {
                    api_key: Some("   ".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        assert_eq!(detect_provider_from_config(&config), None);

        config.llm_providers.get_mut("openrouter").unwrap().api_key =
            Some("sk-or-test".to_string());
        assert_eq!(
            detect_provider_from_config(&config).as_deref(),
            Some("openrouter")
        );
    }

    #[test]
    fn detect_provider_from_config_strips_and_lowercases_provider() {
        let config = GatewayConfig {
            model: Some(" OpenRouter : model ".to_string()),
            llm_providers: HashMap::from([(
                "openrouter".to_string(),
                LlmProviderConfig {
                    api_key: Some("sk-or-test".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        assert_eq!(
            detect_provider_from_config(&config).as_deref(),
            Some("openrouter")
        );
    }

    #[test]
    fn detect_provider_from_config_uses_credentials_from_alias_provider_block() {
        let config = GatewayConfig {
            model: Some("moonshot:kimi-k2".to_string()),
            llm_providers: HashMap::from([(
                "moonshot".to_string(),
                LlmProviderConfig {
                    api_key: Some("sk-moonshot-test".to_string()),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        assert_eq!(
            detect_provider_from_config(&config).as_deref(),
            Some("kimi")
        );
    }

    #[test]
    fn copilot_credentials_accept_upstream_env_aliases() {
        assert_eq!(
            provider_env_key_names("copilot"),
            &[
                "COPILOT_GITHUB_TOKEN",
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GITHUB_COPILOT_TOKEN"
            ]
        );
    }

    #[test]
    fn direct_provider_credentials_accept_upstream_aliases() {
        assert_eq!(normalize_provider_name("google-ai-studio"), "gemini");
        assert_eq!(normalize_provider_name("gmicloud"), "gmi");
        assert_eq!(normalize_provider_name("arcee-ai"), "arcee");
        assert_eq!(normalize_provider_name("mimo"), "xiaomi");
        assert_eq!(normalize_provider_name("tokenhub"), "tencent-tokenhub");
        assert_eq!(
            provider_env_key_names("arcee"),
            &["ARCEEAI_API_KEY", "ARCEE_API_KEY"]
        );
        assert_eq!(
            provider_env_key_names("tencent-tokenhub"),
            &["TOKENHUB_API_KEY"]
        );
    }
}
