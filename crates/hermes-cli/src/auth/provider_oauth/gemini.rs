fn gemini_cli_auth_path() -> PathBuf {
    if let Ok(path) = std::env::var("HERMES_GEMINI_OAUTH_FILE") {
        let trimmed = path.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    hermes_config::paths::hermes_home()
        .join("auth")
        .join("google_oauth.json")
}

fn parse_packed_gemini_refresh(
    raw_refresh: Option<&str>,
) -> (Option<String>, Option<String>, Option<String>) {
    let Some(raw) = raw_refresh.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None, None);
    };
    let mut parts = raw.splitn(3, '|');
    let refresh_token = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let project_id = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let managed_project_id = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    (refresh_token, project_id, managed_project_id)
}

fn pack_gemini_refresh(
    refresh_token: Option<&str>,
    project_id: Option<&str>,
    managed_project_id: Option<&str>,
) -> Option<String> {
    let refresh_token = refresh_token
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)?;
    let project_id = project_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let managed_project_id = managed_project_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if project_id.is_empty() && managed_project_id.is_empty() {
        Some(refresh_token)
    } else {
        Some(format!(
            "{}|{}|{}",
            refresh_token, project_id, managed_project_id
        ))
    }
}

fn read_gemini_cli_state() -> Result<GeminiOAuthFileState, AgentError> {
    let auth_path = gemini_cli_auth_path();
    if !auth_path.exists() {
        return Err(AgentError::AuthFailed(
            "Google OAuth credentials not found. Run `hermes auth google-gemini-cli` first.".into(),
        ));
    }
    let raw = std::fs::read_to_string(&auth_path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", auth_path.display(), e)))?;
    let payload: Value = serde_json::from_str(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", auth_path.display(), e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::Config(format!(
            "invalid Google OAuth credentials in {}",
            auth_path.display()
        ))
    })?;

    let packed_refresh = object
        .get("refresh")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let refresh_value = packed_refresh.as_deref().or_else(|| {
        object
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    });
    let (refresh_token, packed_project_id, packed_managed_project_id) =
        parse_packed_gemini_refresh(refresh_value);

    let project_id = object
        .get("project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(packed_project_id);
    let managed_project_id = object
        .get("managed_project_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(packed_managed_project_id);
    let packed = pack_gemini_refresh(
        refresh_token.as_deref(),
        project_id.as_deref(),
        managed_project_id.as_deref(),
    );
    let access = object
        .get("access")
        .or_else(|| object.get("access_token"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let email = object
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires = object
        .get("expires")
        .or_else(|| object.get("expires_at_ms"))
        .and_then(value_as_i64);

    Ok(GeminiOAuthFileState {
        refresh: packed,
        access,
        expires,
        email,
        project_id,
        managed_project_id,
    })
}

fn save_gemini_cli_state(state: &GeminiOAuthFileState) -> Result<PathBuf, AgentError> {
    let auth_path = gemini_cli_auth_path();
    let mut raw = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Config(format!("serialize Google OAuth credentials: {}", e)))?;
    raw.push('\n');
    write_owner_only_atomic(&auth_path, &raw)?;
    Ok(auth_path)
}

fn gemini_access_token_is_expiring(expiry_ms: Option<i64>, skew_seconds: i64) -> bool {
    let Some(expiry_ms) = expiry_ms else {
        return true;
    };
    let skew = skew_seconds.max(0);
    Utc::now().timestamp_millis() + skew.saturating_mul(1000) >= expiry_ms
}

fn default_http_timeout_seconds(timeout_seconds: f64, fallback: f64) -> f64 {
    if timeout_seconds.is_finite() {
        timeout_seconds.clamp(5.0, 120.0)
    } else {
        fallback
    }
}

fn default_gemini_client_id() -> String {
    format!(
        "{}-{}.apps.googleusercontent.com",
        DEFAULT_GEMINI_CLIENT_ID_PROJECT_NUM, DEFAULT_GEMINI_CLIENT_ID_HASH
    )
}

fn default_gemini_client_secret() -> String {
    format!("GOCSPX-{}", DEFAULT_GEMINI_CLIENT_SECRET_SUFFIX)
}

fn build_oauth_pkce_pair() -> (String, String) {
    let mut verifier_bytes = [0u8; 32];
    rand::fill(&mut verifier_bytes[..]);
    let verifier = BASE64_URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = BASE64_URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

fn build_oauth_state_token() -> String {
    let mut state_bytes = [0u8; 16];
    rand::fill(&mut state_bytes[..]);
    BASE64_URL_SAFE_NO_PAD.encode(state_bytes)
}

fn maybe_open_browser(url: &str, enabled: bool) {
    if !enabled {
        return;
    }
    match try_open_url(url) {
        Ok(_) => println!("  (Opened browser for authorization)"),
        Err(err) => println!("  Could not open browser automatically: {}", err),
    }
}

fn parse_code_from_manual_input(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        let parsed = reqwest::Url::parse(trimmed).ok()?;
        for (k, v) in parsed.query_pairs() {
            if k == "code" {
                let code = v.trim().to_string();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
        return None;
    }
    if let Some(query) = trimmed.strip_prefix('?') {
        let parsed = reqwest::Url::parse(&format!("http://localhost/?{}", query)).ok()?;
        for (k, v) in parsed.query_pairs() {
            if k == "code" {
                let code = v.trim().to_string();
                if !code.is_empty() {
                    return Some(code);
                }
            }
        }
        return None;
    }
    Some(trimmed.to_string())
}

fn prompt_line_blocking(prompt: &str) -> Result<String, AgentError> {
    print!("{}", prompt);
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    std::io::stdin()
        .read_line(&mut buf)
        .map_err(|e| AgentError::Io(format!("stdin: {}", e)))?;
    Ok(buf.trim().to_string())
}

fn respond_oauth_callback(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        status,
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn wait_for_gemini_oauth_callback(
    listener: &TcpListener,
    expected_state: &str,
    wait_secs: u64,
) -> Result<Option<String>, AgentError> {
    let listener = listener
        .try_clone()
        .map_err(|e| AgentError::Io(format!("clone OAuth callback listener: {}", e)))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| AgentError::Io(format!("set nonblocking callback listener: {}", e)))?;
    let deadline = Instant::now() + Duration::from_secs(wait_secs.max(1));
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0u8; 8192];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]);
                let request_line = request.lines().next().unwrap_or("");
                let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                let parsed = reqwest::Url::parse(&format!("http://localhost{}", path)).ok();
                let Some(url) = parsed else {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        "<html><body><h1>Invalid callback</h1></body></html>",
                    );
                    continue;
                };
                if url.path() != GEMINI_CALLBACK_PATH {
                    respond_oauth_callback(
                        &mut stream,
                        "404 Not Found",
                        "<html><body><h1>Not found</h1></body></html>",
                    );
                    continue;
                }

                let mut code = None;
                let mut state = None;
                let mut error = None;
                for (k, v) in url.query_pairs() {
                    if k == "code" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            code = Some(value);
                        }
                    } else if k == "state" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            state = Some(value);
                        }
                    } else if k == "error" {
                        let value = v.trim().to_string();
                        if !value.is_empty() {
                            error = Some(value);
                        }
                    }
                }
                if let Some(err) = error {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        &format!(
                            "<html><body><h1>Google sign-in failed</h1><p>{}</p></body></html>",
                            err
                        ),
                    );
                    return Err(AgentError::AuthFailed(format!(
                        "Google OAuth authorization failed: {}",
                        err
                    )));
                }
                if state.as_deref() != Some(expected_state) {
                    respond_oauth_callback(
                        &mut stream,
                        "400 Bad Request",
                        "<html><body><h1>State mismatch</h1></body></html>",
                    );
                    return Err(AgentError::AuthFailed(
                        "Google OAuth callback state mismatch".into(),
                    ));
                }
                if let Some(code) = code {
                    respond_oauth_callback(
                        &mut stream,
                        "200 OK",
                        "<html><body><h1>Signed in to Google</h1><p>You can close this tab and return to Hermes.</p></body></html>",
                    );
                    return Ok(Some(code));
                }
                respond_oauth_callback(
                    &mut stream,
                    "400 Bad Request",
                    "<html><body><h1>Missing authorization code</h1></body></html>",
                );
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => {
                return Err(AgentError::Io(format!(
                    "listen for Google OAuth callback failed: {}",
                    err
                )));
            }
        }
    }
    Ok(None)
}

