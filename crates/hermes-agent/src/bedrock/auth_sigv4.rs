pub fn classify_bedrock_error(message: &str) -> BedrockErrorClass {
    let lower = message.to_ascii_lowercase();
    if lower.contains("input is too long")
        || lower.contains("exceeds the maximum number of input tokens")
        || lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("too many tokens")
    {
        BedrockErrorClass::ContextOverflow
    } else if lower.contains("throttlingexception")
        || lower.contains("too many concurrent requests")
        || lower.contains("too many requests")
        || lower.contains("rate exceeded")
        || lower.contains("rate limit")
    {
        BedrockErrorClass::RateLimit
    } else if lower.contains("modelnotreadyexception")
        || lower.contains("modeltimeoutexception")
        || lower.contains("serviceunavailable")
        || lower.contains("temporarily unavailable")
        || lower.contains("overloaded")
    {
        BedrockErrorClass::Overloaded
    } else {
        BedrockErrorClass::Unknown
    }
}

fn map_bedrock_error(status: u16, body: &str) -> AgentError {
    let lower = body.to_ascii_lowercase();
    if status == 401
        || status == 403
        || lower.contains("unauthorized")
        || lower.contains("accessdenied")
        || lower.contains("invalidsignature")
    {
        AgentError::AuthFailed(format!("Bedrock authorization failed: {body}"))
    } else {
        match classify_bedrock_error(body) {
            BedrockErrorClass::ContextOverflow => AgentError::ContextTooLong,
            BedrockErrorClass::RateLimit => AgentError::RateLimited {
                retry_after_secs: None,
            },
            BedrockErrorClass::Overloaded => {
                AgentError::LlmApi(format!("Bedrock model overloaded: {body}"))
            }
            BedrockErrorClass::Unknown if status == 429 => AgentError::RateLimited {
                retry_after_secs: None,
            },
            BedrockErrorClass::Unknown => {
                AgentError::LlmApi(format!("Bedrock API error {status}: {body}"))
            }
        }
    }
}

fn resolve_bedrock_auth() -> Option<BedrockAuth> {
    std::env::var("AWS_BEARER_TOKEN_BEDROCK")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(BedrockAuth::Bearer)
        .or_else(|| resolve_env_credentials().map(BedrockAuth::SigV4))
        .or_else(|| resolve_shared_credentials().map(BedrockAuth::SigV4))
}

