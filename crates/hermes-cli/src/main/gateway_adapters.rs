fn report_uncompiled_gateway_adapters(config: &hermes_config::GatewayConfig) {
    for (platform, platform_cfg) in &config.platforms {
        if platform_cfg.enabled {
            if let Some(feature) = missing_gateway_adapter_feature(platform) {
                println!(
                    "{platform} is enabled but this binary was built without `{feature}`; skipping adapter registration."
                );
            }
        }
    }
}

#[cfg(feature = "gateway-api-server")]
fn build_api_server_config(platform_cfg: &PlatformConfig) -> ApiServerConfig {
    ApiServerConfig {
        host: extra_string(platform_cfg, "host").unwrap_or_else(|| "127.0.0.1".to_string()),
        port: extra_u16(platform_cfg, "port", 8090),
        auth_token: platform_token_or_extra(platform_cfg)
            .or_else(|| extra_string(platform_cfg, "auth_token")),
    }
}

#[cfg(feature = "gateway-webhook")]
fn build_webhook_config(platform_cfg: &PlatformConfig, secret: String) -> WebhookConfig {
    let routes = platform_cfg
        .extra
        .get("routes")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    WebhookConfig {
        host: extra_string(platform_cfg, "host").unwrap_or_else(|| "0.0.0.0".to_string()),
        port: extra_u16(platform_cfg, "port", 9000),
        path: extra_string(platform_cfg, "path").unwrap_or_else(|| "/webhook".to_string()),
        secret,
        rate_limit: platform_cfg
            .extra
            .get("rate_limit")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(30),
        max_body_bytes: platform_cfg
            .extra
            .get("max_body_bytes")
            .and_then(|v| v.as_u64())
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(1_048_576),
        routes,
    }
}

#[cfg(feature = "gateway-api-server")]
async fn run_api_server_inbound_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<ApiInboundRequest>,
) {
    while let Some(req) = rx.recv().await {
        gateway
            .merge_request_runtime_overrides(
                "api_server",
                &req.session_id,
                &req.user_id,
                req.model.clone(),
                req.provider.clone(),
                req.personality.clone(),
            )
            .await;
        let incoming = GatewayIncomingMessage {
            platform: "api_server".to_string(),
            chat_id: req.session_id.clone(),
            user_id: req.user_id.clone(),
            text: req.prompt.clone(),
            message_id: Some(req.request_id.clone()),
            thread_id: None,
            is_dm: true,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route api_server message: {}", err);
        }
    }
}

#[cfg(feature = "gateway-webhook")]
async fn run_webhook_inbound_loop(gateway: Arc<Gateway>, mut rx: mpsc::Receiver<WebhookPayload>) {
    while let Some(payload) = rx.recv().await {
        let incoming = GatewayIncomingMessage {
            platform: "webhook".to_string(),
            chat_id: payload.chat_id,
            user_id: payload
                .user_id
                .unwrap_or_else(|| "webhook-client".to_string()),
            text: payload.text,
            message_id: None,
            thread_id: None,
            is_dm: true,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route webhook message: {}", err);
        }
    }
}

#[cfg(any(
    feature = "gateway-dingtalk",
    feature = "gateway-ntfy",
    feature = "gateway-weixin"
))]
async fn run_gateway_incoming_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<GatewayIncomingMessage>,
    platform: &'static str,
) {
    while let Some(incoming) = rx.recv().await {
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route {} message: {}", platform, err);
        }
    }
}