async fn fetch_gemini_user_email(access_token: &str, timeout_seconds: f64) -> Option<String> {
    let timeout = default_http_timeout_seconds(timeout_seconds, 15.0);
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .ok()?;
    let response = client
        .get(format!("{}?alt=json", GEMINI_USERINFO_ENDPOINT))
        .bearer_auth(access_token)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    let payload: Value = serde_json::from_str(&body).ok()?;
    payload
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

async fn refresh_gemini_cli_state(
    state: &GeminiOAuthFileState,
    timeout_seconds: f64,
) -> Result<GeminiOAuthFileState, AgentError> {
    let (refresh_token, _, _) = parse_packed_gemini_refresh(state.refresh.as_deref());
    let refresh_token = refresh_token.ok_or_else(|| {
        AgentError::AuthFailed(
            "Google OAuth refresh token missing. Re-run `hermes auth google-gemini-cli`.".into(),
        )
    })?;
    let token_url = std::env::var("HERMES_GEMINI_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| GEMINI_TOKEN_ENDPOINT.to_string());
    let client_id = std::env::var("HERMES_GEMINI_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_id);
    let client_secret = std::env::var("HERMES_GEMINI_CLIENT_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_secret);

    let timeout = default_http_timeout_seconds(timeout_seconds, 20.0);
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Google OAuth client: {}", e)))?;

    let mut form: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    form.insert("grant_type".to_string(), "refresh_token".to_string());
    form.insert("refresh_token".to_string(), refresh_token.clone());
    form.insert("client_id".to_string(), client_id);
    if !client_secret.is_empty() {
        form.insert("client_secret".to_string(), client_secret);
    }
    let response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Google OAuth refresh failed: {}", e)))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("Google OAuth refresh read failed: {}", e)))?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Google OAuth refresh failed ({}): {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body).map_err(|e| {
        AgentError::AuthFailed(format!("Google OAuth refresh JSON parse failed: {}", e))
    })?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Google OAuth refresh response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Google OAuth refresh response missing access_token".into())
        })?
        .to_string();
    let refreshed_refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(refresh_token.as_str())
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(3600)
        .max(60);
    let email = if state.email.is_some() {
        state.email.clone()
    } else {
        fetch_gemini_user_email(&access_token, timeout).await
    };
    Ok(GeminiOAuthFileState {
        refresh: pack_gemini_refresh(
            Some(refreshed_refresh_token.as_str()),
            state.project_id.as_deref(),
            state.managed_project_id.as_deref(),
        ),
        access: Some(access_token),
        expires: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
        email,
        project_id: state.project_id.clone(),
        managed_project_id: state.managed_project_id.clone(),
    })
}

