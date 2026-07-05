#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortalActionKind {
    Setup,
    Info,
}

fn portal_action_kind(action: Option<&str>) -> Result<PortalActionKind, AgentError> {
    match action.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some("setup" | "login" | "auth") => Ok(PortalActionKind::Setup),
        Some("info" | "status" | "check") => Ok(PortalActionKind::Info),
        Some(other) => Err(AgentError::Config(format!(
            "Unknown portal action '{}'. Use `hermes-ultra portal` for setup or `hermes-ultra portal info` for status.",
            other
        ))),
    }
}

fn portal_setup_auth_action() -> &'static str {
    "login"
}

async fn run_portal(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    match portal_action_kind(action.as_deref())? {
        PortalActionKind::Setup => {
            println!("Nous Portal setup ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some(portal_setup_auth_action().to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
        PortalActionKind::Info => {
            println!("Nous Portal info ({DEFAULT_NOUS_PORTAL_URL})");
            run_auth(
                cli,
                Some("status".to_string()),
                Some("nous".to_string()),
                None,
                None,
                None,
                None,
                false,
            )
            .await
        }
    }
}

async fn run_billing(args: Vec<String>) -> Result<(), AgentError> {
    let output = hermes_cli::billing::handle_billing_args(&args).await?;
    println!("{output}");
    Ok(())
}

async fn run_auth(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    target: Option<String>,
    auth_type: Option<String>,
    label: Option<String>,
    api_key: Option<String>,
    qr: bool,
) -> Result<(), AgentError> {
    let provider = resolve_auth_provider(provider);
    let auth_store_path = secret_vault_path_for_cli(&cli);
    let token_store = FileTokenStore::new(auth_store_path).await?;
    let manager = AuthManager::new(token_store.clone());
    let pool_path = auth_pool_path_for_cli(&cli);
    let mut pool_store = load_auth_pool_store(&pool_path)?;
    match action.as_deref().unwrap_or("status") {
        "add" => {
            let provider = normalize_auth_provider(provider.trim());
            let mut auth_type = resolve_auth_type_for_provider(&provider, auth_type.as_deref());

            if auth_type == "oauth" {
                match provider.as_str() {
                    "nous" => {
                        let (resolved, auth_path, _imported_existing, state) =
                            resolve_or_fresh_login_nous(&manager, true).await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .agent_key_obtained_at
                                .as_deref()
                                .map(|_| "device_code".to_string())
                                .unwrap_or_else(|| "discovered_session".to_string()),
                            access_token: resolved.api_key,
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Nous OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai-codex" => {
                        let imported = discover_existing_openai_codex_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI Codex OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_codex_device_code(CodexDeviceCodeOptions::default())
                                .await?
                        };
                        let auth_path = save_codex_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai-codex".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .source
                                .clone()
                                .unwrap_or_else(|| "device_code".to_string()),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI Codex OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "openai" => {
                        let imported = discover_existing_openai_oauth()?;
                        let state = if let Some(imported) = imported {
                            println!(
                                "Detected existing OpenAI OAuth session at {}.",
                                imported.source_path.display()
                            );
                            imported.state
                        } else {
                            login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                        };
                        let auth_path = save_openai_auth_state(&state)?;
                        let expires_at = state
                            .tokens
                            .expires_in
                            .filter(|secs| *secs > 0)
                            .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                        manager
                            .save_credential(OAuthCredential {
                                provider: "openai".to_string(),
                                access_token: state.tokens.access_token.clone(),
                                refresh_token: state.tokens.refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at,
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: state
                                .source
                                .clone()
                                .unwrap_or_else(|| "device_code".to_string()),
                            access_token: state.tokens.access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added OpenAI OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "anthropic" => {
                        let imported = discover_existing_anthropic_oauth()?;
                        let (state, source_label) = if let Some(imported) = imported {
                            println!(
                                "Detected existing Anthropic OAuth session at {}.",
                                imported.source_path.display()
                            );
                            (imported.state, imported.source)
                        } else {
                            (
                                login_anthropic_oauth(AnthropicOAuthLoginOptions::default())
                                    .await?,
                                "hermes_pkce".to_string(),
                            )
                        };
                        let access_token = state.access_token.clone();
                        let refresh_token = state.refresh_token.clone();
                        let expires_at_ms = state.expires_at_ms;
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "source": source_label.clone(),
                        });
                        let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "anthropic".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: source_label,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Anthropic OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "qwen-oauth" => {
                        let creds = resolve_qwen_runtime_credentials(
                            false,
                            true,
                            QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                        )
                        .await?;
                        let auth_state = serde_json::to_value(&creds.tokens)
                            .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                        let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "qwen-oauth".to_string(),
                                access_token: creds.api_key.clone(),
                                refresh_token: creds.refresh_token.clone(),
                                token_type: creds.token_type.clone(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or(default_label),
                            auth_type: "oauth".to_string(),
                            source: creds.source.clone(),
                            access_token: creds.api_key.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Qwen OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Qwen auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    "google-gemini-cli" => {
                        let creds =
                            login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default())
                                .await?;
                        let access_token = creds.api_key.clone();
                        let refresh_token = creds.refresh_token.clone();
                        let expires_at_ms = creds.expires_at_ms;
                        let email = creds.email.clone();
                        let project_id = creds.project_id.clone();
                        let source = creds.source.clone();
                        let auth_state = serde_json::json!({
                            "access_token": access_token.clone(),
                            "refresh_token": refresh_token.clone(),
                            "expires_at_ms": expires_at_ms,
                            "email": email.clone(),
                            "project_id": project_id.clone(),
                            "source": source.clone(),
                        });
                        let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                        manager
                            .save_credential(OAuthCredential {
                                provider: "google-gemini-cli".to_string(),
                                access_token: access_token.clone(),
                                refresh_token: refresh_token.clone(),
                                token_type: "bearer".to_string(),
                                scope: None,
                                expires_at: parse_unix_millis_utc(expires_at_ms),
                            })
                            .await?;
                        let entries = pool_store.providers.entry(provider.clone()).or_default();
                        let default_label = format!("{provider}-{}", entries.len() + 1);
                        let entry = AuthPoolEntry {
                            id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                            label: label.unwrap_or_else(|| email.clone().unwrap_or(default_label)),
                            auth_type: "oauth".to_string(),
                            source: source,
                            access_token: access_token.clone(),
                            last_status: None,
                            last_status_at: None,
                            last_error_code: None,
                        };
                        entries.push(entry.clone());
                        save_auth_pool_store(&pool_path, &pool_store)?;
                        println!(
                            "Added Google Gemini OAuth credential (label='{}', id={}).",
                            entry.label, entry.id
                        );
                        println!("Google auth file: {}", creds.auth_file.display());
                        println!("Saved OAuth state: {}", auth_path.display());
                        return Ok(());
                    }
                    _ => {
                        println!(
                            "OAuth flow is unavailable for provider '{}'; falling back to API key/manual token login.",
                            provider
                        );
                        auth_type = "api_key".to_string();
                    }
                }
            }

            let token = if let Some(raw) = api_key {
                raw.trim().to_string()
            } else {
                resolve_llm_login_token(&cli, &provider).await?
            };
            if token.is_empty() {
                return Err(AgentError::Config("auth add: empty credential".into()));
            }
            let entries = pool_store.providers.entry(provider.clone()).or_default();
            let default_label = format!("{provider}-{}", entries.len() + 1);
            let entry = AuthPoolEntry {
                id: uuid::Uuid::new_v4().simple().to_string()[..6].to_string(),
                label: label.unwrap_or(default_label),
                auth_type,
                source: "manual".to_string(),
                access_token: token.clone(),
                last_status: None,
                last_status_at: None,
                last_error_code: None,
            };
            entries.push(entry.clone());
            save_auth_pool_store(&pool_path, &pool_store)?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: entry.access_token.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!(
                "Added pooled credential for provider '{}' (label='{}', id={}).",
                provider, entry.label, entry.id
            );
            return Ok(());
        }
        "list" => {
            if pool_store.providers.is_empty() {
                println!("No pooled credentials configured.");
                return Ok(());
            }
            if let Some(entries) = pool_store.providers.get(&provider) {
                println!("{} ({} credentials):", provider, entries.len());
                for (idx, e) in entries.iter().enumerate() {
                    let exhausted = if e.last_status.as_deref() == Some("exhausted") {
                        " exhausted"
                    } else {
                        ""
                    };
                    println!(
                        "  #{}  {:<20} {:<8} {}{}",
                        idx + 1,
                        e.label,
                        e.auth_type,
                        e.source,
                        exhausted
                    );
                }
                return Ok(());
            }
            println!("No pooled credentials for provider '{}'.", provider);
            return Ok(());
        }
        "remove" => {
            let target = target.ok_or_else(|| {
                AgentError::Config(
                    "auth remove usage: hermes auth remove <provider> <index|id|label>".into(),
                )
            })?;
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                return Err(AgentError::Config(format!(
                    "No pooled credentials for provider '{}'",
                    provider
                )));
            };
            let Some(index) = resolve_pool_target(entries, &target) else {
                return Err(AgentError::Config(format!(
                    "Could not resolve auth remove target '{}' for provider '{}'",
                    target, provider
                )));
            };
            let removed = entries.remove(index);
            if entries.is_empty() {
                pool_store.providers.remove(&provider);
                token_store.remove(&provider).await?;
                if provider_supports_oauth(&provider) {
                    let _ = clear_provider_auth_state(&provider)?;
                }
            } else if let Some(next) = entries.first() {
                manager
                    .save_credential(OAuthCredential {
                        provider: provider.clone(),
                        access_token: next.access_token.clone(),
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Removed pooled credential for provider '{}' (label='{}', id={}).",
                provider, removed.label, removed.id
            );
            return Ok(());
        }
        "reset" => {
            let Some(entries) = pool_store.providers.get_mut(&provider) else {
                println!("No pooled credentials for provider '{}'.", provider);
                return Ok(());
            };
            let mut reset = 0usize;
            for e in entries.iter_mut() {
                if e.last_status.is_some() || e.last_error_code.is_some() {
                    e.last_status = None;
                    e.last_status_at = None;
                    e.last_error_code = None;
                    reset += 1;
                }
            }
            save_auth_pool_store(&pool_path, &pool_store)?;
            println!(
                "Reset status on {} pooled credential(s) for provider '{}'.",
                reset, provider
            );
            return Ok(());
        }
        "verify" => {
            run_auth_verify(&provider, &token_store, &manager).await?;
            return Ok(());
        }
        "login" => {
            if provider == "telegram" {
                let token = telegram_bot_token_from_env_or_prompt().await?;
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let tg = disk
                    .platforms
                    .entry("telegram".to_string())
                    .or_insert_with(PlatformConfig::default);
                tg.token = Some(token);
                tg.enabled = true;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_WEIXIN_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);
                let mut account_id_opt = disk
                    .platforms
                    .get("weixin")
                    .and_then(|p| p.extra.get("account_id"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let (account_id, token, qr_base_url, qr_user_id) = if qr_preferred {
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    let (start_ep, poll_ep) = weixin_login_endpoints_from_disk(&disk);
                    match weixin_qr_login_flow(
                        &base_url,
                        &start_ep,
                        &poll_ep,
                        account_id_opt.as_deref(),
                    )
                    .await
                    {
                        Ok(pair) => pair,
                        Err(e) => {
                            println!("Weixin QR 登录失败，将回退到手动 token 输入: {}", e);
                            let fallback_account_id = if let Some(v) = account_id_opt.take() {
                                v
                            } else {
                                weixin_account_id_from_env_or_prompt().await?
                            };
                            let fallback_token =
                                weixin_token_from_env_or_prompt(&fallback_account_id).await?;
                            (fallback_account_id, fallback_token, base_url, String::new())
                        }
                    }
                } else {
                    let manual_account_id = if let Some(v) = account_id_opt.take() {
                        v
                    } else {
                        weixin_account_id_from_env_or_prompt().await?
                    };
                    let manual_token = weixin_token_from_env_or_prompt(&manual_account_id).await?;
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    (manual_account_id, manual_token, base_url, String::new())
                };
                let wx = disk
                    .platforms
                    .entry("weixin".to_string())
                    .or_insert_with(PlatformConfig::default);
                wx.enabled = true;
                wx.token = Some(token.clone());
                wx.extra.insert(
                    "account_id".to_string(),
                    serde_json::Value::String(account_id.clone()),
                );
                if !qr_base_url.trim().is_empty() {
                    wx.extra.insert(
                        "base_url".to_string(),
                        serde_json::Value::String(qr_base_url.clone()),
                    );
                }
                save_persisted_weixin_account(
                    &account_id,
                    &token,
                    Some(qr_base_url.as_str()),
                    Some(qr_user_id.as_str()),
                )?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: account_id/token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "qqbot" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_QQBOT_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);

                let existing_app_id = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("app_id"))
                    .and_then(|v| v.as_str());
                let existing_secret = disk
                    .platforms
                    .get("qqbot")
                    .and_then(|p| p.extra.get("client_secret"))
                    .and_then(|v| v.as_str());

                let (app_id, client_secret, user_openid) = if qr_preferred {
                    let portal_host = qqbot_portal_host_from_disk(&disk);
                    let (create_path, poll_path) = qqbot_onboard_endpoints_from_disk(&disk);
                    match qqbot_qr_login_flow(&portal_host, &create_path, &poll_path, 600).await {
                        Ok(tuple) => tuple,
                        Err(e) => {
                            println!(
                                "QQBot QR setup failed, falling back to manual credentials: {}",
                                e
                            );
                            let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                            let client_secret =
                                qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                            (app_id, client_secret, String::new())
                        }
                    }
                } else {
                    let app_id = qqbot_app_id_from_env_or_prompt(existing_app_id).await?;
                    let client_secret =
                        qqbot_client_secret_from_env_or_prompt(existing_secret).await?;
                    (app_id, client_secret, String::new())
                };

                let qq = disk
                    .platforms
                    .entry("qqbot".to_string())
                    .or_insert_with(PlatformConfig::default);
                qq.enabled = true;
                qq.extra.insert(
                    "app_id".to_string(),
                    serde_json::Value::String(app_id.clone()),
                );
                qq.extra.insert(
                    "client_secret".to_string(),
                    serde_json::Value::String(client_secret.clone()),
                );
                if !qq.extra.contains_key("markdown_support") {
                    qq.extra.insert(
                        "markdown_support".to_string(),
                        serde_json::Value::Bool(true),
                    );
                }
                if !user_openid.trim().is_empty() {
                    qq.extra.insert(
                        "user_openid".to_string(),
                        serde_json::Value::String(user_openid.clone()),
                    );
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "QQBot: app_id/client_secret saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                configure_platform_basic_prompts(&mut disk, platform_key).await?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: config updated and platform enabled in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "nous" {
                let (_resolved, auth_path, _imported_existing, _state) =
                    resolve_or_fresh_login_nous(&manager, true).await?;
                println!("Nous OAuth credential saved as provider 'nous'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai-codex" {
                let imported = discover_existing_openai_codex_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI Codex OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_codex_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_codex_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai-codex".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI Codex OAuth credential saved as provider 'openai-codex'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "openai" {
                let imported = discover_existing_openai_oauth()?;
                let state = if let Some(imported) = imported {
                    println!(
                        "Detected existing OpenAI OAuth session at {}.",
                        imported.source_path.display()
                    );
                    imported.state
                } else {
                    login_openai_device_code(CodexDeviceCodeOptions::default()).await?
                };
                let auth_path = save_openai_auth_state(&state)?;
                let expires_at = state
                    .tokens
                    .expires_in
                    .filter(|secs| *secs > 0)
                    .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
                manager
                    .save_credential(OAuthCredential {
                        provider: "openai".to_string(),
                        access_token: state.tokens.access_token.clone(),
                        refresh_token: state.tokens.refresh_token.clone(),
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at,
                    })
                    .await?;
                println!("OpenAI OAuth login complete; credential saved as provider 'openai'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "anthropic" {
                let imported = discover_existing_anthropic_oauth()?;
                let (state, source_label) = if let Some(imported) = imported {
                    println!(
                        "Detected existing Anthropic OAuth session at {}.",
                        imported.source_path.display()
                    );
                    (imported.state, imported.source)
                } else {
                    (
                        login_anthropic_oauth(AnthropicOAuthLoginOptions::default()).await?,
                        "hermes_pkce".to_string(),
                    )
                };
                let access_token = state.access_token.clone();
                let refresh_token = state.refresh_token.clone();
                let expires_at_ms = state.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "source": source_label,
                });
                let auth_path = save_provider_auth_state("anthropic", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "anthropic".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!("Anthropic OAuth credential saved as provider 'anthropic'.");
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let creds = resolve_qwen_runtime_credentials(
                    false,
                    true,
                    QWEN_ACCESS_TOKEN_REFRESH_SKEW_SECONDS,
                )
                .await?;
                let auth_state = serde_json::to_value(&creds.tokens)
                    .map_err(|e| AgentError::Config(format!("encode state: {}", e)))?;
                let auth_path = save_provider_auth_state("qwen-oauth", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "qwen-oauth".to_string(),
                        access_token: creds.api_key.clone(),
                        refresh_token: creds.refresh_token.clone(),
                        token_type: creds.token_type.clone(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(creds.expires_at_ms),
                    })
                    .await?;
                println!(
                    "Qwen OAuth credential imported from {} and stored as provider 'qwen-oauth'.",
                    creds.auth_file.display()
                );
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let creds =
                    login_google_gemini_cli_oauth(GeminiOAuthLoginOptions::default()).await?;
                let access_token = creds.api_key.clone();
                let refresh_token = creds.refresh_token.clone();
                let expires_at_ms = creds.expires_at_ms;
                let auth_state = serde_json::json!({
                    "access_token": access_token.clone(),
                    "refresh_token": refresh_token.clone(),
                    "expires_at_ms": expires_at_ms,
                    "email": creds.email.clone(),
                    "project_id": creds.project_id.clone(),
                    "source": creds.source.clone(),
                });
                let auth_path = save_provider_auth_state("google-gemini-cli", auth_state)?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "google-gemini-cli".to_string(),
                        access_token,
                        refresh_token,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: parse_unix_millis_utc(expires_at_ms),
                    })
                    .await?;
                println!(
                    "Google Gemini OAuth login complete; credential saved as provider 'google-gemini-cli'."
                );
                println!("Google auth file: {}", creds.auth_file.display());
                println!("Saved OAuth state: {}", auth_path.display());
                return Ok(());
            }
            if provider == "copilot" || provider == "github-copilot" {
                let access_token = hermes_cli::copilot_auth::start_copilot_device_flow().await?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "copilot".to_string(),
                        access_token,
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
                println!("GitHub device login complete; credential saved as provider 'copilot'.");
                println!("Ensure COPILOT_GITHUB_TOKEN is set for the agent (see printed instructions above).");
                return Ok(());
            }

            let access_token = resolve_llm_login_token(&cli, &provider).await?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            let msg = hermes_cli::auth::login(&provider).await?;
            println!("{}", msg);
        }
        "logout" => {
            if provider == "telegram" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(tg) = disk.platforms.get_mut("telegram") {
                    tg.token = None;
                    tg.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token cleared and platform disabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(wx) = disk.platforms.get_mut("weixin") {
                    wx.token = None;
                    wx.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: token cleared and platform disabled in {} (account file retained)",
                    cfg_path.display()
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(p) = disk.platforms.get_mut(platform_key) {
                    p.enabled = false;
                    p.token = None;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "{}: disabled and token cleared in {}",
                    platform_key,
                    cfg_path.display()
                );
                return Ok(());
            }
            let msg = hermes_cli::auth::logout(&provider).await?;
            token_store.remove(&provider).await?;
            if provider_supports_oauth(&provider) {
                let _ = clear_provider_auth_state(&provider)?;
            }
            println!("{} (removed credential for provider: {})", msg, provider);
        }
        _ => {
            if provider == "all" || provider == "*" {
                print_auth_status_matrix(&cli, &token_store).await?;
                return Ok(());
            }
            if provider == "telegram" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (has, en) = disk
                    .platforms
                    .get("telegram")
                    .map(|p| {
                        (
                            p.token
                                .as_deref()
                                .map(|t| !t.trim().is_empty())
                                .unwrap_or(false),
                            p.enabled,
                        )
                    })
                    .unwrap_or((false, false));
                println!(
                    "Telegram ({}): token_present={} enabled={}",
                    cfg_path.display(),
                    has,
                    en
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (account_id, has_cfg_token, enabled) = disk
                    .platforms
                    .get("weixin")
                    .map(|p| {
                        let account_id = p
                            .extra
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let has_cfg_token = p
                            .token
                            .as_deref()
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false);
                        (account_id, has_cfg_token, p.enabled)
                    })
                    .unwrap_or_else(|| ("".to_string(), false, false));
                let has_saved_token = if account_id.is_empty() {
                    false
                } else {
                    load_persisted_weixin_token(&account_id).is_some()
                };
                println!(
                    "Weixin ({}): account_id={} cfg_token_present={} saved_token_present={} enabled={}",
                    cfg_path.display(),
                    if account_id.is_empty() {
                        "(none)"
                    } else {
                        account_id.as_str()
                    },
                    has_cfg_token,
                    has_saved_token,
                    enabled
                );
                return Ok(());
            }
            if let Some(platform_key) = gateway_platform_provider_key(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (enabled, token_present) = disk
                    .platforms
                    .get(platform_key)
                    .map(|p| (p.enabled, platform_token_or_extra(p).is_some()))
                    .unwrap_or((false, false));
                println!(
                    "{} ({}): credential_present={} enabled={}",
                    platform_key,
                    cfg_path.display(),
                    token_present,
                    enabled
                );
                return Ok(());
            }
            if provider == "nous" {
                let env_present = provider_api_key_from_env(&provider).is_some();
                let store_present = token_store.get(&provider).await.is_some();
                let auth_state_present = read_valid_nous_auth_state()?.is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "qwen-oauth" {
                let qwen_status = get_qwen_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = token_store.get(&provider).await.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Qwen OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    qwen_status.logged_in,
                    qwen_status.auth_file.display(),
                    qwen_status.source.as_deref().unwrap_or("none"),
                    qwen_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = qwen_status.api_key.as_deref() {
                    println!("Qwen OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = qwen_status.error.as_deref() {
                    println!("Qwen OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "google-gemini-cli" {
                let google_status = get_gemini_oauth_auth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = token_store.get(&provider).await.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Google Gemini OAuth: logged_in={} auth_file={} source={} expires_at_ms={}",
                    google_status.logged_in,
                    google_status.auth_file.display(),
                    google_status.source.as_deref().unwrap_or("none"),
                    google_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(email) = google_status.email.as_deref() {
                    println!("Google account: {}", email);
                }
                if let Some(project_id) = google_status.project_id.as_deref() {
                    println!("Google project_id: {}", project_id);
                }
                if let Some(token) = google_status.api_key.as_deref() {
                    println!("Google OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = google_status.error.as_deref() {
                    println!("Google OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            if provider == "anthropic" {
                let anthropic_status = get_anthropic_oauth_status().await;
                let auth_state_present = read_provider_auth_state(&provider)?.is_some();
                let store_present = token_store.get(&provider).await.is_some();
                let env_present = provider_api_key_from_env(&provider).is_some();
                let (has_token, source) = if env_present {
                    (true, "env")
                } else if store_present {
                    (true, "token_store")
                } else if auth_state_present {
                    (true, "auth_json")
                } else {
                    (false, "none")
                };
                println!(
                    "Anthropic OAuth: logged_in={} source={} expires_at_ms={}",
                    anthropic_status.logged_in,
                    anthropic_status.source.as_deref().unwrap_or("none"),
                    anthropic_status
                        .expires_at_ms
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "unknown".to_string())
                );
                if let Some(token) = anthropic_status.api_key.as_deref() {
                    println!("Anthropic OAuth token: {}", mask_secret(token));
                }
                if let Some(err) = anthropic_status.error.as_deref() {
                    println!("Anthropic OAuth detail: {}", err);
                }
                println!(
                    "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                    provider, has_token, source, auth_state_present
                );
                return Ok(());
            }
            let env_present = provider_api_key_from_env(&provider).is_some();
            let store_present = token_store.get(&provider).await.is_some();
            let auth_state_present = if provider_supports_oauth(&provider) {
                read_provider_auth_state(&provider)?.is_some()
            } else {
                false
            };
            let (has_token, source) = if env_present {
                (true, "env")
            } else if store_present {
                (true, "token_store")
            } else if auth_state_present {
                (true, "auth_json")
            } else {
                (false, "none")
            };
            println!(
                "Auth status: provider='{}', credential_present={}, source={}, oauth_state_present={}",
                provider, has_token, source, auth_state_present
            );
        }
    }
    Ok(())
}

async fn run_secrets(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    value: Option<String>,
    show: bool,
) -> Result<(), AgentError> {
    let path = secret_vault_path_for_cli(&cli);
    let store = FileTokenStore::new(&path).await?;
    let manager = AuthManager::new(store.clone());

    match action.as_deref().unwrap_or("list") {
        "list" | "status" => {
            let providers = store.list_providers().await;
            println!("Secret vault: {}", path.display());
            if providers.is_empty() {
                println!("  (empty)");
            } else {
                println!("Stored providers ({}):", providers.len());
                for p in providers {
                    if let Some(env_var) = provider_env_var(&p) {
                        println!("  - {p} (env: {env_var})");
                    } else {
                        println!("  - {p}");
                    }
                }
            }
            println!("Tip: runtime automatically hydrates env vars from this vault.");
        }
        "set" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets set: usage `hermes secrets set <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let secret = match value {
                Some(v) => v.trim().to_string(),
                None => prompt_line(format!("Enter secret for provider '{provider}': ")).await?,
            };
            if secret.is_empty() {
                return Err(AgentError::Config("Secret cannot be empty.".into()));
            }
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: secret,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!(
                "Saved secret for provider '{provider}' in {}",
                path.display()
            );
            if let Some(env_var) = provider_env_var(&provider) {
                println!("Mapped runtime env: {env_var}");
            }
        }
        "get" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets get: usage `hermes secrets get <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            if let Some((stored_provider, secret)) =
                lookup_secret_from_vault(&store, &provider).await
            {
                if show {
                    if !secret_stdout_allowed() {
                        return Err(AgentError::Config(
                            "Refusing plaintext secret output. Re-run with HERMES_ALLOW_SECRET_STDOUT=1 to opt in."
                                .into(),
                        ));
                    }
                    println!("{secret}");
                } else {
                    println!("{}", mask_secret(&secret));
                }
                if stored_provider != provider {
                    println!("(resolved via provider alias '{}')", stored_provider);
                }
            } else {
                return Err(AgentError::Config(format!(
                    "No secret stored for provider '{}'",
                    provider
                )));
            }
        }
        "remove" | "delete" | "rm" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config(
                    "secrets remove: usage `hermes secrets remove <provider>`".into(),
                )
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let mut removed = false;
            for candidate in secret_provider_aliases(&provider) {
                if store.get(&candidate).await.is_some() {
                    store.remove(&candidate).await?;
                    removed = true;
                }
            }
            if removed {
                println!("Removed secret for provider '{}'.", provider);
            } else {
                println!("No secret found for provider '{}'.", provider);
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown secrets action: {} (use list|status|get|set|remove)",
                other
            )));
        }
    }
    Ok(())
}

fn cron_cli_error(e: CronError) -> AgentError {
    AgentError::Config(e.to_string())
}
