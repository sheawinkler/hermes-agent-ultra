use hermes_auth::{AuthManager, FileTokenStore, OAuthCredential};
use hermes_cli::auth::{
    ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL, AnthropicOAuthLoginOptions,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, CodexDeviceCodeOptions, DEFAULT_CODEX_BASE_URL,
    DEFAULT_NOUS_CLIENT_ID, DEFAULT_NOUS_PORTAL_URL, DEFAULT_OPENAI_BASE_URL,
    GeminiOAuthLoginOptions, QWEN_OAUTH_CLIENT_ID, QWEN_OAUTH_TOKEN_URL,
    discover_existing_anthropic_oauth, discover_existing_openai_codex_oauth,
    discover_existing_openai_oauth, login_anthropic_oauth, login_google_gemini_cli_oauth,
    login_openai_codex_device_code, login_openai_device_code, resolve_qwen_runtime_credentials,
    save_codex_auth_state, save_openai_auth_state, save_provider_auth_state,
};
use hermes_cli::model_switch::{normalize_provider_model, provider_model_ids};
use hermes_config::{load_user_config_file, save_config_yaml, validate_config};
use hermes_core::AgentError;
use std::path::{Path, PathBuf};

fn discover_setup_env_sources() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(explicit) = std::env::var("HERMES_SETUP_IMPORT_ENV_PATH") {
        if !explicit.trim().is_empty() {
            candidates.push(PathBuf::from(explicit));
        }
    }
    if let Ok(py_home) = std::env::var("HERMES_PYTHON_HOME") {
        if !py_home.trim().is_empty() {
            candidates.push(PathBuf::from(py_home).join(".env"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Documents/Projects/hermes-agent/.env"));
        candidates.push(home.join("Projects/hermes-agent/.env"));
        candidates.push(home.join("Documents/Projects/hermes-agent-python/.env"));
    }
    if let Some(claw_dir) = hermes_cli::claw_migrate::find_openclaw_dir(None) {
        candidates.push(claw_dir.join(".env"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("hermes-agent/.env"));
        }
    }

    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|p| p.is_file())
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn parse_env_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn normalize_env_value(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_string()
}

fn read_env_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(crate) fn read_env_key(path: &Path, key: &str) -> Option<String> {
    let raw = read_env_text(path).ok()?;
    for line in raw.lines() {
        if let Some((k, v)) = parse_env_assignment(line) {
            if k == key {
                let value = normalize_env_value(&v);
                if !value.is_empty() {
                    return Some(value);
                }
                return None;
            }
        }
    }
    None
}

const SETUP_OPENAI_ENV_KEYS: &[&str] = &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"];
const SETUP_OPENAI_CODEX_ENV_KEYS: &[&str] = &["HERMES_OPENAI_CODEX_API_KEY"];
const SETUP_ANTHROPIC_ENV_KEYS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];
const SETUP_OPENROUTER_ENV_KEYS: &[&str] = &["OPENROUTER_API_KEY"];
const SETUP_GOOGLE_GEMINI_CLI_ENV_KEYS: &[&str] = &["HERMES_GEMINI_OAUTH_API_KEY"];
const SETUP_GEMINI_ENV_KEYS: &[&str] = &["GOOGLE_API_KEY", "GEMINI_API_KEY"];
const SETUP_NOUS_ENV_KEYS: &[&str] = &["NOUS_API_KEY"];
const SETUP_QWEN_ENV_KEYS: &[&str] = &["DASHSCOPE_API_KEY"];
const SETUP_QWEN_OAUTH_ENV_KEYS: &[&str] = &["HERMES_QWEN_OAUTH_API_KEY", "DASHSCOPE_API_KEY"];
const SETUP_ALIBABA_CODING_PLAN_ENV_KEYS: &[&str] = &["ALIBABA_CODING_PLAN_API_KEY"];
const SETUP_KIMI_CODING_ENV_KEYS: &[&str] = &["KIMI_API_KEY", "KIMI_CODING_API_KEY"];
const SETUP_KIMI_CODING_CN_ENV_KEYS: &[&str] = &["KIMI_CN_API_KEY"];
const SETUP_MINIMAX_ENV_KEYS: &[&str] = &["MINIMAX_API_KEY"];
const SETUP_MINIMAX_CN_ENV_KEYS: &[&str] = &["MINIMAX_CN_API_KEY"];
const SETUP_STEPFUN_ENV_KEYS: &[&str] = &["HERMES_STEPFUN_API_KEY", "STEPFUN_API_KEY"];
const SETUP_COPILOT_ENV_KEYS: &[&str] = &["GITHUB_COPILOT_TOKEN"];
const SETUP_AI_GATEWAY_ENV_KEYS: &[&str] = &["AI_GATEWAY_API_KEY"];
const SETUP_ARCEE_ENV_KEYS: &[&str] = &["ARCEEAI_API_KEY", "ARCEE_API_KEY"];
const SETUP_DEEPSEEK_ENV_KEYS: &[&str] = &["DEEPSEEK_API_KEY"];
const SETUP_HUGGINGFACE_ENV_KEYS: &[&str] = &["HF_TOKEN"];
const SETUP_KILOCODE_ENV_KEYS: &[&str] = &["KILOCODE_API_KEY"];
const SETUP_NVIDIA_ENV_KEYS: &[&str] = &["NVIDIA_API_KEY"];
const SETUP_OLLAMA_CLOUD_ENV_KEYS: &[&str] = &["OLLAMA_API_KEY"];
const SETUP_OLLAMA_LOCAL_ENV_KEYS: &[&str] = &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"];
const SETUP_LLAMA_CPP_ENV_KEYS: &[&str] = &["LLAMA_CPP_API_KEY"];
const SETUP_VLLM_ENV_KEYS: &[&str] = &["VLLM_API_KEY"];
const SETUP_MLX_ENV_KEYS: &[&str] = &["MLX_API_KEY"];
const SETUP_APPLE_ANE_ENV_KEYS: &[&str] = &["APPLE_ANE_API_KEY"];
const SETUP_SGLANG_ENV_KEYS: &[&str] = &["SGLANG_API_KEY"];
const SETUP_TGI_ENV_KEYS: &[&str] = &["TGI_API_KEY"];
const SETUP_OPENCODE_GO_ENV_KEYS: &[&str] = &["OPENCODE_GO_API_KEY"];
const SETUP_OPENCODE_ZEN_ENV_KEYS: &[&str] = &["OPENCODE_ZEN_API_KEY"];
const SETUP_XAI_ENV_KEYS: &[&str] = &["XAI_API_KEY"];
const SETUP_XIAOMI_ENV_KEYS: &[&str] = &["XIAOMI_API_KEY"];
const SETUP_ZAI_ENV_KEYS: &[&str] = &["GLM_API_KEY", "ZAI_API_KEY", "Z_AI_API_KEY"];
const SETUP_BEDROCK_ENV_KEYS: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
];
const HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY: &str = "HERMES_ENABLE_NOUS_MANAGED_TOOLS";

