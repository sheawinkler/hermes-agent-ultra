use super::*;

#[tokio::test]
async fn gateway_emits_agent_start_and_end_hooks() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("main-response".to_string()) })
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

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec!["agent:start".to_string(), "agent:end".to_string()]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "agent:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("agent:end payload should exist");
    assert_eq!(end_payload["success"], serde_json::json!(true));
}

#[tokio::test]
async fn gateway_busy_queue_mode_drains_fifo_followups() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    let texts: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, text)| text.clone())
        .collect();
    assert!(texts
        .iter()
        .any(|text| text.contains("Queued for the next turn")));
    assert!(texts.iter().any(|text| text == "reply:first"));
    assert!(texts.iter().any(|text| text == "reply:second"));
}

#[tokio::test]
async fn gateway_busy_queue_ack_can_be_suppressed() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    cfg.display.busy_ack_enabled = Some(false);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("second"))
        .await
        .expect("second route should queue silently");
    assert!(
        sent.lock()
            .unwrap()
            .iter()
            .all(|(_, text)| !text.contains("Queued for the next turn")),
        "automatic busy ack should be suppressed"
    );
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
}

#[tokio::test]
async fn gateway_queue_command_bypasses_busy_guard_and_drains() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("interrupt".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|m| {
                    (m.role == MessageRole::User)
                        .then(|| m.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    gw.route_message(&test_incoming("/queue second"))
        .await
        .expect("/queue should bypass and enqueue");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Queued follow-up for the active session")));
}

#[tokio::test]
async fn gateway_steer_command_uses_attached_busy_control() {
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

    let control = Arc::new(BusyControlProbe::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let control_for_handler = control.clone();
    let entered_for_handler = entered_tx.clone();
    let release_for_handler = release_rx.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let control = control_for_handler.clone();
        let entered = entered_for_handler.clone();
        let release = release_for_handler.clone();
        Box::pin(async move {
            if let Some(registration) = ctx.busy_control {
                assert!(registration.attach(control).await);
            }
            if let Some(tx) = entered.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = release.lock().await.take() {
                let _ = rx.await;
            }
            Ok("reply:first".to_string())
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should attach control");
    gw.route_message(&test_incoming("/steer check tests"))
        .await
        .expect("/steer should use attached control");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");

    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("Steered the running task")));
}

#[tokio::test]
async fn gateway_hook_event_order_captures_start_status_step_end() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "agent:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let gw_for_handler = gw.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let gw = gw_for_handler.clone();
        Box::pin(async move {
            gw.emit_hook_event(
                "agent:status",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "event_type": "lifecycle",
                    "message": "Context pressure 85%"
                }),
            )
            .await;
            gw.emit_hook_event(
                "agent:step",
                serde_json::json!({
                    "platform": ctx.platform,
                    "user_id": ctx.user_id,
                    "session_id": ctx.session_key,
                    "iteration": 1,
                    "tool_names": ["memory"],
                    "tools": [{"name":"memory","result":"ok"}]
                }),
            )
            .await;
            Ok("done".to_string())
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

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "agent:start".to_string(),
            "agent:status".to_string(),
            "agent:step".to_string(),
            "agent:end".to_string()
        ]
    );
}

#[tokio::test]
async fn gateway_emits_session_start_and_command_hook_events() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process(
        "command:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
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
    assert!(gw.route_message(&incoming).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert!(names.contains(&"session:start".to_string()));
    assert!(names.contains(&"command:status".to_string()));
}

#[tokio::test]
async fn gateway_emits_session_end_and_reset_for_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "session:*",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");

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

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat1".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "session:start".to_string(),
            "session:end".to_string(),
            "session:reset".to_string()
        ]
    );
    let end_payload = events
        .iter()
        .find(|(name, _)| name == "session:end")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:end payload should exist");
    let reset_payload = events
        .iter()
        .find(|(name, _)| name == "session:reset")
        .map(|(_, ctx)| ctx.clone())
        .expect("session:reset payload should exist");
    assert_eq!(end_payload["session_id"], serde_json::json!(session_key));
    assert_eq!(reset_payload["session_id"], serde_json::json!(session_key));
}

