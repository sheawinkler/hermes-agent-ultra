//! Local OpenAI-compatible backend registry.
//!
//! The agent loop and provider-runtime crate both need the same local/private
//! server semantics: aliases, default URLs, optional API-key env vars, and
//! no-key allowance for loopback/private endpoints.

use std::net::IpAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBackendSpec {
    pub provider: &'static str,
    pub display_name: &'static str,
    pub default_base_url: &'static str,
    pub base_url_env_var: &'static str,
    pub api_key_env_vars: &'static [&'static str],
    pub aliases: &'static [&'static str],
}

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

pub const LOCAL_BACKEND_SPECS: &[LocalBackendSpec] = &[
    LocalBackendSpec {
        provider: "ollama-local",
        display_name: "Ollama Local",
        default_base_url: OLLAMA_LOCAL_BASE_URL,
        base_url_env_var: "OLLAMA_BASE_URL",
        api_key_env_vars: &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"],
        aliases: &["ollama"],
    },
    LocalBackendSpec {
        provider: "llama-cpp",
        display_name: "llama.cpp / llamafile",
        default_base_url: LLAMA_CPP_BASE_URL,
        base_url_env_var: "LLAMA_CPP_BASE_URL",
        api_key_env_vars: &["LLAMA_CPP_API_KEY"],
        aliases: &["llama.cpp", "llamacpp", "llamafile"],
    },
    LocalBackendSpec {
        provider: "vllm",
        display_name: "vLLM",
        default_base_url: VLLM_BASE_URL,
        base_url_env_var: "VLLM_BASE_URL",
        api_key_env_vars: &["VLLM_API_KEY"],
        aliases: &["ollvm", "llvm"],
    },
    LocalBackendSpec {
        provider: "mlx",
        display_name: "MLX OpenAI-compatible server",
        default_base_url: MLX_BASE_URL,
        base_url_env_var: "MLX_BASE_URL",
        api_key_env_vars: &["MLX_API_KEY"],
        aliases: &[
            "mlx-lm",
            "apple-mlx",
            "vmlx",
            "omlx",
            "mlx-vlm",
            "mlxvlm",
            "mlx-openai-server",
        ],
    },
    LocalBackendSpec {
        provider: "apple-ane",
        display_name: "Apple ANE endpoint",
        default_base_url: APPLE_ANE_BASE_URL,
        base_url_env_var: "APPLE_ANE_BASE_URL",
        api_key_env_vars: &["APPLE_ANE_API_KEY"],
        aliases: &["ane", "apple-neural-engine", "neural-engine"],
    },
    LocalBackendSpec {
        provider: "sglang",
        display_name: "SGLang",
        default_base_url: SGLANG_BASE_URL,
        base_url_env_var: "SGLANG_BASE_URL",
        api_key_env_vars: &["SGLANG_API_KEY"],
        aliases: &[],
    },
    LocalBackendSpec {
        provider: "tgi",
        display_name: "Text Generation Inference",
        default_base_url: TGI_BASE_URL,
        base_url_env_var: "TGI_BASE_URL",
        api_key_env_vars: &["TGI_API_KEY", "HUGGINGFACE_API_KEY"],
        aliases: &["text-generation-inference"],
    },
    LocalBackendSpec {
        provider: "lmstudio",
        display_name: "LM Studio",
        default_base_url: LMSTUDIO_BASE_URL,
        base_url_env_var: "LMSTUDIO_BASE_URL",
        api_key_env_vars: &["LMSTUDIO_API_KEY"],
        aliases: &["lm-studio", "lm_studio", "lm studio"],
    },
    LocalBackendSpec {
        provider: "lmdeploy",
        display_name: "LMDeploy",
        default_base_url: LMDEPLOY_BASE_URL,
        base_url_env_var: "LMDEPLOY_BASE_URL",
        api_key_env_vars: &["LMDEPLOY_API_KEY"],
        aliases: &["lm-deploy", "lm_deploy"],
    },
    LocalBackendSpec {
        provider: "localai",
        display_name: "LocalAI",
        default_base_url: LOCALAI_BASE_URL,
        base_url_env_var: "LOCALAI_BASE_URL",
        api_key_env_vars: &["LOCALAI_API_KEY"],
        aliases: &["local-ai", "local_ai"],
    },
    LocalBackendSpec {
        provider: "koboldcpp",
        display_name: "KoboldCpp",
        default_base_url: KOBOLDCPP_BASE_URL,
        base_url_env_var: "KOBOLDCPP_BASE_URL",
        api_key_env_vars: &["KOBOLDCPP_API_KEY"],
        aliases: &["kobold-cpp", "kobold"],
    },
    LocalBackendSpec {
        provider: "text-generation-webui",
        display_name: "text-generation-webui / oobabooga",
        default_base_url: TEXT_GENERATION_WEBUI_BASE_URL,
        base_url_env_var: "TEXT_GENERATION_WEBUI_BASE_URL",
        api_key_env_vars: &["TEXT_GENERATION_WEBUI_API_KEY"],
        aliases: &[
            "text-generation-web-ui",
            "textgen-webui",
            "textgen_webui",
            "oobabooga",
        ],
    },
    LocalBackendSpec {
        provider: "tabbyapi",
        display_name: "TabbyAPI / ExLlamaV2",
        default_base_url: TABBYAPI_BASE_URL,
        base_url_env_var: "TABBYAPI_BASE_URL",
        api_key_env_vars: &["TABBYAPI_API_KEY"],
        aliases: &["tabby-api", "tabby_api", "exllama", "exllamav2"],
    },
];

pub fn local_backend_specs() -> &'static [LocalBackendSpec] {
    LOCAL_BACKEND_SPECS
}

pub fn local_backend_spec(provider: &str) -> Option<&'static LocalBackendSpec> {
    let normalized = provider.trim().to_ascii_lowercase();
    LOCAL_BACKEND_SPECS.iter().find(|spec| {
        spec.provider == normalized
            || spec
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(provider.trim()))
    })
}

pub fn is_local_backend_provider(provider: &str) -> bool {
    local_backend_spec(provider).is_some()
}

pub fn local_backend_default_base_url(provider: &str) -> Option<&'static str> {
    local_backend_spec(provider).map(|spec| spec.default_base_url)
}

pub fn local_backend_base_url_env_var(provider: &str) -> Option<&'static str> {
    local_backend_spec(provider).map(|spec| spec.base_url_env_var)
}

pub fn local_backend_api_key_env_vars(provider: &str) -> Option<&'static [&'static str]> {
    local_backend_spec(provider).map(|spec| spec.api_key_env_vars)
}

pub fn local_backend_base_url_from_env(provider: &str) -> Option<String> {
    local_backend_base_url_env_var(provider).and_then(non_empty_env)
}

pub fn local_backend_api_key_from_env(provider: &str) -> Option<String> {
    local_backend_api_key_env_vars(provider)?
        .iter()
        .find_map(|env_var| non_empty_env(env_var))
}

pub fn local_backend_resolved_base_url(provider: &str) -> Option<String> {
    let spec = local_backend_spec(provider)?;
    non_empty_env(spec.base_url_env_var).or_else(|| Some(spec.default_base_url.to_string()))
}

pub fn is_local_or_private_base_url(base_url: &str) -> bool {
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

    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
        };
    }
    false
}

fn non_empty_env(env_var: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}
