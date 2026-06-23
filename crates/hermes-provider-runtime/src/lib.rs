//! Provider/model routing and LLM backend construction.
//!
//! This crate is intentionally below `hermes-cli`: provider/auth routing tests
//! should not compile terminal UI, slash-command handling, or gateway adapter
//! feature surfaces.

use std::sync::Arc;

use futures::StreamExt;
use hermes_agent::bedrock::{
    bedrock_runtime_base_url, resolve_bedrock_region, BedrockProvider, BEDROCK_AUTH_MARKER,
};
use hermes_agent::provider::{
    is_codex_chatgpt_token, AnthropicProvider, GenericProvider, OpenAiProvider, OpenRouterProvider,
    OPENAI_CODEX_BASE_URL,
};
use hermes_agent::provider_profiles;
use hermes_agent::providers_extra::{
    CopilotProvider, KimiProvider, MiniMaxProvider, NousProvider, QwenProvider,
};
use hermes_agent::CodexProvider;
use hermes_config::{GatewayConfig, LlmProviderConfig};
use hermes_core::{AgentError, LlmProvider};
use serde_json::Value;

pub const DEFAULT_NOUS_INFERENCE_URL: &str = "https://inference-api.nousresearch.com/v1";
pub const STEPFUN_BASE_URL: &str = "https://api.stepfun.ai/step_plan/v1";
pub const QWEN_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
pub const ALIBABA_CODING_PLAN_BASE_URL: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
pub const GOOGLE_GEMINI_CLI_BASE_URL: &str = "cloudcode-pa://google";
pub const GEMINI_BASE_URL: &str = provider_profiles::GEMINI_OPENAI_BASE_URL;
pub const AI_GATEWAY_BASE_URL: &str = "https://ai-gateway.vercel.sh/v1";
pub const KIMI_CODING_BASE_URL: &str = provider_profiles::KIMI_CODE_BASE_URL;
pub const KIMI_LEGACY_BASE_URL: &str = provider_profiles::KIMI_LEGACY_BASE_URL;
pub const KIMI_CODING_CN_BASE_URL: &str = provider_profiles::KIMI_CN_BASE_URL;
pub const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/anthropic";
pub const NOVITA_BASE_URL: &str = "https://api.novita.ai/openai/v1";
pub const XAI_BASE_URL: &str = "https://api.x.ai/v1";
pub const NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
pub const COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";
pub const OPENCODE_GO_BASE_URL: &str = "https://opencode.ai/zen/go/v1";
pub const OPENCODE_ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
pub const KILOCODE_BASE_URL: &str = "https://api.kilo.ai/api/gateway";
pub const HUGGINGFACE_BASE_URL: &str = "https://router.huggingface.co/v1";
pub const GMI_BASE_URL: &str = "https://api.gmi-serving.com/v1";
pub const XIAOMI_BASE_URL: &str = "https://api.xiaomimimo.com/v1";
pub const ZAI_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
pub const ARCEE_BASE_URL: &str = "https://api.arcee.ai/api/v1";
pub const TENCENT_TOKENHUB_BASE_URL: &str = "https://tokenhub.tencentmaas.com/v1";
pub const OLLAMA_CLOUD_BASE_URL: &str = "https://ollama.com/v1";
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com/v1";
pub const OLLAMA_LOCAL_BASE_URL: &str = "http://127.0.0.1:11434/v1";
pub const LLAMA_CPP_BASE_URL: &str = "http://127.0.0.1:8080/v1";
pub const VLLM_BASE_URL: &str = "http://127.0.0.1:8000/v1";
pub const MLX_BASE_URL: &str = "http://127.0.0.1:8080/v1";
pub const APPLE_ANE_BASE_URL: &str = "http://127.0.0.1:8081/v1";
pub const SGLANG_BASE_URL: &str = "http://127.0.0.1:30000/v1";
pub const TGI_BASE_URL: &str = "http://127.0.0.1:8082/v1";
pub const LMSTUDIO_BASE_URL: &str = "http://127.0.0.1:1234/v1";
pub const LMDEPLOY_BASE_URL: &str = "http://127.0.0.1:23333/v1";
pub const LOCALAI_BASE_URL: &str = "http://127.0.0.1:8080/v1";
pub const KOBOLDCPP_BASE_URL: &str = "http://127.0.0.1:5001/v1";
pub const TEXT_GENERATION_WEBUI_BASE_URL: &str = "http://127.0.0.1:5000/v1";
pub const TABBYAPI_BASE_URL: &str = "http://127.0.0.1:5000/v1";

pub type OAuthTokenResolver<'a> = dyn Fn(&str) -> Option<String> + 'a;

pub fn active_llm_provider_config<'a>(
    config: &'a GatewayConfig,
    provider_name: &str,
    runtime_provider: &str,
) -> Option<&'a LlmProviderConfig> {
    config
        .llm_providers
        .get(provider_name)
        .or_else(|| config.llm_providers.get(runtime_provider))
        .or_else(|| {
            config.llm_providers.iter().find_map(|(name, cfg)| {
                if name.eq_ignore_ascii_case(provider_name)
                    || name.eq_ignore_ascii_case(runtime_provider)
                {
                    Some(cfg)
                } else {
                    None
                }
            })
        })
}

pub fn normalize_runtime_provider_name(provider: &str) -> String {
    let normalized = provider.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "codex" => "openai-codex".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "azure" | "azure-ai-foundry" | "azure_ai_foundry" => "azure-foundry".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi-coding" | "kimi-coding-cn" => "kimi".to_string(),
        "alibaba" | "alibaba-coding-plan" => "qwen".to_string(),
        "minimax-cn" => "minimax".to_string(),
        "novita-ai" | "novitaai" => "novita".to_string(),
        "glm" | "z-ai" | "z_ai" | "zhipu" => "zai".to_string(),
        "aigateway" | "vercel" => "ai-gateway".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "opencode-zen" | "zen" => "opencode-zen".to_string(),
        "go" => "opencode-go".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" | "llamafile" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" | "vmlx" | "omlx" | "mlx-vlm" | "mlxvlm" | "mlx-openai-server" => {
            "mlx".to_string()
        }
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        "lm-studio" | "lm_studio" | "lm studio" => "lmstudio".to_string(),
        "lm-deploy" | "lm_deploy" => "lmdeploy".to_string(),
        "local-ai" | "local_ai" => "localai".to_string(),
        "kobold-cpp" | "kobold" => "koboldcpp".to_string(),
        "oobabooga" | "textgen-webui" | "textgen_webui" | "text-generation-web-ui" => {
            "text-generation-webui".to_string()
        }
        "tabby-api" | "tabby_api" | "exllama" | "exllamav2" => "tabbyapi".to_string(),
        _ => normalized,
    }
}