#[derive(Clone, Copy)]
pub(crate) struct SetupModelOption {
    pub(crate) provider: &'static str,
    pub(crate) model: &'static str,
    label: &'static str,
}

pub(crate) const SETUP_MODEL_OPTIONS: &[SetupModelOption] = &[
    SetupModelOption {
        provider: "nous",
        model: "nous:openai/gpt-5.5-pro",
        label: "Nous (recommended, OAuth)",
    },
    SetupModelOption {
        provider: "openai",
        model: "openai:gpt-4o",
        label: "OpenAI gpt-4o",
    },
    SetupModelOption {
        provider: "openai",
        model: "openai:gpt-4o-mini",
        label: "OpenAI gpt-4o-mini (fast & cheap)",
    },
    SetupModelOption {
        provider: "anthropic",
        model: "anthropic:claude-3-5-sonnet",
        label: "Anthropic Claude (OAuth/API key)",
    },
    SetupModelOption {
        provider: "openrouter",
        model: "openrouter:auto",
        label: "OpenRouter auto (multi-provider)",
    },
    SetupModelOption {
        provider: "openai-codex",
        model: "openai-codex:gpt-5.3-codex",
        label: "OpenAI Codex (OAuth)",
    },
    SetupModelOption {
        provider: "google-gemini-cli",
        model: "google-gemini-cli:gemini-3.1-pro-preview",
        label: "Google Gemini CLI (OAuth)",
    },
    SetupModelOption {
        provider: "gemini",
        model: "gemini:gemini-3.1-pro-preview",
        label: "Google AI Studio Gemini (API key)",
    },
    SetupModelOption {
        provider: "qwen-oauth",
        model: "qwen-oauth:qwen-plus-latest",
        label: "Qwen OAuth (CLI token)",
    },
    SetupModelOption {
        provider: "qwen",
        model: "qwen:qwen-plus-latest",
        label: "Alibaba DashScope Qwen",
    },
    SetupModelOption {
        provider: "alibaba",
        model: "alibaba:qwen-plus-latest",
        label: "Alibaba Cloud DashScope",
    },
    SetupModelOption {
        provider: "alibaba-coding-plan",
        model: "alibaba-coding-plan:qwen-plus-latest",
        label: "Alibaba Coding Plan",
    },
    SetupModelOption {
        provider: "deepseek",
        model: "deepseek:deepseek-chat",
        label: "DeepSeek",
    },
    SetupModelOption {
        provider: "kimi-coding",
        model: "kimi-coding:kimi-k2.6",
        label: "Kimi Coding (Moonshot)",
    },
    SetupModelOption {
        provider: "kimi-coding-cn",
        model: "kimi-coding-cn:kimi-k2.6",
        label: "Kimi Coding China",
    },
    SetupModelOption {
        provider: "stepfun",
        model: "stepfun:step-3.5-flash",
        label: "StepFun Step Plan",
    },
    SetupModelOption {
        provider: "minimax",
        model: "minimax:MiniMax-M2.7",
        label: "MiniMax",
    },
    SetupModelOption {
        provider: "minimax-cn",
        model: "minimax-cn:MiniMax-M2.7",
        label: "MiniMax China",
    },
    SetupModelOption {
        provider: "zai",
        model: "zai:glm-5.1",
        label: "Z.AI / GLM",
    },
    SetupModelOption {
        provider: "xai",
        model: "xai:grok-3-mini",
        label: "xAI",
    },
    SetupModelOption {
        provider: "nvidia",
        model: "nvidia:nvidia/nemotron-3-super-120b-a12b",
        label: "NVIDIA NIM",
    },
    SetupModelOption {
        provider: "huggingface",
        model: "huggingface:Qwen/Qwen3.5-397B-A17B",
        label: "Hugging Face Router",
    },
    SetupModelOption {
        provider: "opencode-go",
        model: "opencode-go:kimi-k2.6",
        label: "OpenCode Go",
    },
    SetupModelOption {
        provider: "opencode-zen",
        model: "opencode-zen:gpt-5.4",
        label: "OpenCode Zen",
    },
    SetupModelOption {
        provider: "kilocode",
        model: "kilocode:openai/gpt-5.4",
        label: "KiloCode",
    },
    SetupModelOption {
        provider: "ai-gateway",
        model: "ai-gateway:openai/gpt-5.4",
        label: "Vercel AI Gateway",
    },
    SetupModelOption {
        provider: "arcee",
        model: "arcee:trinity-large-preview",
        label: "Arcee AI",
    },
    SetupModelOption {
        provider: "xiaomi",
        model: "xiaomi:mimo-v2.5-pro",
        label: "Xiaomi MiMo",
    },
    SetupModelOption {
        provider: "ollama-cloud",
        model: "ollama-cloud:llama3.1:8b",
        label: "Ollama Cloud",
    },
    SetupModelOption {
        provider: "ollama-local",
        model: "ollama-local:qwen3:14b",
        label: "Ollama Local (OpenAI-compatible)",
    },
    SetupModelOption {
        provider: "llama-cpp",
        model: "llama-cpp:local-gguf",
        label: "llama.cpp server (local)",
    },
    SetupModelOption {
        provider: "vllm",
        model: "vllm:NousResearch/Meta-Llama-3-8B-Instruct",
        label: "vLLM server (local/self-host)",
    },
    SetupModelOption {
        provider: "mlx",
        model: "mlx:mlx-community/Qwen3-8B-4bit",
        label: "MLX server (Apple Silicon)",
    },
    SetupModelOption {
        provider: "apple-ane",
        model: "apple-ane:ane-default",
        label: "Apple ANE private endpoint",
    },
    SetupModelOption {
        provider: "sglang",
        model: "sglang:default",
        label: "SGLang OpenAI-compatible",
    },
    SetupModelOption {
        provider: "tgi",
        model: "tgi:default",
        label: "Text Generation Inference",
    },
    SetupModelOption {
        provider: "copilot",
        model: "copilot:gpt-5.4",
        label: "GitHub Copilot",
    },
];

