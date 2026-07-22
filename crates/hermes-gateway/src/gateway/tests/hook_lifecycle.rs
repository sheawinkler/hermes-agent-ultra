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
    cfg.display.busy_ack_enabled = Some(true);
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
    let gw_second = gw.clone();
    let second_task =
        tokio::spawn(async move { gw_second.route_message(&test_incoming("second")).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second route should remain durably owned while queued");
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    second_task
        .await
        .expect("second task join")
        .expect("second route result");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.is_active(&session_key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("queued turn should drain");

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
async fn failed_automatic_busy_ack_keeps_the_queued_route_owned() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailFirstSendAdapter {
        messages: sends.clone(),
        failures_remaining: std::sync::atomic::AtomicUsize::new(1),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("queue".to_string());
    cfg.display.busy_ack_enabled = Some(true);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter.clone()).await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    gw.set_message_handler(Arc::new(move |messages| {
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|message| {
                    (message.role == MessageRole::User)
                        .then(|| message.content.clone())
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
    let gw_second = gw.clone();
    let second_task =
        tokio::spawn(async move { gw_second.route_message(&test_incoming("second")).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1
            || sends.lock().unwrap().len() != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second route should remain owned after its busy ack fails");
    assert!(!second_task.is_finished());
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert_eq!(
        adapter
            .failures_remaining
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    second_task
        .await
        .expect("second task join")
        .expect("queued route result after failed ack");
}

#[tokio::test]
async fn queued_route_reports_its_own_handler_failure_after_admission() {
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
    gw.set_message_handler(Arc::new(move |messages| {
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|message| {
                    (message.role == MessageRole::User)
                        .then(|| message.content.clone())
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
                Ok("reply:first".to_string())
            } else {
                Err(GatewayError::Platform("queued turn failed".to_string()))
            }
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    let gw_second = gw.clone();
    let second_task =
        tokio::spawn(async move { gw_second.route_message(&test_incoming("second")).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("failed follow-up should remain owned while queued");
    assert!(!second_task.is_finished());

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    let error = second_task
        .await
        .expect("second task join")
        .expect_err("queued handler failure must reach its own route caller");
    assert_eq!(error.to_string(), "Platform error: queued turn failed");
}

#[tokio::test]
async fn reset_waits_for_cancelled_turn_and_preserves_pending_fifo() {
    struct ActiveHandlerGuard(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for ActiveHandlerGuard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

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
    let active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_active = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let calls_for_handler = calls.clone();
    let active_for_handler = active.clone();
    let max_for_handler = max_active.clone();
    let entered_for_handler = entered_tx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let active = active_for_handler.clone();
        let max_active = max_for_handler.clone();
        let entered = entered_for_handler.clone();
        Box::pin(async move {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(current, Ordering::SeqCst);
            let _active_guard = ActiveHandlerGuard(active);
            let latest = messages
                .iter()
                .rev()
                .find_map(|message| {
                    (message.role == MessageRole::User)
                        .then(|| message.content.clone())
                        .flatten()
                })
                .unwrap_or_default();
            calls.lock().unwrap().push(latest.clone());
            if latest == "first" {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                std::future::pending::<()>().await;
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx
        .await
        .expect("first turn should enter the handler");
    let gw_second = gw.clone();
    let second_task =
        tokio::spawn(async move { gw_second.route_message(&test_incoming("second")).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("second turn should remain owned while queued");

    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&test_incoming("/reset")),
    )
    .await
    .expect("reset should cancel and join the active turn")
    .expect("reset should complete");
    first_task
        .await
        .expect("first route task should join")
        .expect("cancelled first route should finish cleanly");
    second_task
        .await
        .expect("second route task should join")
        .expect("reset-promoted queued turn should report success");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.is_active(&session_key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("preserved queued turn should drain after reset");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 1);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let texts: Vec<String> = sent
        .lock()
        .unwrap()
        .iter()
        .map(|(_, text)| text.clone())
        .collect();
    assert!(!texts.iter().any(|text| text == "reply:first"));
    assert!(texts.iter().any(|text| text == "reply:second"));
}

#[tokio::test]
async fn reset_during_reaction_removal_skips_stale_completion_reaction() {
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let (remove_started_tx, remove_started_rx) = tokio::sync::oneshot::channel();
    let release_remove = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(BlockingReactionRemoveAdapter {
        reactions: reactions.clone(),
        remove_started: Mutex::new(Some(remove_started_tx)),
        release_remove: release_remove.clone(),
        completion_started: Mutex::new(None),
        release_completion: None,
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
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
    let gw_route = gw.clone();
    let route_task = tokio::spawn(async move { gw_route.route_message(&incoming).await });
    remove_started_rx
        .await
        .expect("turn should begin removing its start reaction");

    let session_key = gw
        .session_manager
        .compose_session_key("slack", "C123", "user1");
    let (reset_lease, revoked_lease, released_dedup_keys) = {
        let mut busy = gw.busy_sessions.write().await;
        match busy.begin_reset_session(&session_key) {
            ResetAdmission::Started {
                lease,
                revoked_lease,
                released_dedup_keys,
            } => (lease, revoked_lease, released_dedup_keys),
            _ => panic!("test must install a reset barrier"),
        }
    };
    assert!(revoked_lease.is_some());
    gw.complete_dedup_keys(&released_dedup_keys).await;
    release_remove.notify_one();

    route_task
        .await
        .expect("route task should join")
        .expect("cancelled turn should finish cleanly");
    if let Some(revoked_lease) = revoked_lease {
        revoked_lease.wait_finished().await;
    }
    gw.release_reset_barrier(&reset_lease)
        .await
        .expect("test reset barrier should release");

    assert_eq!(
        reactions.lock().unwrap().as_slice(),
        [
            "add:C123:1710000000.123:eyes",
            "remove:C123:1710000000.123:eyes"
        ]
    );
}

#[tokio::test]
async fn reset_during_terminal_reaction_removes_the_stale_result_before_commit() {
    let reactions = Arc::new(Mutex::new(Vec::new()));
    let release_remove = Arc::new(tokio::sync::Notify::new());
    release_remove.notify_one();
    let (completion_started_tx, completion_started_rx) = tokio::sync::oneshot::channel();
    let release_completion = Arc::new(tokio::sync::Notify::new());
    let adapter = Arc::new(BlockingReactionRemoveAdapter {
        reactions: reactions.clone(),
        remove_started: Mutex::new(None),
        release_remove: release_remove.clone(),
        completion_started: Mutex::new(Some(completion_started_tx)),
        release_completion: Some(release_completion.clone()),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
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
        message_id: Some("1710000000.124".into()),
        thread_id: None,
        is_dm: true,
    };
    let gw_route = gw.clone();
    let route_task = tokio::spawn(async move { gw_route.route_message(&incoming).await });
    completion_started_rx
        .await
        .expect("turn should begin its terminal reaction before lane commit");

    let session_key = gw
        .session_manager
        .compose_session_key("slack", "C123", "user1");
    let (reset_lease, revoked_lease, released_dedup_keys) = {
        let mut busy = gw.busy_sessions.write().await;
        match busy.begin_reset_session(&session_key) {
            ResetAdmission::Started {
                lease,
                revoked_lease,
                released_dedup_keys,
            } => (lease, revoked_lease, released_dedup_keys),
            _ => panic!("test must install a reset barrier"),
        }
    };
    assert!(revoked_lease.is_some());
    gw.complete_dedup_keys(&released_dedup_keys).await;
    release_remove.notify_one();
    release_completion.notify_one();

    route_task
        .await
        .expect("route task should join")
        .expect("cancelled turn should finish cleanly");
    if let Some(revoked_lease) = revoked_lease {
        revoked_lease.wait_finished().await;
    }
    gw.release_reset_barrier(&reset_lease)
        .await
        .expect("test reset barrier should release");

    assert_eq!(
        reactions.lock().unwrap().as_slice(),
        [
            "add:C123:1710000000.124:eyes",
            "remove:C123:1710000000.124:eyes",
            "add:C123:1710000000.124:white_check_mark",
            "remove:C123:1710000000.124:white_check_mark"
        ]
    );
}

#[tokio::test]
async fn gateway_mutation_guard_blocks_competing_lane_admission() {
    let gw = Gateway::with_defaults(
        Arc::new(SessionManager::new(SessionConfig::default())),
        GatewayConfig::default(),
    );
    let session_key = gw
        .session_manager
        .compose_session_key("test", "admission-race", "user1");
    let mutation_guard = gw
        .try_lock_session_mutation(&session_key)
        .await
        .expect("idle session mutation should acquire the lane");

    let contender = gw.clone();
    let contender_key = session_key.clone();
    let admission_task = tokio::spawn(async move {
        contender
            .claim_session_lane(
                &contender_key,
                QueuedIncoming::from_validated_ingress(
                    IncomingMessage {
                        platform: "test".into(),
                        chat_id: "admission-race".into(),
                        user_id: "user1".into(),
                        text: "after mutation".into(),
                        message_id: Some("admission-race-1".into()),
                        thread_id: None,
                        is_dm: true,
                    },
                    IncomingSender::human(),
                ),
                BusyInputMode::Queue,
            )
            .await
    });
    tokio::task::yield_now().await;
    assert!(!admission_task.is_finished());
    assert!(!gw.busy_sessions.read().await.is_active(&session_key));

    drop(mutation_guard);
    let admission = tokio::time::timeout(std::time::Duration::from_secs(2), admission_task)
        .await
        .expect("admission should resume after mutation")
        .expect("admission task should join");
    let BusyAdmission::Claimed { lease, .. } = admission else {
        panic!("the sole contender must claim the idle lane");
    };
    assert!(gw.busy_sessions.read().await.is_current(&lease));
    let completion = gw
        .busy_sessions
        .write()
        .await
        .complete_turn(&lease, ProcessingOutcome::Cancelled);
    assert!(matches!(
        completion,
        TurnCompletion::Completed { next: None }
    ));
}

#[tokio::test]
async fn failed_mutation_busy_reply_releases_command_dedup_for_redelivery() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailFirstSendAdapter {
        messages: sends.clone(),
        failures_remaining: std::sync::atomic::AtomicUsize::new(1),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter.clone()).await;

    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    gw.set_message_handler(Arc::new(move |_messages| {
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
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
    entered_rx.await.expect("first route should hold the lane");
    let mut command = test_incoming("/verbose");
    command.message_id = Some("mutation-busy-redelivery".to_string());
    let error = gw
        .route_message(&command)
        .await
        .expect_err("first busy reply should hit the failing adapter");
    assert!(error
        .to_string()
        .contains("deterministic first send failure"));
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert_eq!(
        adapter
            .failures_remaining
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    gw.route_message(&command)
        .await
        .expect("redelivery should retry the previously failed busy reply");
    let attempts = sends.lock().unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts
        .iter()
        .all(|(_, text)| text.contains("Session is busy")));
    drop(attempts);

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
}

#[tokio::test]
async fn cancelled_yolo_ack_does_not_reapply_the_committed_toggle() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let (ack_started_tx, ack_started_rx) = tokio::sync::oneshot::channel();
    let adapter = Arc::new(BlockingFirstSendAdapter {
        messages: sends.clone(),
        block_first: std::sync::atomic::AtomicBool::new(true),
        started: Mutex::new(Some(ack_started_tx)),
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

    let mut command = test_incoming("/yolo");
    command.message_id = Some("cancelled-yolo-ack".to_string());
    let dedup_key = Gateway::dedup_key(&command).expect("message ID should produce a dedup key");
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let gw_command = gw.clone();
    let command_for_task = command.clone();
    let command_task =
        tokio::spawn(async move { gw_command.route_message(&command_for_task).await });
    ack_started_rx
        .await
        .expect("/yolo acknowledgement should enter the blocking adapter");
    assert!(gw
        .runtime_state
        .read()
        .await
        .get(&session_key)
        .is_some_and(|state| state.yolo));
    assert!(gw
        .message_deduplicator
        .lock()
        .completed
        .contains(&dedup_key));

    command_task.abort();
    assert!(command_task
        .await
        .expect_err("blocked /yolo route should be cancelled")
        .is_cancelled());
    gw.route_message(&command)
        .await
        .expect("redelivered /yolo must remain deduplicated");
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert!(gw
        .runtime_state
        .read()
        .await
        .get(&session_key)
        .is_some_and(|state| state.yolo));
    hermes_tools::approval::disable_session_yolo(&session_key);
}

#[tokio::test]
async fn gateway_failed_delivery_releases_dedup_claim_for_redelivery() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailingSendAdapter {
        messages: sent.clone(),
        error: "forbidden",
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let attempts = handler_attempts.clone();
        Box::pin(async move {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(GatewayError::Platform(
                "deterministic handler failure".into(),
            ))
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "dedup-retry".into(),
        user_id: "user1".into(),
        text: "retry me".into(),
        message_id: Some("delivery-1".into()),
        thread_id: None,
        is_dm: true,
    };
    let outcome = gw
        .route_message_from_sender_outcome(&incoming, IncomingSender::human())
        .await
        .expect("reliably rejected notification should produce a route outcome");
    assert!(matches!(
        outcome,
        GatewayRouteOutcome::Retryable {
            error: GatewayError::Platform(error),
            retry_after: None,
        } if error == "deterministic handler failure"
    ));
    assert!(gw.route_message(&incoming).await.is_err());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[tokio::test]
async fn gateway_inflight_redelivery_is_suppressed_after_owner_error_notification() {
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

    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let handler_attempts = attempts.clone();
    let handler_entered = entered_tx.clone();
    let handler_release = release_rx.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let attempt = handler_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let entered = handler_entered.clone();
        let release = handler_release.clone();
        Box::pin(async move {
            if attempt == 0 {
                if let Some(sender) = entered.lock().unwrap().take() {
                    let _ = sender.send(());
                }
                if let Some(receiver) = release.lock().await.take() {
                    let _ = receiver.await;
                }
                Err(GatewayError::Platform("first owner failed".into()))
            } else {
                Ok("retry succeeded".to_string())
            }
        })
    }))
    .await;
    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "inflight-redelivery".into(),
        user_id: "user1".into(),
        text: "coalesce me".into(),
        message_id: Some("coalesced-delivery-1".into()),
        thread_id: None,
        is_dm: true,
    };

    let first_gateway = gw.clone();
    let first_incoming = incoming.clone();
    let first = tokio::spawn(async move { first_gateway.route_message(&first_incoming).await });
    entered_rx.await.expect("first owner should enter handler");
    let second_gateway = gw.clone();
    let second_incoming = incoming.clone();
    let second = tokio::spawn(async move { second_gateway.route_message(&second_incoming).await });
    tokio::task::yield_now().await;
    assert!(!second.is_finished());

    release_tx.send(()).expect("release first owner");
    assert!(first.await.expect("first route should join").is_err());
    tokio::time::timeout(std::time::Duration::from_secs(2), second)
        .await
        .expect("waiting duplicate should resolve after owner completion")
        .expect("second route should join")
        .expect("accepted error notification should suppress same-ID redelivery");
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("first owner failed")));
}

#[tokio::test]
async fn gateway_turn_timeout_releases_lane_and_quarantines_dedup_after_notification() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut config = GatewayConfig::default();
    config.turn_timeout_secs = 0;
    let gw = Gateway::new(session_mgr, dm_manager, config);
    gw.register_adapter("test", adapter).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let control = Arc::new(BusyControlProbe::default());
    let handler_attempts = attempts.clone();
    let handler_control = control.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let attempt = handler_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let control = handler_control.clone();
        Box::pin(async move {
            if attempt == 0 {
                let registration = ctx
                    .busy_control
                    .expect("active turn should expose control registration");
                assert!(registration.attach(control).await);
                std::future::pending::<Result<String, GatewayError>>().await
            } else {
                Ok("retry after timeout".to_string())
            }
        })
    }))
    .await;
    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "turn-timeout".into(),
        user_id: "user1".into(),
        text: "do not wedge".into(),
        message_id: Some("turn-timeout-1".into()),
        thread_id: None,
        is_dm: true,
    };

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&incoming),
    )
    .await
    .expect("turn timeout must remain bounded")
    .expect_err("first turn should time out");
    assert!(error.to_string().contains("timed out"));
    assert_eq!(control.interrupts.lock().unwrap().len(), 1);
    gw.route_message(&incoming)
        .await
        .expect("accepted timeout notification should suppress same-ID redelivery");
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("gateway turn timed out")));
}

#[tokio::test]
async fn gateway_background_ack_failure_does_not_cancel_owned_work() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailingSendAdapter {
        messages: sends,
        error: "deterministic background ack failure",
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        handler_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok("owned work completed".to_string()) })
    }))
    .await;

    let mut incoming = test_incoming("/background guarded work");
    incoming.message_id = Some("background-ack-failure-1".to_string());
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&incoming),
    )
    .await
    .expect("ack failure must not strand the caller")
    .expect("ack failure is non-fatal after work ownership transfers");
    gw.route_message(&incoming)
        .await
        .expect("same-ID redelivery should be suppressed");

    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if gw
                .background_tasks
                .list_tasks()
                .iter()
                .any(|(_, status, _)| matches!(status, TaskStatus::Completed))
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned background work should finish despite ack failure");
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(gw.background_tasks.list_tasks().len(), 1);
}

#[tokio::test]
async fn gateway_background_without_handler_does_not_consume_capacity() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", Arc::new(TestAdapter { messages: sent }))
        .await;

    let result = gw
        .route_message(&test_incoming("/background missing handler"))
        .await;
    assert!(result.is_err());
    assert!(gw.background_tasks.list_tasks().is_empty());
}
#[tokio::test]
async fn gateway_handler_panic_releases_lane_and_quarantines_same_id() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let attempts = handler_attempts.clone();
        Box::pin(async move {
            attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            panic!("deterministic message handler panic");
            #[allow(unreachable_code)]
            Ok(String::new())
        })
    }))
    .await;

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "panic-retry".into(),
        user_id: "user1".into(),
        text: "retry after panic".into(),
        message_id: Some("panic-delivery-1".into()),
        thread_id: None,
        is_dm: true,
    };
    assert!(gw.route_message(&incoming).await.is_err());
    let session_key = gw
        .session_manager
        .compose_session_key("test", "panic-retry", "user1");
    assert!(!gw.busy_sessions.read().await.is_active(&session_key));
    assert!(
        gw.route_message(&incoming).await.is_ok(),
        "accepted panic notification should suppress same-ID redelivery"
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text.contains("gateway turn handler panicked")));
}