fn resolve_env_credentials() -> Option<AwsCredentials> {
    let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())?;
    let session_token = std::env::var("AWS_SESSION_TOKEN")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Some(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

fn resolve_shared_credentials() -> Option<AwsCredentials> {
    let path = aws_shared_credentials_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let profile = aws_profile_name();
    let values = parse_ini_section(&raw, &profile);
    let access_key_id = values.get("aws_access_key_id")?.trim().to_string();
    let secret_access_key = values.get("aws_secret_access_key")?.trim().to_string();
    if access_key_id.is_empty() || secret_access_key.is_empty() {
        return None;
    }
    let session_token = values
        .get("aws_session_token")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    Some(AwsCredentials {
        access_key_id,
        secret_access_key,
        session_token,
    })
}

fn resolve_region_from_aws_config() -> Option<String> {
    let path = aws_config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let profile = aws_profile_name();
    parse_ini_section(&raw, &profile)
        .get("region")
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn aws_profile_name() -> String {
    std::env::var("AWS_PROFILE")
        .or_else(|_| std::env::var("AWS_DEFAULT_PROFILE"))
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

fn aws_shared_credentials_path() -> Option<PathBuf> {
    std::env::var("AWS_SHARED_CREDENTIALS_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".aws").join("credentials")))
}

fn aws_config_path() -> Option<PathBuf> {
    std::env::var("AWS_CONFIG_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".aws").join("config")))
}

fn parse_ini_section(raw: &str, profile: &str) -> HashMap<String, String> {
    let mut current_matches = false;
    let mut out = HashMap::new();
    let profile_section = if profile == "default" {
        "default".to_string()
    } else {
        format!("profile {profile}")
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            let section = trimmed.trim_start_matches('[').trim_end_matches(']').trim();
            current_matches = section == profile || section == profile_section;
            continue;
        }
        if !current_matches {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            out.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

#[derive(Clone, Copy)]
struct BedrockHeaderRequest<'a> {
    method: &'a str,
    url: &'a str,
    region: &'a str,
    service: &'a str,
    body: &'a [u8],
    anthropic_beta: Option<&'a str>,
    now: DateTime<Utc>,
}

fn bedrock_request_headers(
    request: BedrockHeaderRequest<'_>,
    auth: &BedrockAuth,
) -> Result<BTreeMap<String, String>, AgentError> {
    let mut headers = BTreeMap::new();
    headers.insert("accept".to_string(), "application/json".to_string());
    if request.method != "GET" {
        headers.insert("content-type".to_string(), "application/json".to_string());
    }
    if let Some(beta) = request
        .anthropic_beta
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        headers.insert("anthropic-beta".to_string(), beta.to_string());
    }
    match auth {
        BedrockAuth::Bearer(token) => {
            headers.insert("authorization".to_string(), format!("Bearer {token}"));
            Ok(headers)
        }
        BedrockAuth::SigV4(credentials) => sign_sigv4_headers(request, credentials, headers),
    }
}

fn sign_sigv4_headers(
    request: BedrockHeaderRequest<'_>,
    credentials: &AwsCredentials,
    mut headers: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, AgentError> {
    let url = reqwest::Url::parse(request.url)
        .map_err(|err| AgentError::Config(format!("invalid Bedrock URL for SigV4: {err}")))?;
    let host = url
        .host_str()
        .ok_or_else(|| AgentError::Config("Bedrock SigV4 URL missing host".to_string()))?;
    let amz_date = request.now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = request.now.format("%Y%m%d").to_string();
    let payload_hash = hex::encode(Sha256::digest(request.body));

    headers.insert("host".to_string(), host.to_string());
    headers.insert("x-amz-date".to_string(), amz_date.clone());
    headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
    if let Some(token) = credentials
        .session_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        headers.insert("x-amz-security-token".to_string(), token.to_string());
    }

    let canonical_headers = headers
        .iter()
        .map(|(key, value)| format!("{}:{}\n", key.to_ascii_lowercase(), collapse_spaces(value)))
        .collect::<String>();
    let signed_headers = headers
        .keys()
        .map(|key| key.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(";");
    let canonical_query = canonical_query_string(&url);
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        request.method.to_ascii_uppercase(),
        canonical_uri(&url),
        canonical_query,
        canonical_headers,
        signed_headers,
        payload_hash
    );
    let scope = format!(
        "{}/{}/{}/aws4_request",
        short_date,
        normalized_region_or_default(request.region),
        request.service
    );
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        amz_date,
        scope,
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let signing_key = sigv4_signing_key(
        credentials.secret_access_key.as_bytes(),
        short_date.as_bytes(),
        normalized_region_or_default(request.region).as_bytes(),
        request.service.as_bytes(),
    )?;
    let signature = hmac_sha256_hex(&signing_key, string_to_sign.as_bytes())?;
    headers.insert(
        "authorization".to_string(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            credentials.access_key_id, scope, signed_headers, signature
        ),
    );
    Ok(headers)
}

fn sigv4_signing_key(
    secret: &[u8],
    date: &[u8],
    region: &[u8],
    service: &[u8],
) -> Result<Vec<u8>, AgentError> {
    let k_secret = [b"AWS4".as_slice(), secret].concat();
    let k_date = hmac_sha256(&k_secret, date)?;
    let k_region = hmac_sha256(&k_date, region)?;
    let k_service = hmac_sha256(&k_region, service)?;
    hmac_sha256(&k_service, b"aws4_request")
}

fn hmac_sha256(key: &[u8], value: &[u8]) -> Result<Vec<u8>, AgentError> {
    let mut mac = HmacSha256::new_from_slice(key)
        .map_err(|err| AgentError::Config(format!("SigV4 HMAC init failed: {err}")))?;
    mac.update(value);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn hmac_sha256_hex(key: &[u8], value: &[u8]) -> Result<String, AgentError> {
    Ok(hex::encode(hmac_sha256(key, value)?))
}

fn canonical_uri(url: &reqwest::Url) -> String {
    let path = url.path();
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn canonical_query_string(url: &reqwest::Url) -> String {
    let mut pairs = url
        .query_pairs()
        .map(|(key, value)| {
            format!(
                "{}={}",
                percent_encode_query_component(&key),
                percent_encode_query_component(&value)
            )
        })
        .collect::<Vec<_>>();
    pairs.sort();
    pairs.join("&")
}

fn collapse_spaces(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn normalized_region_or_default(region: &str) -> String {
    let trimmed = region.trim();
    if trimmed.is_empty() {
        BEDROCK_DEFAULT_REGION.to_string()
    } else {
        trimmed.to_string()
    }
}

fn anthropic_inference_profile_prefix(region: &str) -> &'static str {
    let region = normalized_region_or_default(region);
    if region.starts_with("eu-") {
        "eu"
    } else if matches!(
        region.as_str(),
        "ap-southeast-2" | "ap-southeast-4" | "ap-southeast-6"
    ) {
        "au"
    } else if matches!(region.as_str(), "ap-northeast-1" | "ap-northeast-3") {
        "jp"
    } else {
        "us"
    }
}

fn amazon_inference_profile_prefix(region: &str) -> &'static str {
    let region = normalized_region_or_default(region);
    if region.starts_with("eu-") {
        "eu"
    } else {
        "us"
    }
}

fn percent_encode_path_segment(input: &str) -> String {
    percent_encode_bytes(input.as_bytes(), false)
}

fn percent_encode_query_component(input: &str) -> String {
    percent_encode_bytes(input.as_bytes(), true)
}

fn percent_encode_bytes(input: &[u8], encode_tilde: bool) -> String {
    let mut out = String::new();
    for &byte in input {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.')
            || (!encode_tilde && byte == b'~');
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn truncate_json(value: &Value, max_chars: usize) -> String {
    let raw = value.to_string();
    if raw.chars().count() <= max_chars {
        raw
    } else {
        raw.chars().take(max_chars).collect::<String>() + "..."
    }
}
