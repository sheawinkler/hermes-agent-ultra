fn is_local_openai_compatible_provider(provider: &str) -> bool {
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

fn local_provider_default_base_url(provider: &str) -> Option<&'static str> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "ollama-local" => Some(OLLAMA_LOCAL_DEFAULT_BASE_URL),
        "llama-cpp" => Some(LLAMA_CPP_DEFAULT_BASE_URL),
        "vllm" => Some(VLLM_DEFAULT_BASE_URL),
        "mlx" => Some(MLX_DEFAULT_BASE_URL),
        "apple-ane" => Some(APPLE_ANE_DEFAULT_BASE_URL),
        "sglang" => Some(SGLANG_DEFAULT_BASE_URL),
        "tgi" => Some(TGI_DEFAULT_BASE_URL),
        "lmstudio" => Some(LMSTUDIO_DEFAULT_BASE_URL),
        "lmdeploy" => Some(LMDEPLOY_DEFAULT_BASE_URL),
        "localai" => Some(LOCALAI_DEFAULT_BASE_URL),
        "koboldcpp" => Some(KOBOLDCPP_DEFAULT_BASE_URL),
        "text-generation-webui" => Some(TEXT_GENERATION_WEBUI_DEFAULT_BASE_URL),
        "tabbyapi" => Some(TABBYAPI_DEFAULT_BASE_URL),
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
        "lmstudio" => Some("LMSTUDIO_BASE_URL"),
        "lmdeploy" => Some("LMDEPLOY_BASE_URL"),
        "localai" => Some("LOCALAI_BASE_URL"),
        "koboldcpp" => Some("KOBOLDCPP_BASE_URL"),
        "text-generation-webui" => Some("TEXT_GENERATION_WEBUI_BASE_URL"),
        "tabbyapi" => Some("TABBYAPI_BASE_URL"),
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
        "lmstudio" => std::env::var("LMSTUDIO_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "lmdeploy" => std::env::var("LMDEPLOY_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "localai" => std::env::var("LOCALAI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "koboldcpp" => std::env::var("KOBOLDCPP_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "text-generation-webui" => std::env::var("TEXT_GENERATION_WEBUI_API_KEY")
            .ok()
            .filter(|v| !v.trim().is_empty()),
        "tabbyapi" => std::env::var("TABBYAPI_API_KEY")
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

fn openai_compatible_models_url(base_url: &str) -> String {
    format!(
        "{}/models?output_modalities=all",
        base_url.trim_end_matches('/')
    )
}

async fn fetch_openai_compatible_live_models(base_url: &str, api_key: Option<&str>) -> Vec<String> {
    if cfg!(test) {
        return Vec::new();
    }
    let url = openai_compatible_models_url(base_url);
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

