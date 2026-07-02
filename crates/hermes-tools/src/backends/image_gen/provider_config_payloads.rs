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
        "openai" | "openai-images" | "openai_image" | "openai-image" => Some("openai"),
        "fal" | "fal-ai" | "fal_ai" => Some("fal"),
        "openrouter" | "open-router" | "or" => Some("openrouter"),
        "nous" | "nous-portal" | "nous_api" | "nous-api" | "nousapi" => Some("nous"),
        "krea" | "krea-ai" | "krea_ai" => Some("krea"),
        "xai" | "x.ai" | "grok" | "grok-imagine" => Some("xai"),
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

fn scoped_image_named_config(provider: &str) -> Option<serde_yaml::Value> {
    let cfg = load_image_gen_config()?;
    yaml_get(&cfg, provider).cloned()
}

fn resolve_named_image_base_url(provider: &str, env_keys: &[&str], default: &str) -> String {
    if let Some(value) = scoped_image_named_config(provider)
        .as_ref()
        .and_then(|cfg| yaml_get_any_str(cfg, &["base_url", "api_base_url"]))
    {
        return value.trim_end_matches('/').to_string();
    }
    for key in env_keys {
        if let Some(value) = env_optional_nonempty(key) {
            return value.trim_end_matches('/').to_string();
        }
    }
    default.to_string()
}

fn resolve_named_image_api_key(provider: &str, env_keys: &[&str]) -> Option<String> {
    if let Some(value) = scoped_image_named_config(provider)
        .as_ref()
        .and_then(resolve_api_key_from_yaml_provider_section)
    {
        return Some(value);
    }
    for key in env_keys {
        if let Some(value) = env_optional_nonempty(key) {
            return Some(value);
        }
    }
    read_provider_auth_string(provider, &["api_key", "access_token", "token", "bearer_token"])
        .or_else(|| read_provider_auth_tokens_string(provider, "access_token"))
}

fn resolve_openai_image_base_url() -> String {
    resolve_named_image_base_url(
        "openai",
        &[
            "HERMES_OPENAI_IMAGE_BASE_URL",
            "OPENAI_IMAGE_BASE_URL",
            "HERMES_OPENAI_BASE_URL",
            "OPENAI_BASE_URL",
        ],
        DEFAULT_OPENAI_IMAGE_BASE_URL,
    )
}

fn resolve_openai_image_api_key() -> Option<String> {
    resolve_named_image_api_key("openai", &["HERMES_OPENAI_API_KEY", "OPENAI_API_KEY"])
}

fn resolve_xai_image_base_url() -> String {
    resolve_named_image_base_url(
        "xai",
        &["HERMES_XAI_IMAGE_BASE_URL", "XAI_IMAGE_BASE_URL", "HERMES_XAI_BASE_URL", "XAI_BASE_URL"],
        DEFAULT_XAI_IMAGE_BASE_URL,
    )
}

fn resolve_xai_image_config() -> XaiImageGenConfig {
    if let Some(api_key) = env_optional_nonempty("HERMES_XAI_API_KEY")
        .or_else(|| env_optional_nonempty("XAI_API_KEY"))
    {
        return XaiImageGenConfig::new(Some(api_key), "env");
    }
    if let Some(api_key) = scoped_image_named_config("xai")
        .as_ref()
        .and_then(resolve_api_key_from_yaml_provider_section)
    {
        return XaiImageGenConfig::new(Some(api_key), "config");
    }
    if let Some(api_key) =
        read_provider_auth_string("xai", &["api_key", "access_token", "token", "bearer_token"])
            .or_else(|| read_provider_auth_tokens_string("xai", "access_token"))
            .or_else(|| {
                read_provider_auth_string(
                    "xai-oauth",
                    &["api_key", "access_token", "token", "bearer_token"],
                )
            })
            .or_else(|| read_provider_auth_tokens_string("xai-oauth", "access_token"))
    {
        return XaiImageGenConfig::new(Some(api_key), "auth-store");
    }
    XaiImageGenConfig::unconfigured()
}

fn resolve_xai_image_model() -> String {
    if let Some(value) = env_optional_nonempty("XAI_IMAGE_MODEL") {
        return normalize_xai_image_model(value.as_str()).to_string();
    }
    scoped_image_named_config("xai")
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .map(normalize_xai_image_model)
        .unwrap_or(DEFAULT_XAI_IMAGE_MODEL)
        .to_string()
}

fn normalize_xai_image_model(value: &str) -> &'static str {
    match value.trim() {
        DEFAULT_XAI_IMAGE_EDIT_MODEL => DEFAULT_XAI_IMAGE_EDIT_MODEL,
        _ => DEFAULT_XAI_IMAGE_MODEL,
    }
}

fn resolve_xai_image_resolution() -> String {
    let value = env_optional_nonempty("XAI_IMAGE_RESOLUTION").or_else(|| {
        scoped_image_named_config("xai")
            .as_ref()
            .and_then(|cfg| yaml_get_str(cfg, "resolution"))
            .map(ToOwned::to_owned)
    });
    match value.as_deref().map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("2k") => "2k".to_string(),
        _ => DEFAULT_XAI_IMAGE_RESOLUTION.to_string(),
    }
}