pub(crate) fn default_setup_model_choice() -> usize {
    SETUP_MODEL_OPTIONS
        .iter()
        .position(|option| option.provider == "nous")
        .map(|idx| idx + 1)
        .unwrap_or(1)
}

pub(crate) fn setup_provider_defaults() -> Vec<SetupModelOption> {
    let mut seen = std::collections::BTreeSet::new();
    let mut providers = Vec::new();
    for option in SETUP_MODEL_OPTIONS {
        if seen.insert(option.provider) {
            providers.push(*option);
        }
    }
    providers
}

pub(crate) fn setup_default_model_pick_index(
    selected_provider: &str,
    current_provider_model: &str,
    displayed_suggested_models: &[String],
) -> usize {
    if displayed_suggested_models.is_empty() {
        return 0;
    }
    let normalized_target = current_provider_model.trim().to_ascii_lowercase();
    let target_model_id = current_provider_model
        .split_once(':')
        .map(|(_, model)| model.trim().to_ascii_lowercase())
        .unwrap_or_else(|| current_provider_model.trim().to_ascii_lowercase());

    if let Some(idx) = displayed_suggested_models.iter().position(|candidate| {
        let candidate_norm = candidate.trim().to_ascii_lowercase();
        if candidate_norm == normalized_target {
            return true;
        }
        if let Some((provider, model)) = candidate_norm.split_once(':') {
            if provider == selected_provider && model == target_model_id {
                return true;
            }
        }
        candidate_norm == target_model_id
    }) {
        return idx;
    }

    if selected_provider == "nous" {
        if let Some(idx) = displayed_suggested_models.iter().position(|candidate| {
            candidate
                .trim()
                .eq_ignore_ascii_case("moonshotai/kimi-k2.6")
        }) {
            return idx;
        }
    }

    0
}

pub(crate) fn setup_provider_display(provider: &str) -> &'static str {
    match provider {
        "openai" => "OpenAI",
        "openai-codex" => "OpenAI Codex",
        "anthropic" => "Anthropic",
        "google-gemini-cli" => "Google Gemini CLI",
        "gemini" => "Google AI Studio",
        "openrouter" => "OpenRouter",
        "qwen" => "Alibaba DashScope",
        "alibaba" => "Alibaba Cloud DashScope",
        "qwen-oauth" => "Qwen OAuth",
        "alibaba-coding-plan" => "Alibaba Coding Plan",
        "deepseek" => "DeepSeek",
        "kimi-coding" => "Kimi Coding",
        "kimi-coding-cn" => "Kimi Coding CN",
        "minimax" => "MiniMax",
        "minimax-cn" => "MiniMax CN",
        "stepfun" => "StepFun",
        "nous" => "Nous",
        "ai-gateway" => "Vercel AI Gateway",
        "arcee" => "Arcee",
        "bedrock" => "AWS Bedrock",
        "copilot" => "GitHub Copilot",
        "huggingface" => "Hugging Face",
        "kilocode" => "KiloCode",
        "nvidia" => "NVIDIA NIM",
        "ollama-cloud" => "Ollama Cloud",
        "ollama-local" => "Ollama Local",
        "llama-cpp" => "llama.cpp Server",
        "vllm" => "vLLM Server",
        "mlx" => "MLX Server",
        "apple-ane" => "Apple ANE Endpoint",
        "sglang" => "SGLang Server",
        "tgi" => "Text Gen Inference",
        "opencode-go" => "OpenCode Go",
        "opencode-zen" => "OpenCode Zen",
        "xai" => "xAI",
        "xiaomi" => "Xiaomi MiMo",
        "zai" => "Z.AI / GLM",
        _ => "Provider",
    }
}

