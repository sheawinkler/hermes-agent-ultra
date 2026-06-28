fn selected_image_provider() -> Option<&'static str> {
    for key in [
        "HERMES_IMAGE_GEN_PROVIDER",
        "HERMES_IMAGE_GEN_BACKEND",
        "IMAGE_GEN_PROVIDER",
        "IMAGE_GEN_BACKEND",
    ] {
        if let Some(value) = env_optional_nonempty(key) {
            if let Some(provider) = normalize_image_provider(&value) {
                return Some(provider);
            }
        }
    }
    configured_image_provider().and_then(|value| normalize_image_provider(&value))
}

fn normalize_image_provider(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "codex" | "openai-codex" | "openai_codex" | "chatgpt" | "chatgpt-codex" => {
            Some("openai-codex")
        }
        "fal" | "fal-ai" | "fal_ai" => Some("fal"),
        "openrouter" | "open-router" | "or" => Some("openrouter"),
        "nous" | "nous-portal" | "nous_api" | "nous-api" | "nousapi" => Some("nous"),
        "krea" | "krea-ai" | "krea_ai" => Some("krea"),
        _ => None,
    }
}

fn configured_image_provider() -> Option<String> {
    let cfg = load_image_gen_config()?;
    for key in ["provider", "backend"] {
        if let Some(value) = yaml_get_str(&cfg, key) {
            return Some(value.to_string());
        }
    }
    None
}

fn load_image_gen_config() -> Option<serde_yaml::Value> {
    let root = load_config_yaml_root()?;
    yaml_get(&root, "image_gen").cloned()
}

fn load_config_yaml_root() -> Option<serde_yaml::Value> {
    let raw = std::fs::read_to_string(hermes_config::paths::config_path()).ok()?;
    serde_yaml::from_str(&raw).ok()
}

fn yaml_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    value
        .as_mapping()?
        .get(serde_yaml::Value::String(key.to_string()))
}

fn yaml_get_str<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a str> {
    yaml_get(value, key)?
        .as_str()
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

fn yaml_get_any_str<'a>(value: &'a serde_yaml::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| yaml_get_str(value, key))
}

fn yaml_get_boolish(value: &serde_yaml::Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| {
        yaml_get(value, key).is_some_and(|value| match value {
            serde_yaml::Value::Bool(value) => *value,
            serde_yaml::Value::String(value) => truthy_or_managed(value),
            _ => false,
        })
    })
}

fn truthy_or_managed(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "gateway" | "managed" | "nous"
    )
}

fn yaml_provider_section(
    root: &serde_yaml::Value,
    provider: OpenRouterCompatImageProviderKind,
) -> Option<&serde_yaml::Value> {
    let aliases: &[&str] = match provider {
        OpenRouterCompatImageProviderKind::OpenRouter => &["openrouter"],
        OpenRouterCompatImageProviderKind::Nous => &["nous", "nous-api", "nous_api", "nousapi"],
    };
    for parent in ["llm_providers", "providers"] {
        if let Some(table) = yaml_get(root, parent) {
            for alias in aliases {
                if let Some(section) = yaml_get(table, alias) {
                    return Some(section);
                }
            }
        }
    }
    None
}

fn scoped_image_provider_config(
    provider: OpenRouterCompatImageProviderKind,
) -> Option<serde_yaml::Value> {
    let cfg = load_image_gen_config()?;
    yaml_get(&cfg, provider.config_key()).cloned()
}

fn resolve_openrouter_compat_model(provider: OpenRouterCompatImageProviderKind) -> String {
    if let Some(value) = env_optional_nonempty(provider.model_env_var()) {
        return value;
    }
    scoped_image_provider_config(provider)
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| DEFAULT_OPENROUTER_COMPAT_IMAGE_MODEL.to_string())
}

fn resolve_openrouter_compat_base_url(provider: OpenRouterCompatImageProviderKind) -> String {
    if let Some(value) = scoped_image_provider_config(provider)
        .as_ref()
        .and_then(|cfg| {
            yaml_get_any_str(cfg, &["base_url", "inference_base_url"]).map(ToOwned::to_owned)
        })
    {
        return value.trim_end_matches('/').to_string();
    }
    if let Some(root) = load_config_yaml_root() {
        if let Some(value) = yaml_provider_section(&root, provider)
            .and_then(|cfg| yaml_get_any_str(cfg, &["base_url", "inference_base_url"]))
        {
            return value.trim_end_matches('/').to_string();
        }
    }
    for key in provider.base_url_env_vars() {
        if let Some(value) = env_optional_nonempty(key) {
            return value.trim_end_matches('/').to_string();
        }
    }
    if provider == OpenRouterCompatImageProviderKind::Nous {
        if let Some(value) = read_provider_auth_string("nous", &["inference_base_url"]) {
            return value.trim_end_matches('/').to_string();
        }
    }
    provider.default_base_url().to_string()
}

fn resolve_openrouter_compat_api_key(
    provider: OpenRouterCompatImageProviderKind,
) -> Option<String> {
    if let Some(value) = scoped_image_provider_config(provider)
        .as_ref()
        .and_then(resolve_api_key_from_yaml_provider_section)
    {
        return Some(value);
    }
    if let Some(root) = load_config_yaml_root() {
        if let Some(value) = yaml_provider_section(&root, provider)
            .and_then(resolve_api_key_from_yaml_provider_section)
        {
            return Some(value);
        }
    }
    for key in provider.api_key_env_vars() {
        if let Some(value) = env_optional_nonempty(key) {
            return Some(value);
        }
    }
    if provider == OpenRouterCompatImageProviderKind::Nous {
        if let Some(value) = env_optional_nonempty("TOOL_GATEWAY_USER_TOKEN") {
            return Some(value);
        }
    }
    read_provider_auth_string(
        provider.provider_id(),
        &["agent_key", "api_key", "access_token"],
    )
    .or_else(|| read_provider_auth_tokens_string(provider.provider_id(), "access_token"))
}

