async fn print_auth_status_matrix(cli: &Cli, token_store: &FileTokenStore) -> Result<(), AgentError> {
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
        let store_present = token_store.get(provider).await.is_some();
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
    Unverified,
    Missing,
    Expired,
    ProbeFailed,
}

impl AuthVerifyOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Unverified => "unverified",
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::ProbeFailed => "probe_failed",
        }
    }

    fn is_success(self) -> bool {
        matches!(self, Self::Valid | Self::Unverified)
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

fn oauth_refresh_endpoints_for_provider(provider: &str) -> Result<OAuth2Endpoints, AgentError> {
    let (token_url, client_id) = oauth_refresh_config_for_provider(provider).ok_or_else(|| {
        AgentError::AuthFailed(format!(
            "OAuth refresh handler is not configured for provider '{}'; configure its OAuth endpoints or run `hermes auth login {}`",
            provider, provider
        ))
    })?;
    Ok(OAuth2Endpoints {
        authorize_url: "http://127.0.0.1/oauth/authorize-unused".to_string(),
        token_url,
        client_id,
        redirect_uri: "http://127.0.0.1/oauth/callback-unused".to_string(),
        scopes: Vec::new(),
        client_secret: None,
        client_auth_method: hermes_auth::OAuth2ClientAuthMethod::default(),
    })
}

/// Explicit credential-management effect. It registers a concrete OAuth2
/// transport and asks `AuthManager` to refresh only an expired stored token.
async fn refresh_oauth_store_credential(
    provider: &str,
    manager: &AuthManager,
) -> Result<hermes_auth::RefreshResolution, AgentError> {
    manager
        .register_oauth2_refresh_handler(provider, oauth_refresh_endpoints_for_provider(provider)?)
        .await;
    manager.refresh_expired_credential(provider).await
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
        .map_err(|_| "live_probe_failed: client initialization failed".to_string())?;
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
        .map_err(|_| "live_probe_failed: request failed".to_string())?;
    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        Err(format!("live_probe_failed: HTTP {status}"))
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

/// Constructs a probe-only Nous credential from the encrypted store. This
/// intentionally performs no token exchange, state repair, or persistence.
fn passive_nous_runtime_credentials(
    credential: &OAuthCredential,
) -> Result<NousRuntimeCredentials, AgentError> {
    if credential.is_expired(NOUS_ACCESS_TOKEN_REFRESH_SKEW_SECONDS) {
        return Err(AgentError::AuthFailed(
            "Nous OAuth credential is expired; run `hermes auth refresh nous` or `hermes auth login nous`."
                .to_string(),
        ));
    }
    let api_key = credential.access_token.trim();
    if api_key.is_empty() {
        return Err(AgentError::AuthFailed(
            "Nous OAuth credential is unusable; run `hermes auth login nous`.".to_string(),
        ));
    }
    let base_url = std::env::var("NOUS_INFERENCE_BASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_NOUS_INFERENCE_URL.to_string());
    let expires_at = credential.expires_at.map(|value| value.to_rfc3339());
    let expires_in = credential
        .expires_at
        .map(|value| (value - chrono::Utc::now()).num_seconds().max(0));
    Ok(NousRuntimeCredentials {
        provider: "nous".to_string(),
        base_url,
        api_key: api_key.to_string(),
        key_id: None,
        expires_at,
        expires_in,
        source: "token_store".to_string(),
        refresh_token: credential.refresh_token.clone(),
        token_type: credential.token_type.clone(),
        scope: credential.scope.clone(),
    })
}

async fn verify_single_oauth_provider(
    provider: &str,
    token_store: &FileTokenStore,
) -> Result<AuthVerifyResult, AgentError> {
    let provider = normalize_auth_provider(provider);
    let env_present = provider_api_key_from_env(&provider).is_some();
    let auth_state_present = if provider == "nous" {
        read_nous_auth_state()?.is_some()
    } else {
        read_provider_auth_state(&provider)?.is_some()
    };
    let stored_credential = token_store.get(&provider).await;
    let stored_present = stored_credential
        .as_ref()
        .map(|credential| !credential.access_token.trim().is_empty())
        .unwrap_or(false);
    let mut result = AuthVerifyResult {
        provider: provider.clone(),
        outcome: AuthVerifyOutcome::Missing,
        source: auth_verify_source(env_present, stored_present, auth_state_present),
        credential_present: env_present || stored_present,
        oauth_state_present: auth_state_present,
        expires_at: stored_credential
            .as_ref()
            .and_then(|credential| credential.expires_at.as_ref().map(|value| value.to_rfc3339())),
        detail: None,
    };

    let Some(credential) = stored_credential.as_ref() else {
        if env_present {
            result.outcome = AuthVerifyOutcome::Unverified;
            result.detail = Some(
                "Environment token is present but no encrypted OAuth credential is available."
                    .to_string(),
            );
        } else if auth_state_present {
            result.outcome = AuthVerifyOutcome::Missing;
            result.detail = Some(
                "OAuth state exists but no encrypted credential is available; run `hermes auth login <provider>`."
                    .to_string(),
            );
        }
        return Ok(result);
    };

    if credential.is_expired(60) {
        result.outcome = AuthVerifyOutcome::Expired;
        return Ok(result);
    }

    if provider != "nous" {
        result.outcome = AuthVerifyOutcome::Valid;
        return Ok(result);
    }

    // `auth verify nous` explicitly probes the inference endpoint, but remains
    // read-only: it never uses a refresh token or changes either credential
    // store. All token exchange and persistence are reserved for `auth refresh`
    // and login/add flows.
    let credentials = match passive_nous_runtime_credentials(credential) {
        Ok(credentials) => credentials,
        Err(error) => {
            result.outcome = AuthVerifyOutcome::ProbeFailed;
            result.detail = Some(error.to_string());
            return Ok(result);
        }
    };
    result.source = credentials.source.clone();
    result.expires_at = credentials.expires_at.clone();
    match verify_nous_runtime_credentials_live(&credentials).await {
        Ok(()) => result.outcome = AuthVerifyOutcome::Valid,
        Err(detail) => {
            result.outcome = AuthVerifyOutcome::ProbeFailed;
            result.detail = Some(detail);
        }
    }
    Ok(result)
}

async fn run_auth_verify(
    provider: &str,
    token_store: &FileTokenStore,
) -> Result<(), AgentError> {
    let targets: Vec<String> = if provider == "all" || provider == "*" {
        hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
            .iter()
            .map(|provider| provider.to_string())
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
        let result = verify_single_oauth_provider(&target, token_store).await?;
        print_auth_verify_result(&result);
        if !result.outcome.is_success() {
            failed.push(result);
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        let failed_ids: Vec<String> = failed.iter().map(|result| result.provider.clone()).collect();
        Err(AgentError::AuthFailed(format!(
            "OAuth verification failed for provider(s): {}",
            failed_ids.join(", ")
        )))
    }
}

include!("auth_commands/portal_secrets.rs");
