pub async fn login_anthropic_oauth(
    options: AnthropicOAuthLoginOptions,
) -> Result<AnthropicOAuthState, AgentError> {
    let timeout = default_http_timeout_seconds(options.timeout_seconds, 20.0);
    let authorize_url = std::env::var("HERMES_ANTHROPIC_OAUTH_AUTHORIZE_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_AUTHORIZE_URL.to_string());
    let token_url = std::env::var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_TOKEN_URL.to_string());
    let client_id = std::env::var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| ANTHROPIC_OAUTH_CLIENT_ID.to_string());

    let (code_verifier, code_challenge) = build_oauth_pkce_pair();
    let mut auth_url = reqwest::Url::parse(&authorize_url)
        .map_err(|e| AgentError::Config(format!("invalid Anthropic authorize URL: {}", e)))?;
    {
        let mut pairs = auth_url.query_pairs_mut();
        pairs.append_pair("code", "true");
        pairs.append_pair("client_id", &client_id);
        pairs.append_pair("response_type", "code");
        pairs.append_pair("redirect_uri", ANTHROPIC_OAUTH_REDIRECT_URI);
        pairs.append_pair("scope", ANTHROPIC_OAUTH_SCOPE);
        pairs.append_pair("code_challenge", &code_challenge);
        pairs.append_pair("code_challenge_method", "S256");
        pairs.append_pair("state", &code_verifier);
    }
    let auth_url = auth_url.to_string();

    println!();
    println!("Authorize Hermes with Claude Pro/Max.");
    println!("Open this URL:");
    println!("  {}", auth_url);
    maybe_open_browser(&auth_url, options.open_browser);
    println!();
    println!("After authorizing, Claude will show a code. Paste it below.");
    let raw_input = prompt_line_blocking("Authorization code: ")?;
    if raw_input.trim().is_empty() {
        return Err(AgentError::AuthFailed(
            "No authorization code entered for Anthropic OAuth".into(),
        ));
    }
    let mut split = raw_input.splitn(2, '#');
    let code = split.next().unwrap_or("").trim().to_string();
    let state = split.next().unwrap_or("").trim().to_string();
    if code.is_empty() {
        return Err(AgentError::AuthFailed(
            "Anthropic OAuth authorization code is empty".into(),
        ));
    }

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout))
        .build()
        .map_err(|e| AgentError::Io(format!("build Anthropic OAuth client: {}", e)))?;
    let exchange_payload = serde_json::json!({
        "grant_type": "authorization_code",
        "client_id": client_id,
        "code": code,
        "state": state,
        "redirect_uri": ANTHROPIC_OAUTH_REDIRECT_URI,
        "code_verifier": code_verifier,
    });
    let response = client
        .post(token_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&exchange_payload)
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("Anthropic OAuth token exchange failed: {}", e))
        })?;
    let status = response.status();
    let body = response.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("Anthropic OAuth response read failed: {}", e))
    })?;
    if !status.is_success() {
        let detail = extract_error_message(&body).unwrap_or(body);
        return Err(AgentError::AuthFailed(format!(
            "Anthropic OAuth token exchange failed ({}): {}",
            status, detail
        )));
    }
    let payload: Value = serde_json::from_str(&body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid Anthropic OAuth response: {}", e)))?;
    let object = payload.as_object().ok_or_else(|| {
        AgentError::AuthFailed("Anthropic OAuth response is not a JSON object".into())
    })?;
    let access_token = object
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("Anthropic OAuth response missing access_token".into())
        })?
        .to_string();
    let refresh_token = object
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_at_ms = object
        .get("expires_in")
        .and_then(value_as_i64)
        .filter(|v| *v > 0)
        .map(|secs| Utc::now().timestamp_millis() + secs * 1000);
    Ok(AnthropicOAuthState {
        access_token,
        refresh_token,
        expires_at_ms,
    })
}

