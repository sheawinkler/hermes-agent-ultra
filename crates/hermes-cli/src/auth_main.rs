//! Auth command handlers extracted from main.rs

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::Cli;
use crate::app::provider_api_key_from_env;
use crate::auth::{
    ANTHROPIC_OAUTH_CLIENT_ID, ANTHROPIC_OAUTH_TOKEN_URL, AnthropicOAuthLoginOptions,
    CODEX_OAUTH_CLIENT_ID, CODEX_OAUTH_TOKEN_URL, CodexDeviceCodeOptions,
    DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS, GeminiOAuthLoginOptions,
    NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, NousAuthState, NousDeviceCodeOptions,
    NousRuntimeCredentials, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS, clear_provider_auth_state,
    discover_existing_anthropic_oauth, discover_existing_nous_oauth,
    discover_existing_openai_codex_oauth, discover_existing_openai_oauth,
    get_anthropic_oauth_status, get_gemini_oauth_auth_status, get_qwen_auth_status,
    login_anthropic_oauth, login_google_gemini_cli_oauth, login_nous_device_code,
    login_openai_codex_device_code, login_openai_device_code, read_provider_auth_state,
    resolve_gemini_oauth_runtime_credentials, resolve_nous_runtime_credentials,
    resolve_qwen_runtime_credentials, save_codex_auth_state, save_nous_auth_state,
    save_openai_auth_state, save_provider_auth_state,
};
use crate::paths::CliStateRoot;
use crate::providers::{OAUTH_CAPABLE_PROVIDERS, known_providers};
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, KeyInit};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::{DateTime, Utc};
use hermes_auth::{
    AuthManager, FileTokenStore, OAuth2Endpoints, OAuthCredential, exchange_refresh_token,
};
use hermes_config::{
    GatewayConfig, PlatformConfig, hermes_home, load_config, load_user_config_file,
    save_config_yaml, validate_config,
};
use hermes_core::AgentError;
use qrcode::QrCode;
use rand::TryRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::gateway_main::{configure_platform_basic_prompts, platform_token_or_extra};
use crate::prompt::prompt_line;
use crate::state_paths::hermes_state_root;

/// Default auth provider: CLI arg, then `HERMES_AUTH_DEFAULT_PROVIDER`, then `nous`.
///
/// Set `HERMES_AUTH_DEFAULT_PROVIDER=telegram` if you primarily use the Telegram gateway.
pub fn resolve_auth_provider(provider: Option<String>) -> String {
    if let Some(raw) = provider.filter(|s| !s.trim().is_empty()) {
        return normalize_auth_provider(&raw);
    }

    if let Ok(pool) = std::env::var("HERMES_AUTH_PROVIDER_POOL") {
        for item in pool.split(',') {
            let item = item.trim();
            if !item.is_empty() {
                return normalize_auth_provider(item);
            }
        }
    }

    let raw = std::env::var("HERMES_AUTH_DEFAULT_PROVIDER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| infer_default_auth_provider_from_config())
        .unwrap_or_else(|| "nous".to_string());
    normalize_auth_provider(&raw)
}

pub fn infer_default_auth_provider_from_config() -> Option<String> {
    let cfg = load_config(None).ok()?;
    let model = cfg.model?;
    let provider = model
        .split_once(':')
        .map(|(provider, _)| provider.trim())
        .filter(|provider| !provider.is_empty())?;
    Some(provider.to_string())
}

pub fn normalize_auth_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "wechat" | "wx" => "weixin".to_string(),
        "qq" => "qqbot".to_string(),
        "tg" => "telegram".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "step" | "step-plan" => "stepfun".to_string(),
        "moonshot" | "kimi" => "kimi-coding".to_string(),
        "minimax-cn" | "minimax_cn" | "minimax-china" => "minimax-cn".to_string(),
        "dashscope" | "aliyun" | "alibaba-cloud" => "alibaba".to_string(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => {
            "alibaba-coding-plan".to_string()
        }
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "api-server" => "api_server".to_string(),
        "home-assistant" => "homeassistant".to_string(),
        "wecom-callback" => "wecom_callback".to_string(),
        "mm" => "mattermost".to_string(),
        "github-copilot" => "copilot".to_string(),
        other => other.to_string(),
    }
}

pub fn gateway_platform_provider_key(provider: &str) -> Option<&'static str> {
    match provider {
        "discord" => Some("discord"),
        "slack" => Some("slack"),
        "matrix" => Some("matrix"),
        "mattermost" => Some("mattermost"),
        "signal" => Some("signal"),
        "whatsapp" => Some("whatsapp"),
        "dingtalk" => Some("dingtalk"),
        "feishu" => Some("feishu"),
        "wecom" => Some("wecom"),
        "wecom_callback" => Some("wecom_callback"),
        "qqbot" | "qq" => Some("qqbot"),
        "bluebubbles" => Some("bluebubbles"),
        "email" => Some("email"),
        "sms" => Some("sms"),
        "homeassistant" => Some("homeassistant"),
        "webhook" => Some("webhook"),
        "api_server" => Some("api_server"),
        _ => None,
    }
}

pub fn normalize_secret_provider(provider: &str) -> String {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "github-copilot" => "copilot".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "moonshot" | "kimi" => "kimi-coding".to_string(),
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
        "ollama" => "ollama-local".to_string(),
        "llama.cpp" | "llamacpp" => "llama-cpp".to_string(),
        "ollvm" | "llvm" => "vllm".to_string(),
        "mlx-lm" | "apple-mlx" => "mlx".to_string(),
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "text-generation-inference" => "tgi".to_string(),
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "dashscope" | "aliyun" | "alibaba-cloud" => "alibaba".to_string(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => {
            "alibaba-coding-plan".to_string()
        }
        _ => p,
    }
}

pub fn secret_provider_aliases(provider: &str) -> Vec<String> {
    match normalize_secret_provider(provider).as_str() {
        "anthropic" => vec![
            "anthropic".to_string(),
            "claude".to_string(),
            "claude-code".to_string(),
        ],
        "moonshot" | "kimi" | "kimi-coding" => vec![
            "kimi-coding".to_string(),
            "kimi".to_string(),
            "moonshot".to_string(),
        ],
        "kimi-coding-cn" => vec!["kimi-coding-cn".to_string()],
        "stepfun" => vec!["stepfun".to_string(), "step".to_string()],
        "copilot" => vec!["copilot".to_string(), "github-copilot".to_string()],
        "openai-codex" => vec!["openai-codex".to_string(), "codex".to_string()],
        "google-gemini-cli" => vec![
            "google-gemini-cli".to_string(),
            "gemini-cli".to_string(),
            "gemini-oauth".to_string(),
        ],
        "zai" => vec![
            "zai".to_string(),
            "glm".to_string(),
            "z-ai".to_string(),
            "z.ai".to_string(),
        ],
        "xai" => vec![
            "xai".to_string(),
            "x-ai".to_string(),
            "x.ai".to_string(),
            "grok".to_string(),
        ],
        "nvidia" => vec![
            "nvidia".to_string(),
            "nvidia-nim".to_string(),
            "nim".to_string(),
        ],
        "huggingface" => vec!["huggingface".to_string(), "hf".to_string()],
        "ai-gateway" => vec!["ai-gateway".to_string(), "aigateway".to_string()],
        "opencode-zen" => vec!["opencode-zen".to_string(), "opencode".to_string()],
        "kilocode" => vec!["kilocode".to_string(), "kilo".to_string()],
        "ollama-local" => vec!["ollama-local".to_string(), "ollama".to_string()],
        "llama-cpp" => vec![
            "llama-cpp".to_string(),
            "llama.cpp".to_string(),
            "llamacpp".to_string(),
        ],
        "vllm" => vec!["vllm".to_string(), "ollvm".to_string(), "llvm".to_string()],
        "mlx" => vec!["mlx".to_string(), "mlx-lm".to_string()],
        "apple-ane" => vec![
            "apple-ane".to_string(),
            "ane".to_string(),
            "apple-neural-engine".to_string(),
        ],
        "sglang" => vec!["sglang".to_string()],
        "tgi" => vec!["tgi".to_string(), "text-generation-inference".to_string()],
        p => vec![p.to_string()],
    }
}

