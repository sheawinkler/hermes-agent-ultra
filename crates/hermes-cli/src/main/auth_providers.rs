/// Default auth provider: CLI arg, then `HERMES_AUTH_DEFAULT_PROVIDER`, then `nous`.
///
/// Set `HERMES_AUTH_DEFAULT_PROVIDER=telegram` if you primarily use the Telegram gateway.
fn resolve_auth_provider(provider: Option<String>) -> String {
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

fn infer_default_auth_provider_from_config() -> Option<String> {
    let cfg = load_config(None).ok()?;
    let model = cfg.model?;
    let provider = model
        .split_once(':')
        .map(|(provider, _)| provider.trim())
        .filter(|provider| !provider.is_empty())?;
    Some(provider.to_string())
}

fn normalize_auth_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "wechat" | "wx" => "weixin".to_string(),
        "qq" => "qqbot".to_string(),
        "tg" => "telegram".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "qwen-cli" | "qwen-portal" => "qwen-oauth".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
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
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "api-server" => "api_server".to_string(),
        "home-assistant" => "homeassistant".to_string(),
        "wecom-callback" => "wecom_callback".to_string(),
        "mm" => "mattermost".to_string(),
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        other => other.to_string(),
    }
}

fn gateway_platform_provider_key(provider: &str) -> Option<&'static str> {
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
        "ntfy" => Some("ntfy"),
        "webhook" => Some("webhook"),
        "api_server" => Some("api_server"),
        _ => None,
    }
}

fn normalize_secret_provider(provider: &str) -> String {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "github-copilot" | "github-models" => "copilot".to_string(),
        "github-copilot-acp" | "copilot-acp-agent" => "copilot-acp".to_string(),
        "claude" | "claude-code" => "anthropic".to_string(),
        "codex" => "openai-codex".to_string(),
        "openai-oauth" | "openai-cli" => "openai".to_string(),
        "nous_api" | "nousapi" | "nous-portal-api" => "nous-api".to_string(),
        "gemini-cli" | "gemini-oauth" => "google-gemini-cli".to_string(),
        "google" | "google-gemini" | "google-ai-studio" => "gemini".to_string(),
        "moonshot" | "kimi" => "kimi-coding".to_string(),
        "aigateway" | "vercel" | "vercel-ai-gateway" => "ai-gateway".to_string(),
        "opencode" | "zen" => "opencode-zen".to_string(),
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
        "kilo" | "kilo-code" | "kilo-gateway" => "kilocode".to_string(),
        "x-ai" | "x.ai" | "grok" => "xai".to_string(),
        "glm" | "z-ai" | "z.ai" | "zhipu" => "zai".to_string(),
        "nim" | "nvidia-nim" | "build-nvidia" | "nemotron" => "nvidia".to_string(),
        "hf" | "hugging-face" | "huggingface-hub" => "huggingface".to_string(),
        "gmi-cloud" | "gmicloud" => "gmi".to_string(),
        "arcee-ai" | "arceeai" => "arcee".to_string(),
        "mimo" | "xiaomi-mimo" => "xiaomi".to_string(),
        "tencent" | "tokenhub" | "tencent-cloud" | "tencentmaas" => "tencent-tokenhub".to_string(),
        "aws" | "aws-bedrock" | "amazon-bedrock" | "amazon" => "bedrock".to_string(),
        "dashscope" | "aliyun" | "alibaba-cloud" => "alibaba".to_string(),
        "alibaba_coding" | "alibaba-coding" | "alibaba_coding_plan" => {
            "alibaba-coding-plan".to_string()
        }
        _ => p,
    }
}