pub(crate) fn setup_provider_env_keys(provider: &str) -> &'static [&'static str] {
    match provider {
        "openai" => SETUP_OPENAI_ENV_KEYS,
        "anthropic" => SETUP_ANTHROPIC_ENV_KEYS,
        "openai-codex" => SETUP_OPENAI_CODEX_ENV_KEYS,
        "google-gemini-cli" => SETUP_GOOGLE_GEMINI_CLI_ENV_KEYS,
        "gemini" => SETUP_GEMINI_ENV_KEYS,
        "openrouter" => SETUP_OPENROUTER_ENV_KEYS,
        "qwen" | "alibaba" => SETUP_QWEN_ENV_KEYS,
        "qwen-oauth" => SETUP_QWEN_OAUTH_ENV_KEYS,
        "alibaba-coding-plan" => SETUP_ALIBABA_CODING_PLAN_ENV_KEYS,
        "deepseek" => SETUP_DEEPSEEK_ENV_KEYS,
        "kimi-coding" => SETUP_KIMI_CODING_ENV_KEYS,
        "kimi-coding-cn" => SETUP_KIMI_CODING_CN_ENV_KEYS,
        "minimax" => SETUP_MINIMAX_ENV_KEYS,
        "minimax-cn" => SETUP_MINIMAX_CN_ENV_KEYS,
        "stepfun" => SETUP_STEPFUN_ENV_KEYS,
        "nous" => SETUP_NOUS_ENV_KEYS,
        "ai-gateway" => SETUP_AI_GATEWAY_ENV_KEYS,
        "arcee" => SETUP_ARCEE_ENV_KEYS,
        "bedrock" => SETUP_BEDROCK_ENV_KEYS,
        "copilot" => SETUP_COPILOT_ENV_KEYS,
        "huggingface" => SETUP_HUGGINGFACE_ENV_KEYS,
        "kilocode" => SETUP_KILOCODE_ENV_KEYS,
        "nvidia" => SETUP_NVIDIA_ENV_KEYS,
        "ollama-cloud" => SETUP_OLLAMA_CLOUD_ENV_KEYS,
        "ollama-local" => SETUP_OLLAMA_LOCAL_ENV_KEYS,
        "llama-cpp" => SETUP_LLAMA_CPP_ENV_KEYS,
        "vllm" => SETUP_VLLM_ENV_KEYS,
        "mlx" => SETUP_MLX_ENV_KEYS,
        "apple-ane" => SETUP_APPLE_ANE_ENV_KEYS,
        "sglang" => SETUP_SGLANG_ENV_KEYS,
        "tgi" => SETUP_TGI_ENV_KEYS,
        "opencode-go" => SETUP_OPENCODE_GO_ENV_KEYS,
        "opencode-zen" => SETUP_OPENCODE_ZEN_ENV_KEYS,
        "xai" => SETUP_XAI_ENV_KEYS,
        "xiaomi" => SETUP_XIAOMI_ENV_KEYS,
        "zai" => SETUP_ZAI_ENV_KEYS,
        _ => &[],
    }
}

pub(crate) fn setup_provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider {
        "openai-codex" => Some("https://chatgpt.com/backend-api/codex"),
        "google-gemini-cli" => Some("cloudcode-pa://google"),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta"),
        "qwen" | "alibaba" => Some("https://dashscope-intl.aliyuncs.com/compatible-mode/v1"),
        "alibaba-coding-plan" => Some("https://coding-intl.dashscope.aliyuncs.com/v1"),
        "deepseek" => Some("https://api.deepseek.com/v1"),
        "kimi-coding" => Some("https://api.moonshot.ai/v1"),
        "kimi-coding-cn" => Some("https://api.moonshot.cn/v1"),
        "minimax-cn" => Some("https://api.minimaxi.com/anthropic"),
        "stepfun" => Some("https://api.stepfun.ai/step_plan/v1"),
        "ai-gateway" => Some("https://ai-gateway.vercel.sh/v1"),
        "arcee" => Some("https://api.arcee.ai/api/v1"),
        "huggingface" => Some("https://router.huggingface.co/v1"),
        "kilocode" => Some("https://api.kilo.ai/api/gateway"),
        "nvidia" => Some("https://integrate.api.nvidia.com/v1"),
        "ollama-cloud" => Some("https://ollama.com/v1"),
        "ollama-local" => Some("http://127.0.0.1:11434/v1"),
        "llama-cpp" => Some("http://127.0.0.1:8080/v1"),
        "vllm" => Some("http://127.0.0.1:8000/v1"),
        "mlx" => Some("http://127.0.0.1:8080/v1"),
        "apple-ane" => Some("http://127.0.0.1:8081/v1"),
        "sglang" => Some("http://127.0.0.1:30000/v1"),
        "tgi" => Some("http://127.0.0.1:8082/v1"),
        "opencode-go" => Some("https://opencode.ai/zen/go/v1"),
        "opencode-zen" => Some("https://opencode.ai/zen/v1"),
        "xai" => Some("https://api.x.ai/v1"),
        "xiaomi" => Some("https://api.xiaomimimo.com/v1"),
        "zai" => Some("https://api.z.ai/api/paas/v4"),
        _ => None,
    }
}

pub(crate) fn setup_provider_requires_api_key(provider: &str) -> bool {
    !matches!(
        provider,
        "ollama-local" | "llama-cpp" | "vllm" | "mlx" | "apple-ane" | "sglang" | "tgi"
    )
}