pub fn provider_env_var(provider: &str) -> Option<&'static str> {
    match normalize_secret_provider(provider).as_str() {
        "openai" => Some("HERMES_OPENAI_API_KEY"),
        "openai-codex" => Some("HERMES_OPENAI_CODEX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "google-gemini-cli" => Some("HERMES_GEMINI_OAUTH_API_KEY"),
        "gemini" => Some("GOOGLE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "qwen" | "alibaba" => Some("DASHSCOPE_API_KEY"),
        "alibaba-coding-plan" => Some("ALIBABA_CODING_PLAN_API_KEY"),
        "qwen-oauth" => Some("HERMES_QWEN_OAUTH_API_KEY"),
        "moonshot" | "kimi" | "kimi-coding" => Some("KIMI_API_KEY"),
        "kimi-coding-cn" => Some("KIMI_CN_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "stepfun" => Some("STEPFUN_API_KEY"),
        "nous" => Some("NOUS_API_KEY"),
        "copilot" => Some("GITHUB_COPILOT_TOKEN"),
        "ai-gateway" => Some("AI_GATEWAY_API_KEY"),
        "arcee" => Some("ARCEEAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "huggingface" => Some("HF_TOKEN"),
        "kilocode" => Some("KILOCODE_API_KEY"),
        "nvidia" => Some("NVIDIA_API_KEY"),
        "ollama-cloud" => Some("OLLAMA_API_KEY"),
        "ollama-local" => Some("OLLAMA_LOCAL_API_KEY"),
        "llama-cpp" => Some("LLAMA_CPP_API_KEY"),
        "vllm" => Some("VLLM_API_KEY"),
        "mlx" => Some("MLX_API_KEY"),
        "apple-ane" => Some("APPLE_ANE_API_KEY"),
        "sglang" => Some("SGLANG_API_KEY"),
        "tgi" => Some("TGI_API_KEY"),
        "opencode-go" => Some("OPENCODE_GO_API_KEY"),
        "opencode-zen" => Some("OPENCODE_ZEN_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "xiaomi" => Some("XIAOMI_API_KEY"),
        "zai" => Some("GLM_API_KEY"),
        _ => None,
    }
}

pub fn provider_supports_oauth(provider: &str) -> bool {
    let normalized = normalize_auth_provider(provider);
    crate::providers::OAUTH_CAPABLE_PROVIDERS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(normalized.as_str()))
}

pub fn resolve_auth_type_for_provider(provider: &str, requested: Option<&str>) -> String {
    if let Some(raw) = requested.map(str::trim).filter(|v| !v.is_empty()) {
        return raw.replace('-', "_").to_ascii_lowercase();
    }
    if provider_supports_oauth(provider) {
        "oauth".to_string()
    } else {
        "api_key".to_string()
    }
}

pub fn parse_rfc3339_utc(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

pub fn parse_unix_millis_utc(value: Option<i64>) -> Option<DateTime<Utc>> {
    value.and_then(DateTime::from_timestamp_millis)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AuthPoolEntry {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) auth_type: String,
    pub(crate) source: String,
    pub(crate) access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_status_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_error_code: Option<u16>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub(crate) struct AuthPoolStore {
    #[serde(default)]
    pub(crate) providers: BTreeMap<String, Vec<AuthPoolEntry>>,
}

pub fn load_auth_pool_store(path: &Path) -> Result<AuthPoolStore, AgentError> {
    if !path.exists() {
        return Ok(AuthPoolStore::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw).map_err(|e| AgentError::Config(format!("parse pool: {}", e)))
}

pub fn save_auth_pool_store(path: &Path, store: &AuthPoolStore) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(store).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

pub fn resolve_pool_target(entries: &[AuthPoolEntry], target: &str) -> Option<usize> {
    if let Ok(index) = target.parse::<usize>() {
        if index >= 1 && index <= entries.len() {
            return Some(index - 1);
        }
    }
    if let Some((idx, _)) = entries.iter().enumerate().find(|(_, e)| e.id == target) {
        return Some(idx);
    }
    entries.iter().position(|e| e.label == target)
}

pub async fn lookup_secret_from_vault(
    token_store: &FileTokenStore,
    provider: &str,
) -> Option<(String, String)> {
    for candidate in secret_provider_aliases(provider) {
        if let Some(cred) = token_store.get(&candidate).await {
            if !cred.access_token.trim().is_empty() {
                return Some((candidate, cred.access_token));
            }
        }
    }
    None
}

pub async fn resolve_llm_login_token(cli: &Cli, provider: &str) -> Result<String, AgentError> {
    if let Some(k) = provider_api_key_from_env(provider) {
        return Ok(k);
    }
    let vault_path = CliStateRoot::from_state_root(&hermes_state_root(cli)).secret_vault();
    if vault_path.exists() {
        let store = FileTokenStore::new(vault_path).await?;
        if let Some((_provider, token)) = lookup_secret_from_vault(&store, provider).await {
            return Ok(token);
        }
    }
    let cfg =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    if let Some(k) = cfg
        .llm_providers
        .get(provider)
        .and_then(|c| c.api_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(k.to_string());
    }
    let fallback_var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    let msg = format!(
        "No API key in env or config for provider '{}'.\n\
         Set {} (or `hermes secrets set {}`; plaintext fallback: `hermes config set llm.{}.api_key ...`) or paste key now: ",
        provider, fallback_var, provider, provider
    );
    let pasted = prompt_line(msg).await?;
    if pasted.is_empty() {
        return Err(AgentError::Config(format!(
            "Missing API key for provider '{}'",
            provider
        )));
    }
    Ok(pasted)
}

pub async fn hydrate_provider_env_from_vault_for_cli(cli: &Cli) -> Result<(), AgentError> {
    let path = CliStateRoot::from_state_root(&hermes_state_root(cli)).secret_vault();
    if !path.exists() {
        return Ok(());
    }
    let store = FileTokenStore::new(path).await?;
    let manager = AuthManager::new(store.clone());
    let mut hydrated_nous_from_vault = false;

    if let Some((_provider, token)) = lookup_secret_from_vault(&store, "nous").await {
        crate::env_vars::set_var("NOUS_API_KEY", token);
        hydrated_nous_from_vault = true;
    }

    if !hydrated_nous_from_vault {
        match resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                crate::env_vars::set_var("NOUS_API_KEY", creds.api_key.clone());
                if !creds.base_url.trim().is_empty() {
                    crate::env_vars::set_var("NOUS_INFERENCE_BASE_URL", creds.base_url.clone());
                }
                let expires_at = parse_rfc3339_utc(creds.expires_at.as_deref());
                let _ = manager
                    .save_credential(OAuthCredential {
                        provider: "nous".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: creds.scope,
                        expires_at,
                    })
                    .await;
            }
            Err(err) => {
                tracing::debug!("Nous runtime credential refresh skipped: {}", err);
            }
        }
    }

    let env_bindings = [
        ("HERMES_OPENAI_API_KEY", "openai"),
        ("OPENAI_API_KEY", "openai"),
        ("HERMES_OPENAI_CODEX_API_KEY", "openai-codex"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("ANTHROPIC_TOKEN", "anthropic"),
        ("CLAUDE_CODE_OAUTH_TOKEN", "anthropic"),
        ("HERMES_GEMINI_OAUTH_API_KEY", "google-gemini-cli"),
        ("GOOGLE_API_KEY", "gemini"),
        ("GEMINI_API_KEY", "gemini"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("ALIBABA_CODING_PLAN_API_KEY", "alibaba-coding-plan"),
        ("HERMES_QWEN_OAUTH_API_KEY", "qwen-oauth"),
        ("KIMI_API_KEY", "kimi-coding"),
        ("KIMI_CODING_API_KEY", "kimi-coding"),
        ("KIMI_CN_API_KEY", "kimi-coding-cn"),
        ("MOONSHOT_API_KEY", "kimi-coding"),
        ("MINIMAX_API_KEY", "minimax"),
        ("MINIMAX_CN_API_KEY", "minimax-cn"),
        ("STEPFUN_API_KEY", "stepfun"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
        ("AI_GATEWAY_API_KEY", "ai-gateway"),
        ("ARCEEAI_API_KEY", "arcee"),
        ("ARCEE_API_KEY", "arcee"),
        ("DEEPSEEK_API_KEY", "deepseek"),
        ("HF_TOKEN", "huggingface"),
        ("KILOCODE_API_KEY", "kilocode"),
        ("NVIDIA_API_KEY", "nvidia"),
        ("OLLAMA_API_KEY", "ollama-cloud"),
        ("OLLAMA_LOCAL_API_KEY", "ollama-local"),
        ("LLAMA_CPP_API_KEY", "llama-cpp"),
        ("VLLM_API_KEY", "vllm"),
        ("MLX_API_KEY", "mlx"),
        ("APPLE_ANE_API_KEY", "apple-ane"),
        ("SGLANG_API_KEY", "sglang"),
        ("TGI_API_KEY", "tgi"),
        ("OPENCODE_GO_API_KEY", "opencode-go"),
        ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
        ("XAI_API_KEY", "xai"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("GLM_API_KEY", "zai"),
        ("ZAI_API_KEY", "zai"),
        ("Z_AI_API_KEY", "zai"),
    ];

    for (env_var, provider) in env_bindings {
        let env_present = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
        if let Some(current) = env_present {
            if provider_supports_oauth(provider) {
                if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await
                {
                    if secret.trim() != current.trim() {
                        crate::env_vars::set_var(env_var, secret);
                    }
                }
            }
            continue;
        }
        if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await {
            crate::env_vars::set_var(env_var, secret);
        }
    }
    Ok(())
}

pub fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        return "(empty)".to_string();
    }
    if secret.len() <= 8 {
        return "*".repeat(secret.len());
    }
    format!(
        "{}***{}",
        &secret[..4],
        &secret[secret.len().saturating_sub(4)..]
    )
}

pub fn is_weixin_provider(provider: &str) -> bool {
    provider == "weixin"
}

pub fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| is_truthy(&v))
}

pub async fn telegram_bot_token_from_env_or_prompt() -> Result<String, AgentError> {
    if let Ok(t) = std::env::var("TELEGRAM_BOT_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Telegram bot token (from @BotFather): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("telegram token prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let t = line.trim().to_string();
    if t.is_empty() {
        return Err(AgentError::Config(
            "Telegram bot token cannot be empty (set TELEGRAM_BOT_TOKEN or paste token)".into(),
        ));
    }
    Ok(t)
}

pub async fn weixin_account_id_from_env_or_prompt() -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("WEIXIN_ACCOUNT_ID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Weixin account_id (个人号 wxid/账号标识): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("weixin account_id prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let v = line.trim().to_string();
    if v.is_empty() {
        return Err(AgentError::Config(
            "Weixin account_id cannot be empty (set WEIXIN_ACCOUNT_ID or input manually)".into(),
        ));
    }
    Ok(v)
}

pub fn weixin_account_file_path(account_id: &str) -> PathBuf {
    hermes_home()
        .join("weixin")
        .join("accounts")
        .join(format!("{account_id}.json"))
}

pub fn load_persisted_weixin_token(account_id: &str) -> Option<String> {
    let p = weixin_account_file_path(account_id);
    let s = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

pub fn save_persisted_weixin_account(
    account_id: &str,
    token: &str,
    base_url: Option<&str>,
    user_id: Option<&str>,
) -> Result<(), AgentError> {
    let p = weixin_account_file_path(account_id);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create weixin account dir: {e}")))?;
    }
    let payload = serde_json::json!({
        "token": token,
        "base_url": base_url.unwrap_or(""),
        "user_id": user_id.unwrap_or(""),
        "saved_at": Utc::now().to_rfc3339(),
    });
    std::fs::write(&p, payload.to_string())
        .map_err(|e| AgentError::Io(format!("write weixin account file {}: {e}", p.display())))?;
    Ok(())
}

pub async fn weixin_token_from_env_or_prompt(account_id: &str) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("WEIXIN_TOKEN") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(v) = load_persisted_weixin_token(account_id) {
        return Ok(v);
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Weixin iLink token (WEIXIN_TOKEN): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("weixin token prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let v = line.trim().to_string();
    if v.is_empty() {
        return Err(AgentError::Config(
            "Weixin token cannot be empty (set WEIXIN_TOKEN / saved account file / input manually)"
                .into(),
        ));
    }
    Ok(v)
}

pub async fn qqbot_app_id_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("QQ_APP_ID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(current) = existing {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter QQBot app_id (QQ_APP_ID): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("qqbot app_id prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let app_id = line.trim().to_string();
    if app_id.is_empty() {
        return Err(AgentError::Config(
            "QQBot app_id cannot be empty (set QQ_APP_ID or input manually)".to_string(),
        ));
    }
    Ok(app_id)
}

pub async fn qqbot_client_secret_from_env_or_prompt(
    existing: Option<&str>,
) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("QQ_CLIENT_SECRET") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(current) = existing {
        let trimmed = current.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter QQBot client_secret (QQ_CLIENT_SECRET): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("qqbot client_secret prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let secret = line.trim().to_string();
    if secret.is_empty() {
        return Err(AgentError::Config(
            "QQBot client_secret cannot be empty (set QQ_CLIENT_SECRET or input manually)"
                .to_string(),
        ));
    }
    Ok(secret)
}

pub fn qqbot_portal_host_from_disk(disk: &GatewayConfig) -> String {
    if let Some(cfg) = disk.platforms.get("qqbot") {
        for key in ["portal_host", "qq_portal_host"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    if let Ok(v) = std::env::var("QQ_PORTAL_HOST") {
        let s = v.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    "q.qq.com".to_string()
}

pub fn qqbot_onboard_endpoints_from_disk(disk: &GatewayConfig) -> (String, String) {
    let mut create_path = "/lite/create_bind_task".to_string();
    let mut poll_path = "/lite/poll_bind_result".to_string();

    if let Some(cfg) = disk.platforms.get("qqbot") {
        for key in ["onboard_create_path", "qr_create_path"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    create_path = s.to_string();
                    break;
                }
            }
        }
        for key in ["onboard_poll_path", "qr_poll_path"] {
            if let Some(v) = cfg.extra.get(key).and_then(|v| v.as_str()) {
                let s = v.trim();
                if !s.is_empty() {
                    poll_path = s.to_string();
                    break;
                }
            }
        }
    }

    if let Ok(v) = std::env::var("QQ_ONBOARD_CREATE_PATH") {
        let s = v.trim();
        if !s.is_empty() {
            create_path = s.to_string();
        }
    }
    if let Ok(v) = std::env::var("QQ_ONBOARD_POLL_PATH") {
        let s = v.trim();
        if !s.is_empty() {
            poll_path = s.to_string();
        }
    }

    (create_path, poll_path)
}

pub fn qqbot_generate_bind_key_base64() -> String {
    let mut key = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut key)
        .expect("rng failed");
    BASE64_STANDARD.encode(key)
}

pub fn qqbot_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = v.get(*key).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn qqbot_extract_i64(v: &serde_json::Value, keys: &[&str]) -> Option<i64> {
    for key in keys {
        if let Some(raw) = v.get(*key) {
            if let Some(parsed) = raw.as_i64() {
                return Some(parsed);
            }
            if let Some(parsed) = raw.as_str().and_then(|s| s.trim().parse::<i64>().ok()) {
                return Some(parsed);
            }
        }
    }
    None
}

pub fn qqbot_decrypt_secret(
    encrypted_base64: &str,
    key_base64: &str,
) -> Result<String, AgentError> {
    let key_bytes = BASE64_STANDARD.decode(key_base64.trim()).map_err(|e| {
        AgentError::Config(format!("qqbot qr decrypt: invalid bind key base64: {e}"))
    })?;
    if key_bytes.len() != 32 {
        return Err(AgentError::Config(format!(
            "qqbot qr decrypt: expected 32-byte key, got {}",
            key_bytes.len()
        )));
    }
    let encrypted_bytes = BASE64_STANDARD
        .decode(encrypted_base64.trim())
        .map_err(|e| {
            AgentError::Config(format!("qqbot qr decrypt: invalid encrypted secret: {e}"))
        })?;
    if encrypted_bytes.len() < 29 {
        return Err(AgentError::Config(
            "qqbot qr decrypt: encrypted payload too short".to_string(),
        ));
    }
    let nonce = aes_gcm::Nonce::from_slice(&encrypted_bytes[..12]);
    let cipher = <Aes256Gcm as KeyInit>::new_from_slice(&key_bytes)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: cipher init failed: {e}")))?;
    let plaintext = cipher
        .decrypt(nonce, &encrypted_bytes[12..])
        .map_err(|_| AgentError::Config("qqbot qr decrypt: decrypt failed".to_string()))?;
    String::from_utf8(plaintext)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: invalid utf-8: {e}")))
}

pub fn qqbot_connect_url(task_id: &str) -> String {
    format!(
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id={}&_wv=2&source=hermes",
        urlencoding::encode(task_id.trim())
    )
}

pub fn qqbot_api_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("HermesAgentUltra/qqbot-onboard"),
    );
    headers
}

pub fn qqbot_join_https_url(host: &str, path: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    let path = path.trim();
    if path.starts_with('/') {
        format!("https://{}{}", host, path)
    } else {
        format!("https://{}/{}", host, path)
    }
}

pub async fn qqbot_create_bind_task(
    client: &reqwest::Client,
    portal_host: &str,
    create_path: &str,
    key_base64: &str,
) -> Result<String, AgentError> {
    let url = qqbot_join_https_url(portal_host, create_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({"key": key_base64}))
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("qqbot create_bind_task request failed: {e}")))?;
    let status = resp.status();
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Config(format!("qqbot create_bind_task parse failed: {e}")))?;
    if !status.is_success() {
        return Err(AgentError::Config(format!(
            "qqbot create_bind_task failed ({}): {}",
            status, payload
        )));
    }
    let retcode = qqbot_extract_i64(&payload, &["retcode"]).unwrap_or(-1);
    if retcode != 0 {
        let msg = qqbot_extract_string(&payload, &["msg", "message"])
            .unwrap_or_else(|| "create_bind_task returned non-zero retcode".to_string());
        return Err(AgentError::Config(format!(
            "qqbot create_bind_task retcode={retcode}: {msg}"
        )));
    }
    let task_id = payload
        .get("data")
        .and_then(|v| qqbot_extract_string(v, &["task_id"]))
        .ok_or_else(|| {
            AgentError::Config("qqbot create_bind_task missing data.task_id".to_string())
        })?;
    Ok(task_id)
}

pub async fn qqbot_poll_bind_result(
    client: &reqwest::Client,
    portal_host: &str,
    poll_path: &str,
    task_id: &str,
) -> Result<(i64, String, String, String), AgentError> {
    let url = qqbot_join_https_url(portal_host, poll_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({"task_id": task_id}))
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("qqbot poll_bind_result request failed: {e}")))?;
    let status = resp.status();
    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AgentError::Config(format!("qqbot poll_bind_result parse failed: {e}")))?;
    if !status.is_success() {
        return Err(AgentError::Config(format!(
            "qqbot poll_bind_result failed ({}): {}",
            status, payload
        )));
    }
    let retcode = qqbot_extract_i64(&payload, &["retcode"]).unwrap_or(-1);
    if retcode != 0 {
        let msg = qqbot_extract_string(&payload, &["msg", "message"])
            .unwrap_or_else(|| "poll_bind_result returned non-zero retcode".to_string());
        return Err(AgentError::Config(format!(
            "qqbot poll_bind_result retcode={retcode}: {msg}"
        )));
    }
    let data = payload.get("data").cloned().unwrap_or_default();
    let status = qqbot_extract_i64(&data, &["status"]).unwrap_or_default();
    let app_id = qqbot_extract_string(&data, &["bot_appid", "app_id"]).unwrap_or_default();
    let encrypted_secret =
        qqbot_extract_string(&data, &["bot_encrypt_secret", "encrypt_secret"]).unwrap_or_default();
    let user_openid = qqbot_extract_string(&data, &["user_openid"]).unwrap_or_default();
    Ok((status, app_id, encrypted_secret, user_openid))
}

