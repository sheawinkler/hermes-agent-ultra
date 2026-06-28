fn qwen_cli_auth_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_QWEN_CLI_AUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".qwen")
        .join("oauth_creds.json")
}

fn value_as_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|v| i64::try_from(v).ok()))
        .or_else(|| value.as_str().and_then(|v| v.trim().parse::<i64>().ok()))
}

fn read_qwen_cli_tokens() -> Result<QwenCliTokens, AgentError> {
    let auth_path = qwen_cli_auth_path();
    if !auth_path.exists() {
        return Err(AgentError::AuthFailed(
            "Qwen CLI credentials not found. Run `qwen auth qwen-oauth` first.".into(),
        ));
    }
    let raw = std::fs::read_to_string(&auth_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", auth_path.display(), e)))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", auth_path.display(), e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::Config(format!(
            "invalid Qwen CLI credentials in {}",
            auth_path.display()
        ))
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(format!(
                "Qwen OAuth access_token missing in {}",
                auth_path.display()
            ))
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Bearer")
        .to_string();
    let resource_url = object
        .get("resource_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("portal.qwen.ai")
        .to_string();
    let expiry_date = object.get("expiry_date").and_then(value_as_i64);
    Ok(QwenCliTokens {
        access_token,
        refresh_token,
        token_type,
        resource_url,
        expiry_date,
    })
}

fn save_qwen_cli_tokens(tokens: &QwenCliTokens) -> Result<PathBuf, AgentError> {
    let auth_path = qwen_cli_auth_path();
    let mut raw = serde_json::to_string_pretty(tokens)
        .map_err(|e| AgentError::Config(format!("serialize Qwen tokens: {}", e)))?;
    raw.push('\n');
    write_owner_only_atomic(&auth_path, &raw)?;
    Ok(auth_path)
}

fn qwen_access_token_is_expiring(expiry_date_ms: Option<i64>, skew_seconds: i64) -> bool {
    let Some(expiry_ms) = expiry_date_ms else {
        return true;
    };
    let skew = skew_seconds.max(0);
    Utc::now().timestamp_millis() + skew.saturating_mul(1000) >= expiry_ms
}

async fn refresh_qwen_cli_tokens(
    tokens: &QwenCliTokens,
    timeout_seconds: f64,
) -> Result<QwenCliTokens, AgentError> {
    let refresh_token = tokens
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Qwen OAuth refresh token missing. Re-run `qwen auth qwen-oauth`.".into(),
            )
        })?
        .to_string();
    let token_url = std::env::var("HERMES_QWEN_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| QWEN_OAUTH_TOKEN_URL.to_string());
    let client_id = std::env::var("HERMES_QWEN_OAUTH_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| QWEN_OAUTH_CLIENT_ID.to_string());
    let timeout = if timeout_seconds.is_finite() {
        timeout_seconds.clamp(5.0, 120.0)
    } else {
        20.0
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build qwen oauth client: {}", e)))?;
    let response = client
        .post(&token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Qwen OAuth refresh failed: {}", e)))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Qwen OAuth refresh read failed: {}", e)))?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Qwen OAuth refresh failed ({}). Re-run `qwen auth qwen-oauth`. {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|e| {
        AgentError::AuthFailed(format!("Qwen OAuth refresh JSON parse failed: {}", e))
    })?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Qwen OAuth refresh response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Qwen OAuth refresh response missing access_token".into())
        })?
        .to_string();
    let refreshed_refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or(refresh_token);
    let token_type = object
        .get("token_type")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(tokens.token_type.as_str())
        .to_string();
    let resource_url = object
        .get("resource_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(tokens.resource_url.as_str())
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(6 * 60 * 60)
        .max(1);
    let refreshed = QwenCliTokens {
        access_token,
        refresh_token: Some(refreshed_refresh_token),
        token_type,
        resource_url,
        expiry_date: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
    };
    let _ = save_qwen_cli_tokens(&refreshed)?;
    Ok(refreshed)
}

pub async fn resolve_qwen_runtime_credentials(
    force_refresh: bool,
    refresh_if_expiring: bool,
    refresh_skew_seconds: i64,
) -> Result<QwenRuntimeCredentials, AgentError> {
    let mut tokens = read_qwen_cli_tokens()?;
    let should_refresh = force_refresh
        || (refresh_if_expiring
            && qwen_access_token_is_expiring(tokens.expiry_date, refresh_skew_seconds));
    if should_refresh {
        tokens = refresh_qwen_cli_tokens(&tokens, 20.0).await?;
    }
    if tokens.access_token.trim().is_empty() {
        return Err(AgentError::AuthFailed(
            "Qwen OAuth access token missing. Re-run `qwen auth qwen-oauth`.".into(),
        ));
    }
    let base_url = std::env::var("HERMES_QWEN_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_QWEN_BASE_URL.to_string());
    Ok(QwenRuntimeCredentials {
        provider: "qwen-oauth".to_string(),
        base_url,
        api_key: tokens.access_token.clone(),
        source: "qwen-cli".to_string(),
        expires_at_ms: tokens.expiry_date,
        auth_file: qwen_cli_auth_path(),
        refresh_token: tokens.refresh_token.clone(),
        token_type: tokens.token_type.clone(),
        tokens,
    })
}

pub async fn get_qwen_auth_status() -> QwenAuthStatus {
    let auth_file = qwen_cli_auth_path();
    match resolve_qwen_runtime_credentials(false, false, QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)
        .await
    {
        Ok(creds) => QwenAuthStatus {
            logged_in: true,
            auth_file,
            source: Some(creds.source),
            api_key: Some(creds.api_key),
            expires_at_ms: creds.expires_at_ms,
            error: None,
        },
        Err(err) => QwenAuthStatus {
            logged_in: false,
            auth_file,
            source: None,
            api_key: None,
            expires_at_ms: None,
            error: Some(err.to_string()),
        },
    }
}