pub(crate) fn local_backend_base_url_env_var(provider: &str) -> Option<&'static str> {
    match provider {
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

pub(crate) fn merge_missing_env_keys(
    src: &Path,
    dst: &Path,
    label: &str,
) -> Result<usize, AgentError> {
    let src_content =
        read_env_text(src).map_err(|e| AgentError::Io(format!("read {}: {}", src.display(), e)))?;
    let existing = read_env_text(dst).unwrap_or_default();

    let existing_keys: std::collections::HashSet<String> = existing
        .lines()
        .filter_map(parse_env_assignment)
        .map(|(k, _)| k)
        .collect();

    let mut to_import = Vec::new();
    for line in src_content.lines() {
        if let Some((k, v)) = parse_env_assignment(line) {
            if existing_keys.contains(&k) {
                continue;
            }
            if normalize_env_value(&v).is_empty() {
                continue;
            }
            to_import.push(line.trim().to_string());
        }
    }

    if to_import.is_empty() {
        return Ok(0);
    }

    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("# Imported by `hermes setup` from {label}\n"));
    for line in &to_import {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(dst, out)
        .map_err(|e| AgentError::Io(format!("write {}: {}", dst.display(), e)))?;
    Ok(to_import.len())
}

pub(crate) fn upsert_env_key(path: &Path, key: &str, value: &str) -> Result<(), AgentError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut updated_lines = Vec::new();
    let mut replaced = false;
    for line in existing.lines() {
        if let Some((k, _)) = parse_env_assignment(line) {
            if k == key {
                updated_lines.push(format!("{key}={value}"));
                replaced = true;
                continue;
            }
        }
        updated_lines.push(line.to_string());
    }
    if !replaced {
        updated_lines.push(format!("{key}={value}"));
    }
    let mut updated = updated_lines.join("\n");
    if !updated.ends_with('\n') {
        updated.push('\n');
    }
    std::fs::write(path, updated)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn maybe_import_legacy_env(
    reader: &mut dyn std::io::BufRead,
    env_path: &Path,
) -> Result<(), AgentError> {
    use std::io::Write;

    let sources: Vec<PathBuf> = discover_setup_env_sources()
        .into_iter()
        .filter(|p| p != env_path)
        .collect();
    if sources.is_empty() {
        return Ok(());
    }

    println!("\nDetected legacy environment file(s):");
    for (idx, src) in sources.iter().enumerate() {
        println!("  {}) {}", idx + 1, src.display());
    }

    print!(
        "Import missing keys into {} from the first source? [Y/n]: ",
        env_path.display()
    );
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    reader.read_line(&mut answer).ok();
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        println!("Skipped legacy .env import.");
        return Ok(());
    }

    let source = &sources[0];
    let imported = merge_missing_env_keys(
        source,
        env_path,
        &source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("legacy source"),
    )?;
    if imported == 0 {
        println!("No new keys to import from {}.", source.display());
    } else {
        println!(
            "Imported {} key(s) from {} into {}.",
            imported,
            source.display(),
            env_path.display()
        );
    }
    Ok(())
}