pub async fn qqbot_qr_login_flow(
    portal_host: &str,
    create_path: &str,
    poll_path: &str,
    timeout_seconds: u64,
) -> Result<(String, String, String), AgentError> {
    const ONBOARD_POLL_INTERVAL: Duration = Duration::from_secs(2);
    const MAX_REFRESHES: usize = 3;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AgentError::Io(format!("qqbot onboard client init failed: {e}")))?;

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);

    for refresh_idx in 0..=MAX_REFRESHES {
        let bind_key = qqbot_generate_bind_key_base64();
        let task_id = qqbot_create_bind_task(&client, portal_host, create_path, &bind_key).await?;
        let connect_url = qqbot_connect_url(&task_id);

        println!();
        println!("QQBot QR setup URL:");
        println!("  {}", connect_url);
        println!("Scan the URL with QQ on your phone.");
        render_qr_to_terminal(&connect_url);
        println!();

        loop {
            if Instant::now() >= deadline {
                return Err(AgentError::Timeout(format!(
                    "qqbot qr login timed out after {timeout_seconds}s"
                )));
            }
            match qqbot_poll_bind_result(&client, portal_host, poll_path, &task_id).await {
                Ok((status, app_id, encrypted_secret, user_openid)) => match status {
                    2 => {
                        if app_id.trim().is_empty() || encrypted_secret.trim().is_empty() {
                            return Err(AgentError::Config(
                                "qqbot qr confirmed but payload missing app_id/encrypted_secret"
                                    .to_string(),
                            ));
                        }
                        let client_secret = qqbot_decrypt_secret(&encrypted_secret, &bind_key)?;
                        return Ok((app_id, client_secret, user_openid));
                    }
                    3 => {
                        if refresh_idx >= MAX_REFRESHES {
                            return Err(AgentError::Timeout(format!(
                                "qqbot qr expired too many times (max {})",
                                MAX_REFRESHES
                            )));
                        }
                        println!(
                            "QQBot QR code expired, refreshing... ({}/{})",
                            refresh_idx + 1,
                            MAX_REFRESHES
                        );
                        break;
                    }
                    _ => {}
                },
                Err(_) => {}
            }
            tokio::time::sleep(ONBOARD_POLL_INTERVAL).await;
        }
    }
    Err(AgentError::Timeout(
        "qqbot qr login exhausted refresh retries".to_string(),
    ))
}

pub(crate) const WECOM_QR_GENERATE_URL: &str = "https://work.weixin.qq.com/ai/qc/generate";
pub(crate) const WECOM_QR_QUERY_URL: &str = "https://work.weixin.qq.com/ai/qc/query_result";
pub(crate) const WECOM_QR_CODE_PAGE: &str =
    "https://work.weixin.qq.com/ai/qc/gen?source=hermes&scode=";

pub fn wecom_qr_page_url(scode: &str) -> String {
    format!(
        "{}{}",
        WECOM_QR_CODE_PAGE,
        urlencoding::encode(scode.trim())
    )
}