fn resolve_openrouter_compat_model(provider: OpenRouterCompatImageProviderKind) -> String {
    if let Some(value) = env_optional_nonempty(provider.model_env_var()) {
        return value;
    }
    scoped_image_provider_config(provider)
        .as_ref()
        .and_then(|cfg| yaml_get_str(cfg, "model"))
        .map(ToOwned::to_owned)
        .or_else(|| {
            load_image_gen_config()
                .as_ref()
                .and_then(|cfg| yaml_get_str(cfg, "model"))
                .map(ToOwned::to_owned)
        })
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

fn resolve_openai_image_tier() -> CodexImageTier {
    if let Some(value) = env_optional_nonempty("OPENAI_IMAGE_MODEL") {
        if let Some(tier) = codex_image_tier(&value) {
            return tier;
        }
    }
    if let Some(cfg) = load_image_gen_config() {
        if let Some(openai) = yaml_get(&cfg, "openai") {
            if let Some(value) = yaml_get_str(openai, "model") {
                if let Some(tier) = codex_image_tier(value) {
                    return tier;
                }
            }
        }
        if let Some(value) = yaml_get_str(&cfg, "model") {
            if let Some(tier) = codex_image_tier(value) {
                return tier;
            }
        }
    }
    codex_image_tier(DEFAULT_OPENAI_IMAGE_MODEL).expect("default OpenAI image model tier")
}

fn openai_image_size_from_tool_size(size: Option<&str>) -> &'static str {
    codex_image_size_from_tool_size(size)
}

fn openai_tool_aspect_from_size(size: &str) -> &'static str {
    match size {
        "1536x1024" => "landscape",
        "1024x1536" => "portrait",
        _ => "square",
    }
}

fn openai_image_generation_payload(prompt: &str, size: &str, quality: &str) -> Value {
    json!({
        "model": OPENAI_IMAGE_API_MODEL,
        "prompt": prompt,
        "size": size,
        "n": 1,
        "quality": quality,
    })
}

async fn openai_image_edit_form(
    client: &Client,
    sources: &[String],
    prompt: &str,
    size: &str,
    quality: &str,
) -> Result<reqwest::multipart::Form, ToolError> {
    let mut form = reqwest::multipart::Form::new()
        .text("model", OPENAI_IMAGE_API_MODEL.to_string())
        .text("prompt", prompt.to_string())
        .text("size", size.to_string())
        .text("quality", quality.to_string())
        .text("n", "1".to_string());
    for source in sources.iter().take(OPENAI_MAX_REFERENCE_IMAGES) {
        let loaded = load_image_source_bytes(client, source).await?;
        let field = if sources.len() == 1 { "image" } else { "image[]" };
        let part = reqwest::multipart::Part::bytes(loaded.bytes)
            .file_name(loaded.filename)
            .mime_str(loaded.mime)
            .map_err(|e| {
                ToolError::ExecutionFailed(format!("Could not set image multipart MIME: {e}"))
            })?;
        form = form.part(field, part);
    }
    Ok(form)
}

struct LoadedImageSource {
    bytes: Vec<u8>,
    filename: String,
    mime: &'static str,
}

async fn load_image_source_bytes(
    client: &Client,
    source: &str,
) -> Result<LoadedImageSource, ToolError> {
    let source = source.trim();
    if source.is_empty() {
        return Err(ToolError::InvalidParams(
            "image source must be a non-empty URL, data URI, or local path".into(),
        ));
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        let resp = client.get(source).send().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Could not download source image {source}: {e}"))
        })?;
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        if !status.is_success() {
            return Err(ToolError::ExecutionFailed(format!(
                "Could not download source image {source}: HTTP {status}"
            )));
        }
        let bytes = resp.bytes().await.map_err(|e| {
            ToolError::ExecutionFailed(format!("Could not read source image {source}: {e}"))
        })?;
        let filename = url::Url::parse(source)
            .ok()
            .and_then(|url| {
                Path::new(url.path())
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(ToOwned::to_owned)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "image.png".to_string());
        let mime = extension_for_mime(content_type.as_deref());
        return Ok(LoadedImageSource {
            bytes: bytes.to_vec(),
            filename,
            mime: mime_to_content_type(mime),
        });
    }
    if source.starts_with("data:") {
        let (header, encoded) = source.split_once(',').ok_or_else(|| {
            ToolError::InvalidParams("image data URI did not contain base64 data".into())
        })?;
        let mime = header
            .strip_prefix("data:")
            .and_then(|value| value.split_once(';').map(|(mime, _)| mime))
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("image/png");
        let bytes = STANDARD.decode(encoded.trim()).map_err(|e| {
            ToolError::InvalidParams(format!("image data URI was not valid base64: {e}"))
        })?;
        return Ok(LoadedImageSource {
            bytes,
            filename: format!("image.{}", extension_for_mime(Some(mime))),
            mime: mime_to_content_type(extension_for_mime(Some(mime))),
        });
    }
    let path = Path::new(source);
    let bytes = std::fs::read(path).map_err(|e| {
        ToolError::ExecutionFailed(format!("Could not read source image {}: {e}", path.display()))
    })?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "image.png".to_string());
    Ok(LoadedImageSource {
        bytes,
        filename,
        mime: mime_type_for_path(path),
    })
}

