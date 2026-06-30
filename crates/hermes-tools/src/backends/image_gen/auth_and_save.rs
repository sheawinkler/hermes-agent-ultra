fn scale_duration(value: Duration, factor: f64, max: Duration) -> Duration {
    if value.is_zero() {
        return value;
    }
    let scaled = value.mul_f64(factor);
    if scaled > max {
        max
    } else {
        scaled
    }
}

async fn save_openrouter_compat_generated_image(
    client: &Client,
    image_url: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    if image_url.trim_start().starts_with("data:") {
        save_openrouter_compat_data_uri(image_url, output_dir, provider)
    } else {
        save_openrouter_compat_remote_image(client, image_url, output_dir, provider).await
    }
}

async fn save_provider_image_from_response(
    client: &Client,
    first: &Value,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    if let Some(b64) = first
        .get("b64_json")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let data_uri = format!("data:image/png;base64,{b64}");
        return save_openrouter_compat_generated_image(client, &data_uri, output_dir, provider)
            .await;
    }
    if let Some(url) = first
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return save_openrouter_compat_generated_image(client, url, output_dir, provider).await;
    }
    if let Some(public_url) = first
        .get("file_output")
        .and_then(|file| file.get("public_url"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return save_openrouter_compat_generated_image(client, public_url, output_dir, provider)
            .await;
    }
    Err(ToolError::ExecutionFailed(format!(
        "{provider} response contained neither b64_json nor URL"
    )))
}

fn save_openrouter_compat_data_uri(
    data_uri: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    let (header, encoded) = data_uri.split_once(',').ok_or_else(|| {
        ToolError::ExecutionFailed("Generated image data URI did not contain base64 data".into())
    })?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|value| value.split_once(';').map(|(mime, _)| mime))
        .filter(|value| !value.trim().is_empty());
    let bytes = STANDARD.decode(encoded.trim()).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Generated image data URI was not valid base64: {e}"
        ))
    })?;
    write_openrouter_compat_image_bytes(
        output_dir,
        provider,
        extension_for_mime(mime),
        bytes.as_slice(),
    )
}

async fn save_openrouter_compat_remote_image(
    client: &Client,
    image_url: &str,
    output_dir: &Path,
    provider: &str,
) -> Result<PathBuf, ToolError> {
    let resp = client.get(image_url).send().await.map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not download generated image {image_url}: {e}"
        ))
    })?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned);
    if !status.is_success() {
        return Err(ToolError::ExecutionFailed(format!(
            "Could not download generated image {image_url}: HTTP {status}"
        )));
    }
    let ext = url_extension(image_url)
        .unwrap_or_else(|| extension_for_mime(content_type.as_deref()).to_string());
    let bytes = resp.bytes().await.map_err(|e| {
        ToolError::ExecutionFailed(format!("Could not read generated image {image_url}: {e}"))
    })?;
    write_openrouter_compat_image_bytes(output_dir, provider, ext.as_str(), bytes.as_ref())
}

fn write_openrouter_compat_image_bytes(
    output_dir: &Path,
    provider: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf, ToolError> {
    std::fs::create_dir_all(output_dir).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not create image cache directory {}: {e}",
            output_dir.display()
        ))
    })?;
    let safe_provider = provider.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_");
    let safe_ext = ext
        .trim_start_matches('.')
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>();
    let safe_ext = if safe_ext.is_empty() {
        "png"
    } else {
        safe_ext.as_str()
    };
    let path = output_dir.join(format!(
        "{}_gen_{}.{}",
        safe_provider,
        uuid::Uuid::new_v4().simple(),
        safe_ext
    ));
    std::fs::write(&path, bytes).map_err(|e| {
        ToolError::ExecutionFailed(format!("Could not save image {}: {e}", path.display()))
    })?;
    Ok(path)
}

fn url_extension(raw_url: &str) -> Option<String> {
    let parsed = url::Url::parse(raw_url).ok()?;
    Path::new(parsed.path())
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.trim_start_matches('.').to_ascii_lowercase())
        .filter(|ext| {
            matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
            )
        })
}

