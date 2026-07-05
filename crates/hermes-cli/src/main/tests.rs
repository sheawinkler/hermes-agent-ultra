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

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.as_deref() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
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

    #[tokio::test]
    async fn auth_status_does_not_refresh_expired_stored_credential() {
        let _guard = env_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvVarGuard::set_path("HERMES_HOME", tmp.path());
        let cli = cli_for_temp_state_root(tmp.path());
        let token_store = FileTokenStore::new(secret_vault_path_for_cli(&cli))
            .await
            .expect("token store");
        AuthManager::new(token_store)
            .save_credential(OAuthCredential {
                provider: "nous".to_string(),
                access_token: "expired-access-token".to_string(),
                refresh_token: Some("refresh-token-without-handler".to_string()),
                token_type: "bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(5)),
            })
            .await
            .expect("save expired credential");

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
        .expect("auth status must be passive");
    }

    #[test]
    fn zsh_completion_uses_catalog_backed_provider_and_model_candidates() {
        let script = concat!(
            "'--model=[Override]:MODEL:_default' \\\n",
            "'--provider=[Override]:PROVIDER:_default' \\\n",
            "'::provider_model -- Provider\\:model identifier (e.g. \"openai\\:gpt-4o\", \"anthropic\\:claude-3-opus\"):_default' \\\n",
        );
        let enhanced = enhance_zsh_provider_completion(script.to_string());

        assert!(enhanced.contains(":MODEL:_hermes_agent_ultra_model_values"));
        assert!(enhanced.contains(":PROVIDER:_hermes_agent_ultra_provider_values"));
        assert!(enhanced.contains("provider_model -- Provider\\:model identifier"));
        assert!(enhanced.contains("_hermes_agent_ultra_model_values()"));
        assert!(enhanced.contains("model --completion-values"));
        assert!(enhanced.contains("model --completion-providers"));
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
            is_video: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            video_file_id: None,
            video_file_name: None,
            video_mime_type: None,
            video_file_size: None,
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
            is_video: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            video_file_id: None,
            video_file_name: None,
            video_mime_type: None,
            video_file_size: None,
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
            is_video: false,
            voice_file_id: None,
            photo_file_id: None,
            sticker_file_id: None,
            document_file_id: None,
            document_file_name: None,
            document_mime_type: None,
            document_file_size: None,
            video_file_id: None,
            video_file_name: None,
            video_mime_type: None,
            video_file_size: None,
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
        let openai_invalidated = AgentError::LlmApi(
            r#"API error 401 Unauthorized: {"error":{"message":"Your authentication token has been invalidated. Please try signing in again.","code":"token_invalidated"}}"#
                .to_string(),
        );
        assert_eq!(
            oneshot_auto_verify_oauth_provider(
                &openai_invalidated,
                Some("openai"),
                Some("openai:gpt-5.5")
            ),
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
        assert!(oneshot_auth_is_refreshable("token_invalidated"));
        assert!(oneshot_auth_is_refreshable("invalid_grant"));
        assert!(!oneshot_auth_is_refreshable("api error 404 not found"));
    }

    #[test]
    fn oneshot_auth_requires_fresh_login_for_invalidated_tokens() {
        let invalidated = AgentError::LlmApi(
            "API error 401 Unauthorized: token_invalidated authentication token has been invalidated"
                .to_string(),
        );
        assert!(oneshot_auth_requires_fresh_login(&invalidated));

        let refreshable = AgentError::LlmApi("API error 401 Unauthorized".to_string());
        assert!(!oneshot_auth_requires_fresh_login(&refreshable));
    }

    #[test]
    fn oneshot_oauth_login_repair_supports_promptable_providers() {
        for provider in [
            "nous",
            "openai",
            "openai-codex",
            "anthropic",
            "qwen-oauth",
            "google-gemini-cli",
        ] {
            assert!(
                oneshot_oauth_provider_supports_login(provider),
                "{provider} should support one-shot login repair"
            );
        }
        assert!(!oneshot_oauth_provider_supports_login("openrouter"));
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

    include!("tests/provider_setup.rs");

    mod env_profile_diagnostics;

}
