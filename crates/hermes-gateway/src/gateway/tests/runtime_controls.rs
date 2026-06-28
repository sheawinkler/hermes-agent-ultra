use super::*;

#[tokio::test]
async fn gateway_runtime_state_is_injected_into_agent_messages() {
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
            let hint = messages
                .iter()
                .find(|m| {
                    m.role == MessageRole::System
                        && m.content
                            .as_deref()
                            .unwrap_or("")
                            .contains("[gateway_runtime]")
                })
                .and_then(|m| m.content.clone())
                .unwrap_or_else(|| "no-runtime-hints".to_string());
            Ok(hint)
        })
    }))
    .await;

    let configured_model = format!("dynamic-runtime-model-{}", std::process::id());
    let set_provider = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/provider openai".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_provider).await.is_ok());

    let set_model = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: format!("/model {configured_model}"),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_model).await.is_ok());

    let set_profile = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/profile prod".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_profile).await.is_ok());

    let set_branch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/branch feature/parity".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_branch).await.is_ok());

    let set_fast = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/fast".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&set_fast).await.is_ok());

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let msgs = sent.lock().unwrap();
    let echoed = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.contains("[gateway_runtime]") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("runtime hint response should exist");

    assert!(echoed.contains(&format!("model={configured_model}")));
    assert!(!echoed.contains("gpt-4o"));
    assert!(echoed.contains("provider=openai"));
    assert!(echoed.contains("profile=prod"));
    assert!(echoed.contains("branch=feature/parity"));
    assert!(echoed.contains("service_tier=priority"));
}

#[tokio::test]
async fn gateway_model_switch_persists_default_and_applies_to_new_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "model: nous:nousresearch/hermes-4-70b\nmodel_switch:\n  persist_switch_by_default: true\n",
    )
    .unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    dm_manager.authorize_user("user2");
    let cfg = GatewayConfig {
        model: Some("nous:nousresearch/hermes-4-70b".to_string()),
        model_switch_config_path: Some(config_path.to_string_lossy().to_string()),
        ..GatewayConfig::default()
    };
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move { Ok(format!("ctx model={:?}", ctx.model)) })
    }))
    .await;

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/model openrouter:zai/glm-5.2".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());
    let disk = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(disk.model.as_deref(), Some("openrouter:zai/glm-5.2"));

    let new_session_message = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat2".into(),
        user_id: "user2".into(),
        text: "hello from a fresh gateway session".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&new_session_message).await.is_ok());

    let msgs = sent.lock().unwrap();
    assert!(msgs.iter().any(|(_, text)| text.contains("Saved to")));
    assert!(msgs
        .iter()
        .any(|(_, text)| text.contains("ctx model=Some(\"openrouter:zai/glm-5.2\")")));
}

#[tokio::test]
async fn gateway_model_switch_session_scope_does_not_persist_and_warns_on_large_context() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(
        &config_path,
        "model: nous:nousresearch/hermes-4-70b\nmodel_switch:\n  persist_switch_by_default: true\n",
    )
    .unwrap();

    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let session_key = session_mgr.compose_session_key("test", "chat1", "user1");
    session_mgr
        .get_or_create_session("test", "chat1", "user1")
        .await;
    session_mgr
        .add_message(&session_key, Message::user("large-context ".repeat(40_000)))
        .await;
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let cfg = GatewayConfig {
        model: Some("nous:nousresearch/hermes-4-70b".to_string()),
        model_switch_config_path: Some(config_path.to_string_lossy().to_string()),
        ..GatewayConfig::default()
    };
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("test", adapter).await;

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/model compact-runtime-model --global --session".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());

    let disk = hermes_config::load_user_config_file(&config_path).unwrap();
    assert_eq!(
        disk.model.as_deref(),
        Some("nous:nousresearch/hermes-4-70b")
    );
    let msgs = sent.lock().unwrap();
    let reply = msgs
        .iter()
        .find_map(|(_, text)| {
            text.contains("compact-runtime-model")
                .then_some(text.clone())
        })
        .expect("model switch reply should exist");
    assert!(reply.contains("Session only"));
    assert!(reply.contains("Context warning"));
    assert!(reply.contains("preflight compression"));
}