fn secret_provider_aliases(provider: &str) -> Vec<String> {
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
        "nous-api" => vec![
            "nous-api".to_string(),
            "nous_api".to_string(),
            "nousapi".to_string(),
            "nous-portal-api".to_string(),
        ],
        "copilot" => vec![
            "copilot".to_string(),
            "github-copilot".to_string(),
            "github-models".to_string(),
        ],
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
        "gmi" => vec![
            "gmi".to_string(),
            "gmi-cloud".to_string(),
            "gmicloud".to_string(),
        ],
        "arcee" => vec![
            "arcee".to_string(),
            "arcee-ai".to_string(),
            "arceeai".to_string(),
        ],
        "xiaomi" => vec![
            "xiaomi".to_string(),
            "mimo".to_string(),
            "xiaomi-mimo".to_string(),
        ],
        "tencent-tokenhub" => vec![
            "tencent-tokenhub".to_string(),
            "tencent".to_string(),
            "tokenhub".to_string(),
            "tencent-cloud".to_string(),
            "tencentmaas".to_string(),
        ],
        "bedrock" => vec![
            "bedrock".to_string(),
            "aws".to_string(),
            "aws-bedrock".to_string(),
            "amazon-bedrock".to_string(),
            "amazon".to_string(),
        ],
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
        "mlx" => vec![
            "mlx".to_string(),
            "mlx-lm".to_string(),
            "apple-mlx".to_string(),
            "vmlx".to_string(),
            "omlx".to_string(),
            "mlx-vlm".to_string(),
        ],
        "apple-ane" => vec![
            "apple-ane".to_string(),
            "ane".to_string(),
            "apple-neural-engine".to_string(),
        ],
        "sglang" => vec!["sglang".to_string()],
        "tgi" => vec!["tgi".to_string(), "text-generation-inference".to_string()],
        "lmstudio" => vec!["lmstudio".to_string(), "lm-studio".to_string()],
        "lmdeploy" => vec!["lmdeploy".to_string(), "lm-deploy".to_string()],
        "localai" => vec!["localai".to_string(), "local-ai".to_string()],
        "koboldcpp" => vec!["koboldcpp".to_string(), "kobold-cpp".to_string()],
        "text-generation-webui" => vec![
            "text-generation-webui".to_string(),
            "textgen-webui".to_string(),
            "oobabooga".to_string(),
        ],
        "tabbyapi" => vec![
            "tabbyapi".to_string(),
            "tabby-api".to_string(),
            "exllama".to_string(),
            "exllamav2".to_string(),
        ],
        p => vec![p.to_string()],
    }
}

