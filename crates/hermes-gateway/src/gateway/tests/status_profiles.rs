struct ThreadRenameAdapter {
    messages: Arc<Mutex<Vec<(String, String)>>>,
    renames: Arc<Mutex<Vec<(String, String)>>>,
}

#[async_trait::async_trait]
impl PlatformAdapter for ThreadRenameAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.messages
            .lock()
            .unwrap()
            .push((chat_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        _chat_id: &str,
        _file_path: &str,
        _caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn rename_thread(&self, thread_id: &str, title: &str) -> Result<bool, GatewayError> {
        self.renames
            .lock()
            .unwrap()
            .push((thread_id.to_string(), title.to_string()));
        Ok(true)
    }

    fn is_running(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "discord"
    }
}

#[tokio::test]
async fn gateway_status_updates_use_platform_status_api() {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(StatusUpdateAdapter {
        updates: updates.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let gw = Gateway::with_defaults(session_mgr, GatewayConfig::default());
    gw.register_adapter("status-test", adapter).await;

    gw.send_or_update_status(
        "status-test",
        "chat1",
        "context_pressure",
        "compressing",
        None,
    )
    .await
    .expect("first status update should succeed");
    gw.send_or_update_status("status-test", "chat1", "context_pressure", "done", None)
        .await
        .expect("second status update should succeed");

    let updates = updates.lock().unwrap();
    assert_eq!(
        *updates,
        vec![
            (
                "chat1".to_string(),
                "context_pressure".to_string(),
                "compressing".to_string()
            ),
            (
                "chat1".to_string(),
                "context_pressure".to_string(),
                "done".to_string()
            )
        ]
    );
}

#[tokio::test]
async fn gateway_route_dm_denied() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "unknown_user".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    // Should succeed (deny silently)
    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn gateway_route_no_handler() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    // Should fail because no message handler is set
    let result = gw.route_message(&incoming).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn gateway_route_group_message_skips_dm_check() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "-group1".into(),
        user_id: "unknown_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false, // Group message, no DM check
    };

    // Should fail because no handler, but DM check is skipped
    let result = gw.route_message(&incoming).await;
    assert!(result.is_err()); // No handler configured
}

#[tokio::test]
async fn gateway_group_allowlist_denies_unauthorized_user() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "other_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "other_user")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_group_allowlist_star_authorizes_any_sender() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("*".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "any_user".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "any_user")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_group_chat_authorization_allows_listed_chat_sender() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("999".to_string());
    policy
        .authorized_group_chats
        .insert("-1001878443972".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let legacy_chat_source = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-1001878443972".into(),
        user_id: "123".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&legacy_chat_source).await.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-1001878443972", "123")
            .await,
        1
    );

    let sender_source = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-1009999999999".into(),
        user_id: "999".into(),
        text: "hello group".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&sender_source).await.is_err());
    assert_eq!(
        gw.session_transcript_len("telegram", "-1009999999999", "999")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_route_unauthorized_dm_pairs_with_code_message() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("whatsapp", adapter).await;

    let incoming = IncomingMessage {
        platform: "whatsapp".into(),
        chat_id: "15551234567@s.whatsapp.net".into(),
        user_id: "15551234567@s.whatsapp.net".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());
    let messages = sent.lock().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].1.contains("pairing code"));
    assert_eq!(
        gw.session_transcript_len(
            "whatsapp",
            "15551234567@s.whatsapp.net",
            "15551234567@s.whatsapp.net"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn gateway_route_rate_limited_dm_sends_no_response() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    dm_manager.record_pairing_rate_limit("whatsapp", "15551234567@s.whatsapp.net");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("whatsapp", adapter).await;

    let incoming = IncomingMessage {
        platform: "whatsapp".into(),
        chat_id: "15551234567@s.whatsapp.net".into(),
        user_id: "15551234567@s.whatsapp.net".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(sent.lock().unwrap().is_empty());
    assert_eq!(
        gw.session_transcript_len(
            "whatsapp",
            "15551234567@s.whatsapp.net",
            "15551234567@s.whatsapp.net"
        )
        .await,
        0
    );
}

#[tokio::test]
async fn gateway_channel_allow_and_ignore_policy_matches_discord_contract() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("handled".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_channels.insert("allowed".to_string());
    policy.ignored_channels.insert("ignored".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let ignored = IncomingMessage {
        platform: "discord".into(),
        chat_id: "ignored".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&ignored).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "ignored", "user1")
            .await,
        0
    );

    let not_allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "other".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&not_allowed).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "other", "user1").await,
        0
    );

    let allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "allowed".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&allowed).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "allowed", "user1")
            .await,
        2
    );
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn gateway_allowed_channel_policy_blocks_mentions_but_not_dms() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("handled".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_channels.insert("-100allowed".to_string());
    policies.insert("telegram".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let mentioned_blocked_group = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100blocked".into(),
        user_id: "user1".into(),
        text: "@hermes_bot hello".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&mentioned_blocked_group).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100blocked", "user1")
            .await,
        0
    );

    let dm = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100blocked".into(),
        user_id: "user1".into(),
        text: "dm hello".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&dm).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100blocked", "user1")
            .await,
        2
    );
}

