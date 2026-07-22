#[tokio::test]
async fn shutdown_after_handler_invocation_quarantines_inbound_dedup() {
    let incoming = IncomingMessage {
        platform: "test".into(),
        chat_id: "chat-retry".into(),
        user_id: "user-retry".into(),
        text: "retry me".into(),
        message_id: Some("message-retryable-shutdown".into()),
        thread_id: None,
        is_dm: true,
    };
    let session_mgr = Arc::new(SessionManager::new(SessionConfig::default()));
    let mut dm_manager = DmManager::with_pair_behavior();
    dm_manager.authorize_user("user-retry");
    let gw = Arc::new(Gateway::new(
        session_mgr,
        dm_manager,
        GatewayConfig::default(),
    ));
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Arc::new(parking_lot::Mutex::new(Some(entered_tx)));
    let calls = Arc::new(AtomicUsize::new(0));
    let handler_calls = Arc::clone(&calls);
    let handler_entered = Arc::clone(&entered_tx);
    gw.set_message_handler(Arc::new(move |_| {
        let calls = Arc::clone(&handler_calls);
        let entered = Arc::clone(&handler_entered);
        Box::pin(async move {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                if let Some(tx) = entered.lock().take() {
                    let _ = tx.send(());
                }
                std::future::pending::<()>().await;
            }
            Ok("second attempt succeeds".to_string())
        })
    }))
    .await;

    let first_gateway = Arc::clone(&gw);
    let first_incoming = incoming.clone();
    let first = tokio::spawn(async move {
        first_gateway
            .route_message_from_sender_outcome(&first_incoming, IncomingSender::human())
            .await
    });
    entered_rx.await.expect("first turn enters handler");
    gw.stop_all().await.expect("gateway shutdown");
    assert!(matches!(
        first.await.expect("route task joins").expect("route outcome"),
        GatewayRouteOutcome::Uncertain(GatewayError::SessionExpired(_))
    ));

    // A handler may have invoked arbitrary model/tool side effects before
    // shutdown. The dedup claim stays terminal, so a fresh owner must not
    // replay this raw ingress.
    gw.start_all().await.expect("gateway restart");
    assert!(matches!(
        gw.route_message_from_sender_outcome(&incoming, IncomingSender::human())
            .await
            .expect("dedup outcome"),
        GatewayRouteOutcome::Committed
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

use super::*;

#[tokio::test]
async fn gateway_drains_thirty_three_unique_message_ids_once_without_pending_work() {
    fn incoming_with_id(text: impl Into<String>, message_id: impl Into<String>) -> IncomingMessage {
        IncomingMessage {
            platform: "test".into(),
            chat_id: "chat1".into(),
            user_id: "user1".into(),
            text: text.into(),
            message_id: Some(message_id.into()),
            thread_id: None,
            is_dm: true,
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
    cfg.display.busy_ack_enabled = Some(false);
    let gw = Arc::new(Gateway::new(session_mgr, dm_manager, cfg));
    gw.register_adapter("test", adapter).await;

    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let (first_entered_tx, first_entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_first_tx, release_first_rx) = tokio::sync::oneshot::channel::<()>();
    let (redeliver_entered_tx, redeliver_entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_redeliver_tx, release_redeliver_rx) = tokio::sync::oneshot::channel::<()>();
    let first_entered_tx = Arc::new(Mutex::new(Some(first_entered_tx)));
    let release_first_rx = Arc::new(tokio::sync::Mutex::new(Some(release_first_rx)));
    let redeliver_entered_tx = Arc::new(Mutex::new(Some(redeliver_entered_tx)));
    let release_redeliver_rx = Arc::new(tokio::sync::Mutex::new(Some(release_redeliver_rx)));
    let calls_for_handler = calls.clone();
    let first_entered_for_handler = first_entered_tx.clone();
    let release_first_for_handler = release_first_rx.clone();
    let redeliver_entered_for_handler = redeliver_entered_tx.clone();
    let release_redeliver_for_handler = release_redeliver_rx.clone();
    gw.set_message_handler(Arc::new(move |messages| {
        let calls = calls_for_handler.clone();
        let first_entered = first_entered_for_handler.clone();
        let release_first = release_first_for_handler.clone();
        let redeliver_entered = redeliver_entered_for_handler.clone();
        let release_redeliver = release_redeliver_for_handler.clone();
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
                if let Some(tx) = first_entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release_first.lock().await.take() {
                    let _ = rx.await;
                }
            } else if latest == "redeliver" {
                if let Some(tx) = redeliver_entered.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                if let Some(rx) = release_redeliver.lock().await.take() {
                    let _ = rx.await;
                }
            }
            Ok(format!("reply:{latest}"))
        })
    }))
    .await;

    let first = incoming_with_id("first", "message-first");
    let gw_first = gw.clone();
    let first_task = tokio::spawn(async move {
        gw_first
            .route_message_from_sender(&first, IncomingSender::human())
            .await
    });
    first_entered_rx
        .await
        .expect("first turn should enter the handler");

    let redeliver = incoming_with_id("redeliver", "message-redeliver");
    let redeliver_gateway = gw.clone();
    let redeliver_message = redeliver.clone();
    let redeliver_task = tokio::spawn(async move {
        redeliver_gateway
            .route_message_from_sender(&redeliver_message, IncomingSender::human())
            .await
    });

    let mut queued_routes = Vec::new();
    for index in 0..32 {
        let gateway = gw.clone();
        let incoming =
            incoming_with_id(format!("queued-{index}"), format!("message-queued-{index}"));
        queued_routes.push(tokio::spawn(async move {
            gateway
                .route_message_from_sender(&incoming, IncomingSender::human())
                .await
        }));
    }

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while gw.busy_sessions.read().await.pending_len(&session_key) != 33 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all unique follow-ups should remain owned while queued");
    release_first_tx.send(()).expect("release first handler");
    redeliver_entered_rx
        .await
        .expect("redelivery target should become active");
    let duplicate_gateway = gw.clone();
    let duplicate_message = redeliver.clone();
    let duplicate_task = tokio::spawn(async move {
        duplicate_gateway
            .route_message_from_sender(&duplicate_message, IncomingSender::human())
            .await
    });
    let dedup_key = Gateway::dedup_key(&redeliver).expect("message ID should produce a dedup key");
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
    .expect("redelivery should register as an in-flight dedup waiter");
    release_redeliver_tx
        .send(())
        .expect("release redelivery target");
    duplicate_task
        .await
        .expect("duplicate route task should join")
        .expect("in-flight redelivery should be suppressed");
    redeliver_task
        .await
        .expect("redelivery route task should join")
        .expect("redelivery route should finish");
    for route in queued_routes {
        route
            .await
            .expect("queued route task should join")
            .expect("unique queued message should finish");
    }
    first_task
        .await
        .expect("first route task should join")
        .expect("first route should finish");

    let session_key = gw
        .session_manager
        .compose_session_key("test", "chat1", "user1");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let busy = gw.busy_sessions.read().await;
            if !busy.is_active(&session_key) && busy.pending_len(&session_key) == 0 {
                break;
            }
            drop(busy);
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all queued IDs should drain");

    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for call in calls.lock().unwrap().iter() {
        *counts.entry(call.clone()).or_default() += 1;
    }
    assert_eq!(counts.len(), 34);
    assert!(counts.values().all(|count| *count == 1));
    assert_eq!(counts.get("first"), Some(&1));
    assert_eq!(counts.get("redeliver"), Some(&1));
    for index in 0..32 {
        assert_eq!(counts.get(&format!("queued-{index}")), Some(&1));
    }
}