fn mime_to_content_type(ext: &str) -> &'static str {
    match ext {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "image/png",
    }
}

fn xai_aspect_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("landscape") | Some("16:9") | Some("1536x1024") => "16:9",
        Some("portrait") | Some("9:16") | Some("1024x1536") => "9:16",
        Some("4:3") => "4:3",
        Some("3:4") => "3:4",
        Some("3:2") => "3:2",
        Some("2:3") => "2:3",
        _ => "1:1",
    }
}

fn xai_tool_aspect_from_xai(aspect: &str) -> &'static str {
    match aspect {
        "16:9" => "landscape",
        "9:16" => "portrait",
        "4:3" => "4:3",
        "3:4" => "3:4",
        "3:2" => "3:2",
        "2:3" => "2:3",
        _ => "square",
    }
}

fn xai_image_generation_payload(
    model: &str,
    prompt: &str,
    aspect_ratio: &str,
    resolution: &str,
) -> Value {
    json!({
        "model": model,
        "prompt": prompt,
        "aspect_ratio": aspect_ratio,
        "resolution": resolution,
    })
}

fn xai_image_edit_payload(prompt: &str, images: &[Value]) -> Value {
    let mut payload = Map::new();
    payload.insert("model".to_string(), json!(DEFAULT_XAI_IMAGE_EDIT_MODEL));
    payload.insert("prompt".to_string(), json!(prompt));
    if images.len() == 1 {
        payload.insert("image".to_string(), images[0].clone());
    } else {
        payload.insert("images".to_string(), Value::Array(images.to_vec()));
    }
    Value::Object(payload)
}

fn xai_image_fields(sources: &mut [String]) -> Result<Vec<Value>, ToolError> {
    sources
        .iter()
        .map(|source| {
            let source = source.trim();
            if source.starts_with("http://") || source.starts_with("https://") || source.starts_with("data:") {
                return Ok(json!({"url": source, "type": "image_url"}));
            }
            let path = Path::new(source);
            let raw = std::fs::read(path).map_err(|e| {
                ToolError::ExecutionFailed(format!(
                    "Could not read xAI source image {}: {e}",
                    path.display()
                ))
            })?;
            Ok(json!({
                "url": format!("data:{};base64,{}", mime_type_for_path(path), STANDARD.encode(raw)),
                "type": "image_url"
            }))
        })
        .collect()
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

fn codex_image_reference_parts(request: &ImageGenerateRequest) -> Result<Vec<String>, ToolError> {
    request
        .source_image_urls()
        .into_iter()
        .take(CODEX_MAX_SOURCE_IMAGES)
        .filter_map(|reference| codex_image_reference_part(reference.as_str()).transpose())
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

fn codex_image_reference_part(reference: &str) -> Result<Option<String>, ToolError> {
    let reference = reference.trim();
    if reference.is_empty() {
        return Ok(None);
    }
    if reference.starts_with("http://") || reference.starts_with("https://") {
        return Ok(Some(reference.to_string()));
    }
    if reference.starts_with("data:") {
        ensure_codex_raster_data_uri(reference)?;
        return Ok(Some(reference.to_string()));
    }

    let path = Path::new(reference);
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(ToolError::ExecutionFailed(format!(
                "Could not read Codex source image {}: {err}",
                path.display()
            )));
        }
    };
    let mime = codex_raster_mime_type_for_path(path).ok_or_else(|| {
        ToolError::InvalidParams(format!(
            "Codex source image {} must be a raster image (png, jpeg, webp, gif, bmp, or avif).",
            path.display()
        ))
    })?;
    Ok(Some(format!("data:{mime};base64,{}", STANDARD.encode(raw))))
}

fn ensure_codex_raster_data_uri(reference: &str) -> Result<(), ToolError> {
    let mime = reference
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(';').map(|(mime, _)| mime.trim()))
        .filter(|mime| !mime.is_empty())
        .ok_or_else(|| {
            ToolError::InvalidParams(
                "Codex source image data URI must include a raster image media type.".into(),
            )
        })?;
    if codex_raster_mime_allowed(mime) {
        Ok(())
    } else {
        Err(ToolError::InvalidParams(format!(
            "Codex source image data URI must be raster image data, got {mime}."
        )))
    }
}

fn codex_raster_mime_type_for_path(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => Some("image/png"),
        Some("jpg" | "jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("bmp") => Some("image/bmp"),
        Some("avif") => Some("image/avif"),
        _ => None,
    }
}

fn codex_raster_mime_allowed(mime: &str) -> bool {
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "image/png"
            | "image/jpeg"
            | "image/jpg"
            | "image/gif"
            | "image/webp"
            | "image/bmp"
            | "image/avif"
    )
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
