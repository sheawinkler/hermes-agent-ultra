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