pub fn provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "openai-codex" | "codex" => Some(OPENAI_CODEX_BASE_URL),
        "nous-api" | "nous_api" | "nousapi" | "nous-portal-api" => Some(DEFAULT_NOUS_INFERENCE_URL),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => Some(GOOGLE_GEMINI_CLI_BASE_URL),
        "gemini" | "google" | "google-gemini" | "google-ai-studio" => Some(GEMINI_BASE_URL),
        "qwen" | "alibaba" => Some(QWEN_BASE_URL),
        "alibaba-coding-plan" => Some(ALIBABA_CODING_PLAN_BASE_URL),
        "stepfun" | "step" | "step-plan" => Some(STEPFUN_BASE_URL),
        "ai-gateway" | "aigateway" | "vercel" => Some(AI_GATEWAY_BASE_URL),
        "kimi-coding" => Some(KIMI_CODING_BASE_URL),
        "kimi" | "moonshot" => Some(KIMI_LEGACY_BASE_URL),
        "kimi-coding-cn" => Some(KIMI_CODING_CN_BASE_URL),
        "minimax-cn" | "minimax_cn" => Some(MINIMAX_CN_BASE_URL),
        "novita" | "novita-ai" | "novitaai" => Some(NOVITA_BASE_URL),
        "xai" => Some(XAI_BASE_URL),
        "nvidia" => Some(NVIDIA_BASE_URL),
        "copilot" | "github-copilot" | "github-models" => Some(COPILOT_BASE_URL),
        "opencode-go" => Some(OPENCODE_GO_BASE_URL),
        "opencode-zen" | "opencode" => Some(OPENCODE_ZEN_BASE_URL),
        "kilocode" | "kilo" => Some(KILOCODE_BASE_URL),
        "huggingface" | "hf" | "hugging-face" | "huggingface-hub" => Some(HUGGINGFACE_BASE_URL),
        "gmi" | "gmi-cloud" | "gmicloud" => Some(GMI_BASE_URL),
        "xiaomi" | "mimo" | "xiaomi-mimo" => Some(XIAOMI_BASE_URL),
        "zai" | "glm" | "z-ai" | "z_ai" | "zhipu" => Some(ZAI_BASE_URL),
        "arcee" | "arcee-ai" | "arceeai" => Some(ARCEE_BASE_URL),
        "tencent-tokenhub" | "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => {
            Some(TENCENT_TOKENHUB_BASE_URL)
        }
        "ollama-cloud" => Some(OLLAMA_CLOUD_BASE_URL),
        "ollama-local" | "ollama" => Some(OLLAMA_LOCAL_BASE_URL),
        "llama-cpp" | "llama.cpp" | "llamacpp" | "llamafile" => Some(LLAMA_CPP_BASE_URL),
        "vllm" | "ollvm" | "llvm" => Some(VLLM_BASE_URL),
        "mlx" | "mlx-lm" | "apple-mlx" | "vmlx" | "omlx" | "mlx-vlm" | "mlxvlm"
        | "mlx-openai-server" => Some(MLX_BASE_URL),
        "apple-ane" | "ane" | "apple-neural-engine" => Some(APPLE_ANE_BASE_URL),
        "sglang" => Some(SGLANG_BASE_URL),
        "tgi" | "text-generation-inference" => Some(TGI_BASE_URL),
        "lmstudio" | "lm-studio" | "lm_studio" | "lm studio" => Some(LMSTUDIO_BASE_URL),
        "lmdeploy" | "lm-deploy" | "lm_deploy" => Some(LMDEPLOY_BASE_URL),
        "localai" | "local-ai" | "local_ai" => Some(LOCALAI_BASE_URL),
        "koboldcpp" | "kobold-cpp" | "kobold" => Some(KOBOLDCPP_BASE_URL),
        "text-generation-webui"
        | "text-generation-web-ui"
        | "textgen-webui"
        | "textgen_webui"
        | "oobabooga" => Some(TEXT_GENERATION_WEBUI_BASE_URL),
        "tabbyapi" | "tabby-api" | "tabby_api" | "exllama" | "exllamav2" => Some(TABBYAPI_BASE_URL),
        "deepseek" => Some(DEEPSEEK_BASE_URL),
        _ => None,
    }
}

pub fn resolve_provider_and_model(config: &GatewayConfig, model: &str) -> (String, String) {
    let trimmed = model.trim();
    if let Some((provider, model_name)) = trimmed.split_once(':') {
        return (provider.trim().to_string(), model_name.trim().to_string());
    }

    if let Some((provider, _)) = config.llm_providers.iter().find(|(_, cfg)| {
        cfg.model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .is_some_and(|m| m == trimmed)
    }) {
        return (provider.to_string(), trimmed.to_string());
    }

    if config.llm_providers.len() == 1 {
        if let Some((provider, _)) = config.llm_providers.iter().next() {
            return (provider.to_string(), trimmed.to_string());
        }
    }

    ("openai".to_string(), trimmed.to_string())
}

fn resolve_api_key_literal_or_env_ref(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_ref) = trimmed.strip_prefix("${").and_then(|s| s.strip_suffix('}')) {
        return std::env::var(env_ref).ok().filter(|v| !v.trim().is_empty());
    }
    Some(trimmed.to_string())
}