#[tokio::test]
async fn gateway_verbose_command_is_config_gated_and_cycles_tool_progress() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter.clone()).await;

    let verbose = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "chat-verbose".into(),
        user_id: "user1".into(),
        text: "/verbose".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&verbose).await.is_ok());
    assert!(sent
        .lock()
        .unwrap()
        .last()
        .expect("disabled reply")
        .1
        .contains("tool_progress_command"));

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.tool_progress_command = true;
    cfg.display.tool_progress = Some("all".to_string());
    let gw = Gateway::new(session_mgr, dm_manager, cfg);
    gw.register_adapter("telegram", adapter.clone()).await;

    assert!(gw.route_message(&verbose).await.is_ok());
    let first = sent
        .lock()
        .unwrap()
        .last()
        .expect("verbose reply")
        .1
        .clone();
    assert!(first.contains("telegram"));
    assert!(first.contains("VERBOSE"));

    let session_key = gw
        .session_manager
        .compose_session_key("telegram", "chat-verbose", "user1");
    let states = gw.runtime_state.read().await;
    let state = states.get(&session_key).expect("runtime state");
    assert_eq!(state.tool_progress.as_deref(), Some("verbose"));
    assert!(state.verbose);
    drop(states);

    assert!(gw.route_message(&verbose).await.is_ok());
    let second = sent.lock().unwrap().last().expect("off reply").1.clone();
    assert!(second.contains("OFF"));
}

#[tokio::test]
async fn gateway_new_clears_yolo_only_for_target_session() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key_1 = gw
        .session_manager
        .compose_session_key("test", "chat-yolo-new-1", "user1");
    let session_key_2 = gw
        .session_manager
        .compose_session_key("test", "chat-yolo-new-2", "user1");
    hermes_tools::approval::clear_session(&session_key_1);
    hermes_tools::approval::clear_session(&session_key_2);
    hermes_tools::approval::approve_session(&session_key_1, "recursive delete");
    hermes_tools::approval::approve_session(&session_key_2, "recursive delete");

    let yolo_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-1".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat1).await.is_ok());

    let yolo_chat2 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-2".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat2).await.is_ok());

    {
        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(true));
        assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
    }
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_1
    ));
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_2
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_1,
        "recursive delete"
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_2,
        "recursive delete"
    ));

    let reset_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-new-1".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset_chat1).await.is_ok());

    let states = gw.runtime_state.read().await;
    assert_eq!(states.get(&session_key_1).map(|s| s.yolo), Some(false));
    assert_eq!(states.get(&session_key_2).map(|s| s.yolo), Some(true));
    assert!(!hermes_tools::approval::is_session_yolo_enabled(
        &session_key_1
    ));
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &session_key_2
    ));
    assert!(!hermes_tools::approval::is_approved(
        &session_key_1,
        "recursive delete"
    ));
    assert!(hermes_tools::approval::is_approved(
        &session_key_2,
        "recursive delete"
    ));
    hermes_tools::approval::clear_session(&session_key_2);
}

#[tokio::test]
async fn telegram_topic_chat_ids_are_independent_session_lanes() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let user_turns = messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count();
            Ok(format!("topic-turns={user_turns}"))
        })
    }))
    .await;

    for (chat_id, text) in [
        ("208214988:111", "topic a first"),
        ("208214988:222", "topic b first"),
        ("208214988:111", "topic a second"),
    ] {
        gw.route_message(&IncomingMessage {
            platform: "telegram".into(),
            chat_id: chat_id.into(),
            user_id: "user1".into(),
            text: text.into(),
            message_id: None,
            thread_id: None,
            is_dm: true,
        })
        .await
        .expect("route telegram topic message");
    }

    let topic_a_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let topic_b_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    let topic_a = session_mgr.get_messages(&topic_a_key).await;
    let topic_b = session_mgr.get_messages(&topic_b_key).await;

    assert_eq!(topic_a.len(), 4);
    assert_eq!(topic_b.len(), 2);
    assert_eq!(
        topic_a
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["topic a first", "topic a second"]
    );
    assert_eq!(
        topic_b
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .filter_map(|message| message.content.as_deref())
            .collect::<Vec<_>>(),
        vec!["topic b first"]
    );

    let sent = sent.lock().expect("sent lock");
    assert_eq!(
        sent.iter()
            .map(|(chat_id, text)| (chat_id.as_str(), text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            ("208214988:111", "topic-turns=1"),
            ("208214988:222", "topic-turns=1"),
            ("208214988:111", "topic-turns=2"),
        ]
    );
}