fn resolve_api_key_from_yaml_provider_section(section: &serde_yaml::Value) -> Option<String> {
    if let Some(value) = yaml_get_str(section, "api_key").and_then(resolve_env_ref_or_literal) {
        return Some(value);
    }
    if let Some(env_name) = yaml_get_str(section, "api_key_env") {
        return env_optional_nonempty(env_name);
    }
    None
}

fn resolve_env_ref_or_literal(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(env_name) = trimmed
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        return env_optional_nonempty(env_name);
    }
    Some(trimmed.to_string())
}

fn openrouter_compat_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("landscape") | Some("16:9") | Some("1536x1024") => "16:9",
        Some("portrait") | Some("9:16") | Some("1024x1536") => "9:16",
        _ => "1:1",
    }
}

fn openrouter_compat_chat_payload(
    model: &str,
    prompt: &str,
    aspect_ratio: &str,
    reference_image_parts: &[String],
) -> Value {
    let mut content = vec![json!({"type": "text", "text": prompt})];
    content.extend(reference_image_parts.iter().map(|url| {
        json!({
            "type": "image_url",
            "image_url": {"url": url},
        })
    }));
    json!({
        "model": model,
        "modalities": ["image", "text"],
        "messages": [{
            "role": "user",
            "content": content,
        }],
        "image_config": {"aspect_ratio": aspect_ratio},
    })
}

fn openrouter_compat_reference_image_parts(
    request: &ImageGenerateRequest,
) -> Result<Vec<String>, ToolError> {
    request
        .source_image_urls()
        .into_iter()
        .take(OPENROUTER_COMPAT_MAX_REFERENCE_IMAGES)
        .filter_map(|reference| openrouter_compat_image_url_part(reference.as_str()).transpose())
        .collect()
}

fn krea_style_reference_objects(request: &ImageGenerateRequest) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut refs = Vec::new();
    for reference in request.source_image_urls() {
        let reference = reference.trim();
        if reference.is_empty() || !seen.insert(reference.to_string()) {
            continue;
        }
        refs.push(json!({
            "url": reference,
            "strength": DEFAULT_KREA_STYLE_REFERENCE_STRENGTH,
        }));
        if refs.len() >= KREA_MAX_REFERENCE_IMAGES {
            break;
        }
    }
    refs
}

fn krea_submit_payload(
    prompt: &str,
    aspect_ratio: &str,
    creativity: &str,
    style_references: &[Value],
) -> Value {
    let mut payload = Map::new();
    payload.insert("prompt".to_string(), json!(prompt));
    payload.insert("aspect_ratio".to_string(), json!(aspect_ratio));
    payload.insert("resolution".to_string(), json!(DEFAULT_KREA_RESOLUTION));
    payload.insert("creativity".to_string(), json!(creativity));
    if !style_references.is_empty() {
        // Krea rejects bare URL strings here with a 422. The Rust surface only
        // accepts URL/path references, so normalize them to Krea's object shape.
        payload.insert(
            "image_style_references".to_string(),
            Value::Array(style_references.to_vec()),
        );
    }
    Value::Object(payload)
}

fn extract_krea_result_image_url(job: &Value) -> Option<String> {
    let result = job.get("result")?;
    if let Some(url) = result
        .get("urls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|url| !url.is_empty())
    {
        return Some(url.to_string());
    }
    result
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
}

fn openrouter_compat_image_url_part(reference: &str) -> Result<Option<String>, ToolError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    if reference.starts_with("http://")
        || reference.starts_with("https://")
        || reference.starts_with("data:")
    {
        return Ok(Some(reference.to_string()));
    }
    let path = Path::new(reference);
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Could not read reference image {}: {err}",
                path.display()
            )));
        }
    };
    let mime = mime_type_for_path(path);
    Ok(Some(format!("data:{mime};base64,{}", STANDARD.encode(raw))))
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("bmp") => "image/bmp",
        Some("svg") => "image/svg+xml",
        _ => "image/png",
    }
}

fn extension_for_mime(mime: Option<&str>) -> &'static str {
    match mime
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
    {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/svg+xml" => "svg",
        _ => "png",
    }
}

fn extract_openrouter_compat_images(payload: &Value) -> Vec<String> {
    payload
        .get("choices")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|choice| choice.get("message"))
        .filter_map(|message| message.get("images"))
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(|image| image.get("image_url"))
        .filter_map(|image_url| image_url.get("url"))
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn openrouter_compat_error_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| raw.chars().take(500).collect())
}

fn krea_error_message(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| {
            if let Some(message) = value
                .get("error")
                .and_then(|error| error.get("message").or(Some(error)))
                .and_then(Value::as_str)
            {
                return Some(message.trim().to_string());
            }
            value
                .get("message")
                .or_else(|| value.get("detail"))
                .and_then(Value::as_str)
                .map(str::trim)
                .map(ToOwned::to_owned)
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| raw.chars().take(500).collect())
}