pub async fn resolve_gemini_oauth_runtime_credentials(
    force_refresh: bool,
) -> Result<GeminiRuntimeCredentials, AgentError> {
    let mut state = read_gemini_cli_state()?;
    if force_refresh
        || gemini_access_token_is_expiring(
            state.expires,
            GEMINI_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        )
    {
        state = refresh_gemini_cli_state(&state, 20.0).await?;
        let _ = save_gemini_cli_state(&state)?;
    }
    let api_key = state
        .access
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Google OAuth access token missing. Re-run `hermes auth google-gemini-cli`.".into(),
            )
        })?
        .to_string();
    Ok(GeminiRuntimeCredentials {
        provider: "google-gemini-cli".to_string(),
        base_url: DEFAULT_GEMINI_CLOUDCODE_BASE_URL.to_string(),
        api_key,
        source: "google-oauth".to_string(),
        expires_at_ms: state.expires,
        auth_file: gemini_cli_auth_path(),
        email: state.email,
        project_id: state.project_id,
        refresh_token: parse_packed_gemini_refresh(state.refresh.as_deref()).0,
    })
}

pub async fn get_gemini_oauth_auth_status() -> GeminiOAuthStatus {
    let auth_file = gemini_cli_auth_path();
    match read_gemini_cli_state() {
        Ok(state) => {
            let api_key = state
                .access
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            GeminiOAuthStatus {
                logged_in: api_key.is_some(),
                auth_file,
                source: Some("google-oauth".to_string()),
                api_key,
                expires_at_ms: state.expires,
                email: state.email,
                project_id: state.project_id,
                error: None,
            }
        }
        Err(err) => GeminiOAuthStatus {
            logged_in: false,
            auth_file,
            source: None,
            api_key: None,
            expires_at_ms: None,
            email: None,
            project_id: None,
            error: Some(err.to_string()),
        },
    }
}