pub fn provider_base_url_from_env(provider: &str) -> Option<String> {
    let raw_provider = provider.trim().to_ascii_lowercase();
    let normalized_provider = normalize_runtime_provider_name(raw_provider.as_str());
    let env_var = match raw_provider.as_str() {
        "minimax-cn" | "minimax_cn" => "MINIMAX_CN_BASE_URL",
        _ => match normalized_provider.as_str() {
            "openai" => "OPENAI_BASE_URL",
            "openai-codex" | "codex" => "HERMES_OPENAI_CODEX_BASE_URL",
            "nous-api" => "NOUS_BASE_URL",
            "anthropic" => "ANTHROPIC_BASE_URL",
            "bedrock" => "BEDROCK_BASE_URL",
            "google-gemini-cli" => "HERMES_GEMINI_BASE_URL",
            "gemini" | "google" => "GEMINI_BASE_URL",
            "qwen" => "DASHSCOPE_BASE_URL",
            "qwen-oauth" => "HERMES_QWEN_BASE_URL",
            "stepfun" => "STEPFUN_BASE_URL",
            "ai-gateway" => "AI_GATEWAY_BASE_URL",
            "kimi" => "KIMI_BASE_URL",
            "minimax" => "MINIMAX_BASE_URL",
            "novita" => "NOVITA_BASE_URL",
            "xai" => "XAI_BASE_URL",
            "nvidia" => "NVIDIA_BASE_URL",
            "copilot" => "COPILOT_API_BASE_URL",
            "opencode-go" => "OPENCODE_GO_BASE_URL",
            "opencode-zen" => "OPENCODE_ZEN_BASE_URL",
            "kilocode" => "KILOCODE_BASE_URL",
            "huggingface" => "HF_BASE_URL",
            "gmi" => "GMI_BASE_URL",
            "xiaomi" => "XIAOMI_BASE_URL",
            "zai" => "GLM_BASE_URL",
            "arcee" => "ARCEE_BASE_URL",
            "tencent-tokenhub" => "TOKENHUB_BASE_URL",
            "deepseek" => "DEEPSEEK_BASE_URL",
            "ollama-local" | "ollama" => "OLLAMA_BASE_URL",
            "llama-cpp" | "llama.cpp" | "llamacpp" | "llamafile" => "LLAMA_CPP_BASE_URL",
            "vllm" | "ollvm" | "llvm" => "VLLM_BASE_URL",
            "mlx" | "mlx-lm" | "apple-mlx" | "vmlx" | "omlx" | "mlx-vlm" | "mlxvlm"
            | "mlx-openai-server" => "MLX_BASE_URL",
            "apple-ane" | "ane" | "apple-neural-engine" => "APPLE_ANE_BASE_URL",
            "sglang" => "SGLANG_BASE_URL",
            "tgi" | "text-generation-inference" => "TGI_BASE_URL",
            "lmstudio" => "LMSTUDIO_BASE_URL",
            "lmdeploy" => "LMDEPLOY_BASE_URL",
            "localai" => "LOCALAI_BASE_URL",
            "koboldcpp" => "KOBOLDCPP_BASE_URL",
            "text-generation-webui" => "TEXT_GENERATION_WEBUI_BASE_URL",
            "tabbyapi" => "TABBYAPI_BASE_URL",
            _ => return None,
        },
    };
    std::env::var(env_var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn provider_is_local_backend(provider: &str) -> bool {
    matches!(
        provider.trim().to_ascii_lowercase().as_str(),
        "ollama-local"
            | "llama-cpp"
            | "vllm"
            | "mlx"
            | "apple-ane"
            | "sglang"
            | "tgi"
            | "lmstudio"
            | "lmdeploy"
            | "localai"
            | "koboldcpp"
            | "text-generation-webui"
            | "tabbyapi"
    )
}

pub fn allow_no_api_key(
    provider_name: &str,
    runtime_provider: &str,
    base_url: Option<&str>,
) -> bool {
    provider_is_local_backend(runtime_provider)
        || provider_is_local_backend(provider_name)
        || runtime_provider == "bedrock"
        || provider_name == "bedrock"
        || base_url.is_some_and(url_is_local_or_private)
}

fn url_is_local_or_private(base_url: &str) -> bool {
    let trimmed = base_url.trim();
    let no_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let authority = no_scheme.split('/').next().unwrap_or(no_scheme).trim();
    let host = if authority.starts_with('[') {
        authority
            .find(']')
            .map(|idx| authority[1..idx].to_string())
            .unwrap_or_else(|| authority.trim_matches(&['[', ']'][..]).to_string())
    } else {
        authority
            .split(':')
            .next()
            .unwrap_or(authority)
            .trim()
            .to_string()
    }
    .to_ascii_lowercase();

    if host == "localhost" {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
        };
    }
    false
}

/// Resolve API key / token for a named LLM provider from well-known environment variables.
pub fn provider_api_key_from_env(provider: &str) -> Option<String> {
    let raw_provider = provider.trim().to_ascii_lowercase();
    if raw_provider == "kimi-coding-cn" {
        return ["KIMI_CN_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            .iter()
            .find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            });
    }
    if matches!(raw_provider.as_str(), "minimax-cn" | "minimax_cn") {
        return std::env::var("MINIMAX_CN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_API_KEY").ok())
            .filter(|s| !s.trim().is_empty());
    }
    let provider = normalize_runtime_provider_name(raw_provider.as_str());
    match provider.as_str() {
        "openai" => std::env::var("HERMES_OPENAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "openai-codex" | "codex" => std::env::var("HERMES_OPENAI_CODEX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "anthropic" | "claude" | "claude-code" => std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ANTHROPIC_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "bedrock" => Some(BEDROCK_AUTH_MARKER.to_string()),
        "google-gemini-cli" | "gemini-cli" | "gemini-oauth" => {
            std::env::var("HERMES_GEMINI_OAUTH_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GOOGLE_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
                .or_else(|| std::env::var("GEMINI_API_KEY").ok())
                .filter(|s| !s.trim().is_empty())
        }
        "openrouter" => std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen" => std::env::var("DASHSCOPE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "qwen-oauth" => std::env::var("HERMES_QWEN_OAUTH_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("DASHSCOPE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "kimi" | "moonshot" => {
            let env_vars: &[&str] = if raw_provider == "kimi-coding" {
                &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
            } else {
                &[
                    "KIMI_API_KEY",
                    "KIMI_CODING_API_KEY",
                    "MOONSHOT_API_KEY",
                    "KIMI_CN_API_KEY",
                ]
            };
            env_vars.iter().find_map(|env_var| {
                std::env::var(env_var)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
        }
        "minimax" => std::env::var("MINIMAX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("MINIMAX_CN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "stepfun" => std::env::var("HERMES_STEPFUN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("STEPFUN_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "novita" => std::env::var("NOVITA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nous" | "nous-api" => std::env::var("NOUS_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "copilot" => std::env::var("COPILOT_GITHUB_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GH_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_TOKEN").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("GITHUB_COPILOT_TOKEN").ok())
            .filter(|s| !s.trim().is_empty()),
        "ai-gateway" => std::env::var("AI_GATEWAY_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "arcee" => std::env::var("ARCEEAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ARCEE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "deepseek" => std::env::var("DEEPSEEK_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "huggingface" => std::env::var("HF_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "gmi" => std::env::var("GMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "kilocode" => std::env::var("KILOCODE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "nvidia" => std::env::var("NVIDIA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-cloud" => std::env::var("OLLAMA_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "ollama-local" => std::env::var("OLLAMA_LOCAL_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("OLLAMA_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "llama-cpp" => std::env::var("LLAMA_CPP_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "vllm" => std::env::var("VLLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "mlx" => std::env::var("MLX_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "apple-ane" => std::env::var("APPLE_ANE_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "sglang" => std::env::var("SGLANG_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tgi" => std::env::var("TGI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("HUGGINGFACE_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        "lmstudio" => std::env::var("LMSTUDIO_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "lmdeploy" => std::env::var("LMDEPLOY_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "localai" => std::env::var("LOCALAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "koboldcpp" => std::env::var("KOBOLDCPP_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "text-generation-webui" => std::env::var("TEXT_GENERATION_WEBUI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tabbyapi" => std::env::var("TABBYAPI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-go" => std::env::var("OPENCODE_GO_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "opencode-zen" => std::env::var("OPENCODE_ZEN_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xai" => std::env::var("XAI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "xiaomi" => std::env::var("XIAOMI_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "tencent-tokenhub" => std::env::var("TOKENHUB_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty()),
        "zai" => std::env::var("GLM_API_KEY")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ZAI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("Z_AI_API_KEY").ok())
            .filter(|s| !s.trim().is_empty()),
        _ => None,
    }
}

pub fn build_provider(config: &GatewayConfig, model: &str) -> Arc<dyn LlmProvider> {
    build_provider_with_auth_resolver(config, model, None)
}

pub fn build_provider_with_auth_resolver(
    config: &GatewayConfig,
    model: &str,
    oauth_token_resolver: Option<&OAuthTokenResolver<'_>>,
) -> Arc<dyn LlmProvider> {
    let (provider_name, model_name) = resolve_provider_and_model(config, model);
    let runtime_provider = normalize_runtime_provider_name(provider_name.as_str());
    let model_name = hermes_agent::model_normalize::normalize_model_for_provider(
        model_name.as_str(),
        runtime_provider.as_str(),
    );

    let provider_config =
        active_llm_provider_config(config, provider_name.as_str(), runtime_provider.as_str());
    let request_timeout_seconds = provider_config.and_then(|c| c.request_timeout_seconds);

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
        .or_else(|| provider_api_key_from_env(runtime_provider.as_str()))
        .or_else(|| oauth_token_resolver.and_then(|resolver| resolver(provider_name.as_str())))
        .or_else(|| oauth_token_resolver.and_then(|resolver| resolver(runtime_provider.as_str())));

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
                "No API key for provider '{}'(runtime '{}'); using NoBackendProvider",
                provider_name,
                runtime_provider
            );
            return Arc::new(NoBackendProvider {
                model: model.to_string(),
            });
        }
    };

    let use_openai_pro_backend = matches!(runtime_provider.as_str(), "openai-codex" | "codex")
        || (runtime_provider == "openai" && is_codex_chatgpt_token(&api_key));
    let base_url = if use_openai_pro_backend {
        Some(base_url.unwrap_or_else(|| OPENAI_CODEX_BASE_URL.to_string()))
    } else {
        base_url
    };

    match runtime_provider.as_str() {
        "openai" => {
            if use_openai_pro_backend {
                let mut p = CodexProvider::openai_pro(&api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            } else {
                let mut p = OpenAiProvider::new(&api_key)
                    .with_model(model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds);
                if let Some(url) = base_url {
                    p = p.with_base_url(url);
                }
                Arc::new(p)
            }
        }
        "openai-codex" | "codex" => {
            let mut p = CodexProvider::openai_pro(&api_key, model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "anthropic" => {
            let mut p = AnthropicProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "bedrock" => {
            let mut p = BedrockProvider::new()
                .with_region(resolve_bedrock_region())
                .with_model(model_name.as_str());
            if let Some(url) =
                base_url.or_else(|| Some(bedrock_runtime_base_url(&resolve_bedrock_region())))
            {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "openrouter" => {
            let p = OpenRouterProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            Arc::new(p)
        }
        "qwen" | "qwen-oauth" => {
            let mut p = QwenProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "kimi" | "moonshot" => {
            let mut p = KimiProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "minimax" => {
            let mut p = MiniMaxProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "gemini" => {
            let url = base_url
                .as_deref()
                .map(provider_profiles::gemini_openai_compatible_base_url)
                .unwrap_or_else(|| provider_profiles::GEMINI_OPENAI_BASE_URL.to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
        "stepfun" => {
            let url = base_url.unwrap_or_else(|| STEPFUN_BASE_URL.to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
        "nous" | "nous-api" => {
            let mut p = NousProvider::new(&api_key)
                .with_model(model_name.as_str())
                .with_optional_request_timeout_seconds(request_timeout_seconds);
            if let Some(url) = base_url {
                p = p.with_base_url(url);
            }
            Arc::new(p)
        }
        "copilot" => {
            let p = CopilotProvider::new(
                base_url.unwrap_or_else(|| COPILOT_BASE_URL.to_string()),
                &api_key,
            )
            .with_model(model_name.as_str())
            .with_optional_request_timeout_seconds(request_timeout_seconds);
            Arc::new(p)
        }
        "ollama-local"
        | "llama-cpp"
        | "vllm"
        | "mlx"
        | "apple-ane"
        | "sglang"
        | "tgi"
        | "lmstudio"
        | "lmdeploy"
        | "localai"
        | "koboldcpp"
        | "text-generation-webui"
        | "tabbyapi" => {
            let url = base_url.unwrap_or_else(|| "http://127.0.0.1:11434/v1".to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
        _ => {
            let url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
            Arc::new(
                GenericProvider::new(url, &api_key, model_name.as_str())
                    .with_optional_request_timeout_seconds(request_timeout_seconds)
                    .with_provider_profile(runtime_provider.as_str()),
            )
        }
    }
}

pub struct NoBackendProvider {
    pub model: String,
}

#[async_trait::async_trait]
impl LlmProvider for NoBackendProvider {
    async fn chat_completion(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> Result<hermes_core::LlmResponse, AgentError> {
        Err(AgentError::LlmApi(format!(
            "NoBackendProvider: no LLM backend configured for model '{}'. \
             Configure an API key and provider in the config file.",
            self.model
        )))
    }

    fn chat_completion_stream(
        &self,
        _messages: &[hermes_core::Message],
        _tools: &[hermes_core::ToolSchema],
        _max_tokens: Option<u32>,
        _temperature: Option<f64>,
        _model: Option<&str>,
        _extra_body: Option<&Value>,
    ) -> futures::stream::BoxStream<'static, Result<hermes_core::StreamChunk, AgentError>> {
        futures::stream::once(async move {
            Err(AgentError::LlmApi(
                "NoBackendProvider: no LLM backend configured for streaming.".to_string(),
            ))
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_agent::provider_profiles;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .expect("env test lock poisoned")
    }

    struct EnvSnapshot {
        vars: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                vars: keys
                    .iter()
                    .map(|key| (*key, std::env::var(key).ok()))
                    .collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

    #[tokio::test]
    async fn build_provider_routes_chatgpt_openai_oauth_to_responses_backend() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
            "OPENAI_BASE_URL",
            "HERMES_OPENAI_CODEX_BASE_URL",
        ]);
        for key in [
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
            "OPENAI_BASE_URL",
            "HERMES_OPENAI_CODEX_BASE_URL",
        ] {
            std::env::remove_var(key);
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [
                    {
                        "type": "message",
                        "content": [{"type": "output_text", "text": "openai-pro-ok"}]
                    }
                ],
                "model": "gpt-5.5",
                "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                api_key: Some("eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3Qtb3BlbmFpLXByby1wYXJpdHkiLCJjaGF0Z3B0X3BsYW5fdHlwZSI6InBsdXMifX0.sig".to_string()),
                base_url: Some(server.uri()),
                ..LlmProviderConfig::default()
            },
        );

        let provider = build_provider(&config, "openai:gpt-5.5");
        let response = provider
            .chat_completion(
                &[hermes_core::Message::user("hello")],
                &[],
                None,
                None,
                Some("gpt-5.5"),
                None,
            )
            .await
            .expect("OpenAI ChatGPT OAuth provider should use Responses API");

        assert_eq!(response.message.content.as_deref(), Some("openai-pro-ok"));
        server.verify().await;
    }

    #[tokio::test]
    async fn provider_auth_resolver_supplies_openai_oauth_token() {
        let _guard = env_test_lock();
        let _env = EnvSnapshot::capture(&[
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
        ]);
        for key in [
            "HERMES_OPENAI_API_KEY",
            "OPENAI_API_KEY",
            "HERMES_OPENAI_CODEX_API_KEY",
        ] {
            std::env::remove_var(key);
        }

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [
                    {
                        "type": "message",
                        "content": [{"type": "output_text", "text": "resolver-oauth-ok"}]
                    }
                ],
                "model": "gpt-5.5",
                "status": "completed"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = GatewayConfig::default();
        config.llm_providers.insert(
            "openai".to_string(),
            LlmProviderConfig {
                base_url: Some(server.uri()),
                ..LlmProviderConfig::default()
            },
        );
        let token = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJzdWIiOiJ1c2VyLXh5eiIsImV4cCI6OTk5OTk5OTk5OSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY3Qtb3BlbmFpLXByby1yZXNvbHZlciIsImNoYXRncHRfcGxhbl90eXBlIjoicGx1cyJ9fQ.sig";
        let provider = build_provider_with_auth_resolver(
            &config,
            "openai:gpt-5.5",
            Some(&|provider| {
                if provider == "openai" {
                    Some(token.to_string())
                } else {
                    None
                }
            }),
        );

        let response = provider
            .chat_completion(
                &[hermes_core::Message::user("hello")],
                &[],
                None,
                None,
                Some("gpt-5.5"),
                None,
            )
            .await
            .expect("OpenAI OAuth resolver token should use Responses API");

        assert_eq!(
            response.message.content.as_deref(),
            Some("resolver-oauth-ok")
        );
        server.verify().await;
    }

    #[test]
    fn resolve_provider_and_model_uses_single_provider_fallback() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers
            .insert("stepfun".to_string(), LlmProviderConfig::default());

        let (provider, model) = resolve_provider_and_model(&cfg, "step-3.5-flash");
        assert_eq!(provider, "stepfun");
        assert_eq!(model, "step-3.5-flash");
    }

    #[test]
    fn test_resolve_provider_and_model_uses_named_custom_provider_model() {
        let mut cfg = GatewayConfig::default();
        cfg.llm_providers.insert(
            "custom".to_string(),
            LlmProviderConfig {
                model: Some("my-model".to_string()),
                ..LlmProviderConfig::default()
            },
        );

        let (provider, model) = resolve_provider_and_model(&cfg, "my-model");
        assert_eq!(provider, "custom");
        assert_eq!(model, "my-model");
    }

    #[test]
    fn provider_api_key_from_env_supports_stepfun() {
        let _guard = env_test_lock();
        let hermes_var = "HERMES_STEPFUN_API_KEY";
        let stepfun_var = "STEPFUN_API_KEY";
        let _env = EnvSnapshot::capture(&[hermes_var, stepfun_var]);
        std::env::remove_var(hermes_var);
        std::env::remove_var(stepfun_var);

        std::env::set_var(stepfun_var, "stepfun-direct");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-direct")
        );

        std::env::set_var(hermes_var, "stepfun-hermes");
        assert_eq!(
            provider_api_key_from_env("stepfun").as_deref(),
            Some("stepfun-hermes")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_openai_codex() {
        let _guard = env_test_lock();
        let var = "HERMES_OPENAI_CODEX_API_KEY";
        let _env = EnvSnapshot::capture(&[var]);
        std::env::remove_var(var);
        std::env::set_var(var, "codex-oauth-token");
        assert_eq!(
            provider_api_key_from_env("openai-codex").as_deref(),
            Some("codex-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_anthropic_aliases() {
        let _guard = env_test_lock();
        let primary = "ANTHROPIC_API_KEY";
        let secondary = "ANTHROPIC_TOKEN";
        let tertiary = "CLAUDE_CODE_OAUTH_TOKEN";
        let _env = EnvSnapshot::capture(&[primary, secondary, tertiary]);
        std::env::remove_var(primary);
        std::env::remove_var(secondary);
        std::env::remove_var(tertiary);

        std::env::set_var(tertiary, "claude-oauth-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("claude-oauth-token")
        );

        std::env::set_var(secondary, "anthropic-token");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-token")
        );

        std::env::set_var(primary, "anthropic-api-key");
        assert_eq!(
            provider_api_key_from_env("anthropic").as_deref(),
            Some("anthropic-api-key")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_qwen_oauth() {
        let _guard = env_test_lock();
        let oauth_var = "HERMES_QWEN_OAUTH_API_KEY";
        let fallback_var = "DASHSCOPE_API_KEY";
        let _env = EnvSnapshot::capture(&[oauth_var, fallback_var]);
        std::env::remove_var(oauth_var);
        std::env::remove_var(fallback_var);

        std::env::set_var(fallback_var, "dashscope-fallback");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("dashscope-fallback")
        );

        std::env::set_var(oauth_var, "qwen-oauth-token");
        assert_eq!(
            provider_api_key_from_env("qwen-oauth").as_deref(),
            Some("qwen-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_google_gemini_cli() {
        let _guard = env_test_lock();
        let var = "HERMES_GEMINI_OAUTH_API_KEY";
        let _env = EnvSnapshot::capture(&[var]);
        std::env::remove_var(var);
        std::env::set_var(var, "google-gemini-oauth-token");
        assert_eq!(
            provider_api_key_from_env("google-gemini-cli").as_deref(),
            Some("google-gemini-oauth-token")
        );
    }

    #[test]
    fn provider_api_key_from_env_prefers_kimi_coding_key_for_code_provider() {
        let _guard = env_test_lock();
        let keys = [
            "KIMI_CODING_API_KEY",
            "KIMI_API_KEY",
            "MOONSHOT_API_KEY",
            "KIMI_CN_API_KEY",
        ];
        let _env = EnvSnapshot::capture(&keys);
        for key in keys {
            std::env::remove_var(key);
        }

        std::env::set_var("KIMI_API_KEY", "sk-legacy");
        std::env::set_var("KIMI_CODING_API_KEY", "sk-kimi-code");
        assert_eq!(
            provider_api_key_from_env("kimi-coding").as_deref(),
            Some("sk-kimi-code")
        );
        assert_eq!(
            provider_api_key_from_env("kimi").as_deref(),
            Some("sk-legacy")
        );
        std::env::set_var("KIMI_CN_API_KEY", "sk-cn");
        assert_eq!(
            provider_api_key_from_env("kimi-coding-cn").as_deref(),
            Some("sk-cn")
        );
    }

    #[test]
    fn provider_api_key_from_env_supports_extended_registry() {
        let _guard = env_test_lock();
        let env_vars = [
            "AI_GATEWAY_API_KEY",
            "DEEPSEEK_API_KEY",
            "HF_TOKEN",
            "KILOCODE_API_KEY",
            "NVIDIA_API_KEY",
            "OLLAMA_LOCAL_API_KEY",
            "LLAMA_CPP_API_KEY",
            "VLLM_API_KEY",
            "MLX_API_KEY",
            "APPLE_ANE_API_KEY",
            "SGLANG_API_KEY",
            "TGI_API_KEY",
            "LMSTUDIO_API_KEY",
            "LMDEPLOY_API_KEY",
            "LOCALAI_API_KEY",
            "KOBOLDCPP_API_KEY",
            "TEXT_GENERATION_WEBUI_API_KEY",
            "TABBYAPI_API_KEY",
            "NOVITA_API_KEY",
            "OPENCODE_GO_API_KEY",
            "OPENCODE_ZEN_API_KEY",
            "XAI_API_KEY",
            "XIAOMI_API_KEY",
            "ARCEEAI_API_KEY",
            "ARCEE_API_KEY",
            "GLM_API_KEY",
            "ZAI_API_KEY",
            "Z_AI_API_KEY",
            "GMI_API_KEY",
            "MINIMAX_CN_API_KEY",
            "NOUS_API_KEY",
            "COPILOT_GITHUB_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GITHUB_COPILOT_TOKEN",
            "TOKENHUB_API_KEY",
        ];
        let _env = EnvSnapshot::capture(&env_vars);
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }
        let checks = [
            ("AI_GATEWAY_API_KEY", "ai-gateway"),
            ("AI_GATEWAY_API_KEY", "vercel"),
            ("DEEPSEEK_API_KEY", "deepseek"),
            ("HF_TOKEN", "huggingface"),
            ("HF_TOKEN", "hf"),
            ("HF_TOKEN", "hugging-face"),
            ("HF_TOKEN", "huggingface-hub"),
            ("KILOCODE_API_KEY", "kilocode"),
            ("NVIDIA_API_KEY", "nvidia"),
            ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
            ("LLAMA_CPP_API_KEY", "llama-cpp"),
            ("VLLM_API_KEY", "vllm"),
            ("MLX_API_KEY", "mlx"),
            ("APPLE_ANE_API_KEY", "apple-ane"),
            ("SGLANG_API_KEY", "sglang"),
            ("TGI_API_KEY", "tgi"),
            ("LMSTUDIO_API_KEY", "lm-studio"),
            ("LMDEPLOY_API_KEY", "lm-deploy"),
            ("LOCALAI_API_KEY", "local-ai"),
            ("KOBOLDCPP_API_KEY", "kobold-cpp"),
            ("TEXT_GENERATION_WEBUI_API_KEY", "oobabooga"),
            ("TABBYAPI_API_KEY", "exllamav2"),
            ("NOVITA_API_KEY", "novita"),
            ("OPENCODE_GO_API_KEY", "opencode-go"),
            ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
            ("XAI_API_KEY", "xai"),
            ("XIAOMI_API_KEY", "xiaomi"),
            ("GLM_API_KEY", "zai"),
            ("GLM_API_KEY", "glm"),
            ("ZAI_API_KEY", "z-ai"),
            ("Z_AI_API_KEY", "zhipu"),
            ("GMI_API_KEY", "gmi-cloud"),
            ("GMI_API_KEY", "gmicloud"),
            ("ARCEEAI_API_KEY", "arcee-ai"),
            ("ARCEEAI_API_KEY", "arceeai"),
            ("XIAOMI_API_KEY", "mimo"),
            ("XIAOMI_API_KEY", "xiaomi-mimo"),
            ("TOKENHUB_API_KEY", "tencent-tokenhub"),
            ("TOKENHUB_API_KEY", "tencent"),
            ("TOKENHUB_API_KEY", "tokenhub"),
            ("MINIMAX_CN_API_KEY", "minimax_cn"),
            ("NOUS_API_KEY", "nous-api"),
            ("NOUS_API_KEY", "nous-portal-api"),
            ("COPILOT_GITHUB_TOKEN", "github-copilot"),
            ("GH_TOKEN", "github-models"),
            ("GITHUB_TOKEN", "copilot"),
            ("GITHUB_COPILOT_TOKEN", "copilot"),
        ];
        for (env_var, provider) in checks {
            for env_var in env_vars {
                std::env::remove_var(env_var);
            }
            let expected = format!("token-for-{provider}");
            std::env::set_var(env_var, expected.clone());
            assert_eq!(
                provider_api_key_from_env(provider).as_deref(),
                Some(expected.as_str())
            );
        }
    }

    #[test]
    fn normalize_runtime_provider_name_covers_local_and_cloud_aliases() {
        assert_eq!(
            normalize_runtime_provider_name("gemini-cli"),
            "google-gemini-cli"
        );
        assert_eq!(normalize_runtime_provider_name("nous_api"), "nous-api");
        assert_eq!(normalize_runtime_provider_name("nousapi"), "nous-api");
        assert_eq!(
            normalize_runtime_provider_name("nous-portal-api"),
            "nous-api"
        );
        assert_eq!(normalize_runtime_provider_name("moonshot"), "kimi");
        assert_eq!(normalize_runtime_provider_name("novita-ai"), "novita");
        assert_eq!(
            normalize_runtime_provider_name("alibaba-coding-plan"),
            "qwen"
        );
        assert_eq!(normalize_runtime_provider_name("opencode"), "opencode-zen");
        assert_eq!(normalize_runtime_provider_name("ollama"), "ollama-local");
        assert_eq!(normalize_runtime_provider_name("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("llamafile"), "llama-cpp");
        assert_eq!(normalize_runtime_provider_name("ollvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("llvm"), "vllm");
        assert_eq!(normalize_runtime_provider_name("mlx-lm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("vmlx"), "mlx");
        assert_eq!(normalize_runtime_provider_name("omlx"), "mlx");
        assert_eq!(normalize_runtime_provider_name("mlx-vlm"), "mlx");
        assert_eq!(normalize_runtime_provider_name("ane"), "apple-ane");
        assert_eq!(normalize_runtime_provider_name("lm-studio"), "lmstudio");
        assert_eq!(normalize_runtime_provider_name("lm_deploy"), "lmdeploy");
        assert_eq!(normalize_runtime_provider_name("local-ai"), "localai");
        assert_eq!(normalize_runtime_provider_name("kobold-cpp"), "koboldcpp");
        assert_eq!(
            normalize_runtime_provider_name("oobabooga"),
            "text-generation-webui"
        );
        assert_eq!(normalize_runtime_provider_name("tabby-api"), "tabbyapi");
        assert_eq!(normalize_runtime_provider_name("exllamav2"), "tabbyapi");
        assert_eq!(normalize_runtime_provider_name("glm"), "zai");
        assert_eq!(normalize_runtime_provider_name("z-ai"), "zai");
        assert_eq!(normalize_runtime_provider_name("zhipu"), "zai");
        assert_eq!(normalize_runtime_provider_name("github-copilot"), "copilot");
        assert_eq!(normalize_runtime_provider_name("github-models"), "copilot");
        assert_eq!(
            normalize_runtime_provider_name("github-copilot-acp"),
            "copilot-acp"
        );
        assert_eq!(
            normalize_runtime_provider_name("copilot-acp-agent"),
            "copilot-acp"
        );
        assert_eq!(normalize_runtime_provider_name("hf"), "huggingface");
        assert_eq!(
            normalize_runtime_provider_name("hugging-face"),
            "huggingface"
        );
        assert_eq!(
            normalize_runtime_provider_name("huggingface-hub"),
            "huggingface"
        );
        assert_eq!(normalize_runtime_provider_name("aigateway"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("vercel"), "ai-gateway");
        assert_eq!(normalize_runtime_provider_name("gmi-cloud"), "gmi");
        assert_eq!(normalize_runtime_provider_name("gmicloud"), "gmi");
        assert_eq!(
            normalize_runtime_provider_name("google-ai-studio"),
            "gemini"
        );
        assert_eq!(normalize_runtime_provider_name("arcee-ai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("arceeai"), "arcee");
        assert_eq!(normalize_runtime_provider_name("azure"), "azure-foundry");
        assert_eq!(
            normalize_runtime_provider_name("azure-ai-foundry"),
            "azure-foundry"
        );
        assert_eq!(normalize_runtime_provider_name("mimo"), "xiaomi");
        assert_eq!(normalize_runtime_provider_name("xiaomi-mimo"), "xiaomi");
        assert_eq!(
            normalize_runtime_provider_name("tencent-cloud"),
            "tencent-tokenhub"
        );
        assert_eq!(
            normalize_runtime_provider_name("tokenhub"),
            "tencent-tokenhub"
        );
        assert_eq!(normalize_runtime_provider_name("aws"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("aws-bedrock"), "bedrock");
        assert_eq!(normalize_runtime_provider_name("amazon"), "bedrock");
    }

    #[test]
    fn provider_base_url_from_env_supports_api_provider_aliases() {
        let _guard = env_test_lock();
        let env_vars = [
            "COPILOT_API_BASE_URL",
            "GLM_BASE_URL",
            "KIMI_BASE_URL",
            "MINIMAX_CN_BASE_URL",
            "GMI_BASE_URL",
            "HF_BASE_URL",
            "AI_GATEWAY_BASE_URL",
            "TOKENHUB_BASE_URL",
            "ARCEE_BASE_URL",
            "XIAOMI_BASE_URL",
            "BEDROCK_BASE_URL",
            "LMSTUDIO_BASE_URL",
            "LMDEPLOY_BASE_URL",
            "LOCALAI_BASE_URL",
            "KOBOLDCPP_BASE_URL",
            "TEXT_GENERATION_WEBUI_BASE_URL",
            "TABBYAPI_BASE_URL",
        ];
        let _env = EnvSnapshot::capture(&env_vars);
        for env_var in env_vars {
            std::env::remove_var(env_var);
        }

        std::env::set_var("COPILOT_API_BASE_URL", "https://copilot.example/v1");
        assert_eq!(
            provider_base_url_from_env("github-copilot").as_deref(),
            Some("https://copilot.example/v1")
        );
        std::env::set_var("GLM_BASE_URL", "https://glm.example/v4");
        assert_eq!(
            provider_base_url_from_env("z-ai").as_deref(),
            Some("https://glm.example/v4")
        );
        std::env::set_var("KIMI_BASE_URL", "https://kimi.example/v1");
        assert_eq!(
            provider_base_url_from_env("moonshot").as_deref(),
            Some("https://kimi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("kimi-coding").as_deref(),
            Some("https://kimi.example/v1")
        );
        std::env::set_var("MINIMAX_CN_BASE_URL", "https://minimax-cn.example/v1");
        assert_eq!(
            provider_base_url_from_env("minimax_cn").as_deref(),
            Some("https://minimax-cn.example/v1")
        );
        std::env::set_var("GMI_BASE_URL", "https://gmi.example/v1");
        assert_eq!(
            provider_base_url_from_env("gmi-cloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        assert_eq!(
            provider_base_url_from_env("gmicloud").as_deref(),
            Some("https://gmi.example/v1")
        );
        std::env::set_var("HF_BASE_URL", "https://hf.example/v1");
        assert_eq!(
            provider_base_url_from_env("huggingface-hub").as_deref(),
            Some("https://hf.example/v1")
        );
        std::env::set_var("AI_GATEWAY_BASE_URL", "https://gateway.example/v1");
        assert_eq!(
            provider_base_url_from_env("vercel").as_deref(),
            Some("https://gateway.example/v1")
        );
        std::env::set_var("TOKENHUB_BASE_URL", "https://tokenhub.example/v1");
        assert_eq!(
            provider_base_url_from_env("tencent").as_deref(),
            Some("https://tokenhub.example/v1")
        );
        std::env::set_var("ARCEE_BASE_URL", "https://arcee.example/v1");
        assert_eq!(
            provider_base_url_from_env("arcee-ai").as_deref(),
            Some("https://arcee.example/v1")
        );
        std::env::set_var("XIAOMI_BASE_URL", "https://mimo.example/v1");
        assert_eq!(
            provider_base_url_from_env("mimo").as_deref(),
            Some("https://mimo.example/v1")
        );
        std::env::set_var("BEDROCK_BASE_URL", "https://bedrock-runtime.example");
        assert_eq!(
            provider_base_url_from_env("aws").as_deref(),
            Some("https://bedrock-runtime.example")
        );
        std::env::set_var("LMSTUDIO_BASE_URL", "http://localhost:1234/v1");
        assert_eq!(
            provider_base_url_from_env("lm-studio").as_deref(),
            Some("http://localhost:1234/v1")
        );
        std::env::set_var("LMDEPLOY_BASE_URL", "http://localhost:23333/v1");
        assert_eq!(
            provider_base_url_from_env("lm-deploy").as_deref(),
            Some("http://localhost:23333/v1")
        );
        std::env::set_var("LOCALAI_BASE_URL", "http://localhost:8080/v1");
        assert_eq!(
            provider_base_url_from_env("local-ai").as_deref(),
            Some("http://localhost:8080/v1")
        );
        std::env::set_var("KOBOLDCPP_BASE_URL", "http://localhost:5001/v1");
        assert_eq!(
            provider_base_url_from_env("kobold-cpp").as_deref(),
            Some("http://localhost:5001/v1")
        );
        std::env::set_var("TEXT_GENERATION_WEBUI_BASE_URL", "http://localhost:5000/v1");
        assert_eq!(
            provider_base_url_from_env("oobabooga").as_deref(),
            Some("http://localhost:5000/v1")
        );
        std::env::set_var("TABBYAPI_BASE_URL", "http://localhost:5000/v1");
        assert_eq!(
            provider_base_url_from_env("exllamav2").as_deref(),
            Some("http://localhost:5000/v1")
        );
    }

    #[test]
    fn provider_default_base_url_supports_upstream_aliases() {
        assert_eq!(
            provider_default_base_url("github-copilot"),
            Some(COPILOT_BASE_URL)
        );
        assert_eq!(provider_default_base_url("glm"), Some(ZAI_BASE_URL));
        assert_eq!(
            provider_default_base_url("minimax_cn"),
            Some(MINIMAX_CN_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("huggingface-hub"),
            Some(HUGGINGFACE_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("vercel"),
            Some(AI_GATEWAY_BASE_URL)
        );
        assert_eq!(provider_default_base_url("gmi-cloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("gmicloud"), Some(GMI_BASE_URL));
        assert_eq!(provider_default_base_url("arcee-ai"), Some(ARCEE_BASE_URL));
        assert_eq!(provider_default_base_url("mimo"), Some(XIAOMI_BASE_URL));
        assert_eq!(
            provider_default_base_url("tencent"),
            Some(TENCENT_TOKENHUB_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("gemini"),
            Some(provider_profiles::GEMINI_OPENAI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("google-ai-studio"),
            Some(provider_profiles::GEMINI_OPENAI_BASE_URL)
        );
        assert_eq!(
            provider_profiles::gemini_openai_compatible_base_url(
                provider_profiles::GEMINI_NATIVE_BASE_URL
            ),
            provider_profiles::GEMINI_OPENAI_BASE_URL
        );
        assert_eq!(
            provider_default_base_url("llamafile"),
            Some(LLAMA_CPP_BASE_URL)
        );
        assert_eq!(provider_default_base_url("vmlx"), Some(MLX_BASE_URL));
        assert_eq!(provider_default_base_url("omlx"), Some(MLX_BASE_URL));
        assert_eq!(
            provider_default_base_url("lm-studio"),
            Some(LMSTUDIO_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("lmdeploy"),
            Some(LMDEPLOY_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("local-ai"),
            Some(LOCALAI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("kobold-cpp"),
            Some(KOBOLDCPP_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("oobabooga"),
            Some(TEXT_GENERATION_WEBUI_BASE_URL)
        );
        assert_eq!(
            provider_default_base_url("tabby-api"),
            Some(TABBYAPI_BASE_URL)
        );
    }

    #[test]
    fn allow_no_api_key_for_local_backends_and_private_base_urls() {
        assert!(allow_no_api_key("ollama-local", "ollama-local", None));
        assert!(allow_no_api_key("lmstudio", "lmstudio", None));
        assert!(allow_no_api_key("koboldcpp", "koboldcpp", None));
        assert!(allow_no_api_key(
            "text-generation-webui",
            "text-generation-webui",
            None
        ));
        assert!(allow_no_api_key(
            "openai",
            "openai",
            Some("http://127.0.0.1:11434/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://192.168.1.20:8000/v1")
        ));
        assert!(allow_no_api_key(
            "custom",
            "custom",
            Some("http://[::1]:11434/v1")
        ));
        assert!(!allow_no_api_key(
            "openai",
            "openai",
            Some("https://api.openai.com/v1")
        ));
    }
}