pub async fn wecom_bot_id_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Some(v) = existing.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    if let Ok(v) = std::env::var("WECOM_BOT_ID") {
        let s = v.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    let v = prompt_line("WeCom AI Bot bot_id (WECOM_BOT_ID): ").await?;
    let s = v.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "WeCom bot_id is required (set WECOM_BOT_ID or enter at prompt)".to_string(),
        ));
    }
    Ok(s.to_string())
}

pub async fn wecom_secret_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
    if let Some(v) = existing.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(v.to_string());
    }
    if let Ok(v) = std::env::var("WECOM_SECRET") {
        let s = v.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    let v = prompt_line("WeCom AI Bot secret (WECOM_SECRET): ").await?;
    let s = v.trim();
    if s.is_empty() {
        return Err(AgentError::Config(
            "WeCom secret is required (set WECOM_SECRET or enter at prompt)".to_string(),
        ));
    }
    Ok(s.to_string())
}

pub async fn wecom_qr_login_flow(timeout_seconds: u64) -> Result<(String, String), AgentError> {
    const WECOM_QR_POLL_INTERVAL: Duration = Duration::from_secs(3);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| AgentError::Io(format!("wecom qr client init failed: {e}")))?;

    print!("  Connecting to WeCom...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let generate_url = format!("{WECOM_QR_GENERATE_URL}?source=hermes");
    let raw = client
        .get(&generate_url)
        .header("User-Agent", "HermesAgent/1.0")
        .send()
        .await
        .map_err(|e| {
            println!(" failed: {e}");
            AgentError::Io(format!("wecom qr generate request: {e}"))
        })?
        .json::<serde_json::Value>()
        .await
        .map_err(|e| {
            println!(" failed: {e}");
            AgentError::Io(format!("wecom qr generate parse: {e}"))
        })?;

    let data = raw.get("data").cloned().unwrap_or_default();
    let scode = weixin_extract_string(&data, &["scode"]).ok_or_else(|| {
        println!(" failed: unexpected response format");
        AgentError::Config("wecom qr response missing scode".to_string())
    })?;
    let auth_url = weixin_extract_string(&data, &["auth_url"]).ok_or_else(|| {
        println!(" failed: unexpected response format");
        AgentError::Config("wecom qr response missing auth_url".to_string())
    })?;

    println!(" done.");
    println!();
    render_qr_to_terminal(&auth_url);
    let page_url = wecom_qr_page_url(&scode);
    println!("\n  Scan the QR code above, or open this URL directly:\n  {page_url}");
    println!();
    print!("  Fetching configuration results...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let query_url = format!(
        "{WECOM_QR_QUERY_URL}?scode={}",
        urlencoding::encode(scode.trim())
    );

    while Instant::now() < deadline {
        if let Ok(resp) = client
            .get(&query_url)
            .header("User-Agent", "HermesAgent/1.0")
            .send()
            .await
        {
            if let Ok(result) = resp.json::<serde_json::Value>().await {
                let result_data = result.get("data").cloned().unwrap_or_default();
                let status = weixin_extract_string(&result_data, &["status"])
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                print!(".");
                let _ = std::io::Write::flush(&mut std::io::stdout());
                if status == "success" {
                    println!();
                    let bot_info = result_data.get("bot_info").cloned().unwrap_or_default();
                    let bot_id =
                        weixin_extract_string(&bot_info, &["botid", "bot_id"]).unwrap_or_default();
                    let secret = weixin_extract_string(&bot_info, &["secret"]).unwrap_or_default();
                    if !bot_id.is_empty() && !secret.is_empty() {
                        return Ok((bot_id, secret));
                    }
                    return Err(AgentError::Config(
                        "wecom qr scan reported success but bot credentials were incomplete"
                            .to_string(),
                    ));
                }
            }
        }
        tokio::time::sleep(WECOM_QR_POLL_INTERVAL).await;
    }

    println!();
    Err(AgentError::Timeout(format!(
        "wecom qr login timed out after {timeout_seconds}s"
    )))
}

pub fn weixin_login_base_url_from_disk(disk: &GatewayConfig) -> String {
    if let Some(wx) = disk.platforms.get("weixin") {
        if let Some(v) = wx.extra.get("base_url").and_then(|v| v.as_str()) {
            let s = v.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Ok(v) = std::env::var("WEIXIN_BASE_URL") {
        let s = v.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    "https://ilinkai.weixin.qq.com".to_string()
}

pub fn weixin_login_endpoints_from_disk(disk: &GatewayConfig) -> (String, String) {
    let mut start_ep = "ilink/bot/get_bot_qrcode".to_string();
    let mut poll_ep = "ilink/bot/get_qrcode_status".to_string();
    if let Some(wx) = disk.platforms.get("weixin") {
        if let Some(v) = wx
            .extra
            .get("qr_get_bot_qrcode_endpoint")
            .or_else(|| wx.extra.get("qr_start_endpoint"))
            .and_then(|v| v.as_str())
        {
            let s = v.trim();
            if !s.is_empty() {
                start_ep = s.to_string();
            }
        }
        if let Some(v) = wx
            .extra
            .get("qr_get_qrcode_status_endpoint")
            .or_else(|| wx.extra.get("qr_poll_endpoint"))
            .and_then(|v| v.as_str())
        {
            let s = v.trim();
            if !s.is_empty() {
                poll_ep = s.to_string();
            }
        }
    }
    (start_ep, poll_ep)
}

pub fn weixin_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = v.get(*key).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn render_qr_to_terminal(data: &str) {
    let code = match QrCode::new(data.as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            println!("  (QR code generation failed: {e})");
            println!("  Please open the URL above in your browser to scan.");
            return;
        }
    };

    let colors = code.to_colors();
    let width = code.width();

    let quiet = 2;
    let total_w = width + quiet * 2;

    let mut row = 0usize;
    while row < width + quiet * 2 {
        let mut line = String::new();
        line.push_str("  ");
        for col in 0..total_w {
            let top_dark =
                if row >= quiet && row < width + quiet && col >= quiet && col < width + quiet {
                    colors[(row - quiet) * width + (col - quiet)] == qrcode::Color::Dark
                } else {
                    false
                };
            let bot_dark = if row + 1 >= quiet
                && row + 1 < width + quiet
                && col >= quiet
                && col < width + quiet
            {
                colors[(row + 1 - quiet) * width + (col - quiet)] == qrcode::Color::Dark
            } else {
                false
            };

            match (top_dark, bot_dark) {
                (true, true) => line.push('\u{2588}'),
                (true, false) => line.push('\u{2580}'),
                (false, true) => line.push('\u{2584}'),
                (false, false) => line.push(' '),
            }
        }
        println!("{line}");
        row += 2;
    }
}

pub async fn weixin_qr_login_flow(
    base_url: &str,
    start_ep: &str,
    poll_ep: &str,
    _account_id_hint: Option<&str>,
) -> Result<(String, String, String, String), AgentError> {
    let initial_base = base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::new();

    let mut current_base = initial_base.clone();
    let mut qr_json = fetch_weixin_qr(&client, &current_base, start_ep).await?;
    let mut qrcode_value = weixin_extract_string(&qr_json, &["qrcode"])
        .ok_or_else(|| AgentError::Config("weixin qr response missing qrcode".to_string()))?;
    let mut qrcode_url =
        weixin_extract_string(&qr_json, &["qrcode_img_content"]).unwrap_or_default();
    let qr_scan_data = if !qrcode_url.trim().is_empty() {
        qrcode_url.clone()
    } else {
        qrcode_value.clone()
    };
    println!();
    if !qrcode_url.trim().is_empty() {
        println!("{}", qrcode_url);
    }
    render_qr_to_terminal(&qr_scan_data);
    println!();
    println!("请使用微信扫描二维码，并在手机端确认登录。");

    let poll_interval = Duration::from_secs(1);
    let timeout = Duration::from_secs(480);
    let started = Instant::now();
    let mut refresh_count = 0u8;
    loop {
        if started.elapsed() >= timeout {
            return Err(AgentError::Config(
                "weixin qr login timed out after 480s".to_string(),
            ));
        }
        tokio::time::sleep(poll_interval).await;
        let poll_url = format!(
            "{}/{}",
            current_base.trim_end_matches('/'),
            poll_ep.trim_start_matches('/')
        );
        let poll_resp = match client
            .get(&poll_url)
            .query(&[("qrcode", qrcode_value.as_str())])
            .timeout(Duration::from_secs(35))
            .send()
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !poll_resp.status().is_success() {
            continue;
        }
        let poll_json: serde_json::Value = match poll_resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let status = weixin_extract_string(&poll_json, &["status"])
            .unwrap_or_else(|| "wait".to_string())
            .to_ascii_lowercase();
        match status.as_str() {
            "wait" => {}
            "scaned" => {
                println!("已扫码，请在微信里确认...");
            }
            "scaned_but_redirect" => {
                if let Some(redirect_host) =
                    weixin_extract_string(&poll_json, &["redirect_host"]).filter(|s| !s.is_empty())
                {
                    current_base = format!("https://{}", redirect_host);
                }
            }
            "expired" => {
                refresh_count = refresh_count.saturating_add(1);
                if refresh_count > 3 {
                    return Err(AgentError::Config(
                        "weixin qr expired too many times".to_string(),
                    ));
                }
                println!("二维码已过期，正在刷新... ({}/3)", refresh_count);
                qr_json = fetch_weixin_qr(&client, &initial_base, start_ep).await?;
                qrcode_value = weixin_extract_string(&qr_json, &["qrcode"]).ok_or_else(|| {
                    AgentError::Config("weixin qr refresh missing qrcode".to_string())
                })?;
                qrcode_url =
                    weixin_extract_string(&qr_json, &["qrcode_img_content"]).unwrap_or_default();
                let refreshed_qr = if !qrcode_url.trim().is_empty() {
                    qrcode_url.clone()
                } else {
                    qrcode_value.clone()
                };
                if !qrcode_url.trim().is_empty() {
                    println!("{}", qrcode_url);
                }
                render_qr_to_terminal(&refreshed_qr);
            }
            "confirmed" => {
                let account_id = weixin_extract_string(&poll_json, &["ilink_bot_id", "account_id"])
                    .unwrap_or_default();
                let token =
                    weixin_extract_string(&poll_json, &["bot_token", "token"]).unwrap_or_default();
                let resolved_base_url =
                    weixin_extract_string(&poll_json, &["baseurl"]).unwrap_or(initial_base.clone());
                let user_id = weixin_extract_string(&poll_json, &["ilink_user_id", "user_id"])
                    .unwrap_or_default();
                if account_id.trim().is_empty() || token.trim().is_empty() {
                    return Err(AgentError::Config(
                        "weixin qr confirmed but payload missing ilink_bot_id/bot_token"
                            .to_string(),
                    ));
                }
                return Ok((account_id, token, resolved_base_url, user_id));
            }
            _ => {}
        }
    }
}

/// Separate top-level function (not nested inside weixin_qr_login_flow)
pub async fn fetch_weixin_qr(
    client: &reqwest::Client,
    base: &str,
    start_ep: &str,
) -> Result<serde_json::Value, AgentError> {
    let url = format!(
        "{}/{}",
        base.trim_end_matches('/'),
        start_ep.trim_start_matches('/')
    );
    let resp = client
        .get(&url)
        .query(&[("bot_type", "3")])
        .timeout(Duration::from_secs(35))
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("weixin qr get_bot_qrcode request: {e}")))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "weixin qr get_bot_qrcode failed ({}): {}",
            status, body
        )));
    }
    resp.json::<serde_json::Value>()
        .await
        .map_err(|e| AgentError::Io(format!("weixin qr get_bot_qrcode parse: {e}")))
}