fn resolve_gemini_project_id_from_env() -> Option<String> {
    for name in [
        "HERMES_GEMINI_PROJECT_ID",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_PROJECT_ID",
    ] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

pub async fn login_google_gemini_cli_oauth(
    options: GeminiOAuthLoginOptions,
) -> Result<GeminiRuntimeCredentials, AgentError> {
    let timeout = default_http_timeout_seconds(options.timeout_seconds, 20.0);
    let client_id = std::env::var("HERMES_GEMINI_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_id);
    let client_secret = std::env::var("HERMES_GEMINI_CLIENT_SECRET")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(default_gemini_client_secret);
    let token_url = std::env::var("HERMES_GEMINI_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| GEMINI_TOKEN_ENDPOINT.to_string());

    let (code_verifier, code_challenge) = build_oauth_pkce_pair();
    let state_token = build_oauth_state_token();
    let listener = TcpListener::bind((GEMINI_CALLBACK_HOST, GEMINI_CALLBACK_PORT))
        .or_else(|_| TcpListener::bind((GEMINI_CALLBACK_HOST, 0)))
        .map_err(|e| {
            AgentError::Io(format!("bind Google OAuth callback listener failed: {}", e))
        })?;
    let callback_port = listener
        .local_addr()
        .map_err(|e| AgentError::Io(format!("read callback listener addr failed: {}", e)))?
        .port();
    let redirect_uri = format!(
        "http://{}:{}{}",
        GEMINI_CALLBACK_HOST, callback_port, GEMINI_CALLBACK_PATH
    );

    let mut auth_url = reqwest::Url::parse(GEMINI_AUTH_ENDPOINT)
        .map_err(|e| AgentError::Config(format!("invalid Google OAuth authorize URL: {}", e)))?;
    {
        let mut pairs = auth_url.query_pairs_mut();
        pairs.append_pair("client_id", &client_id);
        pairs.append_pair("redirect_uri", &redirect_uri);
        pairs.append_pair("response_type", "code");
        pairs.append_pair("scope", GEMINI_OAUTH_SCOPE);
        pairs.append_pair("state", &state_token);
        pairs.append_pair("code_challenge", &code_challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("access_type", "offline");
        pairs.append_pair("prompt", "consent");
    }
    let auth_url = auth_url.to_string();

    println!();
    println!("Authorize Hermes with Google (Gemini CLI OAuth).");
    println!("Open this URL:");
    println!("  {}", auth_url);
    maybe_open_browser(&auth_url, options.open_browser);

    let code =
        match wait_for_gemini_oauth_callback(&listener, &state_token, GEMINI_CALLBACK_WAIT_SECS)? {
            Some(code) => code,
            None => {
                println!();
                println!(
                    "OAuth callback timed out. Paste the full callback URL or the code value:"
                );
                let raw = prompt_line_blocking("Callback URL or code: ")?;
                parse_code_from_manual_input(&raw).ok_or_else(|| {
                    AgentError::AuthFailed(
                        "No Google OAuth authorization code provided. Aborting.".into(),
                    )
                })?
            }
        };

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Google OAuth client: {}", e)))?;
    let mut form: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    form.insert("grant_type".to_string(), "authorization_code".to_string());
    form.insert("code".to_string(), code.clone());
    form.insert("code_verifier".to_string(), code_verifier.clone());
    form.insert("client_id".to_string(), client_id.clone());
    form.insert("redirect_uri".to_string(), redirect_uri.clone());
    if !client_secret.is_empty() {
        form.insert("client_secret".to_string(), client_secret.clone());
    }
    let token_response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("Google OAuth token exchange failed: {}", e))
        })?;
    let token_status = token_response.status();
    let token_body = token_response.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("Google OAuth token response read failed: {}", e))
    })?;
    if !token_status.is_success() {
        let detail = extract_error_message(&token_body).unwrap_or(token_body);
        return Err(AgentError::AuthFailed(format!(
            "Google OAuth token exchange failed ({}): {}",
            token_status, detail
        )));
    }
    let token_payload: Value = serde_json::from_str(&token_body).map_err(|e| {
        AgentError::AuthFailed(format!("invalid Google OAuth token response: {}", e))
    })?;
    let object = token_payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Google OAuth token response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Google OAuth token response missing access_token".into())
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed(
                "Google OAuth token response missing refresh_token; re-run login and grant offline access."
                    .into(),
            )
        })?
        .to_string();
    let expires_in_seconds = object
        .get("expires_in")
        .and_then(value_as_i64)
        .unwrap_or(3600)
        .max(60);
    let email = fetch_gemini_user_email(&access_token, timeout).await;
    let project_id = resolve_gemini_project_id_from_env();
    let state = GeminiOAuthFileState {
        refresh: pack_gemini_refresh(Some(&refresh_token), project_id.as_deref(), None),
        access: Some(access_token.clone()),
        expires: Some(Utc::now().timestamp_millis() + expires_in_seconds * 1000),
        email: email.clone(),
        project_id: project_id.clone(),
        managed_project_id: None,
    };
    let auth_file = save_gemini_cli_state(&state)?;
    Ok(GeminiRuntimeCredentials {
        provider: "google-gemini-cli".to_string(),
        base_url: DEFAULT_GEMINI_CLOUDCODE_BASE_URL.to_string(),
        api_key: access_token,
        source: "google-oauth".to_string(),
        expires_at_ms: state.expires,
        auth_file,
        email,
        project_id,
        refresh_token: Some(refresh_token),
    })
}