#[tokio::test]
async fn telegram_topic_new_resets_only_current_topic_lane() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|messages| {
        Box::pin(async move {
            let user_turns = messages
                .iter()
                .filter(|message| message.role == MessageRole::User)
                .count();
            Ok(format!("topic-turns={user_turns}"))
        })
    }))
    .await;

    let topic_a = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:111".into(),
        user_id: "user1".into(),
        text: "topic a before reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    let topic_b = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:222".into(),
        user_id: "user1".into(),
        text: "topic b remains".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    gw.route_message(&topic_a).await.expect("route topic a");
    gw.route_message(&topic_b).await.expect("route topic b");

    gw.route_message(&IncomingMessage {
        text: "/new".into(),
        ..topic_a.clone()
    })
    .await
    .expect("reset topic a");

    let topic_a_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let topic_b_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    assert!(session_mgr.get_messages(&topic_a_key).await.is_empty());
    assert_eq!(session_mgr.get_messages(&topic_b_key).await.len(), 2);

    gw.route_message(&IncomingMessage {
        text: "topic a after reset".into(),
        ..topic_a
    })
    .await
    .expect("route topic a after reset");

    let topic_a_messages = session_mgr.get_messages(&topic_a_key).await;
    let topic_b_messages = session_mgr.get_messages(&topic_b_key).await;
    assert_eq!(topic_a_messages.len(), 2);
    assert_eq!(topic_b_messages.len(), 2);
    assert_eq!(
        topic_a_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("topic a after reset")
    );
    assert_eq!(
        topic_b_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("topic b remains")
    );
}

#[tokio::test]
async fn telegram_topic_restore_reuses_session_switch_path() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;

    let target_key = session_mgr.compose_session_key("telegram", "208214988:111", "user1");
    let current_key = session_mgr.compose_session_key("telegram", "208214988:222", "user1");
    let sibling_key = session_mgr.compose_session_key("telegram", "208214988:333", "user1");

    let _ = session_mgr
        .get_or_create_session("telegram", "208214988:111", "user1")
        .await;
    session_mgr
        .add_message(&target_key, Message::user("restored topic history"))
        .await;
    let _ = session_mgr
        .get_or_create_session("telegram", "208214988:333", "user1")
        .await;
    session_mgr
        .add_message(&sibling_key, Message::user("sibling history"))
        .await;

    gw.route_message(&IncomingMessage {
        platform: "telegram".into(),
        chat_id: "208214988:222".into(),
        user_id: "user1".into(),
        text: format!("/topic {}", target_key),
        message_id: None,
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("restore topic session");

    let current_messages = session_mgr.get_messages(&current_key).await;
    let sibling_messages = session_mgr.get_messages(&sibling_key).await;
    assert_eq!(
        current_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("restored topic history")
    );
    assert_eq!(
        sibling_messages
            .iter()
            .find(|message| message.role == MessageRole::User)
            .and_then(|message| message.content.as_deref()),
        Some("sibling history")
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Switched to session")));
}

#[tokio::test]
async fn gateway_switch_session_clears_yolo_for_current_chat_context() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let current_key =
        gw.session_manager
            .compose_session_key("test", "chat-yolo-switch-current", "user1");
    let target_key =
        gw.session_manager
            .compose_session_key("test", "chat-yolo-switch-target", "user1");
    hermes_tools::approval::clear_session(&current_key);
    hermes_tools::approval::approve_session(&current_key, "recursive delete");

    let _ = gw
        .session_manager
        .get_or_create_session("test", "chat-yolo-switch-target", "user1")
        .await;
    gw.session_manager
        .add_message(&target_key, Message::user("history from another session"))
        .await;

    let yolo_chat1 = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-switch-current".into(),
        user_id: "user1".into(),
        text: "/yolo".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&yolo_chat1).await.is_ok());
    {
        let states = gw.runtime_state.read().await;
        assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(true));
    }
    assert!(hermes_tools::approval::is_session_yolo_enabled(
        &current_key
    ));
    assert!(hermes_tools::approval::is_approved(
        &current_key,
        "recursive delete"
    ));

    let switch = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-yolo-switch-current".into(),
        user_id: "user1".into(),
        text: format!("/sessions {}", target_key),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&switch).await.is_ok());

    let states = gw.runtime_state.read().await;
    assert_eq!(states.get(&current_key).map(|s| s.yolo), Some(false));
    assert!(!hermes_tools::approval::is_session_yolo_enabled(
        &current_key
    ));
    assert!(!hermes_tools::approval::is_approved(
        &current_key,
        "recursive delete"
    ));
}

