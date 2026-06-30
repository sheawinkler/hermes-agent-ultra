async fn print_auth_status_matrix(cli: &Cli, manager: &AuthManager) -> Result<(), AgentError> {
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let disk = load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    println!("Auth status matrix:");
    println!("-------------------");

    let mut llm_providers = hermes_cli::providers::known_providers();
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
enum AuthVerifyOutcome {
    Valid,
    ValidRefreshed,
    Unverified,
    Missing,
    Expired,
    RefreshFailed,
}

impl AuthVerifyOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::ValidRefreshed => "valid_refreshed",
            Self::Unverified => "unverified",
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::RefreshFailed => "refresh_failed",
        }
    }

    fn is_success(self) -> bool {
        matches!(self, Self::Valid | Self::ValidRefreshed | Self::Unverified)
    }
}

#[derive(Debug, Clone)]
struct AuthVerifyResult {
    provider: String,
    outcome: AuthVerifyOutcome,
    source: String,
    credential_present: bool,
    oauth_state_present: bool,
    expires_at: Option<String>,
    detail: Option<String>,
}

fn auth_verify_source(env_present: bool, store_present: bool, auth_state_present: bool) -> String {
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

fn oauth_refresh_config_for_provider(provider: &str) -> Option<(String, String)> {
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

async fn refresh_oauth_store_credential(
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
                "OAuth refresh token missing for provider '{}'",
                provider
            ))
        })?;
    let (token_url, client_id) = oauth_refresh_config_for_provider(provider).ok_or_else(|| {
        AgentError::AuthFailed(format!(
            "OAuth refresh not configured for provider '{}'",
            provider
        ))
    })?;
    let endpoints = OAuth2Endpoints {
        authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
        token_url,
        client_id,
        redirect_uri: "http://127.0.0.1/oauth/callback-unused".to_string(),
        scopes: Vec::new(),
        client_secret: None,
        client_auth_method: hermes_auth::OAuth2ClientAuthMethod::default(),
    };
    let mut refreshed = exchange_refresh_token(provider, &endpoints, refresh_token).await?;
    refreshed.provider = provider.to_string();
    Ok(refreshed)
}