#[tokio::test]
async fn gateway_discord_slash_requires_allowlist() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;

    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Open,
        slash_requires_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("allowed_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let denied = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "random_user".into(),
        text: "/status".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&denied).await.is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "random_user")
            .await,
        0
    );

    let allowed = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "allowed_user".into(),
        text: "/status".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&allowed).await.is_ok());
    let sent_msgs = sent.lock().unwrap();
    assert_eq!(sent_msgs.len(), 1);
    assert_eq!(sent_msgs[0].0, "guild:1");
    assert!(!sent_msgs[0].1.trim().is_empty());
}

#[tokio::test]
async fn gateway_discord_bot_sender_can_bypass_user_allowlist_when_enabled() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("human_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "worker_bot".into(),
        text: "notion event".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };

    assert!(gw
        .route_message_from_sender(&incoming, IncomingSender::bot())
        .await
        .is_err());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "worker_bot")
            .await,
        1
    );
}

#[tokio::test]
async fn gateway_discord_bot_sender_still_rejected_when_bypass_disabled() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: false,
        ..PlatformAccessPolicy::default()
    };
    policy.allowed_users.insert("human_user".to_string());
    policies.insert("discord".to_string(), policy);
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "worker_bot".into(),
        text: "notion event".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };

    assert!(gw
        .route_message_from_sender(&incoming, IncomingSender::bot())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "worker_bot")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_discord_bot_bypass_does_not_apply_to_humans_or_other_platforms() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_ignore_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let mut policies = HashMap::new();
    let mut discord_policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    discord_policy
        .allowed_users
        .insert("human_user".to_string());
    policies.insert("discord".to_string(), discord_policy);
    let mut telegram_policy = PlatformAccessPolicy {
        group_mode: GroupAccessMode::Allowlist,
        bot_sender_bypasses_allowlist: true,
        ..PlatformAccessPolicy::default()
    };
    telegram_policy
        .allowed_users
        .insert("human_user".to_string());
    policies.insert("telegram".to_string(), telegram_policy);
    gw.set_platform_access_policies(policies).await;

    let discord_human = IncomingMessage {
        platform: "discord".into(),
        chat_id: "guild:1".into(),
        user_id: "other_human".into(),
        text: "hello".into(),
        message_id: Some("m1".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw
        .route_message_from_sender(&discord_human, IncomingSender::human())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("discord", "guild:1", "other_human")
            .await,
        0
    );

    let telegram_bot = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "-100123".into(),
        user_id: "worker_bot".into(),
        text: "hello".into(),
        message_id: Some("m2".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw
        .route_message_from_sender(&telegram_bot, IncomingSender::bot())
        .await
        .is_ok());
    assert_eq!(
        gw.session_transcript_len("telegram", "-100123", "worker_bot")
            .await,
        0
    );
}

#[tokio::test]
async fn gateway_executes_status_command_without_agent_handler() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    let result = gw.route_message(&incoming).await;
    assert!(result.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("Gateway status")));
}

#[tokio::test]
async fn gateway_compress_command_appends_warning_when_summary_unavailable() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat1", "user1")
        .await;
    gw.session_manager
        .add_message(&session_key, Message::system("sys"))
        .await;
    for _ in 0..40 {
        gw.session_manager
            .add_message(
                &session_key,
                Message {
                    role: MessageRole::Tool,
                    content: None,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    anthropic_content_blocks: None,
                    cache_control: None,
                },
            )
            .await;
    }

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/compress".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
    assert!(reply.contains("Context compressed"));
    assert!(reply.contains("⚠️ Context compression summary failed"));
    assert!(reply.contains("historical message(s) were removed"));
}

#[tokio::test]
async fn gateway_compress_command_emits_summary_without_warning() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat1", "user1")
        .await;
    gw.session_manager
        .add_message(&session_key, Message::system("sys"))
        .await;
    for i in 0..40 {
        let message = if i % 2 == 0 {
            Message::user(format!("turn {i} content"))
        } else {
            Message::assistant(format!("turn {i} content"))
        };
        gw.session_manager.add_message(&session_key, message).await;
    }

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/compress".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let reply = msgs.last().map(|(_, t)| t.clone()).unwrap_or_default();
    assert!(reply.contains("Context compressed"));
    assert!(!reply.contains("⚠️"));
    drop(msgs);

    let updated = gw.session_manager.get_messages(&session_key).await;
    assert!(
        updated.iter().any(|m| {
            m.content
                .as_deref()
                .unwrap_or("")
                .contains("[CONTEXT COMPACTION] Earlier conversation was compacted")
        }),
        "summary marker should be persisted into compressed transcript"
    );
}