pub async fn get_anthropic_oauth_status() -> AnthropicOAuthStatus {
    let auth_state = match read_provider_auth_state("anthropic") {
        Ok(value) => value,
        Err(err) => {
            return AnthropicOAuthStatus {
                logged_in: false,
                source: None,
                api_key: None,
                expires_at_ms: None,
                error: Some(err.to_string()),
            };
        }
    };
    let Some(value) = auth_state else {
        return AnthropicOAuthStatus {
            logged_in: false,
            source: None,
            api_key: None,
            expires_at_ms: None,
            error: Some("not logged in".to_string()),
        };
    };
    let object = match value.as_object() {
        Some(v) => v,
        None => {
            return AnthropicOAuthStatus {
                logged_in: false,
                source: None,
                api_key: None,
                expires_at_ms: None,
                error: Some("invalid stored anthropic oauth state".to_string()),
            };
        }
    };
    let api_key = object
        .get("access_token")
        .or_else(|| object.get("api_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let source = object
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(Some("hermes_pkce".to_string()));
    let expires_at_ms = object
        .get("expires_at_ms")
        .or_else(|| object.get("expires"))
        .and_then(value_as_i64);
    let is_expired = expires_at_ms
        .map(|exp| {
            gemini_access_token_is_expiring(
                Some(exp),
                ANTHROPIC_OAUTH_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
            )
        })
        .unwrap_or(false);
    AnthropicOAuthStatus {
        logged_in: api_key.is_some() && !is_expired,
        source,
        api_key,
        expires_at_ms,
        error: None,
    }
}

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn extract_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let err = value.get("error").and_then(|v| v.as_str()).unwrap_or("");
    let desc = value
        .get("error_description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if err.is_empty() && desc.is_empty() {
        None
    } else if err.is_empty() {
        Some(desc.to_string())
    } else if desc.is_empty() {
        Some(err.to_string())
    } else {
        Some(format!("{err}: {desc}"))
    }
}

fn try_open_url(url: &str) -> Result<(), AgentError> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    cmd.arg(url);

    let status = cmd
        .status()
        .map_err(|e| AgentError::Io(format!("open browser command failed: {}", e)))?;
    if status.success() {
        Ok(())
    } else {
        Err(AgentError::Io(format!(
            "open browser command exited with status {}",
            status
        )))
    }
}

