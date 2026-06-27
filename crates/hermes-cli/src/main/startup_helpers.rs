fn auth_error_message(err: &AgentError) -> Option<String> {
    match err {
        AgentError::LlmApi(msg)
        | AgentError::Config(msg)
        | AgentError::ToolExecution(msg)
        | AgentError::Gateway(msg)
        | AgentError::AuthFailed(msg) => Some(msg.to_ascii_lowercase()),
        _ => None,
    }
}

fn oneshot_auth_is_refreshable(message: &str) -> bool {
    message.contains("401")
        || message.contains("403")
        || message.contains("unauthorized")
        || message.contains("invalid token")
        || message.contains("token_invalidated")
        || message.contains("token expired")
        || message.contains("authentication failed")
        || message.contains("authentication token has been invalidated")
        || message.contains("invalid_grant")
        || message.contains("expired")
}

fn oneshot_auth_requires_fresh_login(err: &AgentError) -> bool {
    let Some(message) = auth_error_message(err) else {
        return false;
    };
    message.contains("token_invalidated")
        || message.contains("authentication token has been invalidated")
        || message.contains("invalid_grant")
        || message.contains("refresh token")
        || message.contains("stored nous auth state is invalid")
        || message.contains("missing refresh token")
}

fn oneshot_oauth_provider_supports_login(provider: &str) -> bool {
    matches!(
        normalize_auth_provider(provider).as_str(),
        "nous"
            | "openai"
            | "openai-codex"
            | "anthropic"
            | "qwen-oauth"
            | "google-gemini-cli"
    )
}

fn oneshot_login_prompt_available() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn infer_oauth_provider_from_error_message(message: &str) -> Option<String> {
    if message.contains("portal.nousresearch.com")
        || message.contains("inference-api.nousresearch.com")
        || message.contains(" provider nous")
        || message.contains("nous:")
    {
        return Some("nous".to_string());
    }
    if message.contains("console.anthropic.com")
        || message.contains("claude.ai")
        || message.contains("anthropic")
    {
        return Some("anthropic".to_string());
    }
    if message.contains("chat.qwen.ai") || message.contains("dashscope") || message.contains("qwen")
    {
        return Some("qwen-oauth".to_string());
    }
    if message.contains("oauth2.googleapis.com")
        || message.contains("googleapis.com")
        || message.contains("gemini")
        || message.contains("google")
    {
        return Some("google-gemini-cli".to_string());
    }
    if message.contains("auth.openai.com")
        || message.contains("chatgpt.com")
        || message.contains("openai")
        || message.contains("codex")
    {
        if message.contains("codex") || message.contains("chatgpt.com") {
            return Some("openai-codex".to_string());
        }
        return Some("openai".to_string());
    }
    None
}

async fn run_fresh_openai_oneshot_login(cli: &Cli, provider: &str) -> Result<(), AgentError> {
    let provider = normalize_auth_provider(provider);
    let token_store = FileTokenStore::new(secret_vault_path_for_cli(cli)).await?;
    token_store.remove(&provider).await?;
    let _ = clear_provider_auth_state(&provider)?;
    let manager = AuthManager::new(token_store);
    match provider.as_str() {
        "openai" => {
            let state = login_openai_device_code(CodexDeviceCodeOptions::default()).await?;
            let auth_path = save_openai_auth_state(&state)?;
            let expires_at = state
                .tokens
                .expires_in
                .filter(|secs| *secs > 0)
                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
            manager
                .save_credential(OAuthCredential {
                    provider,
                    access_token: state.tokens.access_token,
                    refresh_token: state.tokens.refresh_token,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at,
                })
                .await?;
            println!("OpenAI OAuth fresh login complete; credential saved as provider 'openai'.");
            println!("Saved OAuth state: {}", auth_path.display());
            Ok(())
        }
        "openai-codex" => {
            let state = login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?;
            let auth_path = save_codex_auth_state(&state)?;
            let expires_at = state
                .tokens
                .expires_in
                .filter(|secs| *secs > 0)
                .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
            manager
                .save_credential(OAuthCredential {
                    provider,
                    access_token: state.tokens.access_token,
                    refresh_token: state.tokens.refresh_token,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at,
                })
                .await?;
            println!(
                "OpenAI Codex OAuth fresh login complete; credential saved as provider 'openai-codex'."
            );
            println!("Saved OAuth state: {}", auth_path.display());
            Ok(())
        }
        _ => Err(AgentError::Config(format!(
            "fresh OpenAI one-shot login does not support provider '{}'",
            provider
        ))),
    }
}

async fn run_oneshot_oauth_login_repair(
    cli: Cli,
    provider: &str,
    force_fresh: bool,
) -> Result<(), AgentError> {
    let provider = normalize_auth_provider(provider);
    if force_fresh && matches!(provider.as_str(), "openai" | "openai-codex") {
        return run_fresh_openai_oneshot_login(&cli, &provider).await;
    }
    run_auth(
        cli,
        Some("login".to_string()),
        Some(provider),
        None,
        None,
        None,
        None,
        false,
    )
    .await
}

fn query_is_local_slash_command(query: &str) -> bool {
    query.trim_start().starts_with('/')
}

fn env_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on" | "auto"
            )
        })
        .unwrap_or(false)
}

fn oneshot_should_use_app_runtime(query: &str) -> bool {
    !query_is_local_slash_command(query)
        && (env_truthy("HERMES_ONESHOT_APP_RUNTIME") || env_truthy("HERMES_QUORUM_AUTO_ARM"))
}

fn print_app_oneshot_result(app: &App) {
    if let Some(reply) = app.messages.iter().rev().find_map(|message| {
        if message.role == MessageRole::Assistant {
            message
                .content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        } else {
            None
        }
    }) {
        println!("{}", reply);
    }
}

async fn handle_local_slash_query(cli: Cli, query: &str) -> Result<bool, AgentError> {
    if !query_is_local_slash_command(query) {
        return Ok(false);
    }
    let mut app = App::new(cli).await?;
    app.handle_input(query).await?;
    Ok(true)
}

fn oneshot_auto_verify_oauth_provider(
    err: &AgentError,
    provider_override: Option<&str>,
    model_override: Option<&str>,
) -> Option<String> {
    let Some(message) = auth_error_message(err) else {
        return None;
    };

    if !oneshot_auth_is_refreshable(&message) {
        return None;
    }

    let mut candidates: Vec<String> = Vec::new();
    if let Some(raw_provider) = provider_override.map(str::trim).filter(|v| !v.is_empty()) {
        candidates.push(normalize_auth_provider(raw_provider));
    }
    if let Some(raw_model_provider) = model_override
        .and_then(|m| m.split_once(':').map(|(provider, _)| provider.trim()))
        .filter(|v| !v.is_empty())
    {
        candidates.push(normalize_auth_provider(raw_model_provider));
    }
    if let Some(from_message) = infer_oauth_provider_from_error_message(&message) {
        candidates.push(from_message);
    }

    let mut seen = HashSet::new();
    for candidate in candidates {
        let normalized = normalize_auth_provider(&candidate);
        if !seen.insert(normalized.clone()) {
            continue;
        }
        if provider_supports_oauth(&normalized) {
            return Some(normalized);
        }
    }
    None
}