#[tokio::test]
async fn gateway_usage_text_includes_last_nous_credits_state() {
    hermes_core::credits::clear_last_nous_credits_state();
    hermes_core::credits::capture_nous_credits_from_pairs([
        ("x-nous-credits-version", "1"),
        ("x-nous-credits-remaining-micros", "12000000"),
        ("x-nous-credits-remaining-usd", "12.00"),
        ("x-nous-credits-subscription-micros", "5000000"),
        ("x-nous-credits-subscription-usd", "5.00"),
        ("x-nous-credits-subscription-limit-micros", "10000000"),
        ("x-nous-credits-subscription-limit-usd", "10.00"),
        ("x-nous-credits-rollover-micros", "0"),
        ("x-nous-credits-purchased-micros", "7000000"),
        ("x-nous-credits-purchased-usd", "7.00"),
        ("x-nous-credits-denominator-kind", "subscription_cap"),
        ("x-nous-credits-paid-access", "true"),
    ])
    .expect("capture credits");

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let text = gw.build_usage_text("test:chat:user").await;

    assert!(text.contains("Usage"));
    assert!(text.contains("Nous credits"));
    assert!(text.contains("Subscription: 50% remaining (50% used)"));
    hermes_core::credits::clear_last_nous_credits_state();
}

#[tokio::test]
async fn gateway_background_task_lifecycle_commands_work() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let prompt = messages
                .last()
                .and_then(|m| m.content.clone())
                .unwrap_or_else(|| "none".to_string());
            Ok(format!("done: {}", prompt))
        })
    }))
    .await;

    let start = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/background ping".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&start).await.is_ok());

    let task_id = {
        let msgs = sent.lock().unwrap();
        let queued = msgs
            .iter()
            .find(|(_, text)| text.contains("Background task started"))
            .expect("queue ack should exist");
        queued
            .1
            .lines()
            .find_map(|line| line.strip_prefix("Task ID: ").map(str::trim))
            .expect("task id line")
            .to_string()
    };

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: format!("/background status {}", task_id),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("completed")));
}

#[tokio::test]
async fn gateway_admin_approve_and_deny_affects_dm_authorization() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_ignore_behavior();
    dm_manager.add_admin("admin1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let approve = IncomingMessage {
        platform: "test".into(),
        chat_id: "admin-chat".into(),
        user_id: "admin1".into(),
        text: "/approve user2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&approve).await.is_ok());

    // user2 should now pass DM authorization, then fail because no handler is configured.
    let authorized_dm = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-u2".into(),
        user_id: "user2".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&authorized_dm).await.is_err());

    let deny = IncomingMessage {
        platform: "test".into(),
        chat_id: "admin-chat".into(),
        user_id: "admin1".into(),
        text: "/deny user2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&deny).await.is_ok());

    // user2 should be denied again, and route should return Ok (silently denied).
    let denied_dm = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-u2".into(),
        user_id: "user2".into(),
        text: "hello again".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&denied_dm).await.is_ok());
}

#[tokio::test]
async fn gateway_reload_mcp_and_status_reflect_runtime_state() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let provider = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/provider openrouter".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&provider).await.is_ok());

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile prod".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let reload = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reload_mcp".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reload).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.contains("Gateway status") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("status response should exist");
    assert!(status_text.contains("provider: openrouter"));
    assert!(status_text.contains("profile: prod"));
    assert!(status_text.contains("mcp generation: 1"));
}

#[tokio::test]
async fn gateway_title_command_persists_and_surfaces_session_title() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let hooks = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let mut registry = HookRegistry::new();
    registry.register_in_process(
        "session:title",
        Arc::new(RecordingHook {
            seen: hooks.clone(),
        }),
    );
    gw.set_hook_registry(Arc::new(registry)).await;

    let title = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-title".into(),
        user_id: "user1".into(),
        text: "/title Release readiness".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&title).await.is_ok());
    let session_key = session_mgr.compose_session_key("test", "chat-title", "user1");
    assert_eq!(
        session_mgr.get_title(&session_key).await.as_deref(),
        Some("Release readiness")
    );

    let show_title = IncomingMessage {
        text: "/title".into(),
        ..title.clone()
    };
    assert!(gw.route_message(&show_title).await.is_ok());

    let status = IncomingMessage {
        text: "/status".into(),
        ..title.clone()
    };
    assert!(gw.route_message(&status).await.is_ok());

    let sessions = IncomingMessage {
        text: "/sessions".into(),
        ..title
    };
    assert!(gw.route_message(&sessions).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Session title set to: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Current session title: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("- title: Release readiness")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("title `Release readiness`")));
    drop(msgs);

    let hooks = hooks.lock().unwrap();
    let title_event = hooks
        .iter()
        .find(|(name, _)| name == "session:title")
        .expect("title hook emitted");
    assert_eq!(title_event.1["session_id"], serde_json::json!(session_key));
    assert_eq!(
        title_event.1["title"],
        serde_json::json!("Release readiness")
    );
}