fn read_provider_auth_string(provider: &str, keys: &[&str]) -> Option<String> {
    provider_auth_values(provider)
        .into_iter()
        .find_map(|value| {
            keys.iter().find_map(|key| {
                value
                    .get(*key)
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
}

fn read_provider_auth_tokens_string(provider: &str, key: &str) -> Option<String> {
    provider_auth_values(provider)
        .into_iter()
        .find_map(|value| {
            value
                .get("tokens")
                .and_then(|tokens| tokens.get(key))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
}

fn provider_auth_values(provider: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for path in provider_auth_candidate_paths(provider) {
        let Ok(raw) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(provider_value) = value
            .get("providers")
            .and_then(|providers| providers.get(provider))
            .cloned()
        {
            out.push(provider_value);
        } else if value.as_object().is_some() {
            out.push(value);
        }
    }
    out
}

fn provider_auth_candidate_paths(provider: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_optional_nonempty("HERMES_AUTH_FILE") {
        paths.push(PathBuf::from(path));
    }
    if provider == "nous" {
        if let Some(path) = env_optional_nonempty("HERMES_NOUS_OAUTH_FILE") {
            paths.push(PathBuf::from(path));
        }
        if let Some(home) = env_optional_nonempty("HOME") {
            paths.push(PathBuf::from(home).join(".hermes").join(".nous_oauth.json"));
        }
    }
    paths.push(hermes_config::paths::auth_json_path());
    dedup_paths(paths)
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !out.iter().any(|existing: &PathBuf| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn resolve_codex_image_tier() -> CodexImageTier {
    if let Some(value) = env_optional_nonempty("OPENAI_IMAGE_MODEL") {
        if let Some(tier) = codex_image_tier(&value) {
            return tier;
        }
    }
    if let Some(cfg) = load_image_gen_config() {
        if let Some(openai_codex) = yaml_get(&cfg, "openai-codex") {
            if let Some(value) = yaml_get_str(openai_codex, "model") {
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
    codex_image_tier(DEFAULT_CODEX_IMAGE_MODEL).expect("default Codex image model tier")
}

fn codex_image_tier(model: &str) -> Option<CodexImageTier> {
    match model.trim() {
        "gpt-image-2-low" => Some(CodexImageTier {
            id: "gpt-image-2-low",
            quality: "low",
        }),
        "gpt-image-2-medium" => Some(CodexImageTier {
            id: "gpt-image-2-medium",
            quality: "medium",
        }),
        "gpt-image-2-high" => Some(CodexImageTier {
            id: "gpt-image-2-high",
            quality: "high",
        }),
        _ => None,
    }
}

fn codex_image_size_from_tool_size(size: Option<&str>) -> &'static str {
    match size.map(str::trim) {
        Some("1536x1024") | Some("landscape") => "1536x1024",
        Some("1024x1536") | Some("portrait") => "1024x1536",
        _ => "1024x1024",
    }
}

fn codex_image_responses_payload(
    prompt: &str,
    size: &str,
    quality: &str,
    chat_model: &str,
) -> Value {
    json!({
        "model": chat_model,
        "store": false,
        "instructions": CODEX_IMAGE_INSTRUCTIONS,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{"type": "input_text", "text": prompt}],
        }],
        "tools": [{
            "type": "image_generation",
            "model": CODEX_IMAGE_API_MODEL,
            "size": size,
            "quality": quality,
            "output_format": "png",
            "background": "opaque",
            "partial_images": 1,
        }],
        "tool_choice": {
            "type": "allowed_tools",
            "mode": "required",
            "tools": [{"type": "image_generation"}],
        },
        "stream": true,
    })
}

fn collect_codex_image_b64_from_sse(raw: &str) -> Result<Option<String>, ToolError> {
    let mut event_name: Option<String> = None;
    let mut data_lines: Vec<String> = Vec::new();
    let mut latest: Option<String> = None;
    for line in raw.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            flush_codex_sse_event(&mut event_name, &mut data_lines, &mut latest)?;
            continue;
        }
        if line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    flush_codex_sse_event(&mut event_name, &mut data_lines, &mut latest)?;
    Ok(latest)
}

fn flush_codex_sse_event(
    event_name: &mut Option<String>,
    data_lines: &mut Vec<String>,
    latest: &mut Option<String>,
) -> Result<(), ToolError> {
    if data_lines.is_empty() {
        *event_name = None;
        return Ok(());
    }
    let raw = data_lines.join("\n").trim().to_string();
    let event = event_name.take();
    data_lines.clear();
    if raw.is_empty() || raw == "[DONE]" {
        return Ok(());
    }
    let mut payload: Value = serde_json::from_str(&raw).map_err(|e| {
        ToolError::ExecutionFailed(format!("Failed to parse Codex image SSE payload: {e}"))
    })?;
    if let (Some(event), Some(obj)) = (event, payload.as_object_mut()) {
        obj.entry("type".to_string())
            .or_insert(Value::String(event));
    }
    if let Some(found) = extract_codex_image_b64(&payload) {
        *latest = Some(found);
    }
    Ok(())
}

fn extract_codex_image_b64(value: &Value) -> Option<String> {
    match value {
        Value::Object(obj) => {
            let mut found = None;
            if obj.get("type").and_then(Value::as_str) == Some("image_generation_call") {
                if let Some(result) = obj
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    found = Some(result.to_string());
                }
            }
            if let Some(partial) = obj
                .get("partial_image_b64")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                found = Some(partial.to_string());
            }
            for child in obj.values() {
                if let Some(nested) = extract_codex_image_b64(child) {
                    found = Some(nested);
                }
            }
            found
        }
        Value::Array(items) => items.iter().filter_map(extract_codex_image_b64).next_back(),
        _ => None,
    }
}

fn save_codex_image_b64(
    image_b64: &str,
    output_dir: &Path,
    tier_id: &str,
) -> Result<PathBuf, ToolError> {
    std::fs::create_dir_all(output_dir).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not create Codex image cache directory {}: {e}",
            output_dir.display()
        ))
    })?;
    let encoded = image_b64
        .split_once(',')
        .map(|(_, data)| data)
        .unwrap_or(image_b64)
        .trim();
    let bytes = STANDARD.decode(encoded).map_err(|e| {
        ToolError::ExecutionFailed(format!("Codex image response was not valid base64: {e}"))
    })?;
    let safe_tier = tier_id.replace(|ch: char| !ch.is_ascii_alphanumeric(), "_");
    let path = output_dir.join(format!(
        "openai_codex_{}_{}.png",
        safe_tier,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::write(&path, bytes).map_err(|e| {
        ToolError::ExecutionFailed(format!(
            "Could not save Codex image {}: {e}",
            path.display()
        ))
    })?;
    Ok(path)
}

fn codex_image_auth_from_env_or_store() -> CodexImageAuth {
    let env_token = env_optional_nonempty("HERMES_OPENAI_CODEX_API_KEY")
        .or_else(|| env_optional_nonempty("OPENAI_CODEX_ACCESS_TOKEN"))
        .or_else(|| env_optional_nonempty("CODEX_ACCESS_TOKEN"));
    if let Some(access_token) = env_token {
        return CodexImageAuth {
            access_token: Some(access_token),
            base_url: env_optional_nonempty("HERMES_OPENAI_CODEX_BASE_URL")
                .or_else(|| env_optional_nonempty("OPENAI_CODEX_BASE_URL")),
        };
    }
    for path in codex_auth_store_candidate_paths() {
        if let Some(auth) = codex_image_auth_from_store_path(&path) {
            return auth;
        }
    }
    CodexImageAuth::default()
}

fn codex_auth_store_candidate_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(path) = env_optional_nonempty("HERMES_AUTH_FILE") {
        paths.push(PathBuf::from(path));
    }
    paths.push(hermes_config::paths::auth_json_path());
    paths
}

fn codex_image_auth_from_store_path(path: &Path) -> Option<CodexImageAuth> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    let state = parsed.get("providers")?.get("openai-codex")?;
    let token = state
        .get("tokens")
        .and_then(|tokens| tokens.get("access_token"))
        .or_else(|| state.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    let base_url = state
        .get("base_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned);
    Some(CodexImageAuth {
        access_token: Some(token),
        base_url,
    })
}

fn codex_cloudflare_headers(access_token: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![
        (
            "originator".to_string(),
            CODEX_CLOUDFLARE_ORIGINATOR.to_string(),
        ),
        (
            "User-Agent".to_string(),
            format!(
                "{CODEX_CLOUDFLARE_ORIGINATOR}/{}",
                env!("CARGO_PKG_VERSION")
            ),
        ),
    ];
    if let Some(account_id) = access_token.and_then(codex_chatgpt_account_id) {
        headers.push(("ChatGPT-Account-ID".to_string(), account_id));
    }
    headers
}

fn codex_chatgpt_account_id(token: &str) -> Option<String> {
    let payload = token.trim().split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD
        .decode(payload.as_bytes())
        .or_else(|_| URL_SAFE.decode(payload.as_bytes()))
        .ok()?;
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    claims
        .get("https://api.openai.com/auth")
        .and_then(|auth| auth.get("chatgpt_account_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToOwned::to_owned)
}