#[tokio::test]
async fn gateway_emits_plugin_session_finalize_and_reset_for_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );
    hooks.register_in_process(
        "on_session_reset",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async move { Ok("assistant".to_string()) })
    }))
    .await;
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat-plugin-reset", "user1");

    let normal = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "hello".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&normal).await.is_ok());
    let old_logical_id = gw
        .session_manager
        .get_session(&session_key)
        .await
        .expect("session should exist")
        .id;

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-plugin-reset".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let events = hook_seen.lock().unwrap();
    let names: Vec<String> = events.iter().map(|(name, _)| name.clone()).collect();
    assert_eq!(
        names,
        vec![
            "on_session_finalize".to_string(),
            "on_session_reset".to_string()
        ]
    );
    let finalize_payload = &events[0].1;
    let reset_payload = &events[1].1;
    assert_eq!(
        finalize_payload["session_id"],
        serde_json::json!(old_logical_id)
    );
    assert_eq!(
        finalize_payload["session_key"],
        serde_json::json!(session_key)
    );
    assert_eq!(finalize_payload["reason"], serde_json::json!("reset"));
    assert_eq!(reset_payload["session_key"], serde_json::json!(session_key));
    assert_eq!(reset_payload["reason"], serde_json::json!("reset"));
    assert_ne!(reset_payload["session_id"], finalize_payload["session_id"]);
}

#[tokio::test]
async fn gateway_stop_all_finalizes_active_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let first = session_mgr
        .get_or_create_session("test", "stop-chat-a", "user1")
        .await;
    let second = session_mgr
        .get_or_create_session("test", "stop-chat-b", "user2")
        .await;
    let gw = Gateway::new(
        session_mgr,
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    gw.stop_all().await.expect("stop should succeed");

    let events = hook_seen.lock().unwrap();
    let session_ids: HashSet<String> = events
        .iter()
        .filter(|(name, _)| name == "on_session_finalize")
        .filter_map(|(_, ctx)| ctx["session_id"].as_str().map(ToOwned::to_owned))
        .collect();
    assert_eq!(
        session_ids,
        HashSet::from([first.id.clone(), second.id.clone()])
    );
    assert!(events
        .iter()
        .all(|(_, ctx)| ctx["reason"] == serde_json::json!("shutdown")));
}

#[tokio::test]
async fn gateway_idle_expiry_finalizes_removed_sessions() {
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(RecordingHook {
            seen: hook_seen.clone(),
        }),
    );

    let session_config = SessionConfig {
        reset_policy: hermes_config::session::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    };
    let session_mgr = Arc::new(SessionManager::new(session_config));
    let expired = session_mgr
        .get_or_create_session("test", "idle-chat", "user1")
        .await;
    let session_key = session_mgr.compose_session_key("test", "idle-chat", "user1");
    let gw = Gateway::new(
        session_mgr.clone(),
        DmManager::with_pair_behavior(),
        GatewayConfig::default(),
    );
    gw.set_hook_registry(Arc::new(hooks)).await;

    let expired_count = gw.expire_idle_sessions_once("idle_expiry").await;

    assert_eq!(expired_count, 1);
    assert!(session_mgr.get_session(&session_key).await.is_none());
    let events = hook_seen.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "on_session_finalize");
    assert_eq!(events[0].1["session_id"], serde_json::json!(expired.id));
    assert_eq!(events[0].1["session_key"], serde_json::json!(session_key));
    assert_eq!(events[0].1["reason"], serde_json::json!("idle_expiry"));
}

#[tokio::test]
async fn gateway_hook_error_does_not_break_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("session:*", Arc::new(FailingHook));

    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    let reset = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-hook-error".into(),
        user_id: "user1".into(),
        text: "/new".into(),
        message_id: None,
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&reset).await.is_ok());

    let replies = sent.lock().unwrap();
    assert!(replies
        .iter()
        .any(|(_, text)| { text.contains("New conversation") || text.contains("Session reset") }));
}