#[tokio::test]
async fn gateway_title_command_mirrors_discord_thread_title() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let renames = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ThreadRenameAdapter {
        messages: sent.clone(),
        renames: renames.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;

    let title = IncomingMessage {
        platform: "discord".into(),
        chat_id: "parent-channel".into(),
        user_id: "user1".into(),
        text: "/title Release readiness".into(),
        message_id: Some("msg-1".into()),
        thread_id: Some("thread-42".into()),
        is_dm: false,
    };

    assert!(gw.route_message(&title).await.is_ok());

    let session_key = session_mgr.compose_session_key("discord", "parent-channel", "user1");
    assert_eq!(
        session_mgr.get_title(&session_key).await.as_deref(),
        Some("Release readiness")
    );
    assert_eq!(
        *renames.lock().unwrap(),
        vec![("thread-42".to_string(), "Release readiness".to_string())]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text == "🏷 Session title set to: Release readiness"));
}

#[tokio::test]
async fn gateway_sessions_search_matches_title_without_cross_user_leakage() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let target = session_mgr
        .get_or_create_session("test", "chat-target", "user1")
        .await;
    let target_key = session_mgr.compose_session_key(&target.platform, &target.chat_id, &target.user_id);
    session_mgr
        .set_title(&target_key, "AN-94 Prestige Barrel Build #2")
        .await;
    let filler = session_mgr
        .get_or_create_session("test", "chat-filler", "user1")
        .await;
    let filler_key = session_mgr.compose_session_key(&filler.platform, &filler.chat_id, &filler.user_id);
    session_mgr.set_title(&filler_key, "Filler Build").await;
    let other = session_mgr
        .get_or_create_session("test", "chat-secret", "user2")
        .await;
    let other_key = session_mgr.compose_session_key(&other.platform, &other.chat_id, &other.user_id);
    session_mgr
        .set_title(&other_key, "AN-94 someone else's secret")
        .await;

    let search = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-search".into(),
        user_id: "user1".into(),
        text: "/sessions search an94".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&search).await.is_ok());

    let msgs = sent.lock().unwrap();
    let reply = msgs.last().map(|(_, text)| text.as_str()).unwrap_or("");
    assert!(reply.contains("Sessions matching `an94`"));
    assert!(reply.contains("AN-94 Prestige Barrel Build #2"));
    assert!(reply.contains(&target_key));
    assert!(!reply.contains("Filler Build"));
    assert!(!reply.contains("secret"));
    assert!(!reply.contains(&other_key));
}

#[tokio::test]
async fn gateway_profile_command_applies_profile_yaml_overlay() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = HermesHomeEnvGuard::set(tmp.path());
    let profiles_dir = tmp.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    let profile_home = tmp.path().join("profile-home");
    std::fs::create_dir_all(&profile_home).unwrap();
    std::fs::write(
        profiles_dir.join("prod.yaml"),
        format!(
            "name: prod\nmodel: openrouter:qwen/qwen3-coder\npersonality: strict\nhome_dir: {}\n",
            profile_home.display()
        ),
    )
    .unwrap();
    std::fs::write(profiles_dir.join("aliases.json"), r#"{"work":"prod"}"#).unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile work".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    let profile_reply = msgs
        .iter()
        .find_map(|(_, text)| text.contains("Profile switched").then_some(text.clone()))
        .expect("profile reply should exist");
    assert!(profile_reply.contains("prod"));
    assert!(profile_reply.contains("requested 'work'"));
    assert!(profile_reply.contains("model=openrouter:qwen/qwen3-coder"));
    assert!(profile_reply.contains("provider=openrouter"));
    assert!(profile_reply.contains("personality=strict"));

    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| text.contains("Gateway status").then_some(text.clone()))
        .expect("status response should exist");
    assert!(status_text.contains("model: openrouter:qwen/qwen3-coder"));
    assert!(status_text.contains("provider: openrouter"));
    assert!(status_text.contains("profile: prod"));
    assert!(status_text.contains("personality: strict"));
    assert!(status_text.contains(&format!("home: {}", profile_home.display())));
}

#[tokio::test]
async fn gateway_profile_command_missing_file_preserves_label_with_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let _env = HermesHomeEnvGuard::set(tmp.path());

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile scratch".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&profile).await.is_ok());

    let status = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/status".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&status).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("Profile file not applied")));
    let status_text = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| text.contains("Gateway status").then_some(text.clone()))
        .expect("status response should exist");
    assert!(status_text.contains("profile: scratch"));
}