async fn ensure_openai_oauth_credential(
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
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
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

fn print_auth_verify_result(result: &AuthVerifyResult) {
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

fn nous_auth_error_requires_fresh_login(err: &AgentError) -> bool {
    let text = err.to_string().to_ascii_lowercase();
    text.contains("invalid_grant")
        || text.contains("refresh token reuse")
        || text.contains("refresh session has been revoked")
        || text.contains("session has been revoked")
        || text.contains("stored nous auth state is invalid")
        || text.contains("missing refresh token")
        || text.contains("no refresh token")
}

async fn save_nous_runtime_credential(
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

async fn verify_nous_runtime_credentials_live(
    resolved: &NousRuntimeCredentials,
) -> Result<(), String> {
    let base_url = resolved.base_url.trim().trim_end_matches('/');
    if base_url.is_empty() {
        return Err("live_probe_failed: missing inference base URL".to_string());
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|err| format!("live_probe_failed: build client: {err}"))?;
    let model = std::env::var("HERMES_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "nous:openai/gpt-5.5".to_string());
    let model = model
        .strip_prefix("nous:")
        .unwrap_or(model.as_str())
        .to_string();
    let payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "ping"}],
        "max_tokens": 1,
        "temperature": 0
    });
    let response = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(resolved.api_key.trim())
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&payload)
        .send()
        .await
        .map_err(|err| format!("live_probe_failed: request failed: {err}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    let detail = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("message")
                .or_else(|| value.get("error_description"))
                .or_else(|| value.get("error"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| body.trim().chars().take(240).collect::<String>());
    if detail.is_empty() {
        Err(format!("live_probe_failed: HTTP {status}"))
    } else {
        Err(format!("live_probe_failed: HTTP {status}: {detail}"))
    }
}

async fn fresh_nous_login_and_save(
    manager: &AuthManager,
) -> Result<(NousRuntimeCredentials, std::path::PathBuf, NousAuthState), AgentError> {
    let _ = clear_provider_auth_state("nous")?;
    let state = login_nous_device_code(NousDeviceCodeOptions::default()).await?;
    let auth_path = save_nous_auth_state(&state)?;
    let resolved = resolve_nous_runtime_credentials(
        false,
        true,
        NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
        DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
    )
    .await?;
    save_nous_runtime_credential(manager, &resolved).await?;
    Ok((resolved, auth_path, state))
}

async fn resolve_or_fresh_login_nous(
    manager: &AuthManager,
    use_existing: bool,
) -> Result<
    (
        NousRuntimeCredentials,
        std::path::PathBuf,
        bool,
        NousAuthState,
    ),
    AgentError,
> {
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

async fn verify_single_oauth_provider(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<AuthVerifyResult, AgentError> {
    let provider = normalize_auth_provider(provider);
    let env_present = provider_api_key_from_env(&provider).is_some();
    let auth_state_present = if provider == "nous" {
        read_nous_auth_state()?.is_some()
    } else {
        read_provider_auth_state(&provider)?.is_some()
    };
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
                if let Err(detail) = verify_nous_runtime_credentials_live(&creds).await {
                    result.outcome = AuthVerifyOutcome::RefreshFailed;
                    result.source = creds.source;
                    result.expires_at = creds.expires_at;
                    result.credential_present = true;
                    result.detail = Some(detail);
                    return Ok(result);
                }
                let source = creds.source.clone();
                let expires_at = creds.expires_at.clone();
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
                result.outcome = if source == "portal" {
                    AuthVerifyOutcome::ValidRefreshed
                } else {
                    AuthVerifyOutcome::Valid
                };
                result.source = source;
                result.expires_at = expires_at;
                result.credential_present = true;
                return Ok(result);
            }
            Err(err) => {
                if let Some(credential) = stored_credential.as_ref() {
                    let expires_at = credential.expires_at.as_ref().map(|dt| dt.to_rfc3339());
                    if let Ok(state) = nous_auth_state_from_runtime_token(
                        &credential.access_token,
                        credential.refresh_token.clone(),
                        Some(credential.token_type.as_str()),
                        credential.scope.clone(),
                        expires_at,
                    ) {
                        let _ = save_nous_auth_state(&state)?;
                        if let Ok(creds) = resolve_nous_runtime_credentials(
                            false,
                            true,
                            NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                            DEFAULT_NOUS_AGENT_KEY_MIN_TTL_SECONDS,
                        )
                        .await
                        {
                            if let Err(detail) = verify_nous_runtime_credentials_live(&creds).await
                            {
                                let _ = clear_provider_auth_state("nous");
                                result.outcome = AuthVerifyOutcome::RefreshFailed;
                                result.source = "vault_invoke_jwt".to_string();
                                result.expires_at = creds.expires_at;
                                result.credential_present = true;
                                result.detail = Some(detail);
                                return Ok(result);
                            }
                            let expires_at_text = creds.expires_at.clone();
                            manager
                                .save_credential(OAuthCredential {
                                    provider: "nous".to_string(),
                                    access_token: creds.api_key.clone(),
                                    refresh_token: creds.refresh_token.clone(),
                                    token_type: creds.token_type.clone(),
                                    scope: creds.scope.clone(),
                                    expires_at: parse_rfc3339_utc(expires_at_text.as_deref()),
                                })
                                .await?;
                            result.outcome = AuthVerifyOutcome::ValidRefreshed;
                            result.source = "vault_invoke_jwt".to_string();
                            result.expires_at = expires_at_text;
                            result.credential_present = true;
                            return Ok(result);
                        }
                        let _ = clear_provider_auth_state("nous");
                    }
                }
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
                    .and_then(chrono::DateTime::from_timestamp_millis)
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
                    .and_then(chrono::DateTime::from_timestamp_millis)
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
                    .and_then(serde_json::Value::as_str)
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
                    .and_then(chrono::DateTime::from_timestamp_millis)
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
                let expired = chrono::Utc::now().timestamp_millis() >= expires_ms;
                if expired {
                    result.outcome = AuthVerifyOutcome::Expired;
                    result.expires_at = chrono::DateTime::from_timestamp_millis(expires_ms)
                        .map(|dt| dt.to_rfc3339());
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

async fn run_auth_verify(
    provider: &str,
    token_store: &FileTokenStore,
    manager: &AuthManager,
) -> Result<(), AgentError> {
    let targets: Vec<String> = if provider == "all" || provider == "*" {
        hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
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

include!("auth_commands/portal_secrets.rs");