#[tokio::test]
async fn gateway_error_notification_panic_cannot_wedge_lane_and_quarantines_dedup() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.register_adapter("test", Arc::new(PanickingSendAdapter))
        .await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        handler_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move {
            panic!("deterministic handler panic before notification");
            #[allow(unreachable_code)]
            Ok(String::new())
        })
    }))
    .await;
    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "notification-panic".into(),
        user_id: "user1".into(),
        text: "fail safely".into(),
        message_id: Some("notification-panic-1".into()),
        thread_id: None,
        is_dm: true,
    };

    let first = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&incoming),
    )
    .await
    .expect("notification panic must not orphan the first caller");
    assert!(first.is_err());
    let session_key = gw
        .session_manager
        .compose_session_key("test", "notification-panic", "user1");
    assert!(!gw.busy_sessions.read().await.is_active(&session_key));
    let second = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&incoming),
    )
    .await
    .expect("notification panic must not wedge the duplicate caller");
    assert!(
        second.is_ok(),
        "an ambiguous notification must retain terminal dedup ownership"
    );
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn gateway_committed_response_failure_suppresses_same_id_redelivery() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailingSendAdapter {
        messages: sent,
        error: "deterministic delivery failure",
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr.clone(), dm_manager, GatewayConfig::default());
    gw.register_adapter("test", adapter).await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let handler_attempts = attempts.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        handler_attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { Ok("committed response".to_string()) })
    }))
    .await;
    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "committed-failure".into(),
        user_id: "user1".into(),
        text: "deliver once".into(),
        message_id: Some("committed-delivery-1".into()),
        thread_id: None,
        is_dm: true,
    };

    assert!(gw.route_message(&incoming).await.is_err());
    assert!(gw.route_message(&incoming).await.is_ok());
    assert_eq!(attempts.load(std::sync::atomic::Ordering::SeqCst), 1);
    let session_key = session_mgr.compose_session_key("test", "committed-failure", "user1");
    let messages = session_mgr.get_messages(&session_key).await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
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
    let gw_second = gw.clone();
    let second_task =
        tokio::spawn(async move { gw_second.route_message(&test_incoming("second")).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1 {
            tokio::task::yield_now().await;
        }
    })
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
    second_task
        .await
        .expect("second task join")
        .expect("second route result");
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
    let mut queue_command = test_incoming("/queue second");
    queue_command.message_id = Some("queue-command-1".to_string());
    gw.route_message(&queue_command)
        .await
        .expect("/queue should bypass and enqueue");
    let dedup_key =
        Gateway::dedup_key(&queue_command).expect("message ID should produce a dedup key");
    let gw_redelivery = gw.clone();
    let redelivered_command = queue_command.clone();
    let redelivery_task =
        tokio::spawn(async move { gw_redelivery.route_message(&redelivered_command).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiter_registered = gw
                .message_deduplicator
                .lock()
                .claimed
                .get(&dedup_key)
                .is_some_and(|waiters| !waiters.is_empty());
            if waiter_registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("redelivered /queue should wait on the queued owner");
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    assert_eq!(gw.busy_sessions.read().await.pending_len(&session_key), 1);
    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    redelivery_task
        .await
        .expect("redelivered /queue task join")
        .expect("redelivered /queue command must be deduplicated");

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
async fn failed_queue_command_ack_does_not_duplicate_owned_work() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailFirstSendAdapter {
        messages: sends.clone(),
        failures_remaining: std::sync::atomic::AtomicUsize::new(1),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_input_mode = Some("interrupt".to_string());
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter.clone()).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|message| {
                    (message.role == MessageRole::User)
                        .then(|| message.content.clone())
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
    let mut command = test_incoming("/queue second");
    command.message_id = Some("failed-queue-ack".to_string());
    gw.route_message(&command)
        .await
        .expect("owned /queue must survive its acknowledgement failure");
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert_eq!(
        adapter
            .failures_remaining
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    let dedup_key = Gateway::dedup_key(&command).expect("message ID should produce a dedup key");
    let gw_redelivery = gw.clone();
    let redelivered_command = command.clone();
    let redelivery_task =
        tokio::spawn(async move { gw_redelivery.route_message(&redelivered_command).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiter_registered = gw
                .message_deduplicator
                .lock()
                .claimed
                .get(&dedup_key)
                .is_some_and(|waiters| !waiters.is_empty());
            if waiter_registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("redelivered /queue should wait after acknowledgement failure");
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    assert_eq!(gw.busy_sessions.read().await.pending_len(&session_key), 1);

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    redelivery_task
        .await
        .expect("redelivered /queue task join")
        .expect("redelivered /queue must resolve with its owned work");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.is_active(&session_key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("owned queued command should drain");
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn cancelled_queue_ack_keeps_dedup_with_the_owned_work() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let (ack_started_tx, ack_started_rx) = tokio::sync::oneshot::channel();
    let adapter = Arc::new(BlockingFirstSendAdapter {
        messages: sends,
        block_first: std::sync::atomic::AtomicBool::new(true),
        started: Mutex::new(Some(ack_started_tx)),
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

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            let latest = messages
                .iter()
                .rev()
                .find_map(|message| {
                    (message.role == MessageRole::User)
                        .then(|| message.content.clone())
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
    let mut command = test_incoming("/queue second");
    command.message_id = Some("cancelled-queue-ack".to_string());
    let gw_command = gw.clone();
    let command_for_task = command.clone();
    let command_task =
        tokio::spawn(async move { gw_command.route_message(&command_for_task).await });
    ack_started_rx
        .await
        .expect("/queue acknowledgement should enter the blocking adapter");
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    assert_eq!(gw.busy_sessions.read().await.pending_len(&session_key), 1);
    command_task.abort();
    assert!(command_task
        .await
        .expect_err("blocked /queue route should be cancelled")
        .is_cancelled());

    let dedup_key = Gateway::dedup_key(&command).expect("message ID should produce a dedup key");
    let gw_redelivery = gw.clone();
    let redelivered_command = command.clone();
    let redelivery_task =
        tokio::spawn(async move { gw_redelivery.route_message(&redelivered_command).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiter_registered = gw
                .message_deduplicator
                .lock()
                .claimed
                .get(&dedup_key)
                .is_some_and(|waiters| !waiters.is_empty());
            if waiter_registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("redelivery should wait on the transferred dedup claim");
    assert!(!redelivery_task.is_finished());
    assert_eq!(gw.busy_sessions.read().await.pending_len(&session_key), 1);

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    redelivery_task
        .await
        .expect("redelivered /queue task join")
        .expect("redelivery should resolve with the originally owned work");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.is_active(&session_key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("original queued work should drain after route cancellation");
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["first".to_string(), "second".to_string()]
    );
}

#[tokio::test]
async fn failed_retry_ack_keeps_one_retry_owned_and_deduplicated() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailFirstSendAdapter {
        messages: sends.clone(),
        failures_remaining: std::sync::atomic::AtomicUsize::new(1),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let mut cfg = GatewayConfig::default();
    cfg.display.busy_ack_enabled = Some(true);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter.clone()).await;

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let calls_for_handler = calls.clone();
    gw.set_message_handler(Arc::new(move |_messages| {
        let invocation = calls_for_handler.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let entered = entered_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            if invocation == 0 {
                if let Some(tx) = entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{}", invocation + 1))
        })
    }))
    .await;

    let gw_first = gw.clone();
    let first_task =
        tokio::spawn(async move { gw_first.route_message(&test_incoming("first")).await });
    entered_rx.await.expect("first route should enter handler");
    let mut retry = test_incoming("/retry");
    retry.message_id = Some("failed-retry-ack".to_string());
    let gw_retry = gw.clone();
    let retry_for_task = retry.clone();
    let retry_task = tokio::spawn(async move { gw_retry.route_message(&retry_for_task).await });
    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 1
            || sends.lock().unwrap().len() != 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry should remain queued after its acknowledgement fails");
    assert!(!retry_task.is_finished());
    assert_eq!(
        adapter
            .failures_remaining
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );

    let dedup_key = Gateway::dedup_key(&retry).expect("message ID should produce a dedup key");
    let gw_redelivery = gw.clone();
    let redelivered_retry = retry.clone();
    let redelivery_task =
        tokio::spawn(async move { gw_redelivery.route_message(&redelivered_retry).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiter_registered = gw
                .message_deduplicator
                .lock()
                .claimed
                .get(&dedup_key)
                .is_some_and(|waiters| !waiters.is_empty());
            if waiter_registered {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("retry redelivery should wait on the owned dedup claim");

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
    retry_task
        .await
        .expect("retry route task join")
        .expect("queued retry should report its own completion");
    redelivery_task
        .await
        .expect("retry redelivery task join")
        .expect("retry redelivery should be deduplicated");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(!gw.busy_sessions.read().await.is_active(&session_key));
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
    let mut steer_command = test_incoming("/steer check tests");
    steer_command.message_id = Some("steer-command-1".to_string());
    gw.route_message(&steer_command)
        .await
        .expect("/steer should use attached control");
    gw.route_message(&steer_command)
        .await
        .expect("redelivered /steer command must be deduplicated");
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
async fn cancelled_steer_ack_keeps_the_owned_side_effect_deduplicated() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let (ack_started_tx, ack_started_rx) = tokio::sync::oneshot::channel();
    let adapter = Arc::new(BlockingFirstSendAdapter {
        messages: sends.clone(),
        block_first: std::sync::atomic::AtomicBool::new(true),
        started: Mutex::new(Some(ack_started_tx)),
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
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let control = control_for_handler.clone();
        let entered = entered_tx.clone();
        let release = release_rx.clone();
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
    let mut command = test_incoming("/steer check tests");
    command.message_id = Some("cancelled-steer-ack".to_string());
    let dedup_key = Gateway::dedup_key(&command).expect("message ID should produce a dedup key");
    let gw_command = gw.clone();
    let command_for_task = command.clone();
    let command_task =
        tokio::spawn(async move { gw_command.route_message(&command_for_task).await });
    ack_started_rx
        .await
        .expect("/steer acknowledgement should enter the blocking adapter");
    assert!(gw
        .message_deduplicator
        .lock()
        .completed
        .contains(&dedup_key));
    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );
    command_task.abort();
    assert!(command_task
        .await
        .expect_err("blocked /steer route should be cancelled")
        .is_cancelled());

    gw.route_message(&command)
        .await
        .expect("redelivered /steer must remain deduplicated after cancellation");
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
}

#[tokio::test]
async fn failed_steer_command_ack_does_not_repeat_the_owned_steer() {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(FailFirstSendAdapter {
        messages: sends.clone(),
        failures_remaining: std::sync::atomic::AtomicUsize::new(1),
    });
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.register_adapter("test", adapter.clone()).await;

    let control = Arc::new(BusyControlProbe::default());
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let entered_tx = Arc::new(Mutex::new(Some(entered_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let control_for_handler = control.clone();
    gw.set_message_handler_with_context(Arc::new(move |_messages, ctx| {
        let control = control_for_handler.clone();
        let entered = entered_tx.clone();
        let release = release_rx.clone();
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
    let mut command = test_incoming("/steer check tests");
    command.message_id = Some("failed-steer-ack".to_string());
    gw.route_message(&command)
        .await
        .expect("owned /steer must survive its acknowledgement failure");
    assert_eq!(sends.lock().unwrap().len(), 1);
    assert_eq!(
        adapter
            .failures_remaining
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    gw.route_message(&command)
        .await
        .expect("redelivered /steer must be deduplicated");
    assert_eq!(
        control.steers.lock().unwrap().as_slice(),
        ["check tests".to_string()]
    );

    release_tx.send(()).expect("release first route");
    first_task
        .await
        .expect("first task join")
        .expect("first route result");
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
async fn gateway_stale_reset_command_finalizes_original_identity_once() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let hook_seen = Arc::new(Mutex::new(Vec::new()));
    let mut hooks = HookRegistry::new();
    for event in ["on_session_finalize", "on_session_reset"] {
        hooks.register_in_process(
            event,
            Arc::new(RecordingHook {
                seen: hook_seen.clone(),
            }),
        );
    }
    let session_mgr = Arc::new(SessionManager::new(SessionConfig {
        reset_policy: hermes_config::session::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    }));
    let original = session_mgr
        .get_or_create_session("test", "stale-reset", "user1")
        .await;
    session_mgr
        .add_message(
            &session_mgr.compose_session_key("test", "stale-reset", "user1"),
            Message::user("old history"),
        )
        .await;
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    gw.set_hook_registry(Arc::new(hooks)).await;
    gw.register_adapter("test", adapter).await;

    gw.route_message(&IncomingMessage {
        platform: "test".into(),
        chat_id: "stale-reset".into(),
        user_id: "user1".into(),
        text: "/reset".into(),
        message_id: Some("stale-reset-delivery".into()),
        thread_id: None,
        is_dm: true,
    })
    .await
    .expect("stale reset command should complete");

    let events = hook_seen.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, "on_session_finalize");
    assert_eq!(events[0].1["session_id"], serde_json::json!(original.id));
    assert_eq!(events[0].1["reason"], serde_json::json!("reset"));
    assert_eq!(events[1].0, "on_session_reset");
    assert_eq!(events[1].1["reason"], serde_json::json!("reset"));
    assert_ne!(events[1].1["session_id"], events[0].1["session_id"]);
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
    gw.runtime_state
        .write()
        .await
        .entry(session_key.clone())
        .or_default()
        .yolo = true;
    gw.runtime_state
        .write()
        .await
        .get_mut(&session_key)
        .expect("runtime state")
        .model = Some("stale-model".to_string());
    gw.usage_stats
        .write()
        .await
        .entry(session_key.clone())
        .or_default()
        .user_messages = 7;

    let expired_count = gw.expire_idle_sessions_once("idle_expiry").await;

    assert_eq!(expired_count, 1);
    assert!(session_mgr.get_session(&session_key).await.is_none());
    assert!(!gw.runtime_state.read().await.contains_key(&session_key));
    assert!(!gw.usage_stats.read().await.contains_key(&session_key));
    let reentered = session_mgr
        .get_or_create_session("test", "idle-chat", "user1")
        .await;
    assert_ne!(reentered.id, expired.id);
    assert!(!gw.runtime_state.read().await.contains_key(&session_key));
    assert!(!gw.usage_stats.read().await.contains_key(&session_key));
    let events = hook_seen.lock().unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, "on_session_finalize");
    assert_eq!(events[0].1["session_id"], serde_json::json!(expired.id));
    assert_eq!(events[0].1["session_key"], serde_json::json!(session_key));
    assert_eq!(events[0].1["reason"], serde_json::json!("idle_expiry"));
}

#[tokio::test]
async fn cancelled_idle_expiry_caller_cannot_orphan_the_session_lane() {
    let (hook_started_tx, hook_started_rx) = tokio::sync::oneshot::channel();
    let release_hook = Arc::new(tokio::sync::Notify::new());
    let mut hooks = HookRegistry::new();
    hooks.register_in_process(
        "on_session_finalize",
        Arc::new(BlockingHook {
            started: Mutex::new(Some(hook_started_tx)),
            release: release_hook.clone(),
        }),
    );
    let session_config = SessionConfig {
        reset_policy: hermes_config::session::SessionResetPolicy::Idle { timeout_minutes: 0 },
        ..SessionConfig::default()
    };
    let session_mgr = Arc::new(SessionManager::new(session_config));
    session_mgr
        .get_or_create_session("test", "idle-cancel", "user1")
        .await;
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    gw.set_hook_registry(Arc::new(hooks)).await;
    let sent = Arc::new(Mutex::new(Vec::new()));
    gw.register_adapter(
        "test",
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("after-expiry".to_string()) })
    }))
    .await;

    let gw_expiry = gw.clone();
    let expiry_task =
        tokio::spawn(async move { gw_expiry.expire_idle_sessions_once("idle_expiry").await });
    hook_started_rx
        .await
        .expect("idle-expiry worker should enter the finalization hook");
    expiry_task.abort();
    assert!(expiry_task
        .await
        .expect_err("aborted expiry caller should not return a count")
        .is_cancelled());

    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "idle-cancel".into(),
        user_id: "user1".into(),
        text: "new work".into(),
        message_id: Some("after-expiry-1".into()),
        thread_id: None,
        is_dm: true,
    };
    let gw_route = gw.clone();
    let route_task = tokio::spawn(async move { gw_route.route_message(&incoming).await });
    tokio::task::yield_now().await;
    assert!(!route_task.is_finished());

    release_hook.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(2), route_task)
        .await
        .expect("queued ingress should resume after detached expiry cleanup")
        .expect("route task should join")
        .expect("new ingress should complete");
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text == "after-expiry"));
}

#[tokio::test]
async fn gateway_reset_worker_panic_releases_barrier_for_normal_ingress() {
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user1");
    let gw = Gateway::new(session_mgr, dm_manager, GatewayConfig::default());
    let sent = Arc::new(Mutex::new(Vec::new()));
    gw.register_adapter(
        "test",
        Arc::new(TestAdapter {
            messages: sent.clone(),
        }),
    )
    .await;
    gw.set_message_handler(Arc::new(|_messages| {
        Box::pin(async { Ok("normal ingress recovered".to_string()) })
    }))
    .await;

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    let reset_lease = {
        let mut busy = gw.busy_sessions.write().await;
        let ResetAdmission::Started { lease, .. } = busy.begin_reset_session(&session_key) else {
            panic!("test must install a reset barrier");
        };
        lease
    };
    let reset_result = gw
        .run_reset_barrier_work(&reset_lease, async {
            panic!("deterministic post-admission reset panic");
            #[allow(unreachable_code)]
            Ok(())
        })
        .await;
    assert!(reset_result.is_err());
    assert!(!gw.busy_sessions.read().await.is_current(&reset_lease));
    tokio::time::timeout(
        std::time::Duration::from_secs(2),
        gw.route_message(&test_incoming("after reset panic")),
    )
    .await
    .expect("normal ingress must not wait behind a stale reset barrier")
    .expect("normal ingress should recover");
    assert!(sent
        .lock()
        .unwrap()
        .iter()
        .any(|(_, text)| text == "normal ingress recovered"));
}

#[tokio::test]
async fn gateway_hook_error_does_not_break_reset_command() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let adapter = Arc::new(TestAdapter {
        messages: sent.clone(),
    });
    let mut hooks = HookRegistry::new();
    hooks.register_in_process("session:*", Arc::new(FailingHook));
    hooks.register_in_process("session:*", Arc::new(PanickingHook));

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