pub async fn print_auth_status_matrix(cli: &Cli, manager: &AuthManager) -> Result<(), AgentError> {
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let disk = load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    println!("Auth status matrix:");
    println!("-------------------");

    let mut llm_providers = known_providers();
    llm_providers.sort_unstable();
    llm_providers.dedup();
    for provider in llm_providers {
        let env_present = provider_api_key_from_env(provider).is_some()
            || (provider == "copilot"
                && std::env::var("GITHUB_COPILOT_TOKEN")
                    .ok()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false));
        let store_present = manager.get_access_token(provider).await?.is_some();
        let auth_state_present = if provider_supports_oauth(provider) {
            read_provider_auth_state(provider)?.is_some()
        } else {
            false
        };
        let (present, source) = if env_present {
            (true, "env")
        } else if store_present {
            (true, "token_store")
        } else if auth_state_present {
            (true, "auth_json")
        } else {
            (false, "none")
        };
        println!(
            "  - {:<16} present={} source={} oauth_state_present={}",
            provider, present, source, auth_state_present
        );
    }

    for provider in [
        "telegram",
        "weixin",
        "discord",
        "slack",
        "qqbot",
        "wecom_callback",
    ] {
        let (enabled, cfg_token) = disk
            .platforms
            .get(provider)
            .map(|p| (p.enabled, platform_token_or_extra(p).is_some()))
            .unwrap_or((false, false));
        let env_present = match provider {
            "telegram" => std::env::var("TELEGRAM_BOT_TOKEN")
                .ok()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
            "weixin" => std::env::var("WEIXIN_TOKEN")
                .ok()
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false),
            "qqbot" => {
                std::env::var("QQ_APP_ID")
                    .ok()
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
                    && std::env::var("QQ_CLIENT_SECRET")
                        .ok()
                        .map(|v| !v.trim().is_empty())
                        .unwrap_or(false)
            }
            _ => false,
        };
        let (present, source) = if env_present {
            (true, "env")
        } else if cfg_token {
            (true, "config")
        } else {
            (false, "none")
        };
        println!(
            "  - {:<16} present={} source={} enabled={}",
            provider, present, source, enabled
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthVerifyOutcome {
    Valid,
    ValidRefreshed,
    Unverified,
    Missing,
    Expired,
    RefreshFailed,
}

impl AuthVerifyOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::ValidRefreshed => "valid_refreshed",
            Self::Unverified => "unverified",
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::RefreshFailed => "refresh_failed",
        }
    }

    pub fn is_success(self) -> bool {
        matches!(self, Self::Valid | Self::ValidRefreshed | Self::Unverified)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AuthVerifyResult {
    pub(crate) provider: String,
    pub(crate) outcome: AuthVerifyOutcome,
    pub(crate) source: String,
    pub(crate) credential_present: bool,
    pub(crate) oauth_state_present: bool,
    pub(crate) expires_at: Option<String>,
    pub(crate) detail: Option<String>,
}

pub fn auth_verify_source(
    env_present: bool,
    store_present: bool,
    auth_state_present: bool,
) -> String {
    if env_present {
        "env".to_string()
    } else if store_present {
        "token_store".to_string()
    } else if auth_state_present {
        "auth_json".to_string()
    } else {
        "none".to_string()
    }
}

pub fn oauth_refresh_config_for_provider(provider: &str) -> Option<(String, String)> {
    let token_url = match provider {
        "openai" => std::env::var("HERMES_OPENAI_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| CODEX_OAUTH_TOKEN_URL.to_string()),
        "openai-codex" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODEX_OAUTH_TOKEN_URL.to_string()),
        "anthropic" => std::env::var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| ANTHROPIC_OAUTH_TOKEN_URL.to_string()),
        _ => return None,
    };
    let client_id = match provider {
        "openai" => std::env::var("HERMES_OPENAI_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
            })
            .unwrap_or_else(|| CODEX_OAUTH_CLIENT_ID.to_string()),
        "openai-codex" => std::env::var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| CODEX_OAUTH_CLIENT_ID.to_string()),
        "anthropic" => std::env::var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| ANTHROPIC_OAUTH_CLIENT_ID.to_string()),
        _ => return None,
    };
    Some((token_url, client_id))
}

pub async fn refresh_oauth_store_credential(
    provider: &str,
    current: &OAuthCredential,
) -> Result<OAuthCredential, AgentError> {
    let refresh_token = current
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(format!(
                "OAuth refresh token missing for provider '{provider}'",
            ))
        })?;
    let (token_url, client_id) = oauth_refresh_config_for_provider(provider).ok_or_else(|| {
        AgentError::AuthFailed(format!(
            "OAuth refresh not configured for provider '{provider}'",
        ))
    })?;
    let endpoints = OAuth2Endpoints {
        authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
        token_url,
        client_id,
        redirect_uri: "http://127.0.0.1/oauth/callback-unused".to_string(),
        scopes: Vec::new(),
    };
    let mut refreshed = exchange_refresh_token(provider, &endpoints, refresh_token).await?;
    refreshed.provider = provider.to_string();
    Ok(refreshed)
}

pub async fn ensure_openai_oauth_credential(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<Option<OAuthCredential>, AgentError> {
    if let Some(existing) = token_store.get(provider).await {
        return Ok(Some(existing));
    }
    let imported = if provider == "openai" {
        discover_existing_openai_oauth()?
    } else {
        discover_existing_openai_codex_oauth()?
    };
    let Some(imported) = imported else {
        return Ok(None);
    };
    let expires_at = imported
        .state
        .tokens
        .expires_in
        .filter(|secs| *secs > 0)
        .map(|secs| Utc::now() + chrono::Duration::seconds(secs));
    let credential = OAuthCredential {
        provider: provider.to_string(),
        access_token: imported.state.tokens.access_token.clone(),
        refresh_token: imported.state.tokens.refresh_token.clone(),
        token_type: "bearer".to_string(),
        scope: None,
        expires_at,
    };
    manager.save_credential(credential.clone()).await?;
    Ok(Some(credential))
}

pub fn print_auth_verify_result(result: &AuthVerifyResult) {
    println!(
        "Auth verify: provider='{}', status={}, source={}, credential_present={}, oauth_state_present={}{}{}",
        result.provider,
        result.outcome.as_str(),
        result.source,
        result.credential_present,
        result.oauth_state_present,
        result
            .expires_at
            .as_deref()
            .map(|v| format!(", expires_at={v}"))
            .unwrap_or_default(),
        result
            .detail
            .as_deref()
            .map(|v| format!(", detail={v}"))
            .unwrap_or_default()
    );
}

pub fn nous_auth_error_requires_fresh_login(err: &AgentError) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("invalid_grant")
        || text.contains("refresh token reuse")
        || text.contains("refresh session has been revoked")
        || text.contains("session has been revoked")
        || text.contains("stored nous auth state is invalid")
        || text.contains("missing refresh token")
        || text.contains("no refresh token")
}

pub async fn save_nous_runtime_credential(
    manager: &AuthManager,
    resolved: &NousRuntimeCredentials,
) -> Result<(), AgentError> {
    manager
        .save_credential(OAuthCredential {
            provider: "nous".to_string(),
            access_token: resolved.api_key.clone(),
            refresh_token: resolved.refresh_token.clone(),
            token_type: resolved.token_type.clone(),
            scope: resolved.scope.clone(),
            expires_at: parse_rfc3339_utc(resolved.expires_at.as_deref()),
        })
        .await
}