fn provider_env_var(provider: &str) -> Option<&'static str> {
    let raw_provider = provider.trim().to_ascii_lowercase();
    match raw_provider.as_str() {
        "kimi-coding" => return Some("KIMI_CODING_API_KEY"),
        "moonshot" | "kimi" => return Some("KIMI_API_KEY"),
        _ => {}
    }

    match normalize_secret_provider(provider).as_str() {
        "openai" => Some("HERMES_OPENAI_API_KEY"),
        "openai-codex" => Some("HERMES_OPENAI_CODEX_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "bedrock" => None,
        "google-gemini-cli" => Some("HERMES_GEMINI_OAUTH_API_KEY"),
        "gemini" => Some("GOOGLE_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "qwen" | "alibaba" => Some("DASHSCOPE_API_KEY"),
        "alibaba-coding-plan" => Some("ALIBABA_CODING_PLAN_API_KEY"),
        "qwen-oauth" => Some("HERMES_QWEN_OAUTH_API_KEY"),
        "kimi-coding" => Some("KIMI_CODING_API_KEY"),
        "kimi-coding-cn" => Some("KIMI_CN_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "minimax-cn" => Some("MINIMAX_CN_API_KEY"),
        "stepfun" => Some("STEPFUN_API_KEY"),
        "nous" | "nous-api" => Some("NOUS_API_KEY"),
        "copilot" => Some("COPILOT_GITHUB_TOKEN"),
        "ai-gateway" => Some("AI_GATEWAY_API_KEY"),
        "arcee" => Some("ARCEEAI_API_KEY"),
        "deepseek" => Some("DEEPSEEK_API_KEY"),
        "huggingface" => Some("HF_TOKEN"),
        "gmi" => Some("GMI_API_KEY"),
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
        "lmstudio" => Some("LMSTUDIO_API_KEY"),
        "lmdeploy" => Some("LMDEPLOY_API_KEY"),
        "localai" => Some("LOCALAI_API_KEY"),
        "koboldcpp" => Some("KOBOLDCPP_API_KEY"),
        "text-generation-webui" => Some("TEXT_GENERATION_WEBUI_API_KEY"),
        "tabbyapi" => Some("TABBYAPI_API_KEY"),
        "opencode-go" => Some("OPENCODE_GO_API_KEY"),
        "opencode-zen" => Some("OPENCODE_ZEN_API_KEY"),
        "xai" => Some("XAI_API_KEY"),
        "xiaomi" => Some("XIAOMI_API_KEY"),
        "tencent-tokenhub" => Some("TOKENHUB_API_KEY"),
        "zai" => Some("GLM_API_KEY"),
        _ => None,
    }
}

fn provider_supports_oauth(provider: &str) -> bool {
    let normalized = normalize_auth_provider(provider);
    hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(normalized.as_str()))
}

fn resolve_auth_type_for_provider(provider: &str, requested: Option<&str>) -> String {
    if let Some(raw) = requested.map(str::trim).filter(|v| !v.is_empty()) {
        return raw.replace('-', "_").to_ascii_lowercase();
    }
    if provider_supports_oauth(provider) {
        "oauth".to_string()
    } else {
        "api_key".to_string()
    }
}

fn parse_rfc3339_utc(value: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

fn parse_unix_millis_utc(value: Option<i64>) -> Option<chrono::DateTime<chrono::Utc>> {
    value.and_then(chrono::DateTime::from_timestamp_millis)
}

fn secret_vault_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("tokens.json")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct AuthPoolEntry {
    id: String,
    label: String,
    auth_type: String,
    source: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_status_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error_code: Option<u16>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct AuthPoolStore {
    #[serde(default)]
    providers: std::collections::BTreeMap<String, Vec<AuthPoolEntry>>,
}

fn auth_pool_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("pool.json")
}

fn load_auth_pool_store(path: &Path) -> Result<AuthPoolStore, AgentError> {
    if !path.exists() {
        return Ok(AuthPoolStore::default());
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_json::from_str(&raw).map_err(|e| AgentError::Config(format!("parse pool: {}", e)))
}

fn save_auth_pool_store(path: &Path, store: &AuthPoolStore) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("mkdir {}: {}", parent.display(), e)))?;
    }
    let raw = serde_json::to_string_pretty(store).map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(path, raw)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn resolve_pool_target(entries: &[AuthPoolEntry], target: &str) -> Option<usize> {
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

async fn lookup_secret_from_vault(
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

async fn hydrate_provider_env_from_vault_for_cli(cli: &Cli) -> Result<(), AgentError> {
    hydrate_provider_env_from_vault_for_cli_with_options(cli, true).await
}

fn scrub_unusable_nous_api_key_for_oauth_state() -> Result<(), AgentError> {
    if read_nous_auth_state()?.is_some() && read_valid_nous_auth_state()?.is_none() {
        std::env::remove_var("NOUS_API_KEY");
    }
    Ok(())
}

async fn hydrate_provider_env_from_vault_for_cli_with_options(
    cli: &Cli,
    prefer_nous_runtime_credentials: bool,
) -> Result<(), AgentError> {
    let path = secret_vault_path_for_cli(cli);
    let nous_oauth_state_present =
        prefer_nous_runtime_credentials && read_nous_auth_state()?.is_some();
    if !path.exists() {
        if nous_oauth_state_present {
            std::env::remove_var("NOUS_API_KEY");
        }
        return Ok(());
    }
    let store = FileTokenStore::new(path).await?;
    let manager = AuthManager::new(store.clone());

    if !prefer_nous_runtime_credentials {
        if let Some((_provider, token)) = lookup_secret_from_vault(&store, "nous").await {
            std::env::set_var("NOUS_API_KEY", token);
        }
    } else {
        match resolve_nous_runtime_credentials(
            false,
            true,
            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
        )
        .await
        {
            Ok(creds) => {
                std::env::set_var("NOUS_API_KEY", creds.api_key.clone());
                if !creds.base_url.trim().is_empty() {
                    std::env::set_var("NOUS_INFERENCE_BASE_URL", creds.base_url.clone());
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
                if nous_oauth_state_present {
                    std::env::remove_var("NOUS_API_KEY");
                } else if let Some((_provider, token)) =
                    lookup_secret_from_vault(&store, "nous").await
                {
                    std::env::set_var("NOUS_API_KEY", token);
                }
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
        ("COPILOT_GITHUB_TOKEN", "copilot"),
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
        ("LMSTUDIO_API_KEY", "lmstudio"),
        ("LMDEPLOY_API_KEY", "lmdeploy"),
        ("LOCALAI_API_KEY", "localai"),
        ("KOBOLDCPP_API_KEY", "koboldcpp"),
        ("TEXT_GENERATION_WEBUI_API_KEY", "text-generation-webui"),
        ("TABBYAPI_API_KEY", "tabbyapi"),
        ("OPENCODE_GO_API_KEY", "opencode-go"),
        ("OPENCODE_ZEN_API_KEY", "opencode-zen"),
        ("XAI_API_KEY", "xai"),
        ("XIAOMI_API_KEY", "xiaomi"),
        ("TOKENHUB_API_KEY", "tencent-tokenhub"),
        ("GLM_API_KEY", "zai"),
        ("ZAI_API_KEY", "zai"),
        ("Z_AI_API_KEY", "zai"),
    ];

    for (env_var, provider) in env_bindings {
        if prefer_nous_runtime_credentials && provider == "nous" && nous_oauth_state_present {
            continue;
        }
        let env_present = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty());
        if let Some(current) = env_present {
            if provider_supports_oauth(provider) {
                if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await
                {
                    if secret.trim() != current.trim() {
                        std::env::set_var(env_var, secret);
                    }
                }
            }
            continue;
        }
        if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await {
            std::env::set_var(env_var, secret);
        }
    }
    Ok(())
}

fn mask_secret(secret: &str) -> String {
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

fn is_weixin_provider(provider: &str) -> bool {
    provider == "weixin"
}

fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| is_truthy(&v))
}

async fn telegram_bot_token_from_env_or_prompt() -> Result<String, AgentError> {
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

async fn weixin_account_id_from_env_or_prompt() -> Result<String, AgentError> {
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

fn weixin_account_file_path(account_id: &str) -> PathBuf {
    hermes_home()
        .join("weixin")
        .join("accounts")
        .join(format!("{account_id}.json"))
}

fn load_persisted_weixin_token(account_id: &str) -> Option<String> {
    let p = weixin_account_file_path(account_id);
    let s = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

fn save_persisted_weixin_account(
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
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(&p, payload.to_string())
        .map_err(|e| AgentError::Io(format!("write weixin account file {}: {e}", p.display())))?;
    Ok(())
}

async fn weixin_token_from_env_or_prompt(account_id: &str) -> Result<String, AgentError> {
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

async fn qqbot_app_id_from_env_or_prompt(existing: Option<&str>) -> Result<String, AgentError> {
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

async fn qqbot_client_secret_from_env_or_prompt(
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

fn qqbot_portal_host_from_disk(disk: &hermes_config::GatewayConfig) -> String {
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

fn qqbot_onboard_endpoints_from_disk(disk: &hermes_config::GatewayConfig) -> (String, String) {
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

fn qqbot_generate_bind_key_base64() -> String {
    let mut key = [0u8; 32];
    rand::fill(&mut key[..]);
    BASE64_STANDARD.encode(key)
}

fn qqbot_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
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

fn qqbot_extract_i64(v: &serde_json::Value, keys: &[&str]) -> Option<i64> {
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

fn qqbot_decrypt_secret(encrypted_base64: &str, key_base64: &str) -> Result<String, AgentError> {
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
    let cipher = <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key_bytes)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: cipher init failed: {e}")))?;
    let plaintext = cipher
        .decrypt(nonce, &encrypted_bytes[12..])
        .map_err(|_| AgentError::Config("qqbot qr decrypt: decrypt failed".to_string()))?;
    String::from_utf8(plaintext)
        .map_err(|e| AgentError::Config(format!("qqbot qr decrypt: invalid utf-8: {e}")))
}

fn qqbot_connect_url(task_id: &str) -> String {
    format!(
        "https://q.qq.com/qqbot/openclaw/connect.html?task_id={}&_wv=2&source=hermes",
        urlencoding::encode(task_id.trim())
    )
}

fn qqbot_api_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, USER_AGENT};
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("HermesAgentUltra/qqbot-onboard"),
    );
    headers
}

fn qqbot_join_https_url(host: &str, path: &str) -> String {
    let host = host.trim().trim_end_matches('/');
    let path = path.trim();
    if path.starts_with('/') {
        format!("https://{}{}", host, path)
    } else {
        format!("https://{}/{}", host, path)
    }
}

async fn qqbot_create_bind_task(
    client: &reqwest::Client,
    portal_host: &str,
    create_path: &str,
    key_base64: &str,
) -> Result<String, AgentError> {
    let url = qqbot_join_https_url(portal_host, create_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({ "key": key_base64 }))
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

async fn qqbot_poll_bind_result(
    client: &reqwest::Client,
    portal_host: &str,
    poll_path: &str,
    task_id: &str,
) -> Result<(i64, String, String, String), AgentError> {
    let url = qqbot_join_https_url(portal_host, poll_path);
    let resp = client
        .post(url)
        .headers(qqbot_api_headers())
        .json(&serde_json::json!({ "task_id": task_id }))
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

async fn qqbot_qr_login_flow(
    portal_host: &str,
    create_path: &str,
    poll_path: &str,
    timeout_seconds: u64,
) -> Result<(String, String, String), AgentError> {
    const ONBOARD_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    const MAX_REFRESHES: usize = 3;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| AgentError::Io(format!("qqbot onboard client init failed: {e}")))?;

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_seconds);

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
            if std::time::Instant::now() >= deadline {
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

fn weixin_login_base_url_from_disk(disk: &hermes_config::GatewayConfig) -> String {
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

fn weixin_login_endpoints_from_disk(disk: &hermes_config::GatewayConfig) -> (String, String) {
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

fn weixin_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
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

fn render_qr_to_terminal(data: &str) {
    let len = data.len();
    let side = (len as f64).sqrt().ceil() as usize;
    if side == 0 {
        println!("(empty QR data)");
        return;
    }
    let bytes = data.as_bytes();
    let is_dark = |row: usize, col: usize| -> bool {
        let idx = row * side + col;
        if idx < bytes.len() {
            bytes[idx] % 2 == 1
        } else {
            false
        }
    };
    let mut row = 0;
    while row < side {
        let mut line = String::new();
        for col in 0..side {
            let top = is_dark(row, col);
            let bottom = if row + 1 < side {
                is_dark(row + 1, col)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("  {}", line);
        row += 2;
    }
}

async fn weixin_qr_login_flow(
    base_url: &str,
    start_ep: &str,
    poll_ep: &str,
    _account_id_hint: Option<&str>,
) -> Result<(String, String, String, String), AgentError> {
    let initial_base = base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::new();
    async fn fetch_weixin_qr(
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
            .timeout(std::time::Duration::from_secs(35))
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

    let poll_interval = std::time::Duration::from_secs(1);
    let timeout = std::time::Duration::from_secs(480);
    let started = std::time::Instant::now();
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
            .timeout(std::time::Duration::from_secs(35))
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