#[tokio::test]
async fn gateway_approve_resolves_oldest_blocking_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-approve-command", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let session_for_thread = session_key.clone();
    let handle = std::thread::spawn(move || {
        hermes_tools::approval::check_all_command_guards_with_context(
            "rm -rf /tmp/gateway-approve-command",
            "local",
            hermes_tools::approval::CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: std::time::Duration::from_secs(5),
                tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                ..hermes_tools::approval::CommandGuardContext::default()
            },
            None,
        )
        .expect("approval guard should return")
    });

    let request = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("gateway approval notify should fire");
    assert_eq!(request.command, "rm -rf /tmp/gateway-approve-command");
    assert!(hermes_tools::approval::has_blocking_approval(&session_key));

    let approve = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-approve-command".into(),
        user_id: "user1".into(),
        text: "/approve".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&approve).await.is_ok());

    let result = handle.join().expect("approval guard thread should join");
    assert!(result.approved);
    assert!(result.user_approved);
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));

    let replies = sent.lock().unwrap();
    assert!(replies.iter().any(|(_, text)| {
        text.to_ascii_lowercase().contains("approved") && text.contains("Resuming")
    }));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_deny_all_resolves_all_blocking_commands() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-deny-all-command", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let mut handles = Vec::new();
    for suffix in ["a", "b"] {
        let session_for_thread = session_key.clone();
        handles.push(std::thread::spawn(move || {
            hermes_tools::approval::check_all_command_guards_with_context(
                &format!("rm -rf /tmp/gateway-deny-{suffix}"),
                "local",
                hermes_tools::approval::CommandGuardContext {
                    gateway: true,
                    ask: true,
                    session_key: Some(session_for_thread),
                    gateway_approval_timeout: std::time::Duration::from_secs(5),
                    tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                    ..hermes_tools::approval::CommandGuardContext::default()
                },
                None,
            )
            .expect("approval guard should return")
        }));
    }

    for _ in 0..2 {
        rx.recv_timeout(std::time::Duration::from_secs(2))
            .expect("gateway approval notify should fire");
    }
    assert_eq!(
        hermes_tools::approval::pending_gateway_approval_count(&session_key),
        2
    );

    let deny = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-deny-all-command".into(),
        user_id: "user1".into(),
        text: "/deny all".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&deny).await.is_ok());

    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("approval guard thread should join"))
        .collect::<Vec<_>>();
    assert!(results.iter().all(|result| !result.approved));
    assert!(results.iter().all(|result| {
        result
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("User denied")
    }));
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));

    let replies = sent.lock().unwrap();
    assert!(replies
        .iter()
        .any(|(_, text)| text.contains("Denied 2 pending commands")));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_new_denies_blocked_approval_for_target_session() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;

    let session_key =
        gw.session_manager
            .compose_session_key("test", "chat-boundary-approval", "user1");
    hermes_tools::approval::clear_session(&session_key);
    let (tx, rx) = std::sync::mpsc::channel();
    hermes_tools::approval::register_gateway_notify(&session_key, move |request| {
        tx.send(request).expect("approval request should send");
    });

    let session_for_thread = session_key.clone();
    let handle = std::thread::spawn(move || {
        hermes_tools::approval::check_all_command_guards_with_context(
            "rm -rf /tmp/gateway-boundary-approval",
            "local",
            hermes_tools::approval::CommandGuardContext {
                gateway: true,
                ask: true,
                session_key: Some(session_for_thread),
                gateway_approval_timeout: std::time::Duration::from_secs(5),
                tirith_result: Ok(Some(hermes_tools::approval::TirithResult::allow())),
                ..hermes_tools::approval::CommandGuardContext::default()
            },
            None,
        )
        .expect("approval guard should return")
    });

    rx.recv_timeout(std::time::Duration::from_secs(2))
        .expect("gateway approval notify should fire");
    assert!(hermes_tools::approval::has_blocking_approval(&session_key));

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-boundary-approval".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let result = handle.join().expect("approval guard thread should join");
    assert!(!result.approved);
    assert!(result
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("User denied"));
    assert!(!hermes_tools::approval::has_blocking_approval(&session_key));
    hermes_tools::approval::unregister_gateway_notify(&session_key);
    hermes_tools::approval::clear_session(&session_key);
}