pub async fn login_nous_device_code(
    options: NousDeviceCodeOptions,
) -> Result<NousAuthState, AgentError> {
    let portal_base_url = options
        .portal_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| {
            env_or_default(
                "HERMES_PORTAL_BASE_URL",
                &env_or_default("NOUS_PORTAL_BASE_URL", DEFAULT_NOUS_PORTAL_URL),
            )
            .trim_end_matches('/')
            .to_string()
        });
    let requested_inference_base_url = options
        .inference_base_url
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .unwrap_or_else(|| env_or_default("NOUS_INFERENCE_BASE_URL", DEFAULT_NOUS_INFERENCE_URL));
    let client_id = options
        .client_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOUS_CLIENT_ID)
        .to_string();
    let scope = options
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_NOUS_SCOPE)
        .to_string();
    let timeout_secs = if options.timeout_seconds.is_finite() {
        options.timeout_seconds.clamp(5.0, 120.0)
    } else {
        15.0
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout_secs))
        .build()
        .map_err(|e| AgentError::Io(format!("build oauth client: {}", e)))?;

    println!("Starting Hermes login via Nous Portal...");
    println!("Portal: {}", portal_base_url);

    let mut device_form: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    device_form.insert("client_id".to_string(), client_id.clone());
    if !scope.is_empty() {
        device_form.insert("scope".to_string(), scope.clone());
    }

    let device_resp = client
        .post(format!("{portal_base_url}/api/oauth/device/code"))
        .form(&device_form)
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device code request failed: {}", e)))?;
    let device_status = device_resp.status();
    let device_body = device_resp
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("device code response read failed: {}", e)))?;
    if !device_status.is_success() {
        let detail = extract_error_message(&device_body).unwrap_or(device_body);
        return Err(AgentError::AuthFailed(format!(
            "Nous device code request failed ({}): {}",
            device_status, detail
        )));
    }
    let device_data: NousDeviceCodeResponse = serde_json::from_str(&device_body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid device code response: {}", e)))?;

    let device_code = device_data
        .device_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("device code response missing device_code".into()))?
        .to_string();
    let user_code = device_data
        .user_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("device code response missing user_code".into()))?
        .to_string();
    let verification_uri = device_data
        .verification_uri_complete
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            device_data
                .verification_uri
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or_else(|| {
            AgentError::AuthFailed("device code response missing verification_uri".into())
        })?
        .to_string();
    let expires_in = device_data.expires_in.unwrap_or(900).max(60) as u64;
    let mut poll_interval = (device_data.interval.unwrap_or(5).max(1) as u64)
        .min(NOUS_DEVICE_AUTH_POLL_INTERVAL_CAP_SECONDS);

    println!();
    println!("To continue:");
    println!("  1. Open: {}", verification_uri);
    println!("  2. If prompted, enter code: {}", user_code);
    println!("  3. Click Connect/Refresh in Nous Portal before the code expires.");
    if options.open_browser {
        match try_open_url(&verification_uri) {
            Ok(_) => println!("  (Opened browser for verification)"),
            Err(err) => println!("  Could not open browser automatically: {}", err),
        }
    }
    println!("Waiting for approval (polling every {}s)...", poll_interval);

    let deadline = Instant::now() + Duration::from_secs(expires_in);
    let token_payload = loop {
        if Instant::now() >= deadline {
            return Err(AgentError::AuthFailed(
                "timed out waiting for Nous device authorization".into(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;

        let mut token_form: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        token_form.insert(
            "grant_type".to_string(),
            "urn:ietf:params:oauth:grant-type:device_code".to_string(),
        );
        token_form.insert("client_id".to_string(), client_id.clone());
        token_form.insert("device_code".to_string(), device_code.clone());

        let token_resp = client
            .post(format!("{portal_base_url}/api/oauth/token"))
            .form(&token_form)
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("token poll request failed: {}", e)))?;
        let status = token_resp.status();
        let body = token_resp.text().await.map_err(|e| {
            AgentError::AuthFailed(format!("token poll response read failed: {}", e))
        })?;
        if status.is_success() {
            let payload: NousTokenResponse = serde_json::from_str(&body)
                .map_err(|e| AgentError::AuthFailed(format!("invalid token response: {}", e)))?;
            let has_access_token = payload
                .access_token
                .as_deref()
                .map(str::trim)
                .is_some_and(|s| !s.is_empty());
            if !has_access_token {
                return Err(AgentError::AuthFailed(
                    "token response missing access_token".into(),
                ));
            }
            break payload;
        }
        let payload: NousTokenResponse = serde_json::from_str(&body).unwrap_or(NousTokenResponse {
            access_token: None,
            refresh_token: None,
            token_type: None,
            scope: None,
            expires_in: None,
            inference_base_url: None,
            error: None,
            error_description: extract_error_message(&body),
        });
        match payload.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                poll_interval = (poll_interval + 1).min(30);
                continue;
            }
            _ => {
                let detail = payload
                    .error_description
                    .or(payload.error)
                    .unwrap_or_else(|| format!("status {}: {}", status, body));
                return Err(AgentError::AuthFailed(format!(
                    "Nous token exchange failed: {}",
                    detail
                )));
            }
        }
    };

    let access_token = token_payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("token response missing access_token".into()))?
        .to_string();
    let access_expires_in = token_payload.expires_in.filter(|v| *v > 0);
    let now = Utc::now();
    let access_expires_at = access_expires_in.map(|secs| {
        (now + chrono::Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    let resolved_inference_url = token_payload
        .inference_base_url
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| {
            requested_inference_base_url
                .trim_end_matches('/')
                .to_string()
        });

    let mut state = NousAuthState {
        portal_base_url,
        inference_base_url: resolved_inference_url,
        client_id,
        scope: token_payload.scope.or(Some(scope)),
        token_type: token_payload
            .token_type
            .unwrap_or_else(|| "Bearer".to_string()),
        access_token,
        refresh_token: token_payload.refresh_token,
        obtained_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        expires_at: access_expires_at,
        expires_in: access_expires_in,
        agent_key: None,
        agent_key_id: None,
        agent_key_expires_at: None,
        agent_key_expires_in: None,
        agent_key_reused: None,
        agent_key_obtained_at: None,
    };
    assert_nous_invoke_jwt_usable(&state, None, NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS)?;
    set_nous_agent_key_from_invoke_jwt(&mut state);
    Ok(state)
}

pub async fn login_openai_codex_device_code(
    options: CodexDeviceCodeOptions,
) -> Result<CodexAuthState, AgentError> {
    let issuer = DEFAULT_CODEX_ISSUER;
    let timeout_secs = if options.timeout_seconds.is_finite() {
        options.timeout_seconds.clamp(5.0, 120.0)
    } else {
        15.0
    };
    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs_f64(timeout_secs))
        .build()
        .map_err(|e| AgentError::Io(format!("build oauth client: {}", e)))?;

    let usercode_resp = client
        .post(format!("{issuer}/api/accounts/deviceauth/usercode"))
        .json(&serde_json::json!({
            "client_id": CODEX_OAUTH_CLIENT_ID,
        }))
        .send()
        .await
        .map_err(|e| {
            AgentError::AuthFailed(format!("failed to request codex device code: {}", e))
        })?;
    let usercode_status = usercode_resp.status();
    let usercode_body = usercode_resp.text().await.map_err(|e| {
        AgentError::AuthFailed(format!("failed reading codex device code response: {}", e))
    })?;
    if !usercode_status.is_success() {
        let detail = extract_error_message(&usercode_body).unwrap_or(usercode_body);
        return Err(AgentError::AuthFailed(format!(
            "codex device code request failed ({}): {}",
            usercode_status, detail
        )));
    }
    let usercode_payload: CodexDeviceUserCodeResponse = serde_json::from_str(&usercode_body)
        .map_err(|e| {
            AgentError::AuthFailed(format!("invalid codex device code response: {}", e))
        })?;
    let user_code = usercode_payload
        .user_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex device code response missing user_code".into())
        })?
        .to_string();
    let device_auth_id = usercode_payload
        .device_auth_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex device code response missing device_auth_id".into())
        })?
        .to_string();
    let poll_interval = usercode_payload.interval.unwrap_or(5).max(1) as u64;

    let verify_url = format!("{issuer}/codex/device");
    println!("To continue, follow these steps:\n");
    println!("  1. Open this URL in your browser:");
    println!("     {}", verify_url);
    println!("\n  2. Enter this code:");
    println!("     {}", user_code);
    println!("\nWaiting for sign-in... (press Ctrl+C to cancel)");
    if options.open_browser {
        let _ = try_open_url(&verify_url);
    }

    let deadline = Instant::now() + Duration::from_secs(15 * 60);
    let mut code_payload: Option<CodexDevicePollResponse> = None;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(poll_interval)).await;
        let poll_resp = client
            .post(format!("{issuer}/api/accounts/deviceauth/token"))
            .json(&serde_json::json!({
                "device_auth_id": device_auth_id,
                "user_code": user_code,
            }))
            .send()
            .await
            .map_err(|e| AgentError::AuthFailed(format!("codex device poll failed: {}", e)))?;
        match poll_resp.status().as_u16() {
            200 => {
                let body = poll_resp.text().await.map_err(|e| {
                    AgentError::AuthFailed(format!("codex poll response read failed: {}", e))
                })?;
                let payload: CodexDevicePollResponse =
                    serde_json::from_str(&body).map_err(|e| {
                        AgentError::AuthFailed(format!("invalid codex poll response: {}", e))
                    })?;
                code_payload = Some(payload);
                break;
            }
            403 | 404 => continue,
            status => {
                return Err(AgentError::AuthFailed(format!(
                    "codex device poll failed with status {}",
                    status
                )));
            }
        }
    }
    let code_payload = code_payload
        .ok_or_else(|| AgentError::AuthFailed("codex device login timed out".into()))?;
    let authorization_code = code_payload
        .authorization_code
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AgentError::AuthFailed("codex poll response missing authorization_code".into())
        })?
        .to_string();
    let code_verifier = code_payload
        .code_verifier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("codex poll response missing code_verifier".into()))?
        .to_string();

    let token_resp = client
        .post(CODEX_OAUTH_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization_code.as_str()),
            (
                "redirect_uri",
                "https://auth.openai.com/deviceauth/callback",
            ),
            ("client_id", CODEX_OAUTH_CLIENT_ID),
            ("code_verifier", code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("codex token exchange failed: {}", e)))?;
    let token_status = token_resp.status();
    let token_body = token_resp
        .text()
        .await
        .map_err(|e| AgentError::AuthFailed(format!("codex token response read failed: {}", e)))?;
    if !token_status.is_success() {
        let detail = extract_error_message(&token_body).unwrap_or(token_body);
        return Err(AgentError::AuthFailed(format!(
            "codex token exchange failed ({}): {}",
            token_status, detail
        )));
    }
    let token_payload: CodexTokenResponse = serde_json::from_str(&token_body)
        .map_err(|e| AgentError::AuthFailed(format!("invalid codex token response: {}", e)))?;
    let access_token = token_payload
        .access_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AgentError::AuthFailed("codex token response missing access_token".into()))?
        .to_string();
    let refresh_token = token_payload
        .refresh_token
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    let base_url = std::env::var("HERMES_CODEX_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_BASE_URL.to_string());
    Ok(CodexAuthState {
        tokens: CodexTokens {
            access_token,
            refresh_token,
            expires_in: token_payload.expires_in,
        },
        base_url,
        last_refresh: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        auth_mode: Some("chatgpt".to_string()),
        source: Some("device_code".to_string()),
    })
}

pub async fn login_openai_device_code(
    options: CodexDeviceCodeOptions,
) -> Result<CodexAuthState, AgentError> {
    let mut state = login_openai_codex_device_code(options).await?;
    state.base_url = std::env::var("HERMES_OPENAI_OAUTH_BASE_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_CODEX_BASE_URL.to_string());
    state.auth_mode = Some("chatgpt".to_string());
    state.source = Some("device_code".to_string());
    Ok(state)
}

/// Human-readable line after a successful non-OAuth LLM login (API key stored in token store).
pub async fn login(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "LLM API key stored for provider '{}'.",
        provider.trim()
    ))
}

pub async fn logout(provider: &str) -> Result<String, AgentError> {
    Ok(format!(
        "Removed stored credential for provider '{}'.",
        provider.trim()
    ))
}