pub(crate) async fn run_setup(cli: crate::Cli) -> Result<(), AgentError> {
    use std::io::{self, Write};

    println!("Hermes Agent Ultra \u{2014} Setup Wizard");
    println!("===========================\n");

    let config_dir = crate::hermes_state_root(&cli);
    println!("Config directory: {}", config_dir.display());

    // 1. Create directory structure
    let subdirs = ["profiles", "sessions", "logs", "skills"];
    for dir in [config_dir.clone()]
        .into_iter()
        .chain(subdirs.iter().map(|d| config_dir.join(d)))
    {
        if dir.exists() {
            println!("  ✓ {} exists", dir.display());
        } else {
            std::fs::create_dir_all(&dir).map_err(|e| {
                AgentError::Io(format!("Failed to create {}: {}", dir.display(), e))
            })?;
            println!("  ✓ Created {}", dir.display());
        }
    }

    let config_path = config_dir.join("config.yaml");
    let env_path = config_dir.join(".env");
    let stdin = io::stdin();

    // 2. Optional import from legacy Python/OpenClaw .env files
    {
        let mut reader = stdin.lock();
        maybe_import_legacy_env(&mut reader, &env_path)?;
    }

    // 3. Choose setup depth first (upstream parity: quick/full first).
    let mode_labels = vec![
        "Quick setup (recommended) — provider, auth, model".to_string(),
        "Full setup — quick + personality + optional sections".to_string(),
    ];
    let mode_pick = hermes_cli::curses_select("Choose setup mode", &mode_labels, 0);
    let full_setup = mode_pick.confirmed && mode_pick.index == 1;

    // 4. Prompt for provider first (upstream parity: provider before model).
    let provider_defaults = setup_provider_defaults();
    let default_provider = SETUP_MODEL_OPTIONS
        .get(default_setup_model_choice().saturating_sub(1))
        .map(|option| option.provider)
        .unwrap_or("nous");
    let default_provider_index = provider_defaults
        .iter()
        .position(|option| option.provider == default_provider)
        .unwrap_or(0);
    let provider_labels: Vec<String> = provider_defaults
        .iter()
        .map(|option| {
            let auth_label = if crate::auth_main::provider_supports_oauth(option.provider) {
                "OAuth/API key"
            } else if !setup_provider_requires_api_key(option.provider) {
                "Local / optional key"
            } else if option.provider == "bedrock" {
                "AWS credentials"
            } else {
                "API key"
            };
            format!(
                "{:<22} {:<18} {}",
                setup_provider_display(option.provider),
                format!("({auth_label})"),
                option.label
            )
        })
        .collect();
    println!("\nSetup order: provider -> auth -> model.");
    let selected =
        hermes_cli::curses_select("Select provider", &provider_labels, default_provider_index);
    let selected_option = provider_defaults
        .get(selected.index)
        .unwrap_or(&provider_defaults[default_provider_index]);
    let mut model = selected_option.model.to_string();
    let selected_provider = selected_option.provider.to_string();
    let selected_provider_label = setup_provider_display(&selected_provider);
    let selected_provider_env_keys = setup_provider_env_keys(&selected_provider);
    let env_keys_display = selected_provider_env_keys.join("/");

    // 5. Prompt for selected provider API key (or OAuth device login where supported)
    let has_selected_provider_env_key = selected_provider_env_keys.iter().any(|key| {
        std::env::var(key)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
            || read_env_key(&env_path, key).is_some()
    });
    let mut api_key = String::new();
    let mut stored_provider_secret_in_vault = false;
    let mut selected_base_url_override =
        setup_provider_default_base_url(&selected_provider).map(ToString::to_string);
    let mut selected_oauth_token_url: Option<String> = None;
    let mut selected_oauth_client_id: Option<String> = None;
    let mut selected_nous_oauth_authenticated = false;
    let mut selected_nous_managed_tools_enabled: Option<bool> = None;

    if crate::auth_main::provider_supports_oauth(&selected_provider) {
        print!(
            "\nAuthenticate with {} OAuth flow now? [Y/n]: ",
            selected_provider_label
        );
        io::stdout().flush().ok();
        let answer = crate::read_setup_stdin_line(&stdin);
        let use_oauth = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
        if use_oauth {
            let store = FileTokenStore::new(config_dir.join("auth").join("tokens.json")).await?;
            let manager = AuthManager::new(store);
            match selected_provider.as_str() {
                "nous" => {
                    let (resolved, auth_path, _imported_existing, state) =
                        crate::auth_main::resolve_or_fresh_login_nous(&manager, true).await?;
                    println!("  ✓ Saved Nous OAuth state: {}", auth_path.display());
                    selected_base_url_override = Some(resolved.base_url);
                    selected_oauth_token_url = Some(format!(
                        "{}/api/oauth/token",
                        if state.portal_base_url.trim().is_empty() {
                            DEFAULT_NOUS_PORTAL_URL
                        } else {
                            state.portal_base_url.trim_end_matches('/')
                        }
                    ));
                    selected_oauth_client_id = Some(if state.client_id.trim().is_empty() {
                        DEFAULT_NOUS_CLIENT_ID.to_string()
                    } else {
                        state.client_id.clone()
                    });
                    stored_provider_secret_in_vault = true;
                    selected_nous_oauth_authenticated = true;
                }
                "openai-codex" => {
                    let imported = discover_existing_openai_codex_oauth()?;
                    let state = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing OpenAI Codex OAuth session: {}",
                            imported.source_path.display()
                        );
                        imported.state
                    } else {
                        login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?
                    };
                    let auth_path = save_codex_auth_state(&state)?;
                    println!(
                        "  ✓ Saved OpenAI Codex OAuth state: {}",
                        auth_path.display()
                    );
                    manager
                        .save_credential(OAuthCredential {
                            provider: "openai-codex".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            refresh_token: state.tokens.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: state
                                .tokens
                                .expires_in
                                .filter(|secs| *secs > 0)
                                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs)),
                        })
                        .await?;
                    selected_oauth_token_url = Some(CODEX_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(CODEX_OAUTH_CLIENT_ID.to_string());
                    selected_base_url_override = Some(DEFAULT_CODEX_BASE_URL.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "openai" => {
                    let imported = discover_existing_openai_oauth()?;
                    let state = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing OpenAI OAuth session: {}",
                            imported.source_path.display()
                        );
                        imported.state
                    } else {
                        login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                    };
                    let auth_path = save_openai_auth_state(&state)?;
                    println!("  ✓ Saved OpenAI OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "openai".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            refresh_token: state.tokens.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: state
                                .tokens
                                .expires_in
                                .filter(|secs| *secs > 0)
                                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs)),
                        })
                        .await?;
                    selected_oauth_token_url = Some(CODEX_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(CODEX_OAUTH_CLIENT_ID.to_string());
                    selected_base_url_override = Some(DEFAULT_OPENAI_BASE_URL.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "anthropic" => {
                    let imported = discover_existing_anthropic_oauth()?;
                    let (state, source_label) = if let Some(imported) = imported {
                        println!(
                            "  ✓ Detected existing Anthropic OAuth session: {}",
                            imported.source_path.display()
                        );
                        (imported.state, imported.source)
                    } else {
                        (
                            login_anthropic_oauth(AnthropicOAuthLoginOptions::default()).await?,
                            "hermes_pkce".to_string(),
                        )
                    };
                    let auth_state = serde_json::json!({
                        "access_token": state.access_token.clone(),
                        "refresh_token": state.refresh_token.clone(),
                        "expires_at_ms": state.expires_at_ms,
                        "source": source_label,
                    });
                    let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                    println!("  ✓ Saved Anthropic OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "anthropic".to_string(),
                            access_token: state.access_token.clone(),
                            refresh_token: state.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: crate::auth_main::parse_unix_millis_utc(
                                state.expires_at_ms,
                            ),
                        })
                        .await?;
                    selected_oauth_token_url = Some(ANTHROPIC_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(ANTHROPIC_OAUTH_CLIENT_ID.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "qwen-oauth" => {
                    let creds = resolve_qwen_runtime_credentials(
                        false,
                        true,
                        hermes_cli::auth::QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                    )
                    .await?;
                    let auth_state = serde_json::to_value(&creds.tokens)
                        .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                    let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                    println!("  ✓ Saved Qwen OAuth state: {}", auth_path.display());
                    manager
                        .save_credential(OAuthCredential {
                            provider: "qwen-oauth".to_string(),
                            access_token: creds.api_key.clone(),
                            refresh_token: creds.refresh_token.clone(),
                            token_type: creds.token_type.clone(),
                            scope: None,
                            expires_at: crate::auth_main::parse_unix_millis_utc(
                                creds.expires_at_ms,
                            ),
                        })
                        .await?;
                    selected_base_url_override = Some(creds.base_url.clone());
                    selected_oauth_token_url = Some(QWEN_OAUTH_TOKEN_URL.to_string());
                    selected_oauth_client_id = Some(QWEN_OAUTH_CLIENT_ID.to_string());
                    stored_provider_secret_in_vault = true;
                }
                "google-gemini-cli" => {
                    let creds =
                        login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default()).await?;
                    let auth_state = serde_json::json!({
                        "access_token": creds.api_key.clone(),
                        "refresh_token": creds.refresh_token.clone(),
                        "expires_at_ms": creds.expires_at_ms,
                        "email": creds.email.clone(),
                        "project_id": creds.project_id.clone(),
                        "source": creds.source.clone(),
                    });
                    let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                    println!(
                        "  ✓ Saved Google Gemini OAuth state: {}",
                        auth_path.display()
                    );
                    manager
                        .save_credential(OAuthCredential {
                            provider: "google-gemini-cli".to_string(),
                            access_token: creds.api_key.clone(),
                            refresh_token: creds.refresh_token.clone(),
                            token_type: "bearer".to_string(),
                            scope: None,
                            expires_at: crate::auth_main::parse_unix_millis_utc(
                                creds.expires_at_ms,
                            ),
                        })
                        .await?;
                    selected_base_url_override = Some(creds.base_url.clone());
                    stored_provider_secret_in_vault = true;
                }
                _ => {}
            }
        }
    }

    if selected_provider == "nous" {
        if selected_nous_oauth_authenticated {
            print!("\nEnable Nous managed tool-gateway integrations (recommended) [Y/n]: ");
            io::stdout().flush().ok();
            let answer = crate::read_setup_stdin_line(&stdin);
            let enable = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
            selected_nous_managed_tools_enabled = Some(enable);
        } else {
            println!(
                "\nNote: Nous managed tool-gateway integrations require Nous OAuth login in setup."
            );
            println!(
                "      Re-run setup with Nous OAuth, then set {}=1 if needed.",
                HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY
            );
        }
    }

    if selected_provider == "bedrock" {
        println!(
            "\nAWS Bedrock uses AWS credential chain (env/profile/role). Skipping API key prompt."
        );
    } else if !setup_provider_requires_api_key(&selected_provider) {
        println!(
            "\n{} is a local/self-host OpenAI-compatible backend. API key is optional.",
            selected_provider_label
        );
        if has_selected_provider_env_key {
            print!(
                "{} API key (optional, leave blank to keep {} from environment/{}): ",
                selected_provider_label,
                env_keys_display,
                env_path.display()
            );
            io::stdout().flush().ok();
            api_key = crate::read_setup_stdin_line(&stdin).trim().to_string();
        }
    } else if !stored_provider_secret_in_vault {
        if has_selected_provider_env_key {
            print!(
                "\n{} API key (leave blank to keep {} from environment/{}): ",
                selected_provider_label,
                env_keys_display,
                env_path.display()
            );
        } else {
            print!(
                "\n{} API key (leave blank to skip): ",
                selected_provider_label
            );
        }
        io::stdout().flush().ok();
        api_key = crate::read_setup_stdin_line(&stdin).trim().to_string();
    }

    if !api_key.is_empty() {
        print!(
            "Store {} key in encrypted vault (recommended) [Y/n]: ",
            selected_provider_label
        );
        io::stdout().flush().ok();
        let answer = crate::read_setup_stdin_line(&stdin);
        let use_vault = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
        if use_vault {
            let store = FileTokenStore::new(config_dir.join("auth").join("tokens.json")).await?;
            let manager = AuthManager::new(store);
            manager
                .save_credential(OAuthCredential {
                    provider: selected_provider.clone(),
                    access_token: api_key.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            stored_provider_secret_in_vault = true;
        }
    }

    // 6. Prompt for model after provider auth is established.
    let suggested_provider_models = provider_model_ids(&selected_provider).await;
    let suggested_limit = if selected_provider == "nous" {
        usize::MAX
    } else {
        25
    };
    let displayed_suggested_models: Vec<String> = suggested_provider_models
        .into_iter()
        .take(suggested_limit)
        .collect();
    if displayed_suggested_models.is_empty() {
        print!("Model ID for {} [{}]: ", selected_provider_label, model);
        io::stdout().flush().ok();
        let model_override = crate::read_setup_stdin_line(&stdin);
        let model_override = model_override.trim();
        if !model_override.is_empty() {
            let candidate = if model_override.contains(':') {
                model_override.to_string()
            } else {
                format!("{}:{}", selected_provider, model_override)
            };
            model = normalize_provider_model(&candidate)?;
        }
    } else {
        let mut suggested_labels: Vec<String> = displayed_suggested_models
            .iter()
            .map(|candidate| {
                if candidate.contains(':') {
                    candidate.to_string()
                } else {
                    format!("{}:{}", selected_provider, candidate)
                }
            })
            .collect();
        suggested_labels.push("Custom model ID…".to_string());
        let model_title = if selected_provider == "nous" {
            format!(
                "Select {} model ({} available)",
                selected_provider_label,
                displayed_suggested_models.len()
            )
        } else {
            format!("Select {} model", selected_provider_label)
        };
        let default_model_index =
            setup_default_model_pick_index(&selected_provider, &model, &displayed_suggested_models);
        let suggested_pick =
            hermes_cli::curses_select(&model_title, &suggested_labels, default_model_index);
        if suggested_pick.confirmed && suggested_pick.index < displayed_suggested_models.len() {
            let candidate = &displayed_suggested_models[suggested_pick.index];
            model = if candidate.contains(':') {
                candidate.to_string()
            } else {
                format!("{}:{}", selected_provider, candidate)
            };
        } else if suggested_pick.confirmed {
            print!(
                "Custom model ID for {} (provider prefix optional) [{}]: ",
                selected_provider_label, model
            );
            io::stdout().flush().ok();
            let model_override = crate::read_setup_stdin_line(&stdin);
            let model_override = model_override.trim();
            if !model_override.is_empty() {
                let candidate = if model_override.contains(':') {
                    model_override.to_string()
                } else {
                    format!("{}:{}", selected_provider, model_override)
                };
                model = normalize_provider_model(&candidate)?;
            }
        }
    }

    // 7. Prompt for personality (full setup only).
    let personality = if full_setup {
        let builtin_personalities = hermes_agent::builtin_personality_names();
        let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
        println!("\nBuilt-in personality guide:");
        for (name, usage) in builtin_descriptions {
            println!("  - {:<14} {}", name, usage);
        }
        print!(
            "\nPersonality (default, {}) [default]: ",
            builtin_personalities.join(", ")
        );
        io::stdout().flush().ok();
        let personality = crate::read_setup_stdin_line(&stdin);
        let personality = personality.trim();
        if personality.is_empty() {
            "default".to_string()
        } else {
            if !personality.contains(char::is_whitespace)
                && !personality.eq_ignore_ascii_case("default")
                && !builtin_personalities
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(personality))
            {
                println!(
                    "  ! '{}' is not built-in. Hermes will look for personalities/{}.md.",
                    personality, personality
                );
            }
            personality.to_string()
        }
    } else {
        println!("\nQuick setup: using default personality.");
        "default".to_string()
    };

    // 8. Write config.yaml
    let mut overwrite_config = true;
    if config_path.exists() {
        print!("\nconfig.yaml already exists. Overwrite? [y/N]: ");
        io::stdout().flush().ok();
        let answer = crate::read_setup_stdin_line(&stdin);
        if !answer.trim().eq_ignore_ascii_case("y") {
            overwrite_config = false;
            println!("Keeping existing config.yaml.");
        }
    }

    // Preserve existing fields (including platform_toolsets) instead of
    // rewriting config.yaml from scratch.
    let mut disk =
        load_user_config_file(&config_path).map_err(|e| AgentError::Config(e.to_string()))?;
    if overwrite_config {
        disk.model = Some(model.clone());
        disk.personality = Some(personality.to_string());
        disk.max_turns = 250;

        let _ = upsert_env_key(
            &env_path,
            "HERMES_AUTH_DEFAULT_PROVIDER",
            selected_provider.as_str(),
        );

        if !api_key.is_empty() && !stored_provider_secret_in_vault {
            let provider = disk
                .llm_providers
                .entry(selected_provider.clone())
                .or_insert_with(hermes_config::LlmProviderConfig::default);
            provider.api_key = Some(api_key.clone());
        } else if stored_provider_secret_in_vault {
            println!(
                "  ✓ Stored {} key in encrypted vault: {}",
                selected_provider_label,
                config_dir.join("auth").join("tokens.json").display()
            );
        } else if has_selected_provider_env_key {
            println!(
                "  ✓ Keeping {} from environment/{} for runtime auth",
                env_keys_display,
                env_path.display(),
            );
        }
        let provider = disk
            .llm_providers
            .entry(selected_provider.clone())
            .or_insert_with(hermes_config::LlmProviderConfig::default);
        if let Some(base_url) = selected_base_url_override {
            provider.base_url = Some(base_url);
        }
        if let Some(token_url) = selected_oauth_token_url {
            provider.oauth_token_url = Some(token_url);
        }
        if let Some(client_id) = selected_oauth_client_id {
            provider.oauth_client_id = Some(client_id);
        }
        validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
        save_config_yaml(&config_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
        println!("\n  ✓ Wrote config.yaml");
    }

    if let Some(enabled) = selected_nous_managed_tools_enabled {
        let flag = if enabled { "1" } else { "0" };
        upsert_env_key(&env_path, HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY, flag)?;
        println!("  ✓ {}={}", HERMES_ENABLE_NOUS_MANAGED_TOOLS_ENV_KEY, flag);
    }

    // 6. Write default profile
    let default_profile = config_dir.join("profiles").join("default.yaml");
    if !default_profile.exists() {
        let profile_model = disk.model.clone().unwrap_or_else(|| model.clone());
        let profile_personality = disk
            .personality
            .clone()
            .unwrap_or_else(|| personality.to_string());
        let profile_content = format!(
            "# Default Hermes Profile\nname: default\nmodel: {}\npersonality: {}\n",
            profile_model, profile_personality,
        );
        std::fs::write(&default_profile, profile_content)
            .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
        println!("  ✓ Created default profile");
    }

    // 7. Ensure SOUL.md exists so users can customize persona immediately.
    let soul_path = config_dir.join("SOUL.md");
    if !soul_path.exists() {
        let soul_template = "# Hermes Agent Persona\n\n<!--\nCustomize this file to control how Hermes communicates.\nThis file is loaded every message; no restart needed.\nDelete this file (or leave it empty) to use the default personality.\n-->\n";
        std::fs::write(&soul_path, soul_template)
            .map_err(|e| AgentError::Io(format!("Failed to write SOUL.md: {}", e)))?;
        println!("  ✓ Created SOUL.md");
    }

    if full_setup
        && hermes_cli::gateway_main::prompt_yes_no("\nConfigure optional setup sections now?", true)
            .await?
    {
        crate::cli_setup::run_optional_setup_sections(&cli, &disk).await?;
    } else if !full_setup {
        println!("Skipped optional setup sections (quick setup mode).");
    }

    println!(
        "\nSetup complete! Run `hermes-ultra` (or `hermes-agent-ultra`/`hermes`) to start an interactive session."
    );
    println!(
        "Run `hermes-ultra doctor` (or `hermes-agent-ultra doctor`/`hermes doctor`) to check system requirements."
    );
    Ok(())
}
