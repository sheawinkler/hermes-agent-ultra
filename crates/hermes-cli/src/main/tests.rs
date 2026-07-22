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

    #[test]
    fn gateway_dm_grant_store_path_uses_cli_state_root() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let cli = cli_for_temp_state_root(temporary.path());

        assert_eq!(
            gateway_dm_grant_store_path(&cli),
            hermes_state_root(&cli).join("dm-grants.json")
        );
    }

    #[test]
    fn gateway_dm_manager_fails_closed_when_durable_grant_store_is_invalid() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let store = temporary.path().join("dm-grants.json");
        std::fs::write(&store, r#"{"version":99,"grants":[]}"#).expect("write invalid grant store");

        assert!(
            build_gateway_dm_manager_with_grant_store(&GatewayConfig::default(), store).is_err()
        );
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

    #[cfg(feature = "gateway-discord")]
    #[test]
    fn default_discord_gateway_config_requests_direct_messages() {
        let config = build_discord_config(
            &make_platform(true, Some("test-discord-token")),
            None,
            "test-discord-token".to_string(),
        );
        assert_ne!(
            config.intents & (1 << 12),
            0,
            "the CLI default must enable Discord DIRECT_MESSAGES"
        );
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
        let tmp = tempfile::tempdir().expect("tempdir");
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

    #[tokio::test]
    async fn auth_verify_expired_credential_is_byte_stable_and_never_calls_refresh_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _guard = env_lock();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let token_path = secret_vault_path_for_cli(&cli);
        let pool_path = auth_pool_path_for_cli(&cli);
        std::fs::create_dir_all(pool_path.parent().expect("pool parent")).expect("pool parent");
        std::fs::write(
            &pool_path,
            r#"{"providers":{"openai":[{"id":"legacy-a","label":"legacy","auth_type":"api_key","source":"manual","access_token":"legacy-pool-verify-sentinel"}]}}"#,
        )
        .expect("write legacy pool");
        let pool_before = std::fs::read(&pool_path).expect("read legacy pool");

        let store = FileTokenStore::new(&token_path)
            .await
            .expect("token store");
        AuthManager::new(store)
            .save_credential(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "expired-verify-sentinel".to_string(),
                refresh_token: Some("refresh-verify-sentinel".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            })
            .await
            .expect("save expired credential");
        let before = std::fs::read(&token_path).expect("read encrypted vault");

        let previous_token_url = std::env::var("HERMES_OPENAI_OAUTH_TOKEN_URL").ok();
        std::env::set_var("HERMES_OPENAI_OAUTH_TOKEN_URL", format!("{}/token", server.uri()));
        let result = run_auth(
            cli,
            Some("verify".to_string()),
            Some("openai".to_string()),
            None,
            None,
            None,
            None,
            false,
        )
        .await;
        match previous_token_url {
            Some(value) => std::env::set_var("HERMES_OPENAI_OAUTH_TOKEN_URL", value),
            None => std::env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL"),
        }

        assert!(matches!(result, Err(AgentError::AuthFailed(_))));
        assert_eq!(
            std::fs::read(&token_path).expect("read encrypted vault"),
            before,
            "auth verify must not rewrite an expired credential"
        );
        assert_eq!(
            std::fs::read(&pool_path).expect("read legacy pool"),
            pool_before,
            "auth verify must not migrate auth-pool metadata"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn auth_verify_legacy_store_requires_explicit_migration_without_rewrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let token_path = secret_vault_path_for_cli(&cli);
        std::fs::create_dir_all(token_path.parent().expect("vault parent")).expect("vault parent");
        let sentinel = "legacy-verify-sentinel-must-not-leak";
        std::fs::write(
            &token_path,
            serde_json::to_vec(&serde_json::json!({
                "openai": {
                    "provider": "openai",
                    "access_token": sentinel,
                    "refresh_token": "legacy-refresh-sentinel",
                    "token_type": "Bearer",
                    "scope": null,
                    "expires_at": (chrono::Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
                }
            }))
            .expect("serialize legacy store"),
        )
        .expect("write legacy store");
        let before = std::fs::read(&token_path).expect("read legacy store");
        let key_path = token_path.with_extension("key");
        assert!(!key_path.exists(), "verify must not create a store key");

        let error = run_auth(
            cli,
            Some("verify".to_string()),
            Some("openai".to_string()),
            None,
            None,
            None,
            None,
            false,
        )
        .await
        .expect_err("legacy migration must be an explicit credential-management effect");

        assert!(matches!(error, AgentError::AuthFailed(_)));
        assert!(error.to_string().contains("migration is required"));
        assert!(!error.to_string().contains(sentinel));
        assert_eq!(
            std::fs::read(&token_path).expect("read legacy store"),
            before,
            "auth verify must not migrate a legacy credential store"
        );
        assert!(!key_path.exists(), "verify must not create a store key");
    }

    #[tokio::test]
    async fn legacy_auth_pool_migrates_credentials_into_encrypted_vault_and_scrubs_metadata() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let pool_path = auth_pool_path_for_cli(&cli);
        std::fs::create_dir_all(pool_path.parent().expect("pool parent")).expect("pool parent");
        let sentinel = "legacy-pool-sentinel-must-not-leak";
        std::fs::write(
            &pool_path,
            format!(
                r#"{{"providers":{{"openai":[{{"id":"legacy-a","label":"legacy","auth_type":"api_key","source":"manual","access_token":"{sentinel}"}}]}}}}"#
            ),
        )
        .expect("legacy pool fixture");

        let token_store = FileTokenStore::new(secret_vault_path_for_cli(&cli))
            .await
            .expect("token store");
        let manager = AuthManager::new(token_store.clone());
        let pool = load_auth_pool_store(&pool_path, &manager, &token_store)
            .await
            .expect("migrate pool");
        let entry = &pool.providers["openai"][0];
        assert_eq!(entry.credential_ref, "pool:openai:legacy-a");
        assert_eq!(
            token_store
                .get("pool:openai:legacy-a")
                .await
                .expect("migrated credential")
                .access_token,
            sentinel
        );
        assert_eq!(
            token_store
                .get("openai")
                .await
                .expect("runtime credential")
                .access_token,
            sentinel
        );

        let metadata = std::fs::read_to_string(&pool_path).expect("scrubbed metadata");
        let encrypted = std::fs::read_to_string(secret_vault_path_for_cli(&cli))
            .expect("encrypted vault");
        assert!(!metadata.contains(sentinel));
        assert!(!metadata.contains("access_token"));
        assert!(!metadata.contains("refresh_token"));
        assert!(!encrypted.contains(sentinel));
        assert!(!format!("{pool:?}").contains(sentinel));
    }

    #[tokio::test]
    async fn auth_refresh_uses_registered_oauth_handler_and_encrypted_store() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _guard = env_lock();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "refreshed-cli-sentinel",
                "refresh_token": "rotated-cli-refresh-sentinel",
                "token_type": "Bearer",
                "expires_in": 120
            })))
            .expect(1)
            .mount(&server)
            .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let cli = cli_for_temp_state_root(tmp.path());
        let token_path = secret_vault_path_for_cli(&cli);
        let store = FileTokenStore::new(&token_path)
            .await
            .expect("token store");
        AuthManager::new(store)
            .save_credential(OAuthCredential {
                provider: "openai".to_string(),
                access_token: "expired-cli-sentinel".to_string(),
                refresh_token: Some("old-cli-refresh-sentinel".to_string()),
                token_type: "Bearer".to_string(),
                scope: None,
                expires_at: Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            })
            .await
            .expect("expired credential");

        let old_url = std::env::var("HERMES_OPENAI_OAUTH_TOKEN_URL").ok();
        let old_client_id = std::env::var("HERMES_OPENAI_OAUTH_CLIENT_ID").ok();
        std::env::set_var("HERMES_OPENAI_OAUTH_TOKEN_URL", format!("{}/token", server.uri()));
        std::env::set_var("HERMES_OPENAI_OAUTH_CLIENT_ID", "test-client");
        let result = run_auth(
            cli.clone(),
            Some("refresh".to_string()),
            Some("openai".to_string()),
            None,
            None,
            None,
            None,
            false,
        )
        .await;
        match old_url {
            Some(value) => std::env::set_var("HERMES_OPENAI_OAUTH_TOKEN_URL", value),
            None => std::env::remove_var("HERMES_OPENAI_OAUTH_TOKEN_URL"),
        }
        match old_client_id {
            Some(value) => std::env::set_var("HERMES_OPENAI_OAUTH_CLIENT_ID", value),
            None => std::env::remove_var("HERMES_OPENAI_OAUTH_CLIENT_ID"),
        }
        result.expect("explicit refresh");

        let reopened = FileTokenStore::new(&token_path)
            .await
            .expect("reopen vault");
        let credential = reopened.get("openai").await.expect("refreshed credential");
        assert_eq!(credential.access_token, "refreshed-cli-sentinel");
        assert!(!format!("{credential:?}").contains("refreshed-cli-sentinel"));
        let raw = std::fs::read_to_string(token_path).expect("encrypted store");
        assert!(!raw.contains("refreshed-cli-sentinel"));
        assert!(!raw.contains("expired-cli-sentinel"));
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
    fn telegram_gateway_message_keeps_group_parent_and_topic_separate() {
        let incoming = TelegramIncomingMessage {
            chat_id: -1001,
            user_id: Some(42),
            username: Some("alice".to_string()),
            is_bot: Some(false),
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
        assert_eq!(
            telegram_gateway_sender(&incoming),
            GatewayIncomingSender::human()
        );

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "-1001");
        assert_eq!(routed.thread_id.as_deref(), Some("17585"));
        assert_eq!(routed.user_id, "42");
        assert!(!routed.is_dm);
    }

    #[cfg(feature = "gateway-telegram")]
    #[test]
    fn telegram_gateway_message_keeps_private_parent_and_topic_separate() {
        let incoming = TelegramIncomingMessage {
            chat_id: 208214988,
            user_id: Some(42),
            username: Some("alice".to_string()),
            is_bot: Some(true),
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
        assert_eq!(
            telegram_gateway_sender(&incoming),
            GatewayIncomingSender::bot()
        );

        let routed = telegram_gateway_message(incoming);
        assert_eq!(routed.chat_id, "208214988");
        assert_eq!(routed.thread_id.as_deref(), Some("17585"));
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
            is_bot: Some(false),
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
    fn oneshot_oauth_remediation_provider_detects_nous_401_errors() {
        let err = AgentError::LlmApi(
            "API error 401 Unauthorized: https://portal.nousresearch.com".to_string(),
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&err, Some("nous"), Some("nous:openai/gpt-5.5")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&err, None, Some("nous:moonshotai/kimi-k2.6")),
            Some("nous".to_string())
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&err, None, None),
            Some("nous".to_string())
        );
    }

    #[test]
    fn oneshot_oauth_remediation_provider_supports_core_oauth_providers() {
        let openai = AgentError::LlmApi("API error 401 Unauthorized: auth.openai.com".to_string());
        assert_eq!(
            oneshot_oauth_remediation_provider(&openai, Some("openai"), Some("openai:gpt-5.5")),
            Some("openai".to_string())
        );
        let openai_invalidated = AgentError::LlmApi(
            r#"API error 401 Unauthorized: {"error":{"message":"Your authentication token has been invalidated. Please try signing in again.","code":"token_invalidated"}}"#
                .to_string(),
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(
                &openai_invalidated,
                Some("openai"),
                Some("openai:gpt-5.5")
            ),
            Some("openai".to_string())
        );
        let codex = AgentError::LlmApi("API error 401 Unauthorized: chatgpt.com codex".to_string());
        assert_eq!(
            oneshot_oauth_remediation_provider(&codex, None, Some("openai-codex:codex-mini")),
            Some("openai-codex".to_string())
        );
        let anthropic = AgentError::LlmApi(
            "API error 401 Unauthorized: console.anthropic.com token expired".to_string(),
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&anthropic, Some("claude"), None),
            Some("anthropic".to_string())
        );
        let gemini = AgentError::LlmApi(
            "API error 401 Unauthorized: oauth2.googleapis.com invalid_grant".to_string(),
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&gemini, Some("gemini-cli"), None),
            Some("google-gemini-cli".to_string())
        );
        let qwen = AgentError::LlmApi(
            "API error 401 Unauthorized: chat.qwen.ai token expired".to_string(),
        );
        assert_eq!(
            oneshot_oauth_remediation_provider(&qwen, Some("qwen-cli"), None),
            Some("qwen-oauth".to_string())
        );
    }

    #[test]
    fn oneshot_oauth_remediation_provider_ignores_non_oauth_or_non_auth_errors() {
        let not_auth = AgentError::LlmApi("API error 404 Not Found".to_string());
        assert_eq!(
            oneshot_oauth_remediation_provider(
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
            oneshot_oauth_remediation_provider(
                &other_provider,
                Some("openrouter"),
                Some("openrouter:openai/gpt-4o")
            ),
            None
        );

        let missing_signal = AgentError::LlmApi("API error 500 Internal Server Error".to_string());
        assert_eq!(
            oneshot_oauth_remediation_provider(
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

    #[tokio::test]
    async fn oneshot_oauth_failure_returns_original_error_without_retry_or_verify() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        const ORIGINAL_ERROR: &str = "API error 401 Unauthorized: portal.nousresearch.com";
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let original_request_calls = Arc::clone(&provider_calls);
        let error = run_oneshot_once_with_oauth_guidance(
            move || async move {
                original_request_calls.fetch_add(1, Ordering::SeqCst);
                Err(AgentError::LlmApi(ORIGINAL_ERROR.to_string()))
            },
            Some("nous"),
            Some("nous:openai/gpt-5.5"),
        )
        .await
        .expect_err("the original provider error must be returned");

        assert!(
            matches!(error, AgentError::LlmApi(message) if message == ORIGINAL_ERROR),
            "one-shot remediation must preserve the original typed error"
        );
        assert_eq!(
            provider_calls.load(Ordering::SeqCst),
            1,
            "a failed one-shot must not retry, verify, or make another provider call"
        );
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