pub async fn fresh_nous_login_and_save(
    manager: &AuthManager,
) -> Result<(NousRuntimeCredentials, PathBuf, NousAuthState), AgentError> {
    let _ = clear_provider_auth_state("nous")?;
    let state = login_nous_device_code(NousDeviceCodeOptions::default()).await?;
    let auth_path = save_nous_auth_state(&state)?;
    let resolved = resolve_nous_runtime_credentials(
        true,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await?;
    save_nous_runtime_credential(manager, &resolved).await?;
    Ok((resolved, auth_path, state))
}

pub async fn resolve_or_fresh_login_nous(
    manager: &AuthManager,
    use_existing: bool,
) -> Result<(NousRuntimeCredentials, PathBuf, bool, NousAuthState), AgentError> {
    if use_existing {
        if let Some(imported) = discover_existing_nous_oauth()? {
            println!(
                "Detected existing Nous OAuth session at {}.",
                imported.source_path.display()
            );
            let imported_state = imported.state.clone();
            let auth_path = save_nous_auth_state(&imported.state)?;
            match resolve_nous_runtime_credentials(
                true,
                true,
                NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
            )
            .await
            {
                Ok(resolved) => {
                    save_nous_runtime_credential(manager, &resolved).await?;
                    return Ok((resolved, auth_path, true, imported_state));
                }
                Err(err) if nous_auth_error_requires_fresh_login(&err) => {
                    eprintln!(
                        "Existing Nous OAuth session is stale/revoked; starting a fresh login flow."
                    );
                }
                Err(err) => return Err(err),
            }
        }
    }
    let (resolved, auth_path, state) = fresh_nous_login_and_save(manager).await?;
    Ok((resolved, auth_path, false, state))
}

pub async fn verify_single_oauth_provider(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<AuthVerifyResult, AgentError> {
    let provider = normalize_auth_provider(provider);
    let env_present = provider_api_key_from_env(&provider).is_some();
    let auth_state_present = read_provider_auth_state(&provider)?.is_some();
    let mut stored_credential = token_store.get(&provider).await;

    if matches!(provider.as_str(), "openai" | "openai-codex") && stored_credential.is_none() {
        stored_credential = ensure_openai_oauth_credential(&provider, token_store, manager).await?;
    }

    let stored_present = stored_credential
        .as_ref()
        .map(|c| !c.access_token.trim().is_empty())
        .unwrap_or(false);
    let mut result = AuthVerifyResult {
        provider: provider.clone(),
        outcome: AuthVerifyOutcome::Missing,
        source: auth_verify_source(env_present, stored_present, auth_state_present),
        credential_present: env_present || stored_present,
        oauth_state_present: auth_state_present,
        expires_at: stored_credential
            .as_ref()
            .and_then(|c| c.expires_at.as_ref().map(|dt| dt.to_rfc3339())),
        detail: None,
    };

    match provider.as_str() {
        "nous" => match resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "nous".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: creds.scope,
                        expires_at: parse_rfc3339_utc(creds.expires_at.as_deref()),
                    })
                    .await?;
                result.outcome = if creds.source == "portal" {
                    AuthVerifyOutcome::ValidRefreshed
                } else {
                    AuthVerifyOutcome::Valid
                };
                result.source = creds.source;
                result.expires_at = creds.expires_at;
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "qwen-oauth" => match resolve_qwen_runtime_credentials(
            false,
            true,
            QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "qwen-oauth".to_string(),
                        access_token: creds.api_key.clone(),
                        refresh_token: creds.refresh_token,
                        token_type: creds.token_type,
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                result.outcome = if creds.expires_at_ms.is_some() {
                    AuthVerifyOutcome::ValidRefreshed
                } else {
                    AuthVerifyOutcome::Valid
                };
                result.source = creds.source;
                result.expires_at = creds
                    .expires_at_ms
                    .and_then(DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "google-gemini-cli" => match resolve_gemini_oauth_runtime_credentials(false).await {
            Ok(creds) => {
                manager
                    .save_credential(OAuthCredential {
                        provider: "google-gemini-cli".to_string(),
                        access_token: creds.api_key,
                        refresh_token: creds.refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                result.outcome = AuthVerifyOutcome::Valid;
                result.source = creds.source;
                result.expires_at = creds
                    .expires_at_ms
                    .and_then(DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                result.outcome = if env_present || stored_present || auth_state_present {
                    AuthVerifyOutcome::RefreshFailed
                } else {
                    AuthVerifyOutcome::Missing
                };
                result.detail = Some(err.to_string());
                return Ok(result);
            }
        },
        "anthropic" => {
            let oauth_state = read_provider_auth_state("anthropic")?;
            let refresh_token = oauth_state.as_ref().and_then(|state| {
                let object = state.as_object()?;
                object
                    .get("refresh_token")
                    .or_else(|| object.get("refreshToken"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
            let status = get_anthropic_oauth_status().await;
            if status.logged_in && status.api_key.is_some() {
                result.outcome = AuthVerifyOutcome::Valid;
                result.source = status
                    .source
                    .clone()
                    .unwrap_or_else(|| "anthropic_oauth".to_string());
                result.expires_at = status
                    .expires_at_ms
                    .and_then(DateTime::from_timestamp_millis)
                    .map(|dt| dt.to_rfc3339());
                result.credential_present = true;
                return Ok(result);
            }
            if let Some(refresh_token) = refresh_token {
                match refresh_oauth_store_credential(
                    "anthropic",
                    &OAuthCredential {
                        provider: "anthropic".to_string(),
                        access_token: status.api_key.unwrap_or_default(),
                        refresh_token: Some(refresh_token.clone()),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(status.expires_at_ms),
                    },
                )
                .await
                {
                    Ok(refreshed) => {
                        manager.save_credential(refreshed.clone()).await?;
                        let expires_at_ms = refreshed.expires_at.map(|dt| dt.timestamp_millis());
                        let auth_state = serde_json::json!({
                            "access_token": refreshed.access_token,
                            "refresh_token": refreshed.refresh_token,
                            "expires_at_ms": expires_at_ms,
                            "source": "hermes_pkce_refresh",
                        });
                        let _ = save_provider_auth_state("anthropic", auth_state)?;
                        result.outcome = AuthVerifyOutcome::ValidRefreshed;
                        result.source = "hermes_pkce_refresh".to_string();
                        result.expires_at = refreshed.expires_at.map(|dt| dt.to_rfc3339());
                        result.credential_present = true;
                        return Ok(result);
                    }
                    Err(err) => {
                        result.outcome = AuthVerifyOutcome::RefreshFailed;
                        result.detail = Some(err.to_string());
                        return Ok(result);
                    }
                }
            }
            if let Some(expires_ms) = status.expires_at_ms {
                let expired = Utc::now().timestamp_millis() >= expires_ms;
                if expired {
                    result.outcome = AuthVerifyOutcome::Expired;
                    result.expires_at =
                        DateTime::from_timestamp_millis(expires_ms).map(|dt| dt.to_rfc3339());
                } else {
                    result.outcome = AuthVerifyOutcome::Unverified;
                }
            } else {
                result.outcome = if env_present {
                    AuthVerifyOutcome::Unverified
                } else {
                    AuthVerifyOutcome::Missing
                };
            }
            if let Some(err) = status.error {
                result.detail = Some(err);
            }
            return Ok(result);
        }
        "openai" | "openai-codex" => {
            if let Some(credential) = stored_credential {
                if !credential.is_expired(60) && !credential.access_token.trim().is_empty() {
                    result.outcome = AuthVerifyOutcome::Valid;
                    result.expires_at = credential.expires_at.map(|dt| dt.to_rfc3339());
                    result.credential_present = true;
                    return Ok(result);
                }
                if credential
                    .refresh_token
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|v| !v.is_empty())
                {
                    match refresh_oauth_store_credential(provider.as_str(), &credential).await {
                        Ok(refreshed) => {
                            manager.save_credential(refreshed.clone()).await?;
                            result.outcome = AuthVerifyOutcome::ValidRefreshed;
                            result.source = "token_store_refresh".to_string();
                            result.expires_at = refreshed.expires_at.map(|dt| dt.to_rfc3339());
                            result.credential_present = true;
                            return Ok(result);
                        }
                        Err(err) => {
                            result.outcome = AuthVerifyOutcome::RefreshFailed;
                            result.detail = Some(err.to_string());
                            return Ok(result);
                        }
                    }
                }
                result.outcome = AuthVerifyOutcome::Expired;
                result.expires_at = credential.expires_at.map(|dt| dt.to_rfc3339());
                return Ok(result);
            }
            if env_present {
                result.outcome = AuthVerifyOutcome::Unverified;
                result.detail = Some(
                    "Environment token is present but no OAuth credential state was available."
                        .to_string(),
                );
                return Ok(result);
            }
            result.outcome = AuthVerifyOutcome::Missing;
            return Ok(result);
        }
        _ => {}
    }

    if env_present {
        result.outcome = AuthVerifyOutcome::Unverified;
        result.detail = Some(
            "Provider uses env credential source; live OAuth verification is unavailable.".into(),
        );
    } else if stored_present {
        if let Some(cred) = stored_credential {
            if cred.is_expired(60) {
                result.outcome = AuthVerifyOutcome::Expired;
                result.expires_at = cred.expires_at.map(|dt| dt.to_rfc3339());
            } else {
                result.outcome = AuthVerifyOutcome::Valid;
            }
        } else {
            result.outcome = AuthVerifyOutcome::Valid;
        }
    } else {
        result.outcome = AuthVerifyOutcome::Missing;
    }
    Ok(result)
}

pub async fn run_auth_verify(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<(), AgentError> {
    let targets: Vec<String> = if provider == "all" || provider == "*" {
        OAUTH_CAPABLE_PROVIDERS
            .iter()
            .map(|p| p.to_string())
            .collect()
    } else {
        vec![normalize_auth_provider(provider)]
    };

    let mut failed: Vec<AuthVerifyResult> = Vec::new();
    for target in targets {
        if !provider_supports_oauth(&target) {
            let result = AuthVerifyResult {
                provider: target.clone(),
                outcome: AuthVerifyOutcome::Unverified,
                source: "unsupported".to_string(),
                credential_present: provider_api_key_from_env(&target).is_some(),
                oauth_state_present: false,
                expires_at: None,
                detail: Some("Provider is not OAuth-capable in Hermes Ultra.".to_string()),
            };
            print_auth_verify_result(&result);
            continue;
        }
        let result = verify_single_oauth_provider(&target, token_store, manager).await?;
        print_auth_verify_result(&result);
        if !result.outcome.is_success() {
            failed.push(result);
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        let failed_ids: Vec<String> = failed.iter().map(|r| r.provider.clone()).collect();
        Err(AgentError::AuthFailed(format!(
            "OAuth verification failed for provider(s): {}",
            failed_ids.join(", ")
        )))
    }
}

pub async fn run_auth(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    target: Option<String>,
    auth_type: Option<String>,
    label: Option<String>,
    api_key: Option<String>,
    qr: bool,
) -> Result<(), AgentError> {
    let provider = resolve_auth_provider(provider);
    let state_root = hermes_state_root(&cli);
    let auth_store_path = CliStateRoot::from_state_root(&state_root).secret_vault();
    let token_store = FileTokenStore::new(auth_store_path).await?;
    let manager = AuthManager::new(token_store.clone());
    let pool_path = CliStateRoot::from_state_root(&state_root).auth_pool();
    let mut pool_store = load_auth_pool_store(&pool_path)?;
    match action.as_deref().unwrap_or("status") {
        "add" => {
            let provider = normalize_auth_provider(provider.trim());
            let mut auth_type = resolve_auth_type_for_provider(&provider, auth_type.as_deref());

            if auth_type == "oauth" {
                match provider.as_str() {
                    "nous" => {
                        let (resolved, auth_path, _imported_existing, state) =
                            resolve_or_fresh_login_nous(&manager, true).await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .agent_key_obtained_at
                                .as_deref()
                                .map(|_| "device_code".to_string())
                                .unwrap_or_else(|| "discovered_session".to_string()),
                            access_token: resolved.api_key,
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Nous OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai-codex" => {
                        let imported = discover_existing_openai_codex_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI Codex OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_codex_device_code(CodexDeviceCodeOptions::default())
                                .await?
                        };
                        let auth_path = save_codex_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai-codex".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .source
                                .clone()
                                .unwrap_or_else(|| "device_code".to_string()),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI Codex OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai" => {
                        let imported = discover_existing_openai_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                        };
                        let auth_path = save_openai_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: "device_code".to_string(),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "anthropic" => {
                        let imported = discover_existing_anthropic_oauth()?;
                        let (state, source_label) = if let Some(imported) = imported {
                            println!(
                                "Detected existing Anthropic OAuth session at {}.",
                                imported.source_path.display()
                            );
                            (imported.state, imported.source)
                        } else {
                            (
                                login_anthropic_oauth(AnthropicOAuthLoginOptions::default())
                                    .await?,
                                "hermes_pkce".to_string(),
                            )
                        };
                        let access_token = state.access_token.clone();
                        let refresh_token = state.refresh_token.clone();
                        let expires_at_ms = state.expires_at_ms;
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "source": source_label.clone(),
                        });
                        let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "anthropic".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: source_label,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Anthropic OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "qwen-oauth" => {
                        let creds = resolve_qwen_runtime_credentials(
                            false,
                            true,
                            QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                        )
                        .await?;
                        let auth_state = serde_json::to_value(&creds.tokens)
                            .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                        let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "qwen-oauth".to_string(),
                                access_token: creds.api_key.clone(),
                                refresh_token: creds.refresh_token.clone(),
                                token_type: creds.token_type.clone(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: creds.source.clone(),
                            access_token: creds.api_key.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Qwen OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Qwen auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "google-gemini-cli" => {
                        let creds =
                            login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default())
                                .await?;
                        let access_token = creds.api_key.clone();
                        let refresh_token = creds.refresh_token.clone();
                        let expires_at_ms = creds.expires_at_ms;
                        let email = creds.email.clone();
                        let project_id = creds.project_id.clone();
                        let source = creds.source.clone();
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "email": email.clone(),
                            "project_id": project_id.clone(),
                            "source": source.clone(),
                        });
                        let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "google-gemini-cli".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or_else(|| email.clone().unwrap_or(default_label)),
                            auth_type: "oauth".to_string(),
                            source,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Google Gemini OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Google auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    _ => {
                        println!(
                            "OAuth flow is not implemented for provider '{}'; falling back to API key/manual token login.",
                            provider
                        );
                        auth_type = "api_key".to_string();
                    }
                }
            }

            let token = if let Some(raw) = api_key {
                raw.trim().to_string()
            } else {
                resolve_llm_login_token(&cli, &provider).await?
            };
            if token.is_empty() {
                return Err(AgentError::Config("auth add: empty credential".into()));
            }
            let entries = pool_store.providers.entry(provider.clone()).or_default();
            let default_label = format!("{provider}-{}", entries.len() + 1);
            let entry = AuthPoolEntry {
                id: Uuid::new_v4().simple().to_string()[..6].to_string(),
                label: label.unwrap_or(default_label),
                auth_type,
                source: "manual".to_string(),
                access_token: token.clone(),
                last_status: None,
                last_status_at: None,
                last_error_code: None,
            };
            entries.push(entry.clone());
            save_auth_pool_store(&pool_path, &pool_store)?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: entry.access_token.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!(
                "Added pooled credential for provider '{}' (label='{}', id={}).",
                provider, entry.label, entry.id
            );
            return Ok(());
        }
        "list" => {
            if pool_store.providers.is_empty() {
                println!("No pooled credentials configured.");
                return Ok(());
            }
            if let Some(entries) = pool_store.providers.get(&provider) {
                println!("{} ({} credentials):", provider, entries.len());
                for (idx, e) in entries.iter().enumerate() {
                    let exhausted = if e.last_status.as_deref() == Some("exhausted") {
                        " exhausted"
                    } else {
                        ""
                    };
                    println!(
                        "  #{}  {:<20} {:<8} {}{}",
                        idx + 1,
                        e.label,
                        e.auth_type,
                        e.source,
                        exhausted
                    );
                }
                return Ok(());
            }
            println!("No pooled credentials for provider '{}'.", provider);
            return Ok(());
        }
        "remove" => {
            let target = target.ok_or_else(|| {
                AgentError::Config(
                    "auth remove usage: hermes auth remove <provider> <index|id|label>".into(),
                )
            })?;
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                return Err(AgentError::Config(format!(
                    "No pooled credentials for provider '{}'",
                    provider
                )));
            };
            let Some(index) = resolve_pool_target(entries, &target) else {
                return Err(AgentError::Config(format!(
                    "Could not resolve auth remove target '{}' for provider '{}'",
                    target, provider
                )));
            };
            let removed = entries.remove(index);
            if entries.is_empty() {
                pool_store.providers.remove(&provider);
                token_store.remove(&provider).await?;
                if provider_supports_oauth(&provider) {
                    let _ = clear_provider_auth_state(&provider)?;
                }
            } else if let Some(next) = entries.first() {
                manager
                    .save_credential(OAuthCredential {
                        provider: provider.clone(),
                        access_token: next.access_token.clone(),
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Removed pooled credential for provider '{}' (label='{}', id={}).",
                provider, removed.label, removed.id
            );
            return Ok(());
        }
        "reset" => {
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                println!("No pooled credentials for provider '{}'.", provider);
                return Ok(());
            };
            let mut reset = 0usize;
            for e in entries.iter_mut() {
                if e.last_status.is_some() || e.last_error_code.is_some() {
                    e.last_status = None;
                    e.last_status_at = None;
                    e.last_error_code = None;
                    reset += 1;
                }
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Reset status on {} pooled credential(s) for provider '{}'.",
                reset, provider
            );
            return Ok(());
        }
        "verify" => {
            run_auth_verify(&provider, &token_store, &manager).await?;
            return Ok(());
        }
        "login" => {
            if provider == "telegram" {
                let token = telegram_bot_token_from_env_or_prompt().await?;
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let tg = disk
                    .platforms
                    .entry("telegram".to_string())
                    .or_insert_with(PlatformConfig::default);
                tg.token = Some(token);
                tg.enabled = true;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_WEIXIN_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);
                let mut account_id_opt = disk
                    .platforms
                    .get("weixin")
                    .and_then(|p| p.extra.get("account_id"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let (account_id, token, qr_base_url, qr_user_id) = if qr_preferred {
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    let (start_ep, poll_ep) = weixin_login_endpoints_from_disk(&disk);
                    match weixin_qr_login_flow(
                        &base_url,
                        &start_ep,
                        &poll_ep,
                        account_id_opt.as_deref(),
                    )
                    .await
                    {
                        Ok(pair) => pair,
                        Err(e) => {
                            println!("Weixin QR 登录失败，将回退到手动 token 输入: {}", e);
                            let fallback_account_id = if let Some(v) = account_id_opt.take() {
                                v
                            } else {
                                weixin_account_id_from_env_or_prompt().await?
                            };
                            let fallback_token =
                                weixin_token_from_env_or_prompt(&fallback_account_id).await?;
                            (fallback_account_id, fallback_token, base_url, String::new())
                        }
                    }
                } else {
                    let manual_account_id = if let Some(v) = account_id_opt.take() {
                        v
                    } else {
                        weixin_account_id_from_env_or_prompt().await?
                    };
                    let manual_token = weixin_token_from_env_or_prompt(&manual_account_id).await?;
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    (manual_account_id, manual_token, base_url, String::new())
                };
                let wx = disk
                    .platforms
                    .entry("weixin".to_string())
                    .or_insert_with(PlatformConfig::default);
                wx.enabled = true;
                wx.token = Some(token.clone());
                wx.extra.insert(
                    "account_id".to_string(),
                    serde_json::Value::String(account_id.clone()),
                );
                if !qr_base_url.trim().is_empty() {
                    wx.extra.insert(
                        "base_url".to_string(),
                        serde_json::Value::String(qr_base_url.clone()),
                    );
                }
                save_persisted_weixin_account(
                    &account_id,
                    &token,
                    Some(qr_base_url.as_str()),
                    Some(qr_user_id.as_str()),
                )?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: account_id/token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "qqbot" {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_QQBOT_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);
                let existing_app_id = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("app_id"))
                    .and_then(|v| v.as_str());
                let existing_secret = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("client_secret"))
                    .and_then(|v| v.as_str());
                let (app_id, client_secret, user_openid) = if qr_preferred {
                    let portal_host = qqbot_portal_host_from_disk(&disk);
                    let (create_path, poll_path) = qqbot_onboard_endpoints_from_disk(&disk);
                    match qqbot_qr_login_flow(&portal_host, &create_path, &poll_path, 600).await {
                        Ok(tuple) => tuple,
                        Err(e) => {
                            println!(
                                "QQBot QR setup failed, falling back to manual credentials: {}",
                                e
                            );
                            let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                            let client_secret =
                                qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                            (app_id, client_secret, String::new())
                        }
                    }
                } else {
                    let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                    let client_secret =
                        qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                    (app_id, client_secret, String::new())
                };
                let qq = disk
                    .platforms
                    .entry("qqbot".to_string())
                    .or_insert_with(PlatformConfig::default);
                qq.enabled = true;
                qq.extra.insert(
                    "app_id".to_string(),
                    serde_json::Value::String(app_id.clone()),
                );
                qq.extra.insert(
                    "client_secret".to_string(),
                    serde_json::Value::String(client_secret.clone()),
                );
                if !qq.extra.contains_key("markdown_support") {
                    qq.extra.insert(
                        "markdown_support".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if !user_openid.trim().is_empty() {
                    qq.extra.insert(
                        "user_openid".to_string(),
                        serde_json::Value::String(user_openid.clone()),
                    );
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "QQBot: app_id/client_secret saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "wecom" {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_WECOM_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);
                let existing_bot_id = disk
                    .platforms
                    .get("wecom")
                    .and_then(|p| p.extra.get("bot_id"))
                    .and_then(|v| v.as_str());
                let existing_secret = disk
                    .platforms
                    .get("wecom")
                    .and_then(|p| p.extra.get("secret"))
                    .and_then(|v| v.as_str());
                let (bot_id, secret) = if qr_preferred {
                    match wecom_qr_login_flow(300).await {
                        Ok(pair) => pair,
                        Err(e) => {
                            println!("WeCom QR login failed, falling back to manual input: {e}");
                            let bot_id = wecom_bot_id_from_env_or_prompt(existing_bot_id).await?;
                            let secret = wecom_secret_from_env_or_prompt(existing_secret).await?;
                            (bot_id, secret)
                        }
                    }
                } else {
                    let bot_id = wecom_bot_id_from_env_or_prompt(existing_bot_id).await?;
                    let secret = wecom_secret_from_env_or_prompt(existing_secret).await?;
                    (bot_id, secret)
                };
                let wecom = disk
                    .platforms
                    .entry("wecom".to_string())
                    .or_insert_with(PlatformConfig::default);
                wecom.enabled = true;
                wecom.extra.insert(
                    "bot_id".to_string(),
                    serde_json::Value::String(bot_id.clone()),
                );
                wecom.extra.insert(
                    "secret".to_string(),
                    serde_json::Value::String(secret.clone()),
                );
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "WeCom: bot_id/secret saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                configure_platform_basic_prompts(&mut disk, platform_key).await?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: config updated and platform enabled in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "nous" {
                let (_resolved, auth_path, _imported_existing, _state) =
                    resolve_or_fresh_login_nous(&manager, true).await?;
                println!("Nous OAuth credential saved as provider 'nous'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai-codex" {
                let imported = discover_existing_openai_codex_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI Codex OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_codex_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai-codex".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI Codex OAuth credential saved as provider 'openai-codex'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai" {
                let imported = discover_existing_openai_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_openai_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI OAuth login complete; credential saved as provider 'openai'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "anthropic" {
                let imported = discover_existing_anthropic_oauth()?;
                let (state, source_label) = if let Some(imported) = imported {
                    println!(
                        "Detected existing Anthropic OAuth session at {}.",
                        imported.source_path.display()
                    );
                    (imported.state, imported.source)
                } else {
                    (
                        login_anthropic_oauth(AnthropicOAuthLoginOptions::default()).await?,
                        "hermes_pkce".to_string(),
                    )
                };
                let access_token = state.access_token.clone();
                let refresh_token = state.refresh_token.clone();
                let expires_at_ms = state.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "source": source_label,
                });
                let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "anthropic".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!("Anthropic OAuth credential saved as provider 'anthropic'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let creds = resolve_qwen_runtime_credentials(
                    false,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await?;
                let auth_state = serde_json::to_value(&creds.tokens)
                    .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "qwen-oauth".to_string(),
                        access_token: creds.api_key.clone(),
                        refresh_token: creds.refresh_token.clone(),
                        token_type: creds.token_type.clone(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                println!(
                    "Qwen OAuth credential imported from {} and stored as provider 'qwen-oauth'.",
                    creds.auth_file.display()
                );
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let creds =
                    login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default()).await?;
                let access_token = creds.api_key.clone();
                let refresh_token = creds.refresh_token.clone();
                let expires_at_ms = creds.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "email": creds.email.clone(),
                    "project_id": creds.project_id.clone(),
                    "source": creds.source.clone(),
                });
                let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "google-gemini-cli".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!(
                    "Google Gemini OAuth login complete; credential saved as provider 'google-gemini-cli'."
                );
                println!("Google auth file: {}", creds.auth_file.display());
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "copilot" || provider == "github-copilot" {
                let access_token = crate::copilot_auth::start_copilot_device_flow().await?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "copilot".to_string(),
                        access_token,
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
                println!("GitHub device login complete; credential saved as provider 'copilot'.");
                println!(
                    "Ensure GITHUB_COPILOT_TOKEN is set for the agent (see printed instructions above)."
                );
                return Ok(());
            }

            let access_token = resolve_llm_login_token(&cli, &provider).await?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            let msg = crate::auth::login(&provider).await?;
            println!("{}", msg);
        }
        "logout" => {
            if provider == "telegram" {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(tg) = disk.platforms.get_mut("telegram") {
                    tg.token = None;
                    tg.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token cleared and platform disabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(wx) = disk.platforms.get_mut("weixin") {
                    wx.token = None;
                    wx.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: token cleared and platform disabled in {} (account file retained)",
                    cfg_path.display()
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(p) = disk.platforms.get_mut(platform_key) {
                    p.enabled = false;
                    p.token = None;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: disabled and token cleared in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            let msg = crate::auth::logout(&provider).await?;
            token_store.remove(&provider).await?;
            if provider_supports_oauth(&provider) {
                let _ = clear_provider_auth_state(&provider)?;
            }
            println!("{} (removed credential for provider: {})", msg, provider);
        }
        _ => {
            if provider == "all" || provider == "*" {
                print_auth_status_matrix(&cli, &manager).await?;
                return Ok(());
            }
            if provider == "telegram" {
                let cfg_path = state_root.join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (has, en) = disk
                    .platforms
                    .get("telegram")
                    .map(|p| {
                        (
                            p.token
                                .as_deref()
                                .map(|t| !t.trim().is_empty())
                                .unwrap_or(false),
                            p.enabled,
                        )
                    })
                    .unwrap_or((false, false));
                println!(
                    "Telegram ({}): token_present={} enabled={}",
                    cfg_path.display(),
                    has,
                    en
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (account_id, has_cfg_token, enabled) = disk
                    .platforms
                    .get("weixin")
                    .map(|p| {
                        let account_id = p
                            .extra
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let has_cfg_token = p
                            .token
                            .as_deref()
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false);
                        (account_id, has_cfg_token, p.enabled)
                    })
                    .unwrap_or_else(|| ("".to_string(), false, false));
                let has_saved_token = if account_id.is_empty() {
                    false
                } else {
                    load_persisted_weixin_token(&account_id).is_some()
                };
                println!(
                    "Weixin ({}): account_id={} cfg_token_present={} saved_token_present={} enabled={}",
                    cfg_path.display(),
                    if account_id.is_empty() {
                        "(none)"
                    } else {
                        account_id.as_str()
                    },
                    has_cfg_token,
                    has_saved_token,
                    enabled
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = state_root.join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (enabled, token_present) = disk
                    .platforms
                    .get(platform_key)
                    .map(|p| (p.enabled, platform_token_or_extra(p).is_some()))
                    .unwrap_or((false, false));
                println!(
                    "{} ({}): credential_present={} enabled={}",
                    platform_key,
                    cfg_path.display(),
                    token_present,
                    enabled
                );
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let qwen_status = get_qwen_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Qwen OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    qwen_status.logged_in,
                    qwen_status.auth_file.display(),
                    qwen_status.source.as_deref().unwrap_or("none"),
                    qwen_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = qwen_status.api_key.as_deref() {
                    println!("Qwen OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = qwen_status.error.as_deref() {
                    println!("Qwen OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let google_status = get_gemini_oauth_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Google Gemini OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    google_status.logged_in,
                    google_status.auth_file.display(),
                    google_status.source.as_deref().unwrap_or("none"),
                    google_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(email) = google_status.email.as_deref() {
                    println!("Google account: {}", email);
                }
                if let Some(project_id) = google_status.project_id.as_deref() {
                    println!("Google project_id: {}", project_id);
                }
                if let Some(token) = google_status.api_key.as_deref() {
                    println!("Google OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = google_status.error.as_deref() {
                    println!("Google OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "anthropic" {
                let anthropic_status = get_anthropic_oauth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = manager.get_access_token(&provider).await?.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Anthropic OAuth: logged_in={} source={} expires_at_ms={}",
                    anthropic_status.logged_in,
                    anthropic_status.source.as_deref().unwrap_or("none"),
                    anthropic_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = anthropic_status.api_key.as_deref() {
                    println!("Anthropic OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = anthropic_status.error.as_deref() {
                    println!("Anthropic OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            let env_present = provider_api_key_from_env(&provider).is_some();
            let store_present = manager.get_access_token(&provider).await?.is_some();
            let auth_state_present = if provider_supports_oauth(&provider) {
                read_provider_auth_state(&provider)?.is_some()
            } else {
                false
            };
            let (has_token, source) = if env_present {
                (true, "env")
            } else if store_present {
                (true, "token_store")
            } else if auth_state_present {
                (true, "auth_json")
            } else {
                (false, "none")
            };
            println!(
                "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                provider, has_token, source, auth_state_present
            );
        }
    }
    Ok(())
}

pub async fn run_secrets(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    value: Option<String>,
    show: bool,
) -> Result<(), AgentError> {
    let path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).secret_vault();
    let store = FileTokenStore::new(&path).await?;
    let manager = AuthManager::new(store.clone());

    match action.as_deref().unwrap_or("list") {
        "list" | "status" => {
            let providers = store.list_providers().await;
            println!("Secret vault: {}", path.display());
            if providers.is_empty() {
                println!("  (empty)");
            } else {
                println!("Stored providers ({}):", providers.len());
                for p in providers {
                    if let Some(env_var) = provider_env_var(&p) {
                        println!("  - {p} (env: {env_var})");
                    } else {
                        println!("  - {p}");
                    }
                }
            }
            println!("Tip: runtime automatically hydrates env vars from this vault.");
        }
        "set" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets set: usage `hermes secrets set <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let secret = match value {
                Some(v) => v.trim().to_string(),
                None => prompt_line(format!("Enter secret for provider '{provider}': ")).await?,
            };
            if secret.is_empty() {
                return Err(AgentError::Config("Secret cannot be empty.".into()));
            }
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: secret,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!(
                "Saved secret for provider '{provider}' in {}",
                path.display()
            );
            if let Some(env_var) = provider_env_var(&provider) {
                println!("Mapped runtime env: {env_var}");
            }
        }
        "get" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets get: usage `hermes secrets get <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            if let Some((stored_provider, secret)) =
                lookup_secret_from_vault(&store, &provider).await
            {
                if show {
                    if !secret_stdout_allowed() {
                        return Err(AgentError::Config(
                            "Refusing plaintext secret output. Re-run with HERMES_ALLOW_SECRET_STDOUT=1 to opt in."
                                .into(),
                        ));
                    }
                    println!("{secret}");
                } else {
                    println!("{}", mask_secret(&secret));
                }
                if stored_provider != provider {
                    println!("(resolved via provider alias '{}')", stored_provider);
                }
            } else {
                return Err(AgentError::Config(format!(
                    "No secret stored for provider '{}'",
                    provider
                )));
            }
        }
        "remove" | "delete" | "rm" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config(
                    "secrets remove: usage `hermes secrets remove <provider>`".into(),
                )
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let mut removed = false;
            for candidate in secret_provider_aliases(&provider) {
                if store.get(&candidate).await.is_some() {
                    store.remove(&candidate).await?;
                    removed = true;
                }
            }
            if removed {
                println!("Removed secret for provider '{}'.", provider);
            } else {
                println!("No secret found for provider '{}'.", provider);
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown secrets action: {} (use list|status|get|set|remove)",
                other
            )));
        }
    }
    Ok(())
}