async fn register_gateway_adapters(
    config: &hermes_config::GatewayConfig,
    gateway: Arc<Gateway>,
    sidecar_tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), AgentError> {
    report_uncompiled_gateway_adapters(config);
    let _ = (&gateway, &sidecar_tasks);

    #[cfg(feature = "gateway-telegram")]
    if let Some(platform_cfg) = config.platforms.get("telegram") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let telegram_config = build_telegram_config(platform_cfg, token);
                let telegram_adapter = Arc::new(TelegramAdapter::new(telegram_config)?);
                gateway
                    .register_adapter("telegram", telegram_adapter.clone())
                    .await;
                let gw_clone = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    run_telegram_poll_loop(gw_clone, telegram_adapter).await;
                }));
            } else {
                println!(
                    "Telegram is enabled but token is missing; skipping telegram adapter.\n  Fix: run `hermes auth login telegram` or set `platforms.telegram.token` in config.yaml."
                );
            }
        }
    }

    #[cfg(feature = "gateway-weixin")]
    if let Some(platform_cfg) = config.platforms.get("weixin") {
        if platform_cfg.enabled {
            let account_id_missing = platform_cfg
                .extra
                .get("account_id")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .map(|s| s.is_empty())
                .unwrap_or(true);
            let token_missing = platform_token_or_extra(platform_cfg).is_none();
            if account_id_missing {
                println!(
                    "Weixin is enabled but account_id is missing; skipping weixin adapter.\n  Fix: run `hermes auth login weixin --qr` (recommended) or set `platforms.weixin.extra.account_id`."
                );
            } else if token_missing {
                println!(
                    "Weixin is enabled but token is missing; skipping weixin adapter.\n  Fix: run `hermes auth login weixin --qr` or set `platforms.weixin.token`."
                );
            } else {
                let wx_cfg = WeixinConfig::from_platform_config(platform_cfg);
                match WeChatAdapter::new(wx_cfg) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                        adapter.set_inbound_sender(tx).await;
                        gateway.register_adapter("weixin", adapter).await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            run_gateway_incoming_loop(gw_clone, rx, "weixin").await;
                        }));
                    }
                    Err(e) => {
                        println!(
                            "Weixin is enabled but failed to initialize: {}\n  Hint: rerun `hermes auth login weixin --qr` and check account file under ~/.hermes/weixin/accounts/.",
                            e
                        );
                    }
                }
            }
        }
    }

    #[cfg(feature = "gateway-discord")]
    if let Some(platform_cfg) = config.platforms.get("discord") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let discord_cfg = DiscordConfig {
                    token,
                    application_id: extra_string(platform_cfg, "application_id"),
                    proxy: Default::default(),
                    api_base_url: extra_string(platform_cfg, "api_base_url"),
                    liveness_interval_seconds: platform_cfg
                        .extra
                        .get("liveness_interval_seconds")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(60.0),
                    liveness_failure_threshold: platform_cfg
                        .extra
                        .get("liveness_failure_threshold")
                        .and_then(|v| v.as_u64())
                        .and_then(|v| u32::try_from(v).ok())
                        .unwrap_or(3),
                    require_mention: platform_cfg.require_mention.unwrap_or(false),
                    intents: platform_cfg
                        .extra
                        .get("intents")
                        .and_then(|v| v.as_u64())
                        .unwrap_or((1 << 0) | (1 << 9) | (1 << 15)),
                    reply_to_mode: discord_reply_to_mode_string(platform_cfg)
                        .unwrap_or_else(|| "first".to_string()),
                    channel_controls: DiscordChannelControls::from_extra(&platform_cfg.extra),
                    channel_skill_bindings: DiscordChannelSkillBinding::list_from_json(
                        platform_cfg.extra.get("channel_skill_bindings"),
                    ),
                };
                match DiscordAdapter::new(discord_cfg) {
                    Ok(adapter) => gateway.register_adapter("discord", Arc::new(adapter)).await,
                    Err(e) => println!("Discord enabled but failed to initialize: {}", e),
                }
            } else {
                println!("Discord is enabled but token is missing; skipping discord adapter.");
            }
        }
    }

    #[cfg(feature = "gateway-slack")]
    if let Some(platform_cfg) = config.platforms.get("slack") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let slack_cfg = SlackConfig {
                    token,
                    app_token: extra_string(platform_cfg, "app_token"),
                    socket_mode: extra_bool(platform_cfg, "socket_mode", false),
                    reactions: extra_bool(platform_cfg, "reactions", true),
                    require_mention: platform_cfg
                        .require_mention
                        .or_else(|| extra_bool_loose(platform_cfg, "require_mention"))
                        .unwrap_or(false),
                    bot_user_id: extra_string(platform_cfg, "bot_user_id")
                        .or_else(|| extra_string(platform_cfg, "bot_id")),
                    mention_patterns: extra_string_vec(platform_cfg, "mention_patterns"),
                    proxy: Default::default(),
                };
                match SlackAdapter::new(slack_cfg) {
                    Ok(adapter) => gateway.register_adapter("slack", Arc::new(adapter)).await,
                    Err(e) => println!("Slack enabled but failed to initialize: {}", e),
                }
            } else {
                println!("Slack is enabled but token is missing; skipping slack adapter.");
            }
        }
    }

    #[cfg(feature = "gateway-matrix")]
    if let Some(platform_cfg) = config.platforms.get("matrix") {
        if platform_cfg.enabled {
            let homeserver_url = extra_string(platform_cfg, "homeserver_url")
                .or_else(|| extra_string(platform_cfg, "homeserver"))
                .unwrap_or_default();
            let user_id = extra_string(platform_cfg, "user_id").unwrap_or_default();
            let access_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "access_token"))
                .unwrap_or_default();
            if homeserver_url.is_empty() || user_id.is_empty() || access_token.is_empty() {
                println!(
                    "Matrix is enabled but homeserver_url/user_id/access_token is incomplete; skipping matrix adapter."
                );
            } else {
                let matrix_cfg = MatrixConfig {
                    homeserver_url,
                    user_id,
                    access_token,
                    room_id: matrix_home_room_for_platform(platform_cfg),
                    proxy: Default::default(),
                };
                match MatrixAdapter::new(matrix_cfg) {
                    Ok(adapter) => gateway.register_adapter("matrix", Arc::new(adapter)).await,
                    Err(e) => println!("Matrix enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-mattermost")]
    if let Some(platform_cfg) = config.platforms.get("mattermost") {
        if platform_cfg.enabled {
            let token = platform_token_or_extra(platform_cfg).unwrap_or_default();
            let server_url = extra_string(platform_cfg, "server_url")
                .or_else(|| extra_string(platform_cfg, "url"))
                .unwrap_or_default();
            if token.is_empty() || server_url.is_empty() {
                println!(
                    "Mattermost is enabled but server_url/token is missing; skipping mattermost adapter."
                );
            } else {
                let mm_cfg = MattermostConfig {
                    server_url,
                    token,
                    team_id: extra_string(platform_cfg, "team_id"),
                    proxy: Default::default(),
                    reply_to_mode: extra_string(platform_cfg, "reply_to_mode")
                        .unwrap_or_else(|| "off".to_string()),
                };
                match MattermostAdapter::new(mm_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("mattermost", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("Mattermost enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-signal")]
    if let Some(platform_cfg) = config.platforms.get("signal") {
        if platform_cfg.enabled {
            let phone_number = extra_string(platform_cfg, "phone_number")
                .or_else(|| extra_string(platform_cfg, "account"))
                .unwrap_or_default();
            if phone_number.is_empty() {
                println!("Signal is enabled but phone_number is missing; skipping signal adapter.");
            } else {
                let signal_cfg = SignalConfig {
                    phone_number,
                    api_url: extra_string(platform_cfg, "api_url")
                        .unwrap_or_else(|| "http://localhost:8080".to_string()),
                    proxy: Default::default(),
                };
                match SignalAdapter::new(signal_cfg) {
                    Ok(adapter) => gateway.register_adapter("signal", Arc::new(adapter)).await,
                    Err(e) => println!("Signal enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-whatsapp")]
    if let Some(platform_cfg) = config.platforms.get("whatsapp") {
        if platform_cfg.enabled {
            if let Some(token) = platform_token_or_extra(platform_cfg) {
                let wa_cfg = WhatsAppConfig {
                    token,
                    phone_number_id: extra_string(platform_cfg, "phone_number_id"),
                    business_account_id: extra_string(platform_cfg, "business_account_id"),
                    api_base_url: extra_string(platform_cfg, "api_base_url"),
                    verify_token: extra_string(platform_cfg, "verify_token"),
                    reply_prefix: platform_cfg
                        .extra
                        .get("reply_prefix")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    require_mention: extra_bool(platform_cfg, "require_mention", false),
                    mention_patterns: extra_string_vec(platform_cfg, "mention_patterns"),
                    free_response_chats: extra_string_vec(platform_cfg, "free_response_chats"),
                    dm_policy: extra_string(platform_cfg, "dm_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    allow_from: extra_string_vec(platform_cfg, "allow_from"),
                    group_policy: extra_string(platform_cfg, "group_policy")
                        .unwrap_or_else(|| "open".to_string()),
                    group_allow_from: extra_string_vec(platform_cfg, "group_allow_from"),
                    proxy: Default::default(),
                };
                match WhatsAppAdapter::new(wa_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("whatsapp", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("WhatsApp enabled but failed to initialize: {}", e),
                }
            } else {
                println!("WhatsApp is enabled but token is missing; skipping whatsapp adapter.");
            }
        }
    }

    #[cfg(feature = "gateway-dingtalk")]
    if let Some(platform_cfg) = config.platforms.get("dingtalk") {
        if platform_cfg.enabled {
            let ding_cfg = DingTalkConfig::from_platform_config(platform_cfg);
            match DingTalkAdapter::new(ding_cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                    adapter.set_inbound_sender(tx).await;
                    gateway.register_adapter("dingtalk", adapter).await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        run_gateway_incoming_loop(gw_clone, rx, "dingtalk").await;
                    }));
                }
                Err(e) => println!("DingTalk enabled but failed to initialize: {}", e),
            }
        }
    }

    #[cfg(feature = "gateway-feishu")]
    if let Some(platform_cfg) = config.platforms.get("feishu") {
        if platform_cfg.enabled {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let app_secret = extra_string(platform_cfg, "app_secret").unwrap_or_default();
            if app_id.is_empty() || app_secret.is_empty() {
                println!(
                    "Feishu is enabled but app_id/app_secret is missing; skipping feishu adapter."
                );
            } else {
                let feishu_cfg = FeishuConfig {
                    app_id,
                    app_secret,
                    verification_token: extra_string(platform_cfg, "verification_token"),
                    encrypt_key: extra_string(platform_cfg, "encrypt_key"),
                    proxy: Default::default(),
                };
                match FeishuAdapter::new(feishu_cfg) {
                    Ok(adapter) => gateway.register_adapter("feishu", Arc::new(adapter)).await,
                    Err(e) => println!("Feishu enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-wecom")]
    if let Some(platform_cfg) = config.platforms.get("wecom") {
        if platform_cfg.enabled {
            let corp_id = extra_string(platform_cfg, "corp_id").unwrap_or_default();
            let agent_id = extra_string(platform_cfg, "agent_id").unwrap_or_default();
            let secret = extra_string(platform_cfg, "secret").unwrap_or_default();
            if corp_id.is_empty() || agent_id.is_empty() || secret.is_empty() {
                println!(
                    "WeCom is enabled but corp_id/agent_id/secret is missing; skipping wecom adapter."
                );
            } else {
                let wecom_cfg = WeComConfig {
                    corp_id,
                    agent_id,
                    secret,
                    proxy: Default::default(),
                };
                match WeComAdapter::new(wecom_cfg) {
                    Ok(adapter) => gateway.register_adapter("wecom", Arc::new(adapter)).await,
                    Err(e) => println!("WeCom enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-wecom-callback")]
    if let Some(platform_cfg) = config.platforms.get("wecom_callback") {
        if platform_cfg.enabled {
            let corp_id = extra_string(platform_cfg, "corp_id").unwrap_or_default();
            let corp_secret = extra_string(platform_cfg, "corp_secret").unwrap_or_default();
            let agent_id = extra_string(platform_cfg, "agent_id").unwrap_or_default();
            let token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "token"))
                .unwrap_or_default();
            let encoding_aes_key =
                extra_string(platform_cfg, "encoding_aes_key").unwrap_or_default();
            if corp_id.is_empty()
                || corp_secret.is_empty()
                || agent_id.is_empty()
                || token.is_empty()
                || encoding_aes_key.is_empty()
            {
                println!(
                    "WeCom callback is enabled but corp_id/corp_secret/agent_id/token/encoding_aes_key is incomplete; skipping wecom_callback adapter."
                );
            } else {
                let app = WeComCallbackApp {
                    name: extra_string(platform_cfg, "app_name")
                        .unwrap_or_else(|| "default".to_string()),
                    corp_id,
                    corp_secret,
                    agent_id,
                    token,
                    encoding_aes_key,
                };
                let wecom_cb_cfg = WeComCallbackConfig {
                    host: extra_string(platform_cfg, "host")
                        .unwrap_or_else(|| "0.0.0.0".to_string()),
                    port: extra_u16(platform_cfg, "port", 8645),
                    path: extra_string(platform_cfg, "path")
                        .unwrap_or_else(|| "/wecom/callback".to_string()),
                    apps: vec![app],
                    proxy: Default::default(),
                };
                match WeComCallbackAdapter::new(wecom_cb_cfg) {
                    Ok(adapter) => {
                        let adapter = Arc::new(adapter);
                        let (tx, mut rx) =
                            tokio::sync::mpsc::channel::<GatewayIncomingMessage>(128);
                        adapter.set_inbound_sender(tx).await;
                        gateway
                            .register_adapter("wecom_callback", adapter.clone())
                            .await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            while let Some(incoming) = rx.recv().await {
                                if let Err(err) = gw_clone.route_message(&incoming).await {
                                    tracing::warn!(
                                        "Failed to route wecom_callback message: {}",
                                        err
                                    );
                                }
                            }
                        }));
                    }
                    Err(e) => println!("WeCom callback enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-qqbot")]
    if let Some(platform_cfg) = config
        .platforms
        .get("qqbot")
        .or_else(|| config.platforms.get("qq"))
    {
        if platform_cfg.enabled {
            let app_id = extra_string(platform_cfg, "app_id").unwrap_or_default();
            let client_secret = extra_string(platform_cfg, "client_secret").unwrap_or_default();
            if app_id.is_empty() || client_secret.is_empty() {
                println!(
                    "QQBot is enabled but app_id/client_secret is missing; skipping qqbot adapter."
                );
            } else {
                let qq_cfg = QqBotConfig {
                    app_id,
                    client_secret,
                    markdown_support: extra_bool(platform_cfg, "markdown_support", true),
                    proxy: Default::default(),
                };
                match QqBotAdapter::new(qq_cfg) {
                    Ok(adapter) => gateway.register_adapter("qqbot", Arc::new(adapter)).await,
                    Err(e) => println!("QQBot enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-bluebubbles")]
    if let Some(platform_cfg) = config.platforms.get("bluebubbles") {
        if platform_cfg.enabled {
            let server_url = extra_string(platform_cfg, "server_url").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if server_url.is_empty() || password.is_empty() {
                println!(
                    "BlueBubbles is enabled but server_url/password is missing; skipping bluebubbles adapter."
                );
            } else {
                let bb_cfg = BlueBubblesConfig {
                    server_url,
                    password,
                    proxy: Default::default(),
                };
                match BlueBubblesAdapter::new(bb_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("bluebubbles", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("BlueBubbles enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-email")]
    if let Some(platform_cfg) = config.platforms.get("email") {
        if platform_cfg.enabled {
            let imap_host = extra_string(platform_cfg, "imap_host").unwrap_or_default();
            let smtp_host = extra_string(platform_cfg, "smtp_host").unwrap_or_default();
            let username = extra_string(platform_cfg, "username").unwrap_or_default();
            let password = extra_string(platform_cfg, "password").unwrap_or_default();
            if imap_host.is_empty()
                || smtp_host.is_empty()
                || username.is_empty()
                || password.is_empty()
            {
                println!(
                    "Email is enabled but imap/smtp/username/password is incomplete; skipping email adapter."
                );
            } else {
                let email_cfg = EmailConfig {
                    imap_host,
                    imap_port: extra_u16(platform_cfg, "imap_port", 993),
                    smtp_host,
                    smtp_port: extra_u16(platform_cfg, "smtp_port", 587),
                    username,
                    password,
                    poll_interval_secs: platform_cfg
                        .extra
                        .get("poll_interval_secs")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(60),
                    proxy: Default::default(),
                    require_authenticated_sender: extra_bool_loose(
                        platform_cfg,
                        "require_authenticated_sender",
                    )
                    .unwrap_or(true),
                    authserv_id: extra_string(platform_cfg, "authserv_id"),
                    allowed_users: {
                        let mut users = platform_cfg.allowed_users.clone();
                        for key in GATEWAY_CONFIG_DIRECT_USER_ALLOWLIST_EXTRA_KEYS {
                            users.extend(extra_string_set(platform_cfg, key));
                        }
                        users.extend(env_email_identity_vec("EMAIL_ALLOWED_USERS"));
                        users.extend(env_email_identity_vec("GATEWAY_ALLOWED_USERS"));
                        users
                    },
                    admin_users: platform_cfg.admin_users.clone(),
                    allow_all_users: gateway_platform_config_allows_all_users(platform_cfg)
                        || std::env::var("EMAIL_ALLOW_ALL_USERS")
                            .ok()
                            .is_some_and(|value| env_value_truthy(&value))
                        || std::env::var("GATEWAY_ALLOW_ALL_USERS")
                            .ok()
                            .is_some_and(|value| env_value_truthy(&value)),
                };
                match EmailAdapter::new(email_cfg) {
                    Ok(adapter) => gateway.register_adapter("email", Arc::new(adapter)).await,
                    Err(e) => println!("Email enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-sms")]
    if let Some(platform_cfg) = config.platforms.get("sms") {
        if platform_cfg.enabled {
            let account_sid = extra_string(platform_cfg, "account_sid").unwrap_or_default();
            let auth_token = extra_string(platform_cfg, "auth_token").unwrap_or_default();
            let from_number = extra_string(platform_cfg, "from_number").unwrap_or_default();
            if account_sid.is_empty() || auth_token.is_empty() || from_number.is_empty() {
                println!(
                    "SMS is enabled but account_sid/auth_token/from_number is incomplete; skipping sms adapter."
                );
            } else {
                let sms_cfg = SmsConfig {
                    provider: extra_string(platform_cfg, "provider")
                        .unwrap_or_else(|| "twilio".to_string()),
                    account_sid,
                    auth_token,
                    from_number,
                    proxy: Default::default(),
                };
                match SmsAdapter::new(sms_cfg) {
                    Ok(adapter) => gateway.register_adapter("sms", Arc::new(adapter)).await,
                    Err(e) => println!("SMS enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-homeassistant")]
    if let Some(platform_cfg) = config.platforms.get("homeassistant") {
        if platform_cfg.enabled {
            let base_url = extra_string(platform_cfg, "base_url").unwrap_or_default();
            let long_lived_token = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "long_lived_token"))
                .unwrap_or_default();
            if base_url.is_empty() || long_lived_token.is_empty() {
                println!(
                    "HomeAssistant is enabled but base_url/token is missing; skipping homeassistant adapter."
                );
            } else {
                let ha_cfg = HomeAssistantConfig {
                    base_url,
                    long_lived_token,
                    webhook_id: extra_string(platform_cfg, "webhook_id"),
                    proxy: Default::default(),
                };
                match HomeAssistantAdapter::new(ha_cfg) {
                    Ok(adapter) => {
                        gateway
                            .register_adapter("homeassistant", Arc::new(adapter))
                            .await
                    }
                    Err(e) => println!("HomeAssistant enabled but failed to initialize: {}", e),
                }
            }
        }
    }

    #[cfg(feature = "gateway-ntfy")]
    if let Some(platform_cfg) = config.platforms.get("ntfy") {
        if platform_cfg.enabled {
            let ntfy_cfg = NtfyConfig::from_platform_config(platform_cfg);
            match NtfyAdapter::new(ntfy_cfg) {
                Ok(adapter) => {
                    let adapter = Arc::new(adapter);
                    let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                    adapter.set_inbound_sender(tx).await;
                    gateway.register_adapter("ntfy", adapter).await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        run_gateway_incoming_loop(gw_clone, rx, "ntfy").await;
                    }));
                }
                Err(e) => println!("ntfy enabled but failed to initialize: {}", e),
            }
        }
    }

    #[cfg(feature = "gateway-webhook")]
    if let Some(platform_cfg) = config.platforms.get("webhook") {
        if platform_cfg.enabled {
            let secret = platform_token_or_extra(platform_cfg)
                .or_else(|| extra_string(platform_cfg, "secret"))
                .unwrap_or_default();
            if secret.is_empty() {
                println!("Webhook is enabled but secret is missing; skipping webhook adapter.");
            } else {
                let wh_cfg = build_webhook_config(platform_cfg, secret);
                let adapter = Arc::new(WebhookAdapter::new(wh_cfg));
                let (tx, rx) = mpsc::channel::<WebhookPayload>(512);
                adapter.set_inbound_sender(tx).await;
                gateway.register_adapter("webhook", adapter).await;
                let gw_clone = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    run_webhook_inbound_loop(gw_clone, rx).await;
                }));
            }
        }
    }

    #[cfg(feature = "gateway-api-server")]
    if let Some(platform_cfg) = config
        .platforms
        .get("api_server")
        .or_else(|| config.platforms.get("api-server"))
    {
        if platform_cfg.enabled {
            let api_cfg = build_api_server_config(platform_cfg);
            let adapter = Arc::new(ApiServerAdapter::new(api_cfg.clone()));
            let (tx, rx) = mpsc::channel::<ApiInboundRequest>(256);
            adapter.set_inbound_sender(tx).await;
            gateway.register_adapter("api_server", adapter).await;
            let gw_clone = gateway.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_api_server_inbound_loop(gw_clone, rx).await;
            }));
            println!(
                "API server adapter enabled on {}:{}",
                api_cfg.host, api_cfg.port
            );
        }
    }

    Ok(())
}

#[cfg(feature = "gateway-telegram")]
fn telegram_should_batch_text(msg: &TelegramIncomingMessage) -> bool {
    msg.text
        .as_deref()
        .map(|text| !text.trim().is_empty())
        .unwrap_or(false)
        && !msg.is_voice
        && !msg.is_photo
        && !msg.is_sticker
        && !msg.is_document
        && !msg.is_video
        && msg.callback_query_id.is_none()
        && msg.callback_data.is_none()
}

#[cfg(feature = "gateway-telegram")]
fn telegram_routable_topic_thread(thread_id: Option<i64>) -> Option<i64> {
    match thread_id {
        Some(id) if id > 1 => Some(id),
        _ => None,
    }
}

#[cfg(feature = "gateway-telegram")]
const TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD: u64 = 3;

#[cfg(feature = "gateway-telegram")]
const TELEGRAM_POLLING_STOPPED_RECHECK_MS: u64 = 500;

#[cfg(feature = "gateway-telegram")]
fn telegram_polling_should_pause_for_reconnect(consecutive_errors: u64) -> bool {
    consecutive_errors >= TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD
}

#[cfg(feature = "gateway-telegram")]
fn telegram_gateway_message(msg: TelegramIncomingMessage) -> GatewayIncomingMessage {
    let text = msg.text.unwrap_or_else(|| {
        if msg.is_voice {
            "[voice message]".to_string()
        } else if msg.is_photo {
            "[photo message]".to_string()
        } else if msg.is_video {
            "[video message]".to_string()
        } else {
            "[unsupported message]".to_string()
        }
    });
    let user_id = msg
        .user_id
        .map(|id| id.to_string())
        .or(msg.username)
        .unwrap_or_else(|| "unknown".to_string());

    let thread_id = telegram_routable_topic_thread(msg.message_thread_id);
    let chat_id = match thread_id {
        Some(thread_id) => format!("{}:{}", msg.chat_id, thread_id),
        _ => msg.chat_id.to_string(),
    };

    GatewayIncomingMessage {
        platform: "telegram".to_string(),
        chat_id,
        user_id,
        text,
        message_id: Some(msg.message_id.to_string()),
        thread_id: thread_id.map(|id| id.to_string()),
        is_dm: msg.chat_id > 0,
    }
}

#[cfg(feature = "gateway-telegram")]
async fn route_telegram_message(gateway: &Gateway, msg: TelegramIncomingMessage) {
    let incoming = telegram_gateway_message(msg);
    if let Err(err) = gateway.route_message(&incoming).await {
        tracing::warn!("Failed to route telegram message: {}", err);
    }
}

#[cfg(feature = "gateway-telegram")]
async fn run_telegram_poll_loop(gateway: Arc<Gateway>, adapter: Arc<TelegramAdapter>) {
    let batch_delay = std::time::Duration::from_millis(adapter.config().text_batch_delay_ms);
    let mut text_batcher = TelegramTextBatcher::new(batch_delay);
    let mut webhook_cleared = false;

    loop {
        if !adapter.is_running() {
            webhook_cleared = false;
            tokio::time::sleep(std::time::Duration::from_millis(
                TELEGRAM_POLLING_STOPPED_RECHECK_MS,
            ))
            .await;
            continue;
        }

        if !adapter.config().polling {
            tokio::time::sleep(std::time::Duration::from_millis(
                TELEGRAM_POLLING_STOPPED_RECHECK_MS,
            ))
            .await;
            continue;
        }

        if !webhook_cleared {
            if let Err(err) = adapter.delete_webhook(false).await {
                tracing::warn!("Telegram deleteWebhook before polling failed: {}", err);
            }
            webhook_cleared = true;
        }

        for msg in text_batcher.drain_ready() {
            route_telegram_message(&gateway, msg).await;
        }

        match adapter.poll_with_backoff().await {
            TelegramPollResult::Updates(updates) => {
                for update in updates {
                    if !adapter.should_process_update(&update) {
                        continue;
                    }
                    let Some(msg) = TelegramAdapter::parse_update(&update) else {
                        continue;
                    };

                    if let (Some(callback_id), Some(callback_data)) =
                        (&msg.callback_query_id, &msg.callback_data)
                    {
                        if callback_data.starts_with("approval:") {
                            if let Err(err) = adapter
                                .handle_approval_callback(callback_id, callback_data)
                                .await
                            {
                                tracing::warn!(
                                    "Failed to handle telegram approval callback: {}",
                                    err
                                );
                            }
                            continue;
                        }
                    }

                    if batch_delay.is_zero() || !telegram_should_batch_text(&msg) {
                        route_telegram_message(&gateway, msg).await;
                    } else {
                        text_batcher.enqueue(msg);
                    }
                }
                if text_batcher.pending_len() > 0 && !batch_delay.is_zero() {
                    tokio::time::sleep(batch_delay).await;
                    for msg in text_batcher.drain_ready() {
                        route_telegram_message(&gateway, msg).await;
                    }
                }
            }
            TelegramPollResult::Backoff { error, delay_ms } => {
                let consecutive_errors = adapter.consecutive_error_count();
                if telegram_polling_should_pause_for_reconnect(consecutive_errors)
                    && adapter.polling_reconnect_threshold_reached(
                        TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD,
                    )
                {
                    tracing::warn!(
                        consecutive_errors,
                        error = %error,
                        "Telegram polling exceeded reconnect threshold; pausing poll loop until gateway watcher restarts adapter"
                    );
                    adapter.mark_polling_unhealthy();
                    continue;
                }

                tracing::warn!(
                    consecutive_errors,
                    delay_ms,
                    error = %error,
                    "Telegram polling error; backing off"
                );
                adapter.sleep_backoff().await;
            }
        }
    }
}