#[tokio::test]
async fn gateway_slack_reaction_lifecycle_success() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("1710000000.123".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:1710000000.123:eyes".to_string(),
            "remove:C123:1710000000.123:eyes".to_string(),
            "add:C123:1710000000.123:white_check_mark".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_discord_reaction_lifecycle_success_uses_discord_emojis() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("123456789".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:123456789:👀".to_string(),
            "remove:C123:123456789:👀".to_string(),
            "add:C123:123456789:✅".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_discord_reactions_can_be_disabled_by_policy() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("discord", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let mut policies = HashMap::new();
    policies.insert(
        "discord".to_string(),
        PlatformAccessPolicy {
            reactions_enabled: Some(false),
            ..PlatformAccessPolicy::default()
        },
    );
    gw.set_platform_access_policies(policies).await;

    let incoming = IncomingMessage {
        platform: "discord".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("123456789".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gateway_telegram_reactions_require_explicit_policy_enable() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("telegram", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "telegram".into(),
        chat_id: "123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("456".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());

    let mut policies = HashMap::new();
    policies.insert(
        "telegram".to_string(),
        PlatformAccessPolicy {
            reactions_enabled: Some(true),
            ..PlatformAccessPolicy::default()
        },
    );
    gw.set_platform_access_policies(policies).await;

    let second_incoming = IncomingMessage {
        message_id: Some("457".into()),
        ..incoming
    };
    assert!(gw.route_message(&second_incoming).await.is_ok());
    assert_eq!(
        reactions.lock().unwrap().clone(),
        vec![
            "add:123:457:👀".to_string(),
            "remove:123:457:👀".to_string(),
            "add:123:457:👍".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_slack_reaction_lifecycle_failure_sets_error_reaction() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Err(GatewayError::Platform("boom".to_string())) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: Some("1710000000.456".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_err());

    let got = reactions.lock().unwrap().clone();
    assert_eq!(
        got,
        vec![
            "add:C123:1710000000.456:eyes".to_string(),
            "remove:C123:1710000000.456:eyes".to_string(),
            "add:C123:1710000000.456:x".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_slack_reactions_skip_non_dm_non_mentions() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ReactionTestAdapter {
        messages: sent.clone(),
        reactions: reactions.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let dm_manager = DmManager::with_pair_behavior();
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("slack", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("done".to_string()) })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "slack".into(),
        chat_id: "C123".into(),
        user_id: "user1".into(),
        text: "general channel chatter".into(),
        message_id: Some("1710000000.789".into()),
        thread_id: None,
        is_dm: false,
    };
    assert!(gw.route_message(&incoming).await.is_ok());
    assert!(reactions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn gateway_context_handler_receives_structured_runtime_context() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|messages, ctx| {
            Box::pin(async move {
                let payload = format!(
                    "ctx model={:?} provider={:?} profile={:?} branch={:?} platform={} user={} session={} has_legacy_hint={}",
                    ctx.model,
                    ctx.provider,
                    ctx.profile,
                    ctx.branch,
                    ctx.platform,
                    ctx.user_id,
                    ctx.session_key,
                    messages.iter().any(|m| m
                        .content
                        .as_deref()
                        .unwrap_or("")
                        .contains("[gateway_runtime]"))
                );
                Ok(payload)
            })
        }))
        .await;

    let setup_cmds = vec![
        "/provider openai",
        "/model dynamic-structured-context-model",
        "/profile prod",
        "/branch feat-123",
    ];
    for cmd in setup_cmds {
        let incoming = IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: cmd.to_string(),
            message_id: None,
            thread_id: None,
            is_dm: true,
        };
        assert!(gw.route_message(&incoming).await.is_ok());
    }

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "run".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());

    let msgs = sent.lock().unwrap();
    let echoed = msgs
        .iter()
        .rev()
        .find_map(|(_, text)| {
            if text.starts_with("ctx model=") {
                Some(text.clone())
            } else {
                None
            }
        })
        .expect("context response should exist");
    assert!(echoed.contains("Some(\"dynamic-structured-context-model\")"));
    assert!(echoed.contains("Some(\"openai\")"));
    assert!(echoed.contains("Some(\"prod\")"));
    assert!(echoed.contains("Some(\"feat-123\")"));
    assert!(echoed.contains("platform=test"));
    assert!(echoed.contains("user=user1"));
    assert!(echoed.contains("has_legacy_hint=false"));
}

#[tokio::test]
async fn gateway_deferred_post_delivery_messages_flush_after_main_reply() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            let released = ctx
                .deferred_post_delivery_released
                .expect("release flag should be present");
            assert!(
                !released.load(std::sync::atomic::Ordering::Acquire),
                "release must remain false before main reply delivery"
            );
            pending
                .lock()
                .unwrap()
                .push("💾 deferred-memory-update".to_string());
            Ok("main-response".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "main-response".to_string(),
            "💾 deferred-memory-update".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_replies_and_deferred_messages_preserve_source_thread() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(ThreadOptionTestAdapter {
        sends: sends.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("thread-option-test", adapter).await;
    gw.set_message_handler_with_context(Arc::new(|_messages, ctx| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            pending
                .lock()
                .unwrap()
                .push("deferred follow-up".to_string());
            Ok("final reply".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "thread-option-test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "run".into(),
        message_id: Some("post-2".into()),
        thread_id: Some("root-1".into()),
        is_dm: true,
    };
    gw.route_message(&incoming)
        .await
        .expect("threaded route should succeed");

    let sent = sends.lock().unwrap().clone();
    assert_eq!(
        sent,
        vec![
            (
                "chat1".to_string(),
                "final reply".to_string(),
                Some("root-1".to_string()),
                true,
            ),
            (
                "chat1".to_string(),
                "deferred follow-up".to_string(),
                Some("root-1".to_string()),
                false,
            ),
        ]
    );
}

#[tokio::test]
async fn gateway_status_then_main_then_deferred_order_matches_python_chain() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter).await;

    let gw_for_handler = gw.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let gw = gw_for_handler.clone();
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            pending.lock().unwrap().push("💾 bg-review".to_string());

            // Mirrors Python's status_callback: status is forwarded immediately.
            gw.send_message(&ctx.platform, &ctx.chat_id, "⚠️ context pressure", None)
                .await
                .expect("status callback send should succeed");

            Ok("main-response".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "⚠️ context pressure".to_string(),
            "main-response".to_string(),
            "💾 bg-review".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_streaming_flushes_deferred_after_stream_finishes() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.streaming_enabled = true;
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    gw.set_streaming_handler_with_context(Arc::new(|_messages, ctx, _on_chunk| {
        Box::pin(async move {
            let pending = ctx
                .deferred_post_delivery_messages
                .expect("deferred queue should be present");
            let released = ctx
                .deferred_post_delivery_released
                .expect("release flag should be present");
            assert!(
                !released.load(std::sync::atomic::Ordering::Acquire),
                "release must stay false while stream handler is running"
            );
            pending
                .lock()
                .unwrap()
                .push("💾 stream-bg-review".to_string());
            Ok("stream-final".to_string())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_ok());

    let msgs = sent.lock().unwrap();
    let ordered: Vec<String> = msgs.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        ordered,
        vec![
            "...".to_string(),
            "stream-final".to_string(),
            "💾 stream-bg-review".to_string()
        ]
    );
}
