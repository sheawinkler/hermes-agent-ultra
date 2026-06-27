#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::session::SessionConfig;
    use hermes_config::PlatformConfig;
    use hermes_gateway::dm::DmManager;
    use hermes_gateway::{Gateway, SessionManager};
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[test]
    fn gateway_memory_notifications_follow_display_config() {
        let mut cfg = GatewayConfig::default();
        assert!(gateway_memory_notifications_enabled(&cfg));

        cfg.display.memory_notifications = Some(false);
        assert!(!gateway_memory_notifications_enabled(&cfg));

        cfg.display.memory_notifications = Some(true);
        assert!(gateway_memory_notifications_enabled(&cfg));
    }

    fn cli_for_temp_state_root(temp_root: &std::path::Path) -> Cli {
        use clap::Parser;
        Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            temp_root.to_str().expect("utf8 path"),
        ])
    }

    fn test_nous_invoke_jwt(seconds: i64) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine as _;

        let header = serde_json::json!({ "alg": "none", "typ": "JWT" });
        let claims = serde_json::json!({
            "exp": chrono::Utc::now().timestamp() + seconds,
            "scope": "inference:invoke",
        });
        format!(
            "{}.{}.sig",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).expect("header json")),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).expect("claims json"))
        )
    }

    async fn spawn_nous_live_probe_server(status: &str, body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind probe server");
        let addr = listener.local_addr().expect("probe server addr");
        let status = status.to_string();
        tokio::spawn(async move {
            if let Ok((mut stream, _peer)) = listener.accept().await {
                let mut buf = [0_u8; 2048];
                let _ = stream.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        format!("http://{addr}")
    }

    fn make_platform(enabled: bool, token: Option<&str>) -> PlatformConfig {
        let mut cfg = PlatformConfig {
            enabled,
            ..Default::default()
        };
        if let Some(t) = token {
            cfg.token = Some(t.to_string());
        }
        cfg
    }

    fn make_gateway() -> Arc<Gateway> {
        Arc::new(Gateway::new(
            Arc::new(SessionManager::new(SessionConfig::default())),
            DmManager::with_pair_behavior(),
            hermes_gateway::gateway::GatewayConfig::default(),
        ))
    }

    fn assert_gateway_feature_guard(platform: &str, feature_enabled: bool, feature_name: &str) {
        if feature_enabled {
            assert_eq!(missing_gateway_adapter_feature(platform), None);
        } else {
            assert_eq!(
                missing_gateway_adapter_feature(platform),
                Some(feature_name)
            );
        }
    }

    #[test]
    fn gateway_adapter_feature_guard_normalizes_platform_aliases() {
        assert_gateway_feature_guard(
            "telegram",
            cfg!(feature = "gateway-telegram"),
            "gateway-telegram",
        );
        assert_gateway_feature_guard(
            "api-server",
            cfg!(feature = "gateway-api-server"),
            "gateway-api-server",
        );
        assert_gateway_feature_guard(
            "api_server",
            cfg!(feature = "gateway-api-server"),
            "gateway-api-server",
        );
        assert_gateway_feature_guard(
            "wecom_callback",
            cfg!(feature = "gateway-wecom-callback"),
            "gateway-wecom-callback",
        );
        assert_gateway_feature_guard("qq", cfg!(feature = "gateway-qqbot"), "gateway-qqbot");
        assert_eq!(missing_gateway_adapter_feature("unknown-platform"), None);
    }

    #[tokio::test]
    async fn run_model_persists_default_model_to_config_yaml() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        run_model(
            cli.clone(),
            Some("nous:nousresearch/hermes-4-70b".to_string()),
        )
        .await
        .expect("run model");

        let cfg = load_user_config_file(&tmp.path().join("config.yaml")).expect("load config");
        assert_eq!(cfg.model.as_deref(), Some("nous:nousresearch/hermes-4-70b"));
    }

    #[test]
    fn acp_action_from_flags_maps_entry_flags() {
        assert_eq!(
            acp_action_from_flags(None, true, false, false, false).as_deref(),
            Some("check")
        );
        assert_eq!(
            acp_action_from_flags(None, false, true, false, false).as_deref(),
            Some("setup")
        );
        assert_eq!(
            acp_action_from_flags(None, false, false, true, false).as_deref(),
            Some("setup-browser")
        );
        assert_eq!(
            acp_action_from_flags(None, false, false, false, true).as_deref(),
            Some("version")
        );
        assert_eq!(
            acp_action_from_flags(Some("restart".to_string()), false, false, false, false)
                .as_deref(),
            Some("restart")
        );
    }

    #[test]
    fn acp_setup_browser_answer_accepts_only_explicit_yes() {
        assert!(acp_setup_browser_answer_is_yes("y"));
        assert!(acp_setup_browser_answer_is_yes("YES\n"));
        assert!(!acp_setup_browser_answer_is_yes(""));
        assert!(!acp_setup_browser_answer_is_yes("no"));
    }

    #[tokio::test]
    async fn run_portal_rejects_unknown_action() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let err = run_portal(cli, Some("bogus".to_string()))
            .await
            .expect_err("unknown portal actions must fail before auth side effects");
        assert!(err.to_string().contains("Unknown portal action 'bogus'"));
        assert!(err.to_string().contains("hermes-ultra portal info"));
    }

    #[test]
    fn portal_default_runs_setup_alias() {
        for action in [
            None,
            Some(""),
            Some("  "),
            Some("setup"),
            Some("login"),
            Some("auth"),
        ] {
            assert_eq!(
                portal_action_kind(action).expect("setup portal action"),
                PortalActionKind::Setup
            );
        }
    }

    #[test]
    fn portal_setup_dispatches_to_auth_login() {
        assert_eq!(portal_setup_auth_action(), "login");
    }

    #[test]
    fn portal_info_and_status_are_status_aliases() {
        for action in [Some("info"), Some("status"), Some("check")] {
            assert_eq!(
                portal_action_kind(action).expect("info portal action"),
                PortalActionKind::Info
            );
        }
    }

    #[test]
    fn mask_secret_hides_token_body() {
        let raw = "abcdefgh1234567890";
        let masked = mask_secret(raw);
        assert!(!masked.contains(raw));
        assert!(masked.starts_with("abcd"));
        assert!(masked.ends_with("7890"));
        assert!(masked.contains("***"));
    }

    #[cfg(feature = "gateway-api-server")]
    #[test]
    fn api_server_config_defaults_to_loopback() {
        let platform = PlatformConfig {
            enabled: true,
            ..Default::default()
        };
        let cfg = build_api_server_config(&platform);
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8090);
        assert_eq!(cfg.auth_token, None);
    }

    #[cfg(feature = "gateway-api-server")]
    #[test]
    fn api_server_config_honors_overrides_and_token_precedence() {
        let mut platform = PlatformConfig {
            enabled: true,
            token: Some("platform-token".to_string()),
            ..Default::default()
        };
        platform
            .extra
            .insert("host".to_string(), serde_json::json!("0.0.0.0"));
        platform
            .extra
            .insert("port".to_string(), serde_json::json!(9123));
        platform
            .extra
            .insert("auth_token".to_string(), serde_json::json!("extra-token"));

        let cfg = build_api_server_config(&platform);
        assert_eq!(cfg.host, "0.0.0.0");
        assert_eq!(cfg.port, 9123);
        assert_eq!(cfg.auth_token.as_deref(), Some("platform-token"));
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn telegram_gateway_message_preserves_group_topic_in_chat_id() {
        let incoming = TelegramIncomingMessage {
            chat_id: -1001,
            user_id: Some(42),
            username: Some("alice".to_string()),
            text: Some("topic hello".to_string()),
            message_id: 77,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            reply_to_message_id: None,
            message_thread_id: Some(17585),
            chat_type: hermes_gateway::platforms::telegram::ChatKind::Supergroup,
            is_group: true,
            callback_query_id: None,
            callback_data: None,
        };

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "-1001:17585");
        assert_eq!(routed.user_id, "42");
        assert!(!routed.is_dm);
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn telegram_gateway_message_preserves_private_topic_in_chat_id() {
        let incoming = TelegramIncomingMessage {
            chat_id: 208214988,
            user_id: Some(42),
            username: Some("alice".to_string()),
            text: Some("topic hello".to_string()),
            message_id: 77,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            reply_to_message_id: None,
            message_thread_id: Some(17585),
            chat_type: hermes_gateway::platforms::telegram::ChatKind::Private,
            is_group: false,
            callback_query_id: None,
            callback_data: None,
        };

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "208214988:17585");
        assert_eq!(routed.user_id, "42");
        assert!(routed.is_dm);
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn telegram_gateway_message_treats_general_topic_as_root_lobby() {
        let incoming = TelegramIncomingMessage {
            chat_id: 208214988,
            user_id: Some(42),
            username: Some("alice".to_string()),
            text: Some("root lobby".to_string()),
            message_id: 77,
            is_voice: false,
            is_photo: false,
            is_sticker: false,
            is_document: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            reply_to_message_id: None,
            message_thread_id: Some(1),
            chat_type: hermes_gateway::platforms::telegram::ChatKind::Private,
            is_group: false,
            callback_query_id: None,
            callback_data: None,
        };

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "208214988");
        assert!(routed.is_dm);
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn telegram_polling_pause_threshold_matches_reconnect_policy() {
        assert!(!telegram_polling_should_pause_for_reconnect(0));
        assert!(!telegram_polling_should_pause_for_reconnect(
            TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD - 1
        ));
        assert!(telegram_polling_should_pause_for_reconnect(
            TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD
        ));
        assert!(telegram_polling_should_pause_for_reconnect(
            TELEGRAM_POLLING_RECONNECT_ERROR_THRESHOLD + 1
        ));
    }

    #[test]
    fn cron_deliver_config_preserves_chat_id_and_telegram_thread_env() {
        let _guard = env_lock();
        let previous_thread = std::env::var("TELEGRAM_CRON_THREAD_ID").ok();
        std::env::set_var("TELEGRAM_CRON_THREAD_ID", "777");

        let telegram =
            parse_deliver_config("telegram", Some("208214988")).expect("telegram deliver");
        assert_eq!(telegram.target, hermes_cron::DeliverTarget::Telegram);
        assert_eq!(telegram.platform.as_deref(), Some("208214988"));

        let already_threaded =
            parse_deliver_config("telegram:208214988:999", None).expect("threaded telegram");
        assert_eq!(
            already_threaded.target,
            hermes_cron::DeliverTarget::Telegram
        );
        assert_eq!(already_threaded.platform.as_deref(), Some("208214988:999"));

        let slack = parse_deliver_config("slack:C123ABC", None).expect("slack deliver");
        assert_eq!(slack.target, hermes_cron::DeliverTarget::Slack);
        assert_eq!(slack.platform.as_deref(), Some("C123ABC"));

        match previous_thread {
            Some(value) => std::env::set_var("TELEGRAM_CRON_THREAD_ID", value),
            None => std::env::remove_var("TELEGRAM_CRON_THREAD_ID"),
        }
    }

    #[test]
    fn cron_platform_delivery_target_applies_telegram_cron_thread_at_fire_time() {
        let _guard = env_lock();
        let previous_thread = std::env::var("TELEGRAM_CRON_THREAD_ID").ok();
        let previous_home = std::env::var("TELEGRAM_HOME_CHANNEL").ok();
        std::env::set_var("TELEGRAM_CRON_THREAD_ID", "777");
        std::env::set_var("TELEGRAM_HOME_CHANNEL", "208214988");

        let mut explicit_job = hermes_cron::CronJob::new("0 * * * *", "ping");
        explicit_job.deliver = Some(hermes_cron::DeliverConfig {
            target: hermes_cron::DeliverTarget::Telegram,
            platform: Some("208214988:999".to_string()),
        });
        let explicit = hermes_cron::CronCompletionEvent::new(
            &explicit_job,
            "schedule",
            Ok(&hermes_core::AgentResult {
                messages: vec![hermes_core::Message::assistant("done")],
                finished_naturally: true,
                total_turns: 1,
                tool_errors: vec![],
                usage: None,
                ..Default::default()
            }),
        );
        assert_eq!(
            cron_platform_delivery_target(&explicit),
            Some(CronPlatformDeliveryTarget {
                platform: "telegram",
                chat_id: "208214988".to_string(),
                thread_id: Some("999".to_string()),
            })
        );

        let mut home_job = hermes_cron::CronJob::new("0 * * * *", "ping");
        home_job.deliver = Some(hermes_cron::DeliverConfig {
            target: hermes_cron::DeliverTarget::Telegram,
            platform: None,
        });
        let home = hermes_cron::CronCompletionEvent::new(
            &home_job,
            "schedule",
            Ok(&hermes_core::AgentResult {
                messages: vec![hermes_core::Message::assistant("done")],
                finished_naturally: true,
                total_turns: 1,
                tool_errors: vec![],
                usage: None,
                ..Default::default()
            }),
        );
        assert_eq!(
            cron_platform_delivery_target(&home),
            Some(CronPlatformDeliveryTarget {
                platform: "telegram",
                chat_id: "208214988".to_string(),
                thread_id: Some("777".to_string()),
            })
        );

        match previous_thread {
            Some(value) => std::env::set_var("TELEGRAM_CRON_THREAD_ID", value),
            None => std::env::remove_var("TELEGRAM_CRON_THREAD_ID"),
        }
        match previous_home {
            Some(value) => std::env::set_var("TELEGRAM_HOME_CHANNEL", value),
            None => std::env::remove_var("TELEGRAM_HOME_CHANNEL"),
        }
    }

    #[test]
    fn cron_platform_delivery_text_uses_full_output_and_suppresses_silent() {
        let mut job = hermes_cron::CronJob::new("0 * * * *", "ping");
        job.deliver = Some(hermes_cron::DeliverConfig {
            target: hermes_cron::DeliverTarget::Telegram,
            platform: Some("208214988".to_string()),
        });

        let full = "x".repeat(2505);
        let event = hermes_cron::CronCompletionEvent::new(
            &job,
            "schedule",
            Ok(&hermes_core::AgentResult {
                messages: vec![hermes_core::Message::assistant(full.clone())],
                finished_naturally: true,
                total_turns: 1,
                tool_errors: vec![],
                usage: None,
                ..Default::default()
            }),
        );

        assert_eq!(cron_platform_delivery_text(&event), Some(full));
        let json = serde_json::to_value(&event).expect("completion json");
        assert!(json.get("assistant_output").is_none());
        assert_eq!(
            json.get("assistant_snippet")
                .and_then(serde_json::Value::as_str)
                .map(|value| value.chars().count()),
            Some(2001)
        );

        let silent = hermes_cron::CronCompletionEvent::new(
            &job,
            "schedule",
            Ok(&hermes_core::AgentResult {
                messages: vec![hermes_core::Message::assistant("[SILENT] no-op")],
                finished_naturally: true,
                total_turns: 1,
                tool_errors: vec![],
                usage: None,
                ..Default::default()
            }),
        );
        assert_eq!(cron_platform_delivery_text(&silent), None);
    }

    #[test]
    fn auth_provider_aliases_cover_primary_chains() {
        assert_eq!(normalize_auth_provider("tg"), "telegram");
        assert_eq!(normalize_auth_provider("wechat"), "weixin");
        assert_eq!(normalize_auth_provider("wx"), "weixin");
        assert_eq!(normalize_auth_provider("claude"), "anthropic");
        assert_eq!(normalize_auth_provider("codex"), "openai-codex");
        assert_eq!(normalize_auth_provider("openai-oauth"), "openai");
        assert_eq!(normalize_auth_provider("qwen-cli"), "qwen-oauth");
        assert_eq!(normalize_auth_provider("gemini-cli"), "google-gemini-cli");
        assert_eq!(normalize_auth_provider("google-ai-studio"), "gemini");
        assert_eq!(normalize_auth_provider("step-plan"), "stepfun");
        assert_eq!(normalize_auth_provider("aigateway"), "ai-gateway");
        assert_eq!(normalize_auth_provider("moonshot"), "kimi-coding");
        assert_eq!(normalize_auth_provider("z-ai"), "zai");
        assert_eq!(normalize_auth_provider("grok"), "xai");
        assert_eq!(normalize_auth_provider("hf"), "huggingface");
        assert_eq!(normalize_auth_provider("github-models"), "copilot");
        assert_eq!(normalize_auth_provider("copilot-acp-agent"), "copilot-acp");
        assert_eq!(normalize_auth_provider("gmicloud"), "gmi");
        assert_eq!(normalize_auth_provider("arcee-ai"), "arcee");
        assert_eq!(normalize_auth_provider("mimo"), "xiaomi");
        assert_eq!(normalize_auth_provider("tencent-cloud"), "tencent-tokenhub");
        assert_eq!(normalize_auth_provider("ollama"), "ollama-local");
        assert_eq!(normalize_auth_provider("llama.cpp"), "llama-cpp");
        assert_eq!(normalize_auth_provider("ollvm"), "vllm");
        assert_eq!(normalize_auth_provider("llvm"), "vllm");
        assert_eq!(normalize_auth_provider("mlx-lm"), "mlx");
        assert_eq!(normalize_auth_provider("ane"), "apple-ane");
        assert_eq!(normalize_auth_provider("text-generation-inference"), "tgi");
        assert_eq!(normalize_auth_provider("api-server"), "api_server");
        assert_eq!(normalize_auth_provider("mm"), "mattermost");
    }

    #[test]
    fn oneshot_auto_verify_provider_detects_nous_401_errors() {
        let err = AgentError::LlmApi(
            "API error 401 Unauthorized: https://portal.nousresearch.com".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, Some("nous"), Some("nous:openai/gpt-5.5")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, None, Some("nous:moonshotai/kimi-k2.6")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&err, None, None),
            Some("nous".to_string())
        );
    }

    #[test]
    fn oneshot_auto_verify_provider_supports_core_oauth_providers() {
        let openai = AgentError::LlmApi("API error 401 Unauthorized: auth.openai.com".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&openai, Some("openai"), Some("openai:gpt-5.5")),
            Some("openai".to_string())
        );
        let codex = AgentError::LlmApi("API error 401 Unauthorized: chatgpt.com codex".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&codex, None, Some("openai-codex:codex-mini")),
            Some("openai-codex".to_string())
        );
        let anthropic = AgentError::LlmApi(
            "API error 401 Unauthorized: console.anthropic.com token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&anthropic, Some("claude"), None),
            Some("anthropic".to_string())
        );
        let gemini = AgentError::LlmApi(
            "API error 401 Unauthorized: oauth2.googleapis.com invalid_grant".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&gemini, Some("gemini-cli"), None),
            Some("google-gemini-cli".to_string())
        );
        let qwen = AgentError::LlmApi(
            "API error 401 Unauthorized: chat.qwen.ai token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(&qwen, Some("qwen-cli"), None),
            Some("qwen-oauth".to_string())
        );
    }

    #[test]
    fn oneshot_auto_verify_provider_ignores_non_oauth_or_non_auth_errors() {
        let not_auth = AgentError::LlmApi("API error 404 Not Found".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &not_auth,
                Some("nous"),
                Some("nous:openai/gpt-5.5")
            ),
            None
        );

        let other_provider = AgentError::LlmApi(
            "API error 401 Unauthorized: provider openrouter token expired".to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &other_provider,
                Some("openrouter"),
                Some("openrouter:openai/gpt-4o")
            ),
            None
        );

        let missing_signal = AgentError::LlmApi("API error 500 Internal Server Error".to_string());
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &missing_signal,
                Some("openai"),
                Some("openai:gpt-5.5")
            ),
            None
        );
    }

    #[test]
    fn oneshot_auth_is_refreshable_detects_auth_signals() {
        assert!(oneshot_auth_is_refreshable(
            "api error 401 unauthorized token expired"
        ));
        assert!(oneshot_auth_is_refreshable("invalid_grant"));
        assert!(!oneshot_auth_is_refreshable("api error 404 not found"));
    }

    #[test]
    fn infer_oauth_provider_from_error_message_maps_known_hosts() {
        assert_eq!(
            infer_oauth_provider_from_error_message("portal.nousresearch.com unauthorized"),
            Some("nous".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("auth.openai.com unauthorized"),
            Some("openai".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("chatgpt.com codex token expired"),
            Some("openai-codex".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("console.anthropic.com invalid token"),
            Some("anthropic".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("oauth2.googleapis.com invalid_grant"),
            Some("google-gemini-cli".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("chat.qwen.ai invalid token"),
            Some("qwen-oauth".to_string())
        );
        assert_eq!(
            infer_oauth_provider_from_error_message("openrouter.ai unauthorized"),
            None
        );
    }

    #[test]
    fn resolve_auth_type_prefers_oauth_for_supported_providers() {
        assert_eq!(resolve_auth_type_for_provider("nous", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("openai-codex", None),
            "oauth"
        );
        assert_eq!(resolve_auth_type_for_provider("qwen-oauth", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("google-gemini-cli", None),
            "oauth"
        );
        assert_eq!(resolve_auth_type_for_provider("anthropic", None), "oauth");
        assert_eq!(resolve_auth_type_for_provider("openai", None), "oauth");
        assert_eq!(
            resolve_auth_type_for_provider("openai", Some("API-KEY")),
            "api_key"
        );
        assert_eq!(
            resolve_auth_type_for_provider("openai", Some("oauth")),
            "oauth"
        );
    }

    #[test]
    fn oauth_refresh_config_defaults_cover_core_oauth_providers() {
        let _guard = env_lock();
        std::env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID");
        std::env::remove_var("HERMES_OPENAI_CODEX_OAUTH_CLIENT_ID");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_TOKEN_URL");
        std::env::remove_var("HERMES_ANTHROPIC_OAUTH_CLIENT_ID");

        let (openai_token_url, openai_client_id) =
            oauth_refresh_config_for_provider("openai").expect("openai config");
        assert_eq!(openai_token_url, CODEX_OAUTH_TOKEN_URL);
        assert_eq!(openai_client_id, CODEX_OAUTH_CLIENT_ID);

        let (codex_token_url, codex_client_id) =
            oauth_refresh_config_for_provider("openai-codex").expect("codex config");
        assert_eq!(codex_token_url, CODEX_OAUTH_TOKEN_URL);
        assert_eq!(codex_client_id, CODEX_OAUTH_CLIENT_ID);

        let (anthropic_token_url, anthropic_client_id) =
            oauth_refresh_config_for_provider("anthropic").expect("anthropic config");
        assert_eq!(anthropic_token_url, ANTHROPIC_OAUTH_TOKEN_URL);
        assert_eq!(anthropic_client_id, ANTHROPIC_OAUTH_CLIENT_ID);

        assert!(oauth_refresh_config_for_provider("nous").is_none());
    }

    #[test]
    fn auth_verify_source_priority_is_env_then_store_then_state() {
        assert_eq!(auth_verify_source(true, true, true), "env");
        assert_eq!(auth_verify_source(false, true, true), "token_store");
        assert_eq!(auth_verify_source(false, false, true), "auth_json");
        assert_eq!(auth_verify_source(false, false, false), "none");
    }

    #[test]
    fn provider_env_var_maps_stepfun() {
        assert_eq!(provider_env_var("stepfun"), Some("STEPFUN_API_KEY"));
        assert_eq!(provider_env_var("step"), None);
        assert_eq!(
            provider_env_var("openai-codex"),
            Some("HERMES_OPENAI_CODEX_API_KEY")
        );
        assert_eq!(
            provider_env_var("qwen-oauth"),
            Some("HERMES_QWEN_OAUTH_API_KEY")
        );
        assert_eq!(provider_env_var("kimi-coding"), Some("KIMI_CODING_API_KEY"));
        assert_eq!(provider_env_var("kimi"), Some("KIMI_API_KEY"));
        assert_eq!(provider_env_var("copilot"), Some("COPILOT_GITHUB_TOKEN"));
        assert_eq!(
            secret_provider_aliases("copilot"),
            vec!["copilot", "github-copilot", "github-models"]
        );
        assert_eq!(provider_env_var("gmi-cloud"), Some("GMI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("gmi"),
            vec!["gmi", "gmi-cloud", "gmicloud"]
        );
        assert_eq!(provider_env_var("arcee-ai"), Some("ARCEEAI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("arcee"),
            vec!["arcee", "arcee-ai", "arceeai"]
        );
        assert_eq!(provider_env_var("mimo"), Some("XIAOMI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("xiaomi"),
            vec!["xiaomi", "mimo", "xiaomi-mimo"]
        );
        assert_eq!(provider_env_var("tokenhub"), Some("TOKENHUB_API_KEY"));
        assert_eq!(
            secret_provider_aliases("tencent"),
            vec![
                "tencent-tokenhub",
                "tencent",
                "tokenhub",
                "tencent-cloud",
                "tencentmaas"
            ]
        );
        assert_eq!(
            provider_env_var("google-gemini-cli"),
            Some("HERMES_GEMINI_OAUTH_API_KEY")
        );
        assert_eq!(secret_provider_aliases("stepfun"), vec!["stepfun", "step"]);
        assert_eq!(
            secret_provider_aliases("claude"),
            vec!["anthropic", "claude", "claude-code"]
        );
        assert_eq!(provider_env_var("bedrock"), None);
        assert_eq!(
            secret_provider_aliases("aws-bedrock"),
            vec!["bedrock", "aws", "aws-bedrock", "amazon-bedrock", "amazon"]
        );
        assert_eq!(provider_env_var("ollama"), Some("OLLAMA_LOCAL_API_KEY"));
        assert_eq!(provider_env_var("llama.cpp"), Some("LLAMA_CPP_API_KEY"));
        assert_eq!(provider_env_var("llamafile"), Some("LLAMA_CPP_API_KEY"));
        assert_eq!(provider_env_var("ollvm"), Some("VLLM_API_KEY"));
        assert_eq!(provider_env_var("mlx-lm"), Some("MLX_API_KEY"));
        assert_eq!(provider_env_var("vmlx"), Some("MLX_API_KEY"));
        assert_eq!(provider_env_var("omlx"), Some("MLX_API_KEY"));
        assert_eq!(provider_env_var("ane"), Some("APPLE_ANE_API_KEY"));
        assert_eq!(
            provider_env_var("text-generation-inference"),
            Some("TGI_API_KEY")
        );
        assert_eq!(provider_env_var("lm-studio"), Some("LMSTUDIO_API_KEY"));
        assert_eq!(provider_env_var("lm-deploy"), Some("LMDEPLOY_API_KEY"));
        assert_eq!(provider_env_var("local-ai"), Some("LOCALAI_API_KEY"));
        assert_eq!(provider_env_var("kobold-cpp"), Some("KOBOLDCPP_API_KEY"));
        assert_eq!(
            provider_env_var("oobabooga"),
            Some("TEXT_GENERATION_WEBUI_API_KEY")
        );
        assert_eq!(provider_env_var("exllamav2"), Some("TABBYAPI_API_KEY"));
        assert_eq!(
            secret_provider_aliases("text-generation-webui"),
            vec!["text-generation-webui", "textgen-webui", "oobabooga"]
        );
        assert_eq!(
            secret_provider_aliases("tabby-api"),
            vec!["tabbyapi", "tabby-api", "exllama", "exllamav2"]
        );
    }

    #[test]
    fn matrix_home_room_prefers_platform_config_then_env_fallback() {
        let _guard = env_lock();
        let previous = std::env::var("MATRIX_HOME_ROOM").ok();

        let mut platform = PlatformConfig::default();
        platform
            .extra
            .insert("room_id".to_string(), serde_json::json!("!cfg:matrix.org"));
        std::env::set_var("MATRIX_HOME_ROOM", "!env:matrix.org");
        assert_eq!(
            matrix_home_room_for_platform(&platform).as_deref(),
            Some("!cfg:matrix.org")
        );

        platform.extra.remove("room_id");
        assert_eq!(
            matrix_home_room_for_platform(&platform).as_deref(),
            Some("!env:matrix.org")
        );

        match previous {
            Some(value) => std::env::set_var("MATRIX_HOME_ROOM", value),
            None => std::env::remove_var("MATRIX_HOME_ROOM"),
        }
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn build_telegram_config_reads_reply_secret_and_reactions() {
        let _guard = env_lock();
        let previous_secret = std::env::var("TELEGRAM_WEBHOOK_SECRET").ok();
        std::env::set_var("TELEGRAM_WEBHOOK_SECRET", "env-secret");

        let mut platform = PlatformConfig {
            webhook_url: Some("https://hooks.example.com/tg".to_string()),
            ..PlatformConfig::default()
        };
        platform
            .extra
            .insert("reply_to_mode".to_string(), serde_json::json!("all"));
        platform
            .extra
            .insert("reactions".to_string(), serde_json::json!(true));
        platform
            .extra
            .insert("disable_link_previews".to_string(), serde_json::json!(true));
        platform
            .extra
            .insert("rich_messages".to_string(), serde_json::json!(true));
        platform.extra.insert(
            "command_menu".to_string(),
            serde_json::json!({
                "enabled": false,
                "max_commands": 12,
                "priority": ["status", "model"],
                "priority_mode": "replace"
            }),
        );
        platform.extra.insert(
            "fallback_ips".to_string(),
            serde_json::json!("149.154.167.220,::1"),
        );
        platform.require_mention = Some(true);
        platform
            .extra
            .insert("guest_mode".to_string(), serde_json::json!(true));
        platform.extra.insert(
            "free_response_chats".to_string(),
            serde_json::json!(["-100", "-101"]),
        );
        platform
            .extra
            .insert("allowed_chats".to_string(), serde_json::json!("-200, -201"));
        platform.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-300", "-301"]),
        );
        platform
            .extra
            .insert("ignored_threads".to_string(), serde_json::json!([31, "32"]));
        platform
            .extra
            .insert("allowed_topics".to_string(), serde_json::json!([8, "0"]));
        platform.extra.insert(
            "mention_patterns".to_string(),
            serde_json::json!(["^\\s*chompy\\b", "@hermes"]),
        );
        platform.extra.insert(
            "exclusive_bot_mentions".to_string(),
            serde_json::json!(true),
        );
        platform.extra.insert(
            "observe_unmentioned_group_messages".to_string(),
            serde_json::json!(true),
        );
        platform
            .extra
            .insert("text_batch_delay_ms".to_string(), serde_json::json!(125));

        let cfg = build_telegram_config(&platform, "token".to_string());
        assert_eq!(
            cfg.webhook_url.as_deref(),
            Some("https://hooks.example.com/tg")
        );
        assert_eq!(cfg.webhook_secret.as_deref(), Some("env-secret"));
        assert_eq!(cfg.reply_to_mode, "all");
        assert!(cfg.reactions);
        assert!(cfg.disable_link_previews);
        assert!(cfg.rich_messages);
        assert!(!cfg.command_menu_enabled);
        assert_eq!(cfg.command_menu_max_commands, 12);
        assert_eq!(cfg.command_menu_priority, vec!["status", "model"]);
        assert_eq!(cfg.command_menu_priority_mode, "replace");
        assert_eq!(cfg.fallback_ips, vec!["149.154.167.220", "::1"]);
        assert!(cfg.require_mention);
        assert!(cfg.guest_mode);
        assert_eq!(cfg.free_response_chats, vec!["-100", "-101"]);
        assert_eq!(cfg.allowed_chats, vec!["-200", "-201"]);
        assert_eq!(cfg.group_allowed_chats, vec!["-300", "-301"]);
        assert_eq!(cfg.ignored_threads, vec!["31", "32"]);
        assert_eq!(cfg.allowed_topics, vec!["8", "0"]);
        assert_eq!(cfg.mention_patterns, vec![r"^\s*chompy\b", "@hermes"]);
        assert!(cfg.exclusive_bot_mentions);
        assert!(cfg.observe_unmentioned_group_messages);
        assert_eq!(cfg.text_batch_delay_ms, 125);

        match previous_secret {
            Some(value) => std::env::set_var("TELEGRAM_WEBHOOK_SECRET", value),
            None => std::env::remove_var("TELEGRAM_WEBHOOK_SECRET"),
        }
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn build_telegram_config_maps_yaml_boolean_off_reply_mode() {
        let mut platform = PlatformConfig::default();
        platform
            .extra
            .insert("reply_to_mode".to_string(), serde_json::json!(false));

        let cfg = build_telegram_config(&platform, "token".to_string());
        assert_eq!(cfg.reply_to_mode, "off");
        assert!(!cfg.rich_messages);
        assert!(cfg.command_menu_enabled);
        assert_eq!(cfg.command_menu_max_commands, 60);
        assert!(cfg.command_menu_priority.is_empty());
        assert_eq!(cfg.command_menu_priority_mode, "prepend");
    }

    #[test]
    fn gateway_platform_access_policy_reads_discord_channel_lists() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut discord = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        discord
            .extra
            .insert("allowed_channels".to_string(), serde_json::json!("111, *"));
        discord.extra.insert(
            "ignored_channels".to_string(),
            serde_json::json!(["222", 333]),
        );
        config.platforms.insert("discord".to_string(), discord);

        let policies = build_gateway_platform_access_policies(&config);
        let policy = policies.get("discord").expect("discord policy");
        assert!(policy.allowed_channels.contains("111"));
        assert!(policy.allowed_channels.contains("*"));
        assert!(policy.ignored_channels.contains("222"));
        assert!(policy.ignored_channels.contains("333"));
    }

    #[test]
    fn gateway_platform_access_policy_reads_telegram_chat_lists() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram
            .extra
            .insert("allowed_chats".to_string(), serde_json::json!("-100, *"));
        telegram.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-200", -300]),
        );
        telegram
            .extra
            .insert("ignored_threads".to_string(), serde_json::json!(["31", 32]));
        config.platforms.insert("telegram".to_string(), telegram);

        let policies = build_gateway_platform_access_policies(&config);
        let policy = policies.get("telegram").expect("telegram policy");
        assert!(policy.allowed_channels.contains("-100"));
        assert!(policy.allowed_channels.contains("*"));
        assert!(policy.authorized_group_chats.contains("-200"));
        assert!(policy.authorized_group_chats.contains("-300"));
        assert!(policy.ignored_channels.contains("31"));
        assert!(policy.ignored_channels.contains("32"));
    }

    #[test]
    fn gateway_platform_access_policy_reads_dingtalk_and_matrix_aliases() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut dingtalk = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        dingtalk.extra.insert(
            "allowed_chats".to_string(),
            serde_json::json!("cidABC,cidDEF"),
        );
        config.platforms.insert("dingtalk".to_string(), dingtalk);

        let mut matrix = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        matrix.extra.insert(
            "allowed_rooms".to_string(),
            serde_json::json!(["!room1:srv", "!room2:srv"]),
        );
        config.platforms.insert("matrix".to_string(), matrix);

        let policies = build_gateway_platform_access_policies(&config);
        let dingtalk = policies.get("dingtalk").expect("dingtalk policy");
        assert!(dingtalk.allowed_channels.contains("cidABC"));
        assert!(dingtalk.allowed_channels.contains("cidDEF"));
        let matrix = policies.get("matrix").expect("matrix policy");
        assert!(matrix.allowed_channels.contains("!room1:srv"));
        assert!(matrix.allowed_channels.contains("!room2:srv"));
    }

    #[tokio::test]
    async fn gateway_dm_manager_scopes_configured_allowlists_by_platform() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram.allowed_users = vec!["123".to_string()];
        config.platforms.insert("telegram".to_string(), telegram);

        let dm = build_gateway_dm_manager(&config);
        assert_eq!(
            dm.handle_dm("123", "telegram").await,
            hermes_gateway::DmDecision::Allow
        );
        assert!(matches!(
            dm.handle_dm("123", "discord").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
        assert_eq!(
            dm.handle_dm("999", "telegram").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_allows_explicit_pair_with_allowlist() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut signal = PlatformConfig {
            enabled: true,
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
            ..PlatformConfig::default()
        };
        signal.allowed_users = vec!["+15550000001".to_string()];
        config.platforms.insert("signal".to_string(), signal);

        let dm = build_gateway_dm_manager(&config);
        assert!(matches!(
            dm.handle_dm("+15559999999", "signal").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
    }

    #[tokio::test]
    async fn gateway_dm_manager_global_allowlist_ignores_unauthorized_dm() {
        let mut config = hermes_config::GatewayConfig::default();
        let signal = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        config.platforms.insert("signal".to_string(), signal);
        let env = std::collections::HashMap::from([(
            "GATEWAY_ALLOWED_USERS".to_string(),
            "111111111".to_string(),
        )]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("111111111", "signal").await,
            hermes_gateway::DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm("+15559999999", "signal").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_dm_policy_pairing_overrides_global_allowlist_ignore() {
        let mut config = hermes_config::GatewayConfig::default();
        config.platforms.insert(
            "wecom".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([
            (
                "GATEWAY_ALLOWED_USERS".to_string(),
                "admin-user".to_string(),
            ),
            ("WECOM_DM_POLICY".to_string(), "pairing".to_string()),
        ]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("admin-user", "wecom").await,
            hermes_gateway::DmDecision::Allow
        );
        assert!(matches!(
            dm.handle_dm("stranger", "wecom").await,
            hermes_gateway::DmDecision::Pair { .. }
        ));
    }

    #[tokio::test]
    async fn gateway_dm_manager_dm_policy_allowlist_denies_unlisted_sender_without_pairing() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut weixin = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        weixin.allowed_users = vec!["known-user".to_string()];
        weixin
            .extra
            .insert("dm_policy".to_string(), serde_json::json!("allowlist"));
        config.platforms.insert("weixin".to_string(), weixin);

        let dm = build_gateway_dm_manager_with_lookup(&config, |_key| None);
        assert_eq!(
            dm.handle_dm("known-user", "weixin").await,
            hermes_gateway::DmDecision::Allow
        );
        assert_eq!(
            dm.handle_dm("stranger", "weixin").await,
            hermes_gateway::DmDecision::Deny
        );
    }

    #[tokio::test]
    async fn gateway_dm_manager_group_authorization_matches_upstream_contract() {
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        telegram.extra.insert(
            "group_allow_from".to_string(),
            serde_json::json!(["999", "-1001878443972"]),
        );
        telegram.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["-200"]),
        );
        config.platforms.insert("telegram".to_string(), telegram);
        let mut qq = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        qq.extra.insert(
            "group_allowed_chats".to_string(),
            serde_json::json!(["group-openid-1"]),
        );
        config.platforms.insert("qq".to_string(), qq);

        let dm = build_gateway_dm_manager_with_lookup(&config, |_key| None);
        assert!(dm.is_authorized_source("telegram", "999", "-1009999999999", false));
        assert!(dm.is_authorized_source("telegram", "123", "-1001878443972", false));
        assert!(dm.is_authorized_source("telegram", "123", "-200", false));
        assert!(!dm.is_authorized_source("telegram", "999", "999", true));
        assert_eq!(
            dm.handle_dm("999", "telegram").await,
            hermes_gateway::DmDecision::Deny
        );
        assert!(dm.is_authorized_source("qqbot", "member-openid-999", "group-openid-1", false));
        assert!(!dm.is_authorized_source("qqbot", "member-openid-999", "group-openid-2", false));
    }

    #[tokio::test]
    async fn gateway_dm_manager_whatsapp_lid_mapping_authorizes_phone_allowlist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let session_dir = tmp.path().join("whatsapp").join("session");
        std::fs::create_dir_all(&session_dir).expect("session dir");
        std::fs::write(
            session_dir.join("lid-mapping-15550000001.json"),
            "\"900000000000001\"",
        )
        .expect("forward mapping");
        std::fs::write(
            session_dir.join("lid-mapping-900000000000001_reverse.json"),
            "\"15550000001\"",
        )
        .expect("reverse mapping");

        let mut config = hermes_config::GatewayConfig {
            home_dir: Some(tmp.path().to_string_lossy().to_string()),
            ..hermes_config::GatewayConfig::default()
        };
        config.platforms.insert(
            "whatsapp".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([(
            "WHATSAPP_ALLOWED_USERS".to_string(),
            "15550000001".to_string(),
        )]);

        let dm = build_gateway_dm_manager_with_lookup(&config, |key| env.get(key).cloned());
        assert_eq!(
            dm.handle_dm("900000000000001@lid", "whatsapp").await,
            hermes_gateway::DmDecision::Allow
        );
    }

    #[test]
    fn gateway_platform_access_policy_group_authorization_matches_env_contract() {
        let mut config = hermes_config::GatewayConfig::default();
        config.platforms.insert(
            "telegram".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        config.platforms.insert(
            "qqbot".to_string(),
            PlatformConfig {
                enabled: true,
                ..PlatformConfig::default()
            },
        );
        let env = std::collections::HashMap::from([
            (
                "TELEGRAM_GROUP_ALLOWED_USERS".to_string(),
                "999,-1001878443972".to_string(),
            ),
            (
                "TELEGRAM_GROUP_ALLOWED_CHATS".to_string(),
                "-200".to_string(),
            ),
            (
                "QQ_GROUP_ALLOWED_USERS".to_string(),
                "group-openid-1".to_string(),
            ),
        ]);

        let policies = build_gateway_platform_access_policies_with_lookup(&config, |key| {
            env.get(key).cloned()
        });
        let telegram = policies.get("telegram").expect("telegram policy");
        assert_eq!(telegram.group_mode, GroupAccessMode::Allowlist);
        assert!(telegram.allowed_users.contains("999"));
        assert!(telegram.authorized_group_chats.contains("-1001878443972"));
        assert!(telegram.authorized_group_chats.contains("-200"));
        let qq = policies.get("qqbot").expect("qqbot policy");
        assert_eq!(qq.group_mode, GroupAccessMode::Allowlist);
        assert!(qq.authorized_group_chats.contains("group-openid-1"));
    }

    #[test]
    fn gateway_allowlist_startup_warning_matches_env_contract() {
        let env_lookup = |pairs: &[(&str, &str)]| {
            let env = pairs
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect::<std::collections::HashMap<_, _>>();
            gateway_allowlist_startup_would_warn_from_lookup(|key| env.get(key).cloned())
        };

        assert!(env_lookup(&[]));
        assert!(!env_lookup(&[("SIGNAL_GROUP_ALLOWED_USERS", "user1")]));
        assert!(!env_lookup(&[("TELEGRAM_ALLOW_ALL_USERS", "true")]));
        assert!(!env_lookup(&[("GATEWAY_ALLOW_ALL_USERS", "yes")]));
        assert!(env_lookup(&[("GATEWAY_ALLOW_ALL_USERS", "no")]));

        let empty_env = |_key: &str| -> Option<String> { None };
        let mut config = hermes_config::GatewayConfig::default();
        let mut telegram = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        assert!(gateway_allowlist_startup_would_warn_with_lookup(
            &config, empty_env
        ));

        telegram.allowed_users = vec!["123".to_string()];
        config.platforms.insert("telegram".to_string(), telegram);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &config, empty_env
        ));

        let mut group_config = hermes_config::GatewayConfig::default();
        let mut signal = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        signal.extra.insert(
            "group_allow_from".to_string(),
            serde_json::json!(["+15550000001"]),
        );
        group_config.platforms.insert("signal".to_string(), signal);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &group_config,
            empty_env
        ));

        let mut allow_all_config = hermes_config::GatewayConfig::default();
        let mut discord = PlatformConfig {
            enabled: true,
            ..PlatformConfig::default()
        };
        discord
            .extra
            .insert("allow_all_users".to_string(), serde_json::json!(true));
        allow_all_config
            .platforms
            .insert("discord".to_string(), discord);
        assert!(!gateway_allowlist_startup_would_warn_with_lookup(
            &allow_all_config,
            empty_env
        ));
    }

    #[test]
    fn setup_model_choice_supports_nous() {
        let option = &SETUP_MODEL_OPTIONS[default_setup_model_choice().saturating_sub(1)];
        assert_eq!(option.model, "nous:openai/gpt-5.5-pro");
        assert_eq!(option.provider, "nous");
    }

    #[test]
    fn setup_provider_defaults_are_unique_and_include_nous() {
        let providers = setup_provider_defaults();
        assert!(!providers.is_empty());
        let mut seen = std::collections::BTreeSet::new();
        for option in providers {
            assert!(
                seen.insert(option.provider),
                "duplicate provider {}",
                option.provider
            );
        }
        assert!(seen.contains("nous"));
        assert!(seen.contains("nous-api"));
        assert!(seen.contains("lmstudio"));
        assert!(seen.contains("lmdeploy"));
        assert!(seen.contains("localai"));
        assert!(seen.contains("koboldcpp"));
        assert!(seen.contains("text-generation-webui"));
        assert!(seen.contains("tabbyapi"));
    }

    #[test]
    fn setup_minimax_defaults_use_m3_frontier_model() {
        let providers = setup_provider_defaults();
        let minimax = providers
            .iter()
            .find(|option| option.provider == "minimax")
            .expect("minimax setup option");
        let minimax_cn = providers
            .iter()
            .find(|option| option.provider == "minimax-cn")
            .expect("minimax-cn setup option");

        assert_eq!(minimax.model, "minimax:MiniMax-M3");
        assert_eq!(minimax_cn.model, "minimax-cn:MiniMax-M3");
        assert!(!minimax.model.to_ascii_lowercase().contains("highspeed"));
        assert!(!minimax_cn.model.to_ascii_lowercase().contains("highspeed"));
    }

    #[test]
    fn setup_xai_defaults_to_grok_build() {
        let providers = setup_provider_defaults();
        let xai = providers
            .iter()
            .find(|option| option.provider == "xai")
            .expect("xai setup option");

        assert_eq!(xai.model, "xai:grok-build-0.1");
        assert_eq!(
            setup_provider_default_base_url("xai"),
            Some("https://api.x.ai/v1")
        );
        assert_eq!(setup_provider_env_keys("xai"), &["XAI_API_KEY"]);
    }

    #[test]
    fn setup_default_model_pick_index_matches_provider_prefixed_target() {
        let suggested = vec![
            "nousresearch/hermes-3-llama-3.1-405b".to_string(),
            "openai/gpt-5.5-pro".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
        ];
        let idx = setup_default_model_pick_index("nous", "nous:openai/gpt-5.5-pro", &suggested);
        assert_eq!(idx, 1);
    }

    #[test]
    fn setup_default_model_pick_index_uses_nous_kimi_fallback_when_target_missing() {
        let suggested = vec![
            "nousresearch/hermes-3-llama-3.1-405b".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "openai/gpt-5.5".to_string(),
        ];
        let idx = setup_default_model_pick_index("nous", "nous:nonexistent/model", &suggested);
        assert_eq!(idx, 1);
        let idx =
            setup_default_model_pick_index("nous-api", "nous-api:nonexistent/model", &suggested);
        assert_eq!(idx, 1);
    }

    #[test]
    fn setup_default_model_pick_index_falls_back_to_zero_for_non_nous() {
        let suggested = vec![
            "gpt-4o".to_string(),
            "gpt-4o-mini".to_string(),
            "gpt-5.4".to_string(),
        ];
        let idx = setup_default_model_pick_index("openai", "openai:not-real", &suggested);
        assert_eq!(idx, 0);
    }

    #[test]
    fn setup_provider_env_keys_include_nous() {
        assert_eq!(setup_provider_display("nous"), "Nous");
        assert_eq!(setup_provider_env_keys("nous"), &["NOUS_API_KEY"]);
        assert_eq!(setup_provider_display("nous-api"), "Nous Portal API");
        assert_eq!(setup_provider_env_keys("nous-api"), &["NOUS_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("nous-api"),
            Some(DEFAULT_NOUS_INFERENCE_URL)
        );
        assert_eq!(
            setup_provider_env_keys("kimi-coding"),
            &["KIMI_CODING_API_KEY", "KIMI_API_KEY", "MOONSHOT_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("kimi-coding"),
            Some(provider_profiles::KIMI_CODE_BASE_URL)
        );
        assert_eq!(
            setup_provider_env_keys("ollama-local"),
            &["OLLAMA_LOCAL_API_KEY", "OLLAMA_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("vllm"),
            Some("http://127.0.0.1:8000/v1")
        );
        assert!(!setup_provider_requires_api_key("ollama-local"));
        assert!(!setup_provider_requires_api_key("apple-ane"));
        assert!(!setup_provider_requires_api_key("lmstudio"));
        assert!(!setup_provider_requires_api_key("lmdeploy"));
        assert!(!setup_provider_requires_api_key("localai"));
        assert!(!setup_provider_requires_api_key("koboldcpp"));
        assert!(!setup_provider_requires_api_key("text-generation-webui"));
        assert!(!setup_provider_requires_api_key("tabbyapi"));
        assert!(!setup_provider_requires_api_key("bedrock"));
        assert_eq!(setup_provider_display("bedrock"), "AWS Bedrock");
        assert_eq!(
            setup_provider_env_keys("bedrock"),
            &[
                "AWS_ACCESS_KEY_ID",
                "AWS_SECRET_ACCESS_KEY",
                "AWS_SESSION_TOKEN"
            ]
        );
        assert!(setup_provider_requires_api_key("openai"));
        assert_eq!(setup_provider_display("alibaba"), "Alibaba Cloud DashScope");
        assert_eq!(
            setup_provider_env_keys("google-gemini-cli"),
            &["HERMES_GEMINI_OAUTH_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("gemini"),
            Some(provider_profiles::GEMINI_OPENAI_BASE_URL)
        );
        assert_eq!(
            setup_provider_env_keys("copilot"),
            &["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
        );
        assert_eq!(
            setup_provider_default_base_url("copilot"),
            Some("https://api.githubcopilot.com")
        );
        assert_eq!(setup_provider_display("gmi"), "GMI Cloud");
        assert_eq!(setup_provider_env_keys("gmi"), &["GMI_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("gmi"),
            Some("https://api.gmi-serving.com/v1")
        );
        assert_eq!(
            setup_provider_display("tencent-tokenhub"),
            "Tencent TokenHub"
        );
        assert_eq!(
            setup_provider_env_keys("tencent-tokenhub"),
            &["TOKENHUB_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("tencent-tokenhub"),
            Some("https://tokenhub.tencentmaas.com/v1")
        );
        assert_eq!(
            setup_provider_default_base_url("ai-gateway"),
            Some("https://ai-gateway.vercel.sh/v1")
        );
        assert_eq!(setup_provider_display("novita"), "NovitaAI");
        assert_eq!(setup_provider_env_keys("novita"), &["NOVITA_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("novita"),
            Some("https://api.novita.ai/openai/v1")
        );
        assert_eq!(setup_provider_display("lmstudio"), "LM Studio");
        assert_eq!(setup_provider_env_keys("lmstudio"), &["LMSTUDIO_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("lmstudio"),
            Some("http://127.0.0.1:1234/v1")
        );
        assert_eq!(setup_provider_env_keys("lmdeploy"), &["LMDEPLOY_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("lmdeploy"),
            Some("http://127.0.0.1:23333/v1")
        );
        assert_eq!(setup_provider_env_keys("localai"), &["LOCALAI_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("localai"),
            Some("http://127.0.0.1:8080/v1")
        );
        assert_eq!(setup_provider_env_keys("koboldcpp"), &["KOBOLDCPP_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("koboldcpp"),
            Some("http://127.0.0.1:5001/v1")
        );
        assert_eq!(
            setup_provider_env_keys("text-generation-webui"),
            &["TEXT_GENERATION_WEBUI_API_KEY"]
        );
        assert_eq!(
            setup_provider_default_base_url("text-generation-webui"),
            Some("http://127.0.0.1:5000/v1")
        );
        assert_eq!(setup_provider_env_keys("tabbyapi"), &["TABBYAPI_API_KEY"]);
        assert_eq!(
            setup_provider_default_base_url("tabbyapi"),
            Some("http://127.0.0.1:5000/v1")
        );
        assert!(
            SETUP_MODEL_OPTIONS.len() >= 20,
            "setup provider catalog unexpectedly narrow"
        );
    }

    #[test]
    fn oauth_provider_set_matches_snapshot_registry() {
        let actual: std::collections::BTreeSet<&str> =
            hermes_cli::providers::OAUTH_CAPABLE_PROVIDERS
                .iter()
                .copied()
                .collect();
        let expected_minimum: std::collections::BTreeSet<&str> = [
            "anthropic",
            "nous",
            "openai-codex",
            "qwen-oauth",
            "google-gemini-cli",
        ]
        .into_iter()
        .collect();
        let missing: Vec<&str> = expected_minimum
            .iter()
            .copied()
            .filter(|provider| !actual.contains(provider))
            .collect();
        assert!(
            missing.is_empty(),
            "missing upstream oauth providers: {:?}",
            missing
        );
        assert!(
            actual.contains("openai"),
            "OpenAI OAuth should be enabled in Hermes Ultra"
        );
    }

    #[tokio::test]
    async fn hydrate_provider_env_from_vault_overrides_oauth_provider_env() {
        let _guard = env_lock();
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let vault_path = secret_vault_path_for_cli(&cli);
        let store = FileTokenStore::new(vault_path).await.expect("vault store");
        let manager = AuthManager::new(store);
        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: "vault-good-key".to_string(),
                refresh_token: None,
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let previous = std::env::var("NOUS_API_KEY").ok();
        std::env::set_var("NOUS_API_KEY", "env-stale-key");

        hydrate_provider_env_from_vault_for_cli_with_options(&cli, false)
            .await
            .expect("hydrate env");
        assert_eq!(
            std::env::var("NOUS_API_KEY").as_deref(),
            Ok("vault-good-key")
        );

        match previous {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
        }
    }

    #[tokio::test]
    async fn hydrate_provider_env_prefers_current_nous_oauth_over_stale_vault() {
        let _guard = env_lock();
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let vault_path = secret_vault_path_for_cli(&cli);
        let store = FileTokenStore::new(vault_path).await.expect("vault store");
        let manager = AuthManager::new(store);
        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: "vault-stale-key".to_string(),
                refresh_token: None,
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let oauth_token = test_nous_invoke_jwt(900);

        let previous_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let previous_nous_key = std::env::var("NOUS_API_KEY").ok();
        let auth_path = tmp.path().join("auth.json");
        std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
        std::env::set_var("NOUS_API_KEY", "env-stale-key");
        save_nous_auth_state(&NousAuthState {
            portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some("inference:invoke".to_string()),
            token_type: "Bearer".to_string(),
            access_token: oauth_token.clone(),
            refresh_token: None,
            obtained_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
            expires_in: None,
            agent_key: None,
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        })
        .expect("save nous oauth state");

        hydrate_provider_env_from_vault_for_cli(&cli)
            .await
            .expect("hydrate env");
        assert_eq!(
            std::env::var("NOUS_API_KEY").as_deref(),
            Ok(oauth_token.as_str())
        );

        match previous_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match previous_nous_key {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
        }
    }

    #[tokio::test]
    async fn hydrate_provider_env_rejects_stale_nous_vault_when_oauth_state_is_unusable() {
        let _guard = env_lock();
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let vault_path = secret_vault_path_for_cli(&cli);
        let store = FileTokenStore::new(vault_path).await.expect("vault store");
        let manager = AuthManager::new(store);
        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: "vault-stale-key".to_string(),
                refresh_token: None,
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let previous_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let previous_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
        let previous_nous_key = std::env::var("NOUS_API_KEY").ok();
        let auth_path = tmp.path().join("auth.json");
        std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
        std::env::remove_var("HERMES_NOUS_OAUTH_FILE");
        std::env::set_var("NOUS_API_KEY", "env-stale-key");
        save_nous_auth_state(&NousAuthState {
            portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some("profile".to_string()),
            token_type: "Bearer".to_string(),
            access_token: "header.eyJzY29wZSI6InByb2ZpbGUiLCJleHAiOjQ3Mzk4NTYwMDB9.sig".to_string(),
            refresh_token: None,
            obtained_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
            expires_in: None,
            agent_key: None,
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        })
        .expect("save unusable nous oauth state");

        hydrate_provider_env_from_vault_for_cli(&cli)
            .await
            .expect("hydrate env");
        assert!(
            std::env::var("NOUS_API_KEY").is_err(),
            "stale vault/env Nous key must not hide an unusable OAuth state"
        );

        match previous_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match previous_nous_file {
            Some(value) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", value),
            None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
        }
        match previous_nous_key {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
        }
    }

    #[test]
    fn scrub_unusable_nous_api_key_removes_config_rehydrated_key() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let previous_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let previous_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
        let previous_nous_key = std::env::var("NOUS_API_KEY").ok();
        let auth_path = tmp.path().join("auth.json");
        std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
        std::env::remove_var("HERMES_NOUS_OAUTH_FILE");
        std::env::set_var("NOUS_API_KEY", "config-stale-key");
        save_nous_auth_state(&NousAuthState {
            portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some("profile".to_string()),
            token_type: "Bearer".to_string(),
            access_token: "header.eyJzY29wZSI6InByb2ZpbGUiLCJleHAiOjQ3Mzk4NTYwMDB9.sig".to_string(),
            refresh_token: None,
            obtained_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
            expires_in: None,
            agent_key: None,
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        })
        .expect("save unusable nous oauth state");

        scrub_unusable_nous_api_key_for_oauth_state().expect("scrub nous api key");
        assert!(std::env::var("NOUS_API_KEY").is_err());

        match previous_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match previous_nous_file {
            Some(value) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", value),
            None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
        }
        match previous_nous_key {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
        }
    }

    #[tokio::test]
    async fn verify_nous_runtime_credentials_live_rejects_unauthorized_probe() {
        let base_url =
            spawn_nous_live_probe_server("401 Unauthorized", r#"{"message":"invalid or blocked"}"#)
                .await;
        let err = verify_nous_runtime_credentials_live(&NousRuntimeCredentials {
            provider: "nous".to_string(),
            base_url,
            api_key: test_nous_invoke_jwt(900),
            key_id: None,
            expires_at: None,
            expires_in: None,
            source: "invoke_jwt".to_string(),
            refresh_token: None,
            token_type: "Bearer".to_string(),
            scope: Some("inference:invoke".to_string()),
        })
        .await
        .expect_err("401 probe should reject credentials");
        assert!(
            err.contains("HTTP 401 Unauthorized") && err.contains("invalid or blocked"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn auth_verify_nous_ignores_malformed_global_fallback_state() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev_home = std::env::var("HOME").ok();
        let prev_hermes_home = std::env::var("HERMES_HOME").ok();
        let prev_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let prev_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
        let prev_nous_key = std::env::var("NOUS_API_KEY").ok();

        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("HERMES_HOME");
        let primary_store = tmp.path().join("profile-auth.json");
        std::fs::write(
            &primary_store,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "providers": {
                    "openai": { "access_token": "primary-openai" }
                }
            }))
            .expect("serialize primary auth"),
        )
        .expect("write primary auth");
        std::env::set_var(
            "HERMES_AUTH_FILE",
            primary_store.to_string_lossy().to_string(),
        );
        std::env::remove_var("HERMES_NOUS_OAUTH_FILE");
        std::env::remove_var("NOUS_API_KEY");

        let fallback_store = tmp.path().join(".hermes").join("auth.json");
        std::fs::create_dir_all(fallback_store.parent().expect("fallback parent"))
            .expect("mkdir fallback parent");
        std::fs::write(
            &fallback_store,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 1,
                "providers": {
                    "nous": {
                        "client_id": DEFAULT_NOUS_CLIENT_ID,
                        "inference_base_url": DEFAULT_NOUS_INFERENCE_URL,
                        "last_auth_error": "unauthorized",
                        "portal_base_url": DEFAULT_NOUS_PORTAL_URL,
                        "scope": "inference:invoke",
                        "token_type": "Bearer"
                    }
                }
            }))
            .expect("serialize fallback auth"),
        )
        .expect("write fallback auth");

        let store = FileTokenStore::new(tmp.path().join("tokens.json"))
            .await
            .expect("token store");
        let manager = AuthManager::new(store.clone());
        let result = verify_single_oauth_provider("nous", &store, &manager)
            .await
            .expect("verify nous");
        assert_eq!(result.outcome, AuthVerifyOutcome::Missing);
        assert!(!result.credential_present);
        assert!(!result.oauth_state_present);

        match prev_nous_key {
            Some(value) => std::env::set_var("NOUS_API_KEY", value),
            None => std::env::remove_var("NOUS_API_KEY"),
        }
        match prev_nous_file {
            Some(value) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", value),
            None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
        }
        match prev_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match prev_hermes_home {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match prev_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn auth_verify_nous_does_not_persist_vault_token_when_live_probe_fails() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth_path = tmp.path().join("auth.json");
        let vault_path = tmp.path().join("tokens.json");
        let store = FileTokenStore::new(&vault_path).await.expect("vault store");
        let manager = AuthManager::new(store.clone());
        let oauth_token = test_nous_invoke_jwt(900);

        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: oauth_token,
                refresh_token: None,
                token_type: "Bearer".to_string(),
                scope: Some("inference:invoke".to_string()),
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let previous_home = std::env::var("HOME").ok();
        let previous_hermes_home = std::env::var("HERMES_HOME").ok();
        let previous_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let previous_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
        let previous_inference_base_url = std::env::var("NOUS_INFERENCE_BASE_URL").ok();
        let probe_base_url =
            spawn_nous_live_probe_server("401 Unauthorized", r#"{"message":"invalid or blocked"}"#)
                .await;
        std::env::set_var("HOME", tmp.path());
        std::env::remove_var("HERMES_HOME");
        std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
        std::env::remove_var("HERMES_NOUS_OAUTH_FILE");
        std::env::set_var("NOUS_INFERENCE_BASE_URL", probe_base_url);

        let result = verify_single_oauth_provider("nous", &store, &manager)
            .await
            .expect("verify nous");
        assert_eq!(result.outcome, AuthVerifyOutcome::RefreshFailed);
        assert!(
            result
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("live_probe_failed"),
            "unexpected detail: {:?}",
            result.detail
        );
        assert!(
            read_provider_auth_state("nous")
                .expect("read provider state")
                .is_none(),
            "failed live vault repair must not persist Nous auth state"
        );

        match previous_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match previous_nous_file {
            Some(value) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", value),
            None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
        }
        match previous_inference_base_url {
            Some(value) => std::env::set_var("NOUS_INFERENCE_BASE_URL", value),
            None => std::env::remove_var("NOUS_INFERENCE_BASE_URL"),
        }
        match previous_hermes_home {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
        match previous_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn auth_verify_nous_repairs_stale_singleton_from_vault_invoke_jwt() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let auth_path = tmp.path().join("auth.json");
        let vault_path = tmp.path().join("tokens.json");
        let store = FileTokenStore::new(&vault_path).await.expect("vault store");
        let manager = AuthManager::new(store.clone());
        let oauth_token = test_nous_invoke_jwt(900);

        manager
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: oauth_token.clone(),
                refresh_token: None,
                token_type: "Bearer".to_string(),
                scope: Some("inference:invoke".to_string()),
                expires_at: None,
            })
            .await
            .expect("save vault credential");

        let previous_auth_file = std::env::var("HERMES_AUTH_FILE").ok();
        let previous_nous_file = std::env::var("HERMES_NOUS_OAUTH_FILE").ok();
        let previous_inference_base_url = std::env::var("NOUS_INFERENCE_BASE_URL").ok();
        let probe_base_url =
            spawn_nous_live_probe_server("200 OK", r#"{"object":"list","data":[]}"#).await;
        std::env::set_var("HERMES_AUTH_FILE", auth_path.to_string_lossy().to_string());
        std::env::remove_var("HERMES_NOUS_OAUTH_FILE");
        std::env::set_var("NOUS_INFERENCE_BASE_URL", probe_base_url);
        save_nous_auth_state(&NousAuthState {
            portal_base_url: DEFAULT_NOUS_PORTAL_URL.to_string(),
            inference_base_url: DEFAULT_NOUS_INFERENCE_URL.to_string(),
            client_id: DEFAULT_NOUS_CLIENT_ID.to_string(),
            scope: Some("profile".to_string()),
            token_type: "Bearer".to_string(),
            access_token: "header.eyJzY29wZSI6InByb2ZpbGUiLCJleHAiOjQ3Mzk4NTYwMDB9.sig".to_string(),
            refresh_token: None,
            obtained_at: chrono::Utc::now().to_rfc3339(),
            expires_at: None,
            expires_in: None,
            agent_key: None,
            agent_key_id: None,
            agent_key_expires_at: None,
            agent_key_expires_in: None,
            agent_key_reused: None,
            agent_key_obtained_at: None,
        })
        .expect("save stale nous state");

        let result = verify_single_oauth_provider("nous", &store, &manager)
            .await
            .expect("verify nous");
        assert_eq!(result.outcome, AuthVerifyOutcome::ValidRefreshed);
        assert_eq!(result.source, "vault_invoke_jwt");

        let repaired = read_provider_auth_state("nous")
            .expect("read repaired state")
            .expect("repaired state");
        assert_eq!(
            repaired
                .get("agent_key")
                .and_then(serde_json::Value::as_str),
            Some(oauth_token.as_str())
        );

        match previous_auth_file {
            Some(value) => std::env::set_var("HERMES_AUTH_FILE", value),
            None => std::env::remove_var("HERMES_AUTH_FILE"),
        }
        match previous_nous_file {
            Some(value) => std::env::set_var("HERMES_NOUS_OAUTH_FILE", value),
            None => std::env::remove_var("HERMES_NOUS_OAUTH_FILE"),
        }
        match previous_inference_base_url {
            Some(value) => std::env::set_var("NOUS_INFERENCE_BASE_URL", value),
            None => std::env::remove_var("NOUS_INFERENCE_BASE_URL"),
        }
    }

    #[test]
    fn read_env_key_treats_empty_values_as_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(
            &env_file,
            "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='   '\nOPENAI_API_KEY=real-key\n",
        )
        .expect("write env");

        assert_eq!(read_env_key(&env_file, "OPENROUTER_API_KEY"), None);
        assert_eq!(read_env_key(&env_file, "MINIMAX_API_KEY"), None);
        assert_eq!(
            read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
            Some("real-key")
        );
    }

    #[test]
    fn merge_missing_env_keys_skips_empty_values() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("legacy.env");
        let dst = tmp.path().join("target.env");
        std::fs::write(
            &src,
            "OPENROUTER_API_KEY=\nMINIMAX_API_KEY='  '\nOPENAI_API_KEY=real-key\n",
        )
        .expect("write source env");

        let imported = merge_missing_env_keys(&src, &dst, "legacy.env").expect("merge env keys");
        assert_eq!(imported, 1);
        let contents = std::fs::read_to_string(&dst).expect("read merged env");
        assert!(contents.contains("OPENAI_API_KEY=real-key"));
        assert!(!contents.contains("OPENROUTER_API_KEY="));
        assert!(!contents.contains("MINIMAX_API_KEY="));
    }

    #[test]
    fn read_env_key_handles_non_utf8_bytes_without_crashing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        let mut bytes = b"OPENAI_API_KEY=real-key\nBROKEN=".to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFE, 0x81, b'\n']);
        std::fs::write(&env_file, bytes).expect("write non-utf8 env");

        assert_eq!(
            read_env_key(&env_file, "OPENAI_API_KEY").as_deref(),
            Some("real-key")
        );
    }

    #[test]
    fn provenance_sign_and_verify_round_trip() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");

        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");
        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(verified.ok, "verification should pass");
        assert_eq!(verified.code, "ok");
        assert!(verified.reason.is_none(), "no reason on success");
    }

    #[test]
    fn provenance_verify_detects_tampered_artifact() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        std::fs::write(&artifact, b"{\"ok\":false}").expect("tamper artifact");

        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(!verified.ok, "tamper must fail");
        assert_eq!(verified.code, "artifact_sha256_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("artifact_sha256 mismatch"));
    }

    #[test]
    fn provenance_verify_detects_signature_mismatch() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        let body = b"{\"ok\":true}";
        std::fs::write(&artifact, body).expect("write artifact");
        let sig = sign_artifact_bytes(&cli, body, true).expect("sign");
        let sidecar = write_provenance_sidecar(&artifact, &sig).expect("sidecar");

        let mut parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sidecar).expect("read sidecar"))
                .expect("parse sidecar");
        parsed["signature_hex"] = serde_json::json!("deadbeef");
        std::fs::write(
            &sidecar,
            serde_json::to_string_pretty(&parsed).expect("serialize sidecar"),
        )
        .expect("write tampered sidecar");

        let verified =
            verify_artifact_provenance(&cli, &artifact, Some(sidecar.as_path())).expect("verify");
        assert!(!verified.ok, "signature mismatch must fail");
        assert_eq!(verified.code, "signature_mismatch");
        assert_eq!(verified.reason.as_deref(), Some("signature mismatch"));
    }

    #[test]
    fn provenance_verify_detects_missing_sidecar_with_code() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let artifact = tmp.path().join("doctor-snapshot.json");
        std::fs::write(&artifact, b"{\"ok\":true}").expect("write artifact");

        let verified = verify_artifact_provenance(&cli, &artifact, None).expect("verify");
        assert!(!verified.ok, "missing sidecar must fail");
        assert_eq!(verified.code, "signature_read_error");
        assert!(verified
            .reason
            .as_deref()
            .unwrap_or("")
            .contains(".sig.json"));
    }

    #[tokio::test]
    async fn rotate_provenance_key_archives_previous_key_and_rekeys() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        std::fs::create_dir_all(&config_dir).expect("create cfg dir");
        let cli = Cli::parse_from([
            "hermes-agent-ultra",
            "--config-dir",
            config_dir.to_str().expect("cfg path utf8"),
        ]);

        let old_key = load_or_create_provenance_key(&cli, true).expect("create key");
        run_rotate_provenance_key(cli.clone(), true)
            .await
            .expect("rotate key");
        let new_key = load_or_create_provenance_key(&cli, false).expect("load rotated key");
        assert_ne!(old_key, new_key, "rotation must change active key bytes");

        let auth_dir = provenance_key_path_for_cli(&cli)
            .parent()
            .expect("key path parent")
            .to_path_buf();
        let archived_count = std::fs::read_dir(auth_dir)
            .expect("read auth dir")
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("provenance.key.")
                    && entry.file_name().to_string_lossy().ends_with(".bak")
            })
            .count();
        assert!(archived_count >= 1, "rotation should archive previous key");
    }

    #[test]
    fn upsert_env_key_rewrites_existing_and_appends_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_file = tmp.path().join(".env");
        std::fs::write(
            &env_file,
            "OPENAI_API_KEY=old\nHERMES_AUTH_DEFAULT_PROVIDER=openai\n",
        )
        .expect("write env");
        upsert_env_key(&env_file, "HERMES_AUTH_DEFAULT_PROVIDER", "nous").expect("upsert");
        upsert_env_key(&env_file, "NOUS_API_KEY", "tok").expect("append");
        let raw = std::fs::read_to_string(&env_file).expect("read env");
        assert!(raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=nous"));
        assert!(raw.contains("NOUS_API_KEY=tok"));
        assert!(!raw.contains("HERMES_AUTH_DEFAULT_PROVIDER=openai"));
    }

    #[tokio::test]
    async fn profile_create_no_skills_strips_cloned_skill_overrides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

        let source_profile = profiles_dir.join("source.yaml");
        std::fs::write(
            &source_profile,
            r#"
name: source
model: openai:gpt-4o
personality: technical
max_turns: 50
skills:
  enabled:
    - contextlattice-agent-contract
  disabled:
    - noisy-skill
"#,
        )
        .expect("write source profile");
        write_active_profile_name(&profiles_dir, "source").expect("set active profile");

        run_profile(
            cli,
            Some("create".to_string()),
            Some("target".to_string()),
            None,
            None,
            None,
            None,
            false,
            false,
            true,
            true,
            Some("source".to_string()),
            true,
            true,
        )
        .await
        .expect("create profile");

        let target_profile = profiles_dir.join("target.yaml");
        let parsed: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&target_profile).expect("read target profile"),
        )
        .expect("parse target profile");
        let map = parsed.as_mapping().expect("mapping profile");
        let skills_key = serde_yaml::Value::String("skills".to_string());
        assert!(
            !map.contains_key(&skills_key),
            "skills key should be stripped"
        );
    }

    #[tokio::test]
    async fn profile_create_clone_from_implies_config_clone() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

        let source_profile = profiles_dir.join("coder.yaml");
        std::fs::write(
            &source_profile,
            r#"
name: coder
model: anthropic/claude-sonnet-4
personality: focused
max_turns: 77
"#,
        )
        .expect("write source profile");

        run_profile(
            cli,
            Some("create".to_string()),
            Some("target".to_string()),
            None,
            None,
            None,
            None,
            false,
            false,
            false,
            false,
            Some("coder".to_string()),
            true,
            false,
        )
        .await
        .expect("create profile");

        let target_profile = profiles_dir.join("target.yaml");
        let parsed: serde_yaml::Value = serde_yaml::from_str(
            &std::fs::read_to_string(&target_profile).expect("read target profile"),
        )
        .expect("parse target profile");
        let map = parsed.as_mapping().expect("mapping profile");
        assert_eq!(
            map.get(serde_yaml::Value::String("model".to_string()))
                .and_then(|v| v.as_str()),
            Some("anthropic/claude-sonnet-4")
        );
        assert_eq!(
            map.get(serde_yaml::Value::String("personality".to_string()))
                .and_then(|v| v.as_str()),
            Some("focused")
        );
        assert_eq!(
            map.get(serde_yaml::Value::String("max_turns".to_string()))
                .and_then(|v| v.as_i64()),
            Some(77)
        );
    }

    #[test]
    fn validate_profile_name_rejects_paths() {
        let err = validate_profile_name("../danger").expect_err("should reject traversal");
        assert!(
            err.to_string().contains("path separators"),
            "unexpected error: {err}"
        );
        let err = validate_profile_name("alpha beta").expect_err("should reject spaces");
        assert!(
            err.to_string().contains("letters, numbers"),
            "unexpected error: {err}"
        );
        assert_eq!(
            validate_profile_name("prod-profile_1.2").expect("valid"),
            "prod-profile_1.2"
        );
    }

    #[test]
    fn profile_alias_label_prefers_custom_aliases() {
        let mut aliases = std::collections::BTreeMap::new();
        aliases.insert("steve".to_string(), "steve".to_string());
        aliases.insert("qiaobusi".to_string(), "steve".to_string());
        aliases.insert("jobs".to_string(), "steve".to_string());
        aliases.insert("other".to_string(), "research".to_string());

        assert_eq!(
            profile_alias_label(&aliases, "steve").as_deref(),
            Some("aliases: jobs, qiaobusi")
        );
        assert_eq!(
            profile_alias_label(&aliases, "research").as_deref(),
            Some("alias: other")
        );
        assert_eq!(profile_alias_label(&aliases, "missing"), None);
    }

    #[tokio::test]
    async fn profile_import_refuses_directory_clobber_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).expect("create profiles dir");

        let source_profile = tmp.path().join("source.yaml");
        std::fs::write(
            &source_profile,
            r#"
name: source
model: openai:gpt-4o
personality: default
max_turns: 50
"#,
        )
        .expect("write source profile");

        let clobber_target_dir = profiles_dir.join("target.yaml");
        std::fs::create_dir_all(&clobber_target_dir).expect("create clobber directory");

        let err = run_profile(
            cli,
            Some("import".to_string()),
            Some(source_profile.to_string_lossy().into_owned()),
            None,
            None,
            Some("target".to_string()),
            None,
            false,
            true,
            false,
            false,
            None,
            true,
            false,
        )
        .await
        .expect_err("directory clobber should be rejected");

        assert!(
            err.to_string().contains("target path is a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn qqbot_connect_url_encodes_task_id() {
        let url = qqbot_connect_url("task id/+");
        assert!(url.contains("task_id=task%20id%2F%2B"));
        assert!(url.contains("source=hermes"));
    }

    #[test]
    fn qqbot_decrypt_secret_roundtrip() {
        let key = [7u8; 32];
        let nonce = [3u8; 12];
        let key_b64 = BASE64_STANDARD.encode(key);

        let cipher =
            <Aes256Gcm as aes_gcm::aead::KeyInit>::new_from_slice(&key).expect("cipher init");
        let ciphertext = cipher
            .encrypt(aes_gcm::Nonce::from_slice(&nonce), b"qq-secret".as_ref())
            .expect("encrypt");
        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&ciphertext);
        let encrypted_b64 = BASE64_STANDARD.encode(payload);

        let decrypted = qqbot_decrypt_secret(&encrypted_b64, &key_b64).expect("decrypt");
        assert_eq!(decrypted, "qq-secret");
    }

    #[test]
    fn qqbot_extract_i64_accepts_number_or_string() {
        let numeric = serde_json::json!({ "status": 2 });
        assert_eq!(qqbot_extract_i64(&numeric, &["status"]), Some(2));

        let stringified = serde_json::json!({ "status": "3" });
        assert_eq!(qqbot_extract_i64(&stringified, &["status"]), Some(3));
    }

    #[test]
    fn read_gateway_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("plain.pid");
        std::fs::write(&plain, "12345\n").expect("write plain pid");
        assert_eq!(read_gateway_pid(&plain), Some(12345));

        let json = tmp.path().join("json.pid");
        std::fs::write(
            &json,
            serde_json::json!({
                "pid": 23456,
                "kind": "hermes-gateway",
                "argv": ["hermes-gateway"]
            })
            .to_string(),
        )
        .expect("write json pid");
        assert_eq!(read_gateway_pid(&json), Some(23456));

        let invalid = tmp.path().join("invalid.pid");
        std::fs::write(&invalid, "{bad").expect("write invalid pid");
        assert_eq!(read_gateway_pid(&invalid), None);
    }

    #[test]
    fn read_interactive_lock_pid_supports_plain_and_json_records() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let plain = tmp.path().join("interactive.lock");
        std::fs::write(&plain, "12345\n").expect("write plain lock");
        assert_eq!(read_interactive_lock_pid(&plain), Some(12345));

        let json = tmp.path().join("interactive.json");
        std::fs::write(&json, r#"{"pid":23456}"#).expect("write json lock");
        assert_eq!(read_interactive_lock_pid(&json), Some(23456));
    }

    #[test]
    fn query_is_local_slash_command_detects_prefixed_queries() {
        assert!(query_is_local_slash_command("/model list"));
        assert!(query_is_local_slash_command("   /graph status"));
        assert!(!query_is_local_slash_command("hello world"));
    }

    #[test]
    fn interactive_tty_error_is_actionable() {
        let msg = interactive_tty_error_message();
        assert!(msg.contains("requires a terminal"));
        assert!(msg.contains("hermes-ultra setup"));
        assert!(msg.contains("chat --query"));
        assert!(msg.contains("doctor --deep --snapshot --bundle"));
    }

    #[test]
    fn interactive_session_lock_guard_replaces_stale_pid_and_cleans_up() {
        let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let lock_path = interactive_lock_path_for_cli(&cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        std::fs::write(&lock_path, "999999").expect("write stale lock");
        let guard = InteractiveSessionLockGuard::acquire(&cli)
            .expect("acquire lock")
            .expect("guard enabled");
        assert_eq!(
            read_interactive_lock_pid(&lock_path),
            Some(std::process::id())
        );
        drop(guard);
        assert!(!lock_path.exists(), "lock file should be removed on drop");
        if let Some(value) = old_bypass {
            std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn interactive_session_lock_guard_rejects_live_pid() {
        let old_bypass = std::env::var_os(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        std::env::remove_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV);
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let lock_path = interactive_lock_path_for_cli(&cli);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir lock parent");
        }
        // PID 1 should always be alive on Unix systems.
        std::fs::write(&lock_path, "1").expect("write lock");
        let err = match InteractiveSessionLockGuard::acquire(&cli) {
            Err(err) => err,
            Ok(_) => panic!("must reject live lock holder"),
        };
        let msg = format!("{err}");
        assert!(msg.contains("Another Hermes interactive session is running"));
        assert_eq!(read_interactive_lock_pid(&lock_path), Some(1));
        if let Some(value) = old_bypass {
            std::env::set_var(INTERACTIVE_SESSION_LOCK_BYPASS_ENV, value);
        }
    }

    #[cfg(unix)]
    #[test]
    fn parse_pid_snapshot_line_parses_ppid_tty_and_command() {
        let snap = parse_pid_snapshot_line("1 ?? /Users/sheawinkler/.cargo/bin/hermes-agent-ultra")
            .expect("snapshot");
        assert_eq!(snap.ppid, 1);
        assert_eq!(snap.tty, "??");
        assert!(snap.command.contains("hermes-agent-ultra"));
    }

    #[cfg(unix)]
    #[test]
    fn looks_like_interactive_hermes_process_matches_cli_and_not_gateway() {
        assert!(looks_like_interactive_hermes_process(
            "/Users/sheawinkler/.cargo/bin/hermes-agent-ultra"
        ));
        assert!(looks_like_interactive_hermes_process("hermes-ultra"));
        assert!(!looks_like_interactive_hermes_process(
            "/Users/sheawinkler/.cargo/bin/hermes-gateway"
        ));
    }

    #[test]
    fn looks_like_gateway_process_includes_gateway_script_pattern() {
        assert!(looks_like_gateway_process(
            "python -m hermes_cli.main gateway run"
        ));
        assert!(looks_like_gateway_process(
            "python hermes_cli/main.py gateway run"
        ));
        assert!(looks_like_gateway_process("hermes gateway run"));
        assert!(looks_like_gateway_process(
            "hermes-gateway --config ~/.hermes"
        ));
        assert!(looks_like_gateway_process("python gateway/run.py"));
        assert!(!looks_like_gateway_process("python worker.py"));
    }

    #[test]
    fn cleanup_stale_gateway_metadata_removes_pid_and_lock_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pid_path = tmp.path().join("gateway.pid");
        let lock_path = gateway_lock_path_for_pid_path(&pid_path);
        std::fs::write(&pid_path, "999999\n").expect("write pid");
        std::fs::write(&lock_path, "{\"pid\":999999}").expect("write lock");

        cleanup_stale_gateway_metadata(&pid_path);
        assert!(!pid_path.exists());
        assert!(!lock_path.exists());
    }

    #[test]
    fn capture_debug_log_snapshot_preserves_boundary_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        std::fs::write(&log_path, "line1\nline2\nline3\n").expect("write log");

        let snap = capture_debug_log_snapshot(&log_path, 1, 12);
        let full = snap.full_text.unwrap_or_default();
        assert!(full.contains("line2\nline3"));
        assert!(!full.contains("line1"));
    }

    #[test]
    fn capture_debug_log_snapshot_caps_memory_with_long_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log_path = tmp.path().join("hermes.log");
        let long = "x".repeat(256 * 1024);
        std::fs::write(&log_path, long).expect("write long log");

        let max_bytes = 4096usize;
        let snap = capture_debug_log_snapshot(&log_path, 5, max_bytes);
        let full = snap.full_text.unwrap_or_default();
        assert!(
            full.len() <= (max_bytes * 2) + 128,
            "full snapshot should obey hard cap"
        );
    }

    #[test]
    fn capture_debug_log_snapshot_distinguishes_missing_and_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("missing.log");
        let missing_snap = capture_debug_log_snapshot(&missing, 10, 1024);
        assert_eq!(missing_snap.tail_text, "(file not found)");

        let empty = tmp.path().join("empty.log");
        std::fs::write(&empty, "").expect("write empty log");
        let empty_snap = capture_debug_log_snapshot(&empty, 10, 1024);
        assert_eq!(empty_snap.tail_text, "(file empty)");
    }

    #[test]
    fn sweep_expired_pending_pastes_is_best_effort_and_keeps_fresh_entries() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reports_dir = tmp.path();
        let store = debug_pending_pastes_path(reports_dir);
        let entries = vec![
            PendingPasteDelete {
                url: "https://paste.rs/expired".to_string(),
                expires_at_unix: 100,
            },
            PendingPasteDelete {
                url: "https://paste.rs/fresh".to_string(),
                expires_at_unix: 9_999_999_999,
            },
        ];
        std::fs::write(
            &store,
            serde_json::to_string_pretty(&entries).expect("serialize"),
        )
        .expect("write pending store");

        let removed = sweep_expired_pending_pastes(reports_dir, 1_000).expect("sweep");
        assert_eq!(removed, 1);

        let kept: Vec<PendingPasteDelete> =
            serde_json::from_str(&std::fs::read_to_string(&store).expect("read pending store"))
                .expect("parse pending store");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].url, "https://paste.rs/fresh");
    }

    #[test]
    fn best_effort_sweep_handles_invalid_store_without_failing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let reports_dir = tmp.path();
        let store = debug_pending_pastes_path(reports_dir);
        std::fs::write(&store, "{invalid json").expect("write invalid json");

        let removed = best_effort_sweep_expired_pending_pastes(reports_dir, 1_000);
        assert_eq!(removed, 0);
    }

    #[test]
    fn run_sessions_db_auto_maintenance_degrades_when_home_is_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bad_home = tmp.path().join("not-a-dir");
        std::fs::write(&bad_home, "x").expect("write blocker file");

        let mut cfg = hermes_config::GatewayConfig::default();
        cfg.home_dir = Some(bad_home.to_string_lossy().to_string());
        cfg.sessions.auto_prune = true;

        let result = std::panic::catch_unwind(|| run_sessions_db_auto_maintenance(&cfg));
        assert!(
            result.is_ok(),
            "maintenance should degrade without panicking"
        );
    }

    #[test]
    fn gateway_auth_provider_keys_include_primary_platforms() {
        for key in ["telegram", "weixin", "discord", "slack"] {
            let mapped = gateway_platform_provider_key(key);
            if key == "telegram" || key == "weixin" {
                assert!(mapped.is_none(), "{key} handled by dedicated auth flow");
            } else {
                assert_eq!(mapped, Some(key));
            }
        }
    }

    #[test]
    fn gateway_requirement_check_flags_missing_required_fields() {
        let mut config = hermes_config::GatewayConfig::default();
        config
            .platforms
            .insert("telegram".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("qqbot".to_string(), make_platform(true, None));
        let issues = gateway_requirement_issues(&config);
        assert!(issues.iter().any(|s| s.contains("telegram")));
        assert!(issues.iter().any(|s| s.contains("qqbot")));
    }

    #[test]
    fn gateway_requirement_check_accepts_complete_qqbot_and_wecom_callback() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut qqbot = make_platform(true, None);
        qqbot
            .extra
            .insert("app_id".to_string(), serde_json::json!("qq-app"));
        qqbot
            .extra
            .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
        config.platforms.insert("qqbot".to_string(), qqbot);

        let mut wecom_cb = make_platform(true, Some("cb-token"));
        wecom_cb
            .extra
            .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
        wecom_cb
            .extra
            .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
        wecom_cb
            .extra
            .insert("agent_id".to_string(), serde_json::json!("1000002"));
        wecom_cb.extra.insert(
            "encoding_aes_key".to_string(),
            serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
        );
        config
            .platforms
            .insert("wecom_callback".to_string(), wecom_cb);

        assert!(gateway_requirement_issues(&config).is_empty());
    }

    #[tokio::test]
    async fn register_gateway_adapters_registers_primary_platforms_when_config_is_complete() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut telegram = make_platform(true, Some("tg-token"));
        telegram
            .extra
            .insert("polling".to_string(), serde_json::json!(false));
        telegram
            .extra
            .insert("webhook_secret".to_string(), serde_json::json!("tg-secret"));
        config.platforms.insert("telegram".to_string(), telegram);

        let mut weixin = make_platform(true, Some("wx-token"));
        weixin
            .extra
            .insert("account_id".to_string(), serde_json::json!("wxid_abc"));
        config.platforms.insert("weixin".to_string(), weixin);

        config.platforms.insert(
            "discord".to_string(),
            make_platform(true, Some("discord-token")),
        );
        config
            .platforms
            .insert("slack".to_string(), make_platform(true, Some("xoxb-slack")));

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("primary platform registration should succeed");

        let mut names = gateway.adapter_names().await;
        names.sort();
        assert!(names.contains(&"telegram".to_string()));
        assert!(names.contains(&"weixin".to_string()));
        assert!(names.contains(&"discord".to_string()));
        assert!(names.contains(&"slack".to_string()));

        for task in sidecar_tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn register_gateway_adapters_skips_primary_platforms_when_required_credentials_missing() {
        let mut config = hermes_config::GatewayConfig::default();
        config
            .platforms
            .insert("telegram".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("weixin".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("discord".to_string(), make_platform(true, None));
        config
            .platforms
            .insert("slack".to_string(), make_platform(true, None));

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("missing credentials should be handled gracefully");

        assert!(
            gateway.adapter_names().await.is_empty(),
            "no primary adapter should register when required credentials are missing"
        );
        for task in sidecar_tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn register_gateway_adapters_registers_qqbot_and_wecom_callback() {
        let mut config = hermes_config::GatewayConfig::default();

        let mut qqbot = make_platform(true, None);
        qqbot
            .extra
            .insert("app_id".to_string(), serde_json::json!("qq-app"));
        qqbot
            .extra
            .insert("client_secret".to_string(), serde_json::json!("qq-secret"));
        config.platforms.insert("qqbot".to_string(), qqbot);

        let mut wecom_cb = make_platform(true, None);
        wecom_cb
            .extra
            .insert("corp_id".to_string(), serde_json::json!("wwcorp"));
        wecom_cb
            .extra
            .insert("corp_secret".to_string(), serde_json::json!("corp-secret"));
        wecom_cb
            .extra
            .insert("agent_id".to_string(), serde_json::json!("1000002"));
        wecom_cb
            .extra
            .insert("token".to_string(), serde_json::json!("cb-token"));
        wecom_cb.extra.insert(
            "encoding_aes_key".to_string(),
            serde_json::json!("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
        );
        config
            .platforms
            .insert("wecom_callback".to_string(), wecom_cb);

        let gateway = make_gateway();
        let mut sidecar_tasks = Vec::new();
        register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks)
            .await
            .expect("qqbot and wecom_callback should register");

        let names = gateway.adapter_names().await;
        assert!(names.contains(&"qqbot".to_string()));
        assert!(names.contains(&"wecom_callback".to_string()));
    }

    #[test]
    fn doctor_self_heal_creates_missing_state_dirs() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let state_root = hermes_state_root(&cli);
        assert!(!state_root.join("profiles").exists());

        let actions = run_doctor_self_heal(&cli);
        assert!(state_root.join("profiles").exists());
        assert!(state_root.join("sessions").exists());
        assert!(state_root.join("logs").exists());
        assert!(actions
            .iter()
            .any(|entry| entry.get("status").and_then(|v| v.as_str()) == Some("created")));
    }

    #[test]
    fn doctor_self_heal_removes_stale_gateway_pid_file() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let pid_path = gateway_pid_path_for_cli(&cli);
        if let Some(parent) = pid_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir pid dir");
        }
        std::fs::write(&pid_path, "999999").expect("write stale pid");
        assert!(pid_path.exists());

        let actions = run_doctor_self_heal(&cli);
        assert!(!pid_path.exists(), "stale pid file should be removed");
        assert!(actions.iter().any(|entry| {
            entry
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .contains("removed stale gateway pid file")
        }));
    }

    #[test]
    fn doctor_elite_diagnostics_payload_has_required_sections() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "doctor",
        ]);
        let payload = build_elite_doctor_diagnostics(&cli);
        assert!(payload.get("provenance").is_some());
        assert!(payload.get("route_learning").is_some());
        assert!(payload.get("route_health").is_some());
        assert!(payload.get("tool_policy").is_some());
        assert!(payload.get("elite_gate").is_some());
    }

    #[test]
    fn resolve_resume_session_file_prefers_latest_modified_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let old = sessions_dir.join("old-session.json");
        let new = sessions_dir.join("new-session.json");
        std::fs::write(&old, r#"{"messages":[{"role":"user","content":"old"}]}"#)
            .expect("write old session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new, r#"{"messages":[{"role":"user","content":"new"}]}"#)
            .expect("write new session");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "new-session");
        assert_eq!(path, new);
    }

    #[test]
    fn resolve_resume_session_file_latest_prefers_canonical_session_stem() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let canonical = sessions_dir.join("c0ffee00-0000-4000-8000-000000000001.json");
        std::fs::write(
            &canonical,
            r#"{
  "session_info": {"session_id":"c0ffee00-0000-4000-8000-000000000001","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write canonical");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let named = sessions_dir.join("newest.json");
        std::fs::write(
            &named,
            r#"{
  "session_info": {"session_id":"snap-prune","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"snapshot payload"}]
}"#,
        )
        .expect("write named artifact");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, None).expect("resolve latest");
        assert_eq!(resolved, "c0ffee00-0000-4000-8000-000000000001");
        assert_eq!(path, canonical);
    }

    #[test]
    fn resolve_resume_session_file_searches_session_id_when_exact_file_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let snapshot = sessions_dir.join("saved-snapshot-name.json");
        std::fs::write(
            &snapshot,
            r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"}]
}"#,
        )
        .expect("write snapshot");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, Some("20260603")).expect("resolve prefix");
        assert_eq!(resolved, "saved-snapshot-name");
        assert_eq!(path, snapshot);

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, Some("ABCD12")).expect("resolve substring");
        assert_eq!(resolved, "saved-snapshot-name");
        assert_eq!(path, snapshot);
    }

    #[test]
    fn resolve_resume_session_file_search_ranks_exact_before_prefix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let exact = sessions_dir.join("snap-exact.json");
        std::fs::write(
            &exact,
            r#"{
  "session_info": {"session_id":"20260603_090200_abcd12","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"exact"}]
}"#,
        )
        .expect("write exact");
        let prefix = sessions_dir.join("20260603_090200_abcd12_child.json");
        std::fs::write(
            &prefix,
            r#"{
  "session_info": {"session_id":"20260603_090200_abcd12_child","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"prefix"}]
}"#,
        )
        .expect("write prefix");

        let (resolved, path) =
            resolve_resume_session_file(&sessions_dir, Some("20260603_090200_abcd12"))
                .expect("resolve exact session_info id");
        assert_eq!(resolved, "snap-exact");
        assert_eq!(path, exact);
    }

    #[test]
    fn should_resume_fallback_to_fresh_only_for_latest_missing_state() {
        let latest_missing = AgentError::Config("No saved sessions found in /tmp".to_string());
        assert!(should_resume_fallback_to_fresh(None, &latest_missing));
        assert!(should_resume_fallback_to_fresh(
            Some("latest"),
            &latest_missing
        ));
        assert!(!should_resume_fallback_to_fresh(
            Some("abc123"),
            &latest_missing
        ));

        let other_error = AgentError::Config("Session 'abc123' not found".to_string());
        assert!(!should_resume_fallback_to_fresh(None, &other_error));
    }

    #[test]
    fn load_resume_payload_restores_metadata_and_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("abc123.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "session-xyz",
    "model": "nous:openai/gpt-5.5-pro",
    "personality": "technical"
  },
  "messages": [
    {"role":"System","content":"[SESSION_OBJECTIVE] Keep context fresh"},
    {"role":"User","content":"hello"},
    {"role":"Assistant","content":"world"}
  ]
}"#,
        )
        .expect("write session");

        let payload = load_resume_payload(&cli, Some("abc123")).expect("load payload");
        assert_eq!(payload.resolved_id, "abc123");
        assert_eq!(payload.session_id, "session-xyz");
        assert_eq!(payload.model.as_deref(), Some("nous:openai/gpt-5.5-pro"));
        assert_eq!(payload.personality.as_deref(), Some("technical"));
        assert_eq!(payload.messages.len(), 3);
        assert!(matches!(
            payload.messages[0].role,
            hermes_core::MessageRole::System
        ));
        assert!(matches!(
            payload.messages[1].role,
            hermes_core::MessageRole::User
        ));
        assert!(matches!(
            payload.messages[2].role,
            hermes_core::MessageRole::Assistant
        ));
    }

    #[test]
    fn load_resume_payload_follows_compression_tip_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let state_root = hermes_state_root(&cli);
        let sessions_dir = state_root.join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("root.json"),
            r#"{
  "session_info": {"session_id":"root","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"pre-compression turn"}]
}"#,
        )
        .expect("write root session");
        std::fs::write(
            sessions_dir.join("cont.json"),
            r#"{
  "session_info": {"session_id":"cont","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"Assistant","content":"post-compression reply"}]
}"#,
        )
        .expect("write continuation session");

        let persistence = SessionPersistence::new(&state_root);
        persistence
            .persist_session(
                "root",
                &[hermes_core::Message::user("pre-compression turn")],
                Some("nous:openai/gpt-5.5"),
                Some("cli"),
                None,
                None,
            )
            .unwrap();
        persistence
            .persist_session(
                "cont",
                &[hermes_core::Message::assistant("post-compression reply")],
                Some("nous:openai/gpt-5.5"),
                Some("cli"),
                None,
                None,
            )
            .unwrap();
        let base = chrono::Utc::now() - chrono::Duration::hours(1);
        let root_created = base.to_rfc3339();
        let root_ended = (base + chrono::Duration::seconds(10)).to_rfc3339();
        let cont_created = (base + chrono::Duration::seconds(20)).to_rfc3339();
        assert!(persistence
            .update_session_lineage(
                "root",
                None,
                Some("compression"),
                Some(&root_created),
                Some(&root_ended),
            )
            .expect("mark root compressed"));
        assert!(persistence
            .update_session_lineage("cont", Some("root"), None, Some(&cont_created), None,)
            .expect("link continuation"));

        let payload = load_resume_payload(&cli, Some("root")).expect("load payload");

        assert_eq!(payload.resolved_id, "cont");
        assert_eq!(payload.session_id, "cont");
        assert_eq!(payload.source_path, sessions_dir.join("cont.json"));
        assert_eq!(payload.messages.len(), 1);
        assert_eq!(
            payload.messages[0].content.as_deref(),
            Some("post-compression reply")
        );
    }

    #[test]
    fn load_resume_payload_falls_back_to_legacy_sessions_dir() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");
        let legacy_path = legacy_sessions.join("legacy-abc.json");
        std::fs::write(
            &legacy_path,
            r#"{
  "session_info": {
    "session_id": "legacy-session",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": [
    {"role":"User","content":"from-legacy"}
  ]
}"#,
        )
        .expect("write legacy session");

        std::env::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let payload = load_resume_payload(&cli, Some("legacy-abc")).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-abc");
        assert_eq!(payload.session_id, "legacy-session");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn load_resume_payload_accepts_empty_messages_for_startup_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        let session_path = sessions_dir.join("empty-messages.json");
        std::fs::write(
            &session_path,
            r#"{
  "session_info": {
    "session_id": "empty-messages",
    "model": "nous:nousresearch/hermes-4-70b"
  },
  "messages": []
}"#,
        )
        .expect("write empty session");

        let payload = load_resume_payload(&cli, Some("empty-messages")).expect("load payload");
        assert_eq!(payload.resolved_id, "empty-messages");
        assert_eq!(payload.session_id, "empty-messages");
        assert_eq!(
            payload.model.as_deref(),
            Some("nous:nousresearch/hermes-4-70b")
        );
        assert_eq!(payload.messages.len(), 0);
    }

    #[test]
    fn load_resume_payload_latest_prefers_nonempty_snapshot_over_newer_empty_snapshot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");

        let non_empty = sessions_dir.join("history-real.json");
        std::fs::write(
            &non_empty,
            r#"{
  "session_info": {"session_id":"history-real","model":"nous:openai/gpt-5.5"},
  "messages":[{"role":"User","content":"hello"},{"role":"Assistant","content":"world"}]
}"#,
        )
        .expect("write non-empty session");
        std::thread::sleep(std::time::Duration::from_millis(20));
        let empty_snapshot = sessions_dir.join("startup-empty.json");
        std::fs::write(
            &empty_snapshot,
            r#"{
  "session_info": {"session_id":"startup-empty","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write empty session");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "history-real");
        assert_eq!(payload.messages.len(), 2);
        assert_eq!(payload.source_path, non_empty);
    }

    #[test]
    fn load_resume_payload_latest_falls_back_to_legacy_nonempty_when_primary_empty_only() {
        let _guard = env_lock();
        let prev_home = std::env::var("HOME").ok();
        let tmp = tempfile::tempdir().expect("tempdir");
        let fake_home = tmp.path().join("fake-home");
        let legacy_sessions = fake_home.join(".hermes").join("sessions");
        std::fs::create_dir_all(&legacy_sessions).expect("create legacy sessions dir");

        let legacy_non_empty = legacy_sessions.join("legacy-rich.json");
        std::fs::write(
            &legacy_non_empty,
            r#"{
  "session_info": {"session_id":"legacy-rich","model":"nous:nousresearch/hermes-4-70b"},
  "messages":[{"role":"User","content":"from legacy"}]
}"#,
        )
        .expect("write legacy non-empty session");

        std::env::set_var("HOME", &fake_home);
        let state_root = tmp.path().join("ultra-state");
        let cli = cli_for_temp_state_root(&state_root);
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("empty-only.json"),
            r#"{
  "session_info": {"session_id":"empty-only","model":"nous:openai/gpt-5.5"},
  "messages":[]
}"#,
        )
        .expect("write primary empty session");

        let payload = load_resume_payload(&cli, None).expect("load payload");
        assert_eq!(payload.resolved_id, "legacy-rich");
        assert_eq!(payload.messages.len(), 1);
        assert!(payload.source_path.starts_with(&legacy_sessions));

        match prev_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }

    #[tokio::test]
    async fn run_dump_writes_real_saved_session_export_with_system_prompt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let sessions_dir = hermes_state_root(&cli).join("sessions");
        std::fs::create_dir_all(&sessions_dir).expect("create sessions dir");
        std::fs::write(
            sessions_dir.join("abc123.json"),
            r#"{
  "session_info": {
    "session_id": "session-xyz",
    "model": "nous:openai/gpt-5.5",
    "personality": "technical",
    "created_at": "2026-06-05T09:00:00Z"
  },
  "system_prompt": "persisted system prompt",
  "messages": [
    {"role":"User","content":"hello"},
    {"role":"Assistant","content":"world"}
  ]
}"#,
        )
        .expect("write session");

        run_dump(cli, Some("abc123".to_string()), None)
            .await
            .expect("dump session");

        let saved_dir = tmp.path().join("sessions").join("saved");
        let entries = std::fs::read_dir(&saved_dir)
            .expect("saved dir")
            .collect::<Result<Vec<_>, _>>()
            .expect("saved entries");
        assert_eq!(entries.len(), 1);
        let path = entries[0].path();
        assert!(path
            .file_name()
            .and_then(|v| v.to_str())
            .is_some_and(|name| name.starts_with("hermes_conversation_")));

        let doc: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read dump"))
                .expect("parse dump");
        assert_eq!(doc["session_id"], "session-xyz");
        assert_eq!(doc["resolved_id"], "abc123");
        assert_eq!(doc["model"], "nous:openai/gpt-5.5");
        assert_eq!(doc["personality"], "technical");
        assert_eq!(doc["system_prompt"], "persisted system prompt");
        assert_eq!(doc["session_start"], "2026-06-05T09:00:00Z");
        assert_eq!(doc["messages"].as_array().map(Vec::len), Some(2));
        assert!(doc["source_path"]
            .as_str()
            .is_some_and(|p| p.ends_with("abc123.json")));
    }

    #[test]
    fn route_health_tier_marks_failure_streak_critical() {
        let stats = RouteLearningStatsRecord {
            samples: 8,
            success_rate: 0.61,
            avg_latency_ms: 2200.0,
            consecutive_failures: 6,
            updated_at_unix_ms: 1_700_000_000_000,
        };
        let (tier, reasons, score) = route_health_tier(&stats, route_learning_score(&stats));
        assert_eq!(tier, "critical");
        assert!(reasons.iter().any(|r| r == "failure_streak_critical"));
        assert!(score >= 0.0);
    }

    #[test]
    fn replay_integrity_detects_chain_break() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let replay = tmp.path().join("session.jsonl");
        std::fs::write(
            &replay,
            r#"{"seq":1,"event":"a","prev_hash":"seed","event_hash":"h1","payload":{"ok":true}}
{"seq":2,"event":"b","prev_hash":"BROKEN","event_hash":"h2","payload":{"ok":true}}
"#,
        )
        .expect("write replay");

        let summary = replay_integrity_for_file(&replay);
        assert_eq!(summary.events, 2);
        assert!(!summary.hash_chain_ok);
    }

    #[test]
    fn replay_manifest_aggregates_counts() {
        let items = vec![
            ReplayIntegritySummary {
                file: "a.jsonl".to_string(),
                checksum_sha256: Some("abc".to_string()),
                events: 3,
                invalid_lines: 0,
                hash_chain_ok: true,
                last_event_hash: Some("h1".to_string()),
            },
            ReplayIntegritySummary {
                file: "b.jsonl".to_string(),
                checksum_sha256: Some("def".to_string()),
                events: 2,
                invalid_lines: 1,
                hash_chain_ok: false,
                last_event_hash: Some("h2".to_string()),
            },
        ];
        let manifest = replay_manifest_json(&items);
        assert_eq!(manifest["totals"]["files"], 2);
        assert_eq!(manifest["totals"]["events"], 5);
        assert_eq!(manifest["totals"]["invalid_lines"], 1);
        assert_eq!(manifest["totals"]["hash_chain_ok"], false);
    }

    #[test]
    fn parse_simple_env_file_supports_export_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let env_path = tmp.path().join("route-autotune.env");
        std::fs::write(
            &env_path,
            "# comment\nexport HERMES_SMART_ROUTING_LEARNING_ALPHA=0.240\nHERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS=0.110\n",
        )
        .expect("write env");
        let parsed = parse_simple_env_file(&env_path);
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_ALPHA")
                .map(String::as_str),
            Some("0.240")
        );
        assert_eq!(
            parsed
                .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
                .map(String::as_str),
            Some("0.110")
        );
    }

    #[test]
    fn apply_route_autotune_env_overrides_sets_missing_keys_only() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let env_path = route_autotune_env_path_for_cli(&cli);
        if let Some(parent) = env_path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(
            &env_path,
            "HERMES_SMART_ROUTING_LEARNING_ALPHA=0.300\nHERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN=0.050\n",
        )
        .expect("write env");

        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        std::env::set_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN", "0.999");
        let applied = apply_route_autotune_env_overrides(&cli);
        assert!(applied
            .iter()
            .any(|k| k == "HERMES_SMART_ROUTING_LEARNING_ALPHA"));
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_ALPHA").ok(),
            Some("0.300".to_string())
        );
        assert_eq!(
            std::env::var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN").ok(),
            Some("0.999".to_string()),
            "explicit env var should not be overridden"
        );
        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_ALPHA");
        std::env::remove_var("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN");
    }

    #[test]
    fn build_route_autotune_plan_raises_bias_for_critical_health() {
        use clap::Parser;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = tmp.path().join("cfg");
        let cli = Cli::parse_from([
            "hermes-ultra",
            "--config-dir",
            cfg.to_str().expect("utf8 path"),
            "status",
        ]);
        let entry = RouteHealthEntry {
            key: "openai:gpt-4o".to_string(),
            health_score: 0.2,
            tier: "critical".to_string(),
            reasons: vec!["failure_streak_critical".to_string()],
            stats: RouteLearningStatsRecord {
                samples: 9,
                success_rate: 0.4,
                avg_latency_ms: 5200.0,
                consecutive_failures: 7,
                updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
            },
        };
        let summary = serde_json::json!({
            "entries": 1,
            "overall": "critical",
            "average_score": 0.2,
            "healthy": 0,
            "watch": 0,
            "degraded": 0,
            "critical": 1
        });
        let plan = build_route_autotune_plan(
            &cli,
            Path::new("/tmp/route-learning.json"),
            Path::new("/tmp/route-health.json"),
            &[entry],
            &summary,
        );
        let cheap_bias = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_CHEAP_BIAS")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        let switch_margin = plan
            .overrides
            .get("HERMES_SMART_ROUTING_LEARNING_SWITCH_MARGIN")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0);
        assert!(cheap_bias >= 0.14);
        assert!(switch_margin >= 0.05);
        assert_eq!(plan.confidence, "low");
    }
}
